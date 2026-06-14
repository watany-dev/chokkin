//! Project probe orchestration (pipeline steps 1–4).

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::VERSION;
use crate::config::{
    ChokkinConfig, ConfigSources, ResolvedWorkspaceMember, RuntimeOverrides, TargetVersion,
    apply_overrides, load_config,
};
use crate::discovery::{ProjectRoot, RootMarker, discover_project_root};
use crate::manifest::{LoadedManifest, extract_manifest, resolve_target_version};
use crate::sources::{DiscoveredSources, FileContext, FileKind, discover_sources};

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
/// Returns [`ProbeError`] when a pipeline step fails fatally.
pub fn probe_project(
    start: &Path,
    project_root_override: Option<&Path>,
    overrides: &RuntimeOverrides,
) -> Result<ProbeReport, ProbeError> {
    let discovery_start = project_root_override.unwrap_or(start);
    let canonical_start = canonicalize_path(discovery_start)?;

    let root = discover_project_root(&canonical_start)?;
    let mut loaded = load_config(&root)?;
    apply_overrides(&mut loaded.effective, overrides);

    let manifest = extract_manifest(&root, &loaded)?;
    let target_version = resolve_target_version(&loaded.effective, &manifest);
    loaded.effective.target_version = Some(target_version);

    let sources = discover_sources(&root, &loaded, &manifest)?;
    let workspace_inputs = collect_workspace_inputs(&root, &loaded.workspace_members, overrides)?;
    let warnings = collect_warnings(&manifest, &sources);

    Ok(ProbeReport {
        version: VERSION,
        root,
        config_sources: loaded.sources,
        effective_config: loaded.effective,
        manifest,
        sources,
        workspace_members: loaded.workspace_members,
        workspace_inputs,
        warnings,
    })
}

fn collect_workspace_inputs(
    root: &ProjectRoot,
    members: &[ResolvedWorkspaceMember],
    overrides: &RuntimeOverrides,
) -> Result<Vec<WorkspaceMemberInputs>, ProbeError> {
    let mut inputs = Vec::new();
    for member in members {
        let member_root = member_project_root(root, member);
        if !member_root.path.is_dir() {
            continue;
        }
        let mut loaded = load_config(&member_root)?;
        apply_overrides(&mut loaded.effective, overrides);
        let manifest = extract_manifest(&member_root, &loaded)?;
        let target_version = resolve_target_version(&loaded.effective, &manifest);
        loaded.effective.target_version = Some(target_version);
        let sources = discover_sources(&member_root, &loaded, &manifest)?;
        inputs.push(WorkspaceMemberInputs {
            member: member.clone(),
            manifest,
            sources,
        });
    }
    Ok(inputs)
}

fn member_project_root(root: &ProjectRoot, member: &ResolvedWorkspaceMember) -> ProjectRoot {
    let path = root.path.join(&member.path);
    ProjectRoot {
        path,
        marker: RootMarker::PyProjectToml,
        start: root.start.clone(),
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
        report.root.path.display(),
        report.root.marker
    )?;
    writeln!(
        out,
        "Config  : {}",
        format_config_sources(&report.config_sources)
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
    writeln!(out)?;

    let (python_count, stub_count) = count_files(&report.sources);
    let context_counts = count_contexts(&report.sources);
    writeln!(out, "Sources")?;
    writeln!(out, "  python files     : {python_count}")?;
    writeln!(out, "  stub files (.pyi): {stub_count}")?;
    writeln!(
        out,
        "  contexts         : runtime {}, test {}, dev {}, docs {}",
        context_counts.runtime, context_counts.test, context_counts.dev, context_counts.docs
    )?;
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

fn format_config_sources(sources: &ConfigSources) -> String {
    let mut parts = Vec::new();
    if sources.dot_chokkin_toml.is_some() {
        parts.push(".chokkin.toml".to_owned());
    }
    if sources.chokkin_toml.is_some() {
        parts.push("chokkin.toml".to_owned());
    }
    if sources.pyproject_tool_chokkin {
        parts.push("pyproject.toml [tool.chokkin]".to_owned());
    }
    if parts.is_empty() {
        if sources.used_defaults {
            "defaults".to_owned()
        } else {
            "(none)".to_owned()
        }
    } else {
        parts.join(", ")
    }
}

fn format_layout(sources: &DiscoveredSources) -> String {
    let layout = sources.layout.layout.as_str();
    if sources.layout.packages.is_empty() {
        layout.to_owned()
    } else {
        format!(
            "{layout} (packages: {})",
            sources.layout.packages.join(", ")
        )
    }
}

fn format_lockfile(manifest: &LoadedManifest) -> String {
    if manifest.sources.uv_lock {
        let nodes = manifest.lockfile.edges.len();
        format!("uv.lock ({nodes} nodes)")
    } else {
        "none".to_owned()
    }
}

struct ContextCounts {
    runtime: usize,
    test: usize,
    dev: usize,
    docs: usize,
}

fn count_files(sources: &DiscoveredSources) -> (usize, usize) {
    let mut python = 0;
    let mut stub = 0;
    for file in &sources.files {
        match file.kind {
            FileKind::Python => python += 1,
            FileKind::Stub => stub += 1,
        }
    }
    (python, stub)
}

fn count_contexts(sources: &DiscoveredSources) -> ContextCounts {
    let mut counts = ContextCounts {
        runtime: 0,
        test: 0,
        dev: 0,
        docs: 0,
    };
    for file in &sources.files {
        if file.kind != FileKind::Python {
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
                start: Path::new("/tmp").to_path_buf(),
            },
            layout: crate::sources::LayoutInfo {
                layout: crate::sources::ProjectLayout::Unknown,
                packages: Vec::new(),
                inferred_globs: Vec::new(),
                flat_candidates: Vec::new(),
                ambiguous_flat_resolution: false,
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        assert_eq!(format_layout(&sources), "unknown");
    }
}
