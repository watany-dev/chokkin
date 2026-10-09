//! Project probe orchestration (pipeline steps 1–4).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::VERSION;
use crate::cache::CacheOptions;
use crate::config::{
    ChokkinConfig, ConfigSources, DependencyGroupsConfig, ResolvedWorkspaceMember,
    RuntimeOverrides, TargetVersion, apply_overrides, load_config, load_root_config,
};
use crate::discovery::{ProjectRoot, RootMarker, discover_project_root};
use crate::manifest::{
    DeclaredDependency, InlineScript, LoadedManifest, discover_inline_scripts,
    extract_manifest_with_cache, normalize_distribution_name, resolve_target_version,
};
use crate::plugins::{
    EnablerScope, PluginActivation, PluginActivationReason, resolve_plugin_activations,
};
use crate::rules::deps::{DeclarationBucket, declaration_buckets};
use crate::sources::{
    DiscoveredSources, FileContext, FileKind, MemberLayout, apply_member_docs_context,
    discover_sources,
};

use super::error::ProbeError;
use super::warnings::{ProbeWarning, collect_warnings};

/// Outcome of running pipeline steps 1–4 for CLI probe output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReport {
    /// Crate version string.
    pub version: &'static str,
    /// Resolved project root.
    pub root: ProjectRoot,
    /// Which configuration files contributed.
    pub config_sources: ConfigSources,
    /// Effective configuration after overrides and target resolution.
    pub effective_config: ChokkinConfig,
    /// Extracted manifest metadata and dependencies.
    pub manifest: LoadedManifest,
    /// Discovered source files and layout.
    pub sources: DiscoveredSources,
    /// Resolved workspace members below the project root.
    pub workspace_members: Vec<ResolvedWorkspaceMember>,
    /// Member-scoped manifest and source inventories.
    pub workspace_inputs: Vec<WorkspaceMemberInputs>,
    /// Whether `workspace_members` were inferred from nested `pyproject.toml`
    /// files rather than declared (#488).
    pub auto_workspace: bool,
    /// PEP 723 scripts among the discovered Python files.
    pub scripts: Vec<InlineScript>,
    /// Why each plugin is on or off; already applied to `effective_config.plugins`.
    pub plugin_activations: Vec<PluginActivation>,
    /// Non-fatal warnings from manifest and source discovery.
    pub warnings: Vec<ProbeWarning>,
}

/// Probe data for one resolved workspace member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceMemberInputs {
    /// Resolved workspace member metadata.
    pub member: ResolvedWorkspaceMember,
    /// Member-local manifest extraction.
    pub manifest: LoadedManifest,
    /// Member-local source inventory.
    pub sources: DiscoveredSources,
}

/// Run pipeline steps 1–4 and collect a probe report.
///
/// # Errors
///
/// Returns `ProbeError` when a pipeline step fails fatally.
pub fn probe_project(
    start: &Path,
    project_root_override: Option<&Path>,
    overrides: &RuntimeOverrides,
) -> Result<ProbeReport, ProbeError> {
    probe_project_with_cache(start, project_root_override, overrides, None)
}

/// Run pipeline steps 1–4 with optional manifest cache support.
///
/// # Errors
///
/// Returns [`ProbeError`] when a pipeline step fails fatally.
pub(super) fn probe_project_with_cache(
    start: &Path,
    project_root_override: Option<&Path>,
    overrides: &RuntimeOverrides,
    cache: Option<&CacheOptions>,
) -> Result<ProbeReport, ProbeError> {
    let discovery_start = project_root_override.unwrap_or(start);
    let canonical_start = canonicalize_path(discovery_start)?;

    let root = discover_project_root(&canonical_start)?;
    let mut loaded = load_root_config(&root, overrides.no_auto_workspace != Some(true))?;
    apply_overrides(&mut loaded.effective, overrides);

    let manifest = extract_manifest_with_cache(&root, &loaded, cache)?;
    let target_version = resolve_target_version(&loaded.effective, &manifest);
    loaded.effective.target_version = Some(target_version);

    let mut sources = discover_sources(&root, &loaded, &manifest)?;
    let member_count = loaded.workspace_members.len();
    // The manifest cache lives under each member root; an undeclared monorepo
    // would gain hundreds of untracked `.chokkin/` directories.
    let member_cache = if loaded.auto_workspace { None } else { cache };
    let mut workspace_inputs =
        collect_workspace_inputs(&root, &loaded.workspace_members, overrides, member_cache)?;
    if loaded.effective.production {
        drop_dev_only_members(
            &mut sources,
            &mut loaded.workspace_members,
            &mut workspace_inputs,
            &manifest,
            &loaded.effective.dependencies,
        );
    }
    sources.layout.members = workspace_inputs
        .iter()
        .map(|input| MemberLayout {
            path: input.member.path.clone(),
            layout: input.sources.layout.clone(),
        })
        .collect();
    apply_member_docs_context(&mut sources, loaded.effective.production);
    let (scripts, script_warnings) = discover_inline_scripts(
        &root.path,
        sources.python_files().map(|file| file.path.as_str()),
    );
    let mut warnings = collect_warnings(&manifest, &sources);
    warnings.extend(script_warnings.into_iter().map(ProbeWarning::Manifest));
    if loaded.auto_workspace {
        warnings.push(ProbeWarning::AutoWorkspace { member_count });
    }
    let plugin_activations = activate_plugins(&mut loaded.effective, &manifest, &workspace_inputs);

    Ok(ProbeReport {
        version: VERSION,
        root,
        config_sources: loaded.sources,
        effective_config: loaded.effective,
        manifest,
        sources,
        workspace_members: loaded.workspace_members,
        workspace_inputs,
        auto_workspace: loaded.auto_workspace,
        scripts,
        plugin_activations,
        warnings,
    })
}

/// Drop members that only dependency groups pull in, such as airflow's
/// `devel-common`: they never ship, so `--production` treats them like tests
/// (#613). Runtime references count only from the root and the members it
/// ships, because airflow's test and docs members, which nothing references,
/// depend on `devel-common` at runtime. An unreferenced member stays, and so
/// does one published on its own, like airflow's `airflow-ctl` (#621).
fn drop_dev_only_members(
    sources: &mut DiscoveredSources,
    workspace_members: &mut Vec<ResolvedWorkspaceMember>,
    workspace_inputs: &mut Vec<WorkspaceMemberInputs>,
    manifest: &LoadedManifest,
    groups: &DependencyGroupsConfig,
) {
    let names: BTreeMap<String, usize> = workspace_inputs
        .iter()
        .enumerate()
        .filter_map(|(index, input)| {
            let name = input.manifest.metadata.name.as_deref()?;
            Some((normalize_distribution_name(name), index))
        })
        .collect();
    let is_dev = |dep: &DeclaredDependency| {
        declaration_buckets(dep, groups)
            .iter()
            .all(|bucket| matches!(bucket, DeclarationBucket::Dev | DeclarationBucket::Type))
    };
    let mut shipped = BTreeSet::new();
    let mut dev_referenced = BTreeSet::new();
    let mut pending = vec![manifest];
    while let Some(current) = pending.pop() {
        for dep in &current.dependencies {
            let Some(&index) = names.get(&normalize_distribution_name(&dep.name)) else {
                continue;
            };
            if is_dev(dep) {
                dev_referenced.insert(index);
            } else if shipped.insert(index) {
                pending.push(&workspace_inputs[index].manifest);
            }
        }
    }
    let dev_only: BTreeSet<&str> = dev_referenced
        .difference(&shipped)
        .map(|&index| &workspace_inputs[index])
        .filter(|input| !is_published(&input.manifest))
        .map(|input| input.member.path.as_str())
        .collect();
    if dev_only.is_empty() {
        return;
    }
    // A file belongs to its innermost member, so a shipped member nested in a
    // dropped one keeps its files.
    sources.files.retain(|file| {
        workspace_inputs
            .iter()
            .map(|input| input.member.path.as_str())
            .filter(|member| Path::new(&file.path).starts_with(member))
            .max_by_key(|member| member.len())
            .is_none_or(|member| !dev_only.contains(member))
    });
    let dev_only: BTreeSet<String> = dev_only.into_iter().map(str::to_owned).collect();
    workspace_inputs.retain(|input| !dev_only.contains(&input.member.path));
    workspace_members.retain(|member| !dev_only.contains(&member.path));
}

/// Classifiers mark a member as a distribution of its own, unless it opts out
/// of upload the way airflow's private members do. Console scripts do not:
/// dev helpers declare them too.
fn is_published(manifest: &LoadedManifest) -> bool {
    let metadata = &manifest.metadata;
    if metadata.dynamic.iter().any(|item| item == "classifiers") {
        return true;
    }
    let classifiers = &metadata.classifiers;
    !classifiers.is_empty()
        && !classifiers
            .iter()
            .any(|classifier| classifier.trim() == "Private :: Do Not Upload")
}

fn activate_plugins(
    config: &mut ChokkinConfig,
    manifest: &LoadedManifest,
    workspace_inputs: &[WorkspaceMemberInputs],
) -> Vec<PluginActivation> {
    let scopes: Vec<EnablerScope<'_>> = std::iter::once(EnablerScope {
        member: None,
        manifest,
    })
    .chain(workspace_inputs.iter().map(|input| EnablerScope {
        member: Some(&input.member),
        manifest: &input.manifest,
    }))
    .collect();
    let activations = resolve_plugin_activations(config, &scopes);
    for activation in &activations {
        config.plugins.insert(activation.plugin, activation.enabled);
    }
    activations
}

fn collect_workspace_inputs(
    root: &ProjectRoot,
    members: &[ResolvedWorkspaceMember],
    overrides: &RuntimeOverrides,
    cache: Option<&CacheOptions>,
) -> Result<Vec<WorkspaceMemberInputs>, ProbeError> {
    // Members are independent, and an undeclared monorepo's hundreds of member
    // lockfiles dominate the probe when read one after another (#488).
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(members.len())
        .max(1);
    let cursor = AtomicUsize::new(0);
    let mut results = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = cursor.fetch_add(1, Ordering::Relaxed);
                        let Some(member) = members.get(index) else {
                            return done;
                        };
                        done.push((index, member_inputs(root, member, overrides, cache)));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
            })
            .collect::<Vec<_>>()
    });
    results.sort_by_key(|(index, _)| *index);
    results
        .into_iter()
        .filter_map(|(_, inputs)| inputs.transpose())
        .collect()
}

fn member_inputs(
    root: &ProjectRoot,
    member: &ResolvedWorkspaceMember,
    overrides: &RuntimeOverrides,
    cache: Option<&CacheOptions>,
) -> Result<Option<WorkspaceMemberInputs>, ProbeError> {
    let member_root = member_project_root(root, member);
    if !member_root.path.is_dir() {
        return Ok(None);
    }
    let mut loaded = load_config(&member_root)?;
    apply_overrides(&mut loaded.effective, overrides);
    let manifest = extract_manifest_with_cache(&member_root, &loaded, cache)?;
    let target_version = resolve_target_version(&loaded.effective, &manifest);
    loaded.effective.target_version = Some(target_version);
    let sources = discover_sources(&member_root, &loaded, &manifest)?;
    Ok(Some(WorkspaceMemberInputs {
        member: member.clone(),
        manifest,
        sources,
    }))
}

fn member_project_root(root: &ProjectRoot, member: &ResolvedWorkspaceMember) -> ProjectRoot {
    let path = root.path.join(&member.path);
    ProjectRoot {
        path,
        marker: RootMarker::PyProjectToml,
    }
}

fn canonicalize_path(path: &Path) -> Result<PathBuf, ProbeError> {
    if path.exists() {
        std::fs::canonicalize(path).map_err(ProbeError::StartPath)
    } else {
        Ok(path.to_path_buf())
    }
}

/// Write human-readable probe summary to `out`.
#[allow(clippy::too_many_lines)]
pub fn write_probe_report(report: &ProbeReport, out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "chokkin {} (probe)", report.version)?;
    writeln!(out)?;

    let project_name = report
        .manifest
        .metadata
        .name
        .as_deref()
        .unwrap_or("(unknown)");
    writeln!(out, "Project : {project_name}")?;
    writeln!(
        out,
        "Root    : {} ({})",
        crate::path_util::display_path(&report.root.path),
        report.root.marker
    )?;
    writeln!(
        out,
        "Config  : {}",
        crate::reporters::config_label(&report.config_sources, "pyproject.toml [tool.chokkin]")
    )?;
    writeln!(
        out,
        "Mode    : {} (unresolved)",
        report.effective_config.mode
    )?;
    writeln!(out, "Layout  : {}", format_layout(&report.sources))?;
    if !report.workspace_members.is_empty() {
        writeln!(
            out,
            "Workspace: {} members ({} inventoried)",
            report.workspace_members.len(),
            report.workspace_inputs.len()
        )?;
    }
    writeln!(
        out,
        "Target  : {}",
        report
            .effective_config
            .target_version
            .as_ref()
            .map_or_else(TargetVersion::default_py311, Clone::clone)
    )?;
    writeln!(out)?;

    writeln!(out, "Manifest")?;
    writeln!(
        out,
        "  dependencies     : {}",
        report.manifest.dependencies.len()
    )?;
    writeln!(
        out,
        "  entry points     : {}",
        report.manifest.entry_points.len()
    )?;
    writeln!(
        out,
        "  lockfile         : {}",
        format_lockfile(&report.manifest)
    )?;
    writeln!(
        out,
        "  build backend    : {}",
        format_build_system(&report.manifest)
    )?;
    writeln!(out)?;

    let (python_count, stub_count, notebook_count) = count_files(&report.sources);
    let context_counts = count_contexts(&report.sources);
    writeln!(out, "Sources")?;
    writeln!(out, "  python files      : {python_count}")?;
    writeln!(out, "  stub files (.pyi) : {stub_count}")?;
    writeln!(out, "  notebooks (.ipynb): {notebook_count}")?;
    writeln!(
        out,
        "  contexts         : runtime {}, test {}, dev {}, docs {}",
        context_counts.runtime, context_counts.test, context_counts.dev, context_counts.docs
    )?;
    write_scripts(&report.scripts, out)?;
    writeln!(out)?;

    writeln!(out, "Plugins")?;
    for activation in report.plugin_activations.iter().filter(|activation| {
        activation.enabled || activation.reason != PluginActivationReason::Default
    }) {
        writeln!(out, "  {:<17}: {activation}", activation.plugin.as_key())?;
    }
    writeln!(out)?;

    if report.warnings.is_empty() {
        writeln!(out, "Warnings: 0")?;
    } else {
        writeln!(out, "Warnings: {} (see stderr)", report.warnings.len())?;
    }
    writeln!(out)?;
    writeln!(out, "Summary: probe complete — analyzer not run yet")?;
    Ok(())
}

fn write_scripts(scripts: &[InlineScript], out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "  inline scripts   : {}", scripts.len())?;
    for script in scripts {
        writeln!(
            out,
            "    {} ({} dependencies, requires-python {})",
            script.path,
            script.dependencies.len(),
            script.requires_python.as_deref().unwrap_or("unset")
        )?;
    }
    Ok(())
}

fn format_layout(sources: &DiscoveredSources) -> String {
    let info = &sources.layout;
    let layout = info.layout.as_str();
    let root = if info.package_root.is_empty() || info.package_root == "src" {
        String::new()
    } else {
        format!("root: {}, ", info.package_root)
    };
    if info.packages.is_empty() {
        layout.to_owned()
    } else {
        format!("{layout} ({root}packages: {})", info.packages.join(", "))
    }
}

fn format_lockfile(manifest: &LoadedManifest) -> String {
    manifest.sources.lockfile.as_ref().map_or_else(
        || "none".to_owned(),
        |source| {
            let nodes = manifest.lockfile.edges.len();
            format!("{} ({}, {nodes} nodes)", source.path, source.kind.as_str())
        },
    )
}

fn format_build_system(manifest: &LoadedManifest) -> String {
    let metadata = &manifest.metadata;
    let backend = metadata.build_backend.as_deref().unwrap_or("none");
    if metadata.build_requires.is_empty() {
        return backend.to_owned();
    }
    let requires: Vec<&str> = metadata
        .build_requires
        .iter()
        .map(|dep| dep.name.as_str())
        .collect();
    format!("{backend} (requires: {})", requires.join(", "))
}

struct ContextCounts {
    runtime: usize,
    test: usize,
    dev: usize,
    docs: usize,
}

fn count_files(sources: &DiscoveredSources) -> (usize, usize, usize) {
    let mut python = 0;
    let mut stub = 0;
    let mut notebook = 0;
    for file in &sources.files {
        match file.kind {
            FileKind::Python => python += 1,
            FileKind::Stub => stub += 1,
            FileKind::Notebook => notebook += 1,
        }
    }
    (python, stub, notebook)
}

fn count_contexts(sources: &DiscoveredSources) -> ContextCounts {
    let mut counts = ContextCounts {
        runtime: 0,
        test: 0,
        dev: 0,
        docs: 0,
    };
    for file in &sources.files {
        if !matches!(file.kind, FileKind::Python | FileKind::Notebook) {
            continue;
        }
        match file.context {
            FileContext::Runtime => counts.runtime += 1,
            FileContext::Test => counts.test += 1,
            FileContext::Dev => counts.dev += 1,
            FileContext::Docs => counts.docs += 1,
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;
    use crate::config::RuntimeOverrides;
    use crate::discovery::RootMarker;

    #[test]
    fn probe_empty_pyproject_project() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("pyproject.toml"),
            "[project]\nname = \"empty\"\nversion = \"0.0.0\"\n",
        )
        .expect("write");

        let report = probe_project(temp.path(), None, &RuntimeOverrides::default()).expect("probe");
        assert_eq!(report.manifest.dependencies.len(), 0);
        assert_eq!(report.sources.python_files().count(), 0);
    }

    #[test]
    fn write_probe_report_includes_summary() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("pyproject.toml"),
            "[project]\nname = \"demo\"\nversion = \"0.0.0\"\n",
        )
        .expect("write");

        let report = probe_project(temp.path(), None, &RuntimeOverrides::default()).expect("probe");
        let mut output = Vec::new();
        write_probe_report(&report, &mut output).expect("write");
        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("chokkin"));
        assert!(text.contains("(probe)"));
        assert!(text.contains("Project : demo"));
        assert!(text.contains("Summary: probe complete"));
        assert!(text.contains("build backend    : none"));
    }

    #[test]
    fn write_probe_report_shows_build_system() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("pyproject.toml"),
            "[build-system]\nrequires = [\"hatchling\", \"hatch-vcs\"]\n\
             build-backend = \"hatchling.build\"\n\n\
             [project]\nname = \"demo\"\nversion = \"0.0.0\"\n",
        )
        .expect("write");

        let report = probe_project(temp.path(), None, &RuntimeOverrides::default()).expect("probe");
        let mut output = Vec::new();
        write_probe_report(&report, &mut output).expect("write");
        let text = String::from_utf8(output).expect("utf8");
        assert!(
            text.contains("build backend    : hatchling.build (requires: hatchling, hatch-vcs)"),
            "{text}"
        );
    }

    #[test]
    fn probe_lists_inline_scripts() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(
            temp.path().join("pyproject.toml"),
            "[project]\nname = \"demo\"\nversion = \"0.0.0\"\n",
        )
        .expect("write");
        fs::create_dir(temp.path().join("scripts")).expect("mkdir");
        fs::write(
            temp.path().join("scripts/run.py"),
            "# /// script\n# requires-python = \">=3.12\"\n# dependencies = [\"rich\"]\n# ///\nimport rich\n",
        )
        .expect("write");

        let report = probe_project(temp.path(), None, &RuntimeOverrides::default()).expect("probe");
        assert_eq!(report.scripts.len(), 1);
        let mut output = Vec::new();
        write_probe_report(&report, &mut output).expect("write");
        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("inline scripts   : 1"));
        assert!(text.contains("scripts/run.py (1 dependencies, requires-python >=3.12)"));
    }

    #[test]
    fn broken_pyproject_returns_manifest_error() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("pyproject.toml"), "not valid [[[\n").expect("write");

        let err = probe_project(temp.path(), None, &RuntimeOverrides::default())
            .expect_err("broken manifest");
        assert!(err.is_usage_error());
    }

    #[test]
    fn format_layout_unknown_without_packages() {
        let sources = DiscoveredSources {
            root: crate::discovery::ProjectRoot {
                path: Path::new("/tmp").to_path_buf(),
                marker: RootMarker::PyProjectToml,
            },
            layout: crate::sources::LayoutInfo {
                layout: crate::sources::ProjectLayout::Unknown,
                package_root: String::new(),
                packages: Vec::new(),
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
                members: Vec::new(),
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        assert_eq!(format_layout(&sources), "unknown");
    }
}
