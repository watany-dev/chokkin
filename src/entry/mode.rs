//! `mode = auto` resolution (§8).

use crate::config::{ChokkinConfig, ProjectMode};
use crate::manifest::LoadedManifest;
use crate::resolver::import_root;
use crate::sources::{DiscoveredSources, FileContext, LayoutInfo};

use super::auto::detect_auto_entries;
use super::types::{EntryCandidate, EntryWarning};

const APP_ENTRY_FILE_NAMES: &[&str] = &["manage.py", "asgi.py", "wsgi.py", "app.py"];

/// Resolve effective project mode from config, manifest, and discovered entries.
#[must_use]
pub(super) fn resolve_project_mode(
    config: &ChokkinConfig,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    candidates: &[EntryCandidate],
    warnings: &mut Vec<EntryWarning>,
) -> ProjectMode {
    if config.mode != ProjectMode::Auto {
        return config.mode;
    }

    if let Some(member_count) = workspace_member_count(config, manifest)
        && member_count > 1
    {
        warnings.push(EntryWarning::WorkspaceMode { member_count });
        return ProjectMode::App;
    }

    if has_clear_app_signals(manifest, sources, candidates, true) {
        return ProjectMode::App;
    }

    if is_library_project(manifest, sources) {
        return ProjectMode::Library;
    }

    ProjectMode::App
}

/// Whether a workspace member names a distribution and shows no app signal.
///
/// Namespace packages (`llama_index`) have no `__init__.py`, so unlike root
/// mode resolution no package is required. A monorepo splits its CLI into
/// its own member (`airflow-core`, `llama-dev`), so a member's console
/// script stays an app signal even when it targets the member's package.
#[must_use]
pub(crate) fn is_library_member(manifest: &LoadedManifest, sources: &DiscoveredSources) -> bool {
    names_distribution(manifest)
        && !has_clear_app_signals(manifest, sources, &detect_auto_entries(sources), false)
}

fn workspace_member_count(config: &ChokkinConfig, manifest: &LoadedManifest) -> Option<usize> {
    if let Some(hint) = &manifest.uv_workspace {
        let count = hint.members.len();
        if count > 1 {
            return Some(count);
        }
    }
    if config.workspaces.len() > 1 {
        return Some(config.workspaces.len());
    }
    None
}

/// A `wsgi.py` / `app.py` module of the project's own package (werkzeug)
/// is a library feature, not an app signal; nor is a test fixture's
/// `app.py`. With `own_cli_is_library`, neither is a CLI shipped inside the
/// package (`django-admin`, `httpx`) (#652); a GUI script always is.
fn has_clear_app_signals(
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    candidates: &[EntryCandidate],
    own_cli_is_library: bool,
) -> bool {
    let layout = &sources.layout;
    if manifest.entry_points.iter().any(|entry| {
        entry.group == "gui"
            || (entry.group == "console"
                && !(own_cli_is_library && targets_own_package(&entry.target, &layout.packages)))
    }) {
        return true;
    }

    candidates.iter().any(|candidate| {
        let path = candidate.spec.path.as_str();
        let file_name = path.rsplit('/').next().unwrap_or(path);
        APP_ENTRY_FILE_NAMES.contains(&file_name)
            && candidate.context == FileContext::Runtime
            && !in_package(path, layout)
    })
}

/// Whether a `module:attr` entry point target lives in one of `packages`.
fn targets_own_package(target: &str, packages: &[String]) -> bool {
    let top = import_root(target.split(':').next().unwrap_or(target).trim());
    packages.iter().any(|package| package == top)
}

fn in_package(path: &str, layout: &LayoutInfo) -> bool {
    layout.packages.iter().any(|package| {
        path.strip_prefix(layout.package_dir(package).as_str())
            .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// requests sets `name=about["__title__"]` from an `exec`ed file; the name
/// is unreadable but still declared (#586).
fn names_distribution(manifest: &LoadedManifest) -> bool {
    manifest.metadata.name.is_some() || manifest.sources.setup_py_dynamic_name
}

fn is_library_project(manifest: &LoadedManifest, sources: &DiscoveredSources) -> bool {
    if !names_distribution(manifest) {
        return false;
    }

    if sources.layout.packages.is_empty() {
        return false;
    }

    sources.files.iter().any(|file| {
        sources.layout.packages.iter().any(|package| {
            file.path == format!("{}/__init__.py", sources.layout.package_dir(package))
                || file.path == format!("{package}/__init__.py")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EntrySpec;
    use crate::config::default_config;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::manifest::{
        EntryPointDecl, LoadedManifest, LockfileGraph, ManifestSources, ProjectMetadata,
    };
    use crate::sources::{
        DiscoveredFile, DiscoveredSources, FileContext, FileKind, LayoutInfo, ProjectLayout,
    };

    use super::super::types::EntryOrigin;

    fn empty_manifest() -> LoadedManifest {
        LoadedManifest {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
            },
            metadata: ProjectMetadata::default(),
            dependencies: Vec::new(),
            constraints: Vec::new(),
            uv: crate::manifest::UvToolSettings::default(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources::default(),
            warnings: Vec::new(),
        }
    }

    fn library_sources() -> DiscoveredSources {
        DiscoveredSources {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
            },
            layout: LayoutInfo {
                layout: ProjectLayout::Src,
                package_root: "src".to_owned(),
                packages: vec!["acme".to_owned()],
                local_packages: Vec::new(),
                inferred_globs: Vec::new(),
                members: Vec::new(),
            },
            effective_globs: Vec::new(),
            files: vec![DiscoveredFile {
                path: "src/acme/__init__.py".to_owned(),
                kind: FileKind::Python,
                context: FileContext::Runtime,
            }],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn explicit_mode_is_preserved() {
        let mut config = default_config();
        config.mode = ProjectMode::Library;
        let mut warnings = Vec::new();
        let mode = resolve_project_mode(
            &config,
            &empty_manifest(),
            &library_sources(),
            &[],
            &mut warnings,
        );
        assert_eq!(mode, ProjectMode::Library);
        assert_eq!(warnings, []);
    }

    #[test]
    fn library_mode_for_lib_package_root() {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        let mut config = default_config();
        config.mode = ProjectMode::Auto;
        let mut sources = library_sources();
        sources.layout.package_root = "lib".to_owned();
        sources.files[0].path = "lib/acme/__init__.py".to_owned();
        let mut warnings = Vec::new();
        let mode = resolve_project_mode(&config, &manifest, &sources, &[], &mut warnings);
        assert_eq!(mode, ProjectMode::Library);
    }

    #[test]
    fn library_mode_without_app_signals() {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        let mut config = default_config();
        config.mode = ProjectMode::Auto;
        let mut warnings = Vec::new();
        let mode = resolve_project_mode(&config, &manifest, &library_sources(), &[], &mut warnings);
        assert_eq!(mode, ProjectMode::Library);
    }

    fn console_script(target: &str) -> EntryPointDecl {
        EntryPointDecl {
            name: "acme-cli".to_owned(),
            target: target.to_owned(),
            group: "console".to_owned(),
            origin: crate::manifest::DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                label: "project.scripts.acme-cli".to_owned(),
                line: None,
            },
        }
    }

    fn auto_candidate(path: &str) -> EntryCandidate {
        EntryCandidate {
            spec: EntrySpec {
                path: path.to_owned(),
                symbol: None,
            },
            context: crate::sources::assign_file_context(path),
            origin: EntryOrigin::Auto {
                rule: "auto".to_owned(),
            },
        }
    }

    fn auto_mode(manifest: &LoadedManifest, candidates: &[EntryCandidate]) -> ProjectMode {
        let mut config = default_config();
        config.mode = ProjectMode::Auto;
        let mut warnings = Vec::new();
        resolve_project_mode(
            &config,
            manifest,
            &library_sources(),
            candidates,
            &mut warnings,
        )
    }

    #[test]
    fn app_mode_from_console_scripts_outside_the_package() {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        manifest
            .entry_points
            .push(console_script("acme_server.cli:main"));
        assert_eq!(auto_mode(&manifest, &[]), ProjectMode::App);
    }

    #[test]
    fn library_shipping_its_own_cli_stays_a_library() {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        manifest.entry_points.push(console_script("acme.cli:main"));
        assert_eq!(auto_mode(&manifest, &[]), ProjectMode::Library);
        manifest.entry_points[0].target = "acme".to_owned();
        assert_eq!(auto_mode(&manifest, &[]), ProjectMode::Library);
        // `acme_cli` is not `acme`: the prefix alone is not the package.
        manifest.entry_points.push(console_script("acme_cli:main"));
        assert_eq!(auto_mode(&manifest, &[]), ProjectMode::App);
    }

    #[test]
    fn gui_script_into_the_package_is_an_app() {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        let mut gui = console_script("acme.gui:main");
        gui.group = "gui".to_owned();
        manifest.entry_points.push(gui);
        assert_eq!(auto_mode(&manifest, &[]), ProjectMode::App);
    }

    #[test]
    fn unnamed_project_with_its_own_cli_is_an_app() {
        let mut manifest = empty_manifest();
        manifest.entry_points.push(console_script("acme.cli:main"));
        assert_eq!(auto_mode(&manifest, &[]), ProjectMode::App);
    }

    #[test]
    fn app_file_names_inside_the_package_are_not_app_signals() {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        for path in [
            "src/acme/wsgi.py",
            "src/acme/tools/web/app.py",
            "tests/test_apps/helloworld/wsgi.py",
        ] {
            assert_eq!(
                auto_mode(&manifest, &[auto_candidate(path)]),
                ProjectMode::Library,
                "{path}"
            );
        }
        for path in ["wsgi.py", "app.py", "deploy/asgi.py", "src/acme_web/app.py"] {
            assert_eq!(
                auto_mode(&manifest, &[auto_candidate(path)]),
                ProjectMode::App,
                "{path}"
            );
        }
    }

    #[test]
    fn fallback_is_app_mode() {
        let mut config = default_config();
        config.mode = ProjectMode::Auto;
        let mut warnings = Vec::new();
        let mode = resolve_project_mode(
            &config,
            &empty_manifest(),
            &library_sources(),
            &[],
            &mut warnings,
        );
        assert_eq!(mode, ProjectMode::App);
    }

    #[test]
    fn manage_py_triggers_app_mode() {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        let mut config = default_config();
        config.mode = ProjectMode::Auto;
        let candidates = vec![super::super::types::EntryCandidate {
            spec: EntrySpec {
                path: "manage.py".to_owned(),
                symbol: None,
            },
            context: FileContext::Runtime,
            origin: EntryOrigin::Auto {
                rule: "manage.py".to_owned(),
            },
        }];
        let mut warnings = Vec::new();
        let mode = resolve_project_mode(
            &config,
            &manifest,
            &library_sources(),
            &candidates,
            &mut warnings,
        );
        assert_eq!(mode, ProjectMode::App);
    }

    #[test]
    fn named_member_without_app_signals_is_a_library_even_without_init() {
        let mut sources = library_sources();
        sources.files[0].path = "llama_index/llms/openai/base.py".to_owned();
        let mut manifest = empty_manifest();
        assert!(!is_library_member(&manifest, &sources));
        manifest.sources.setup_py_dynamic_name = true;
        assert!(is_library_member(&manifest, &sources));
        manifest.metadata.name = Some("llama-index-llms-openai".to_owned());
        assert!(is_library_member(&manifest, &sources));
        sources.files[0].path = "manage.py".to_owned();
        assert!(!is_library_member(&manifest, &sources));
    }

    #[test]
    fn member_cli_in_its_own_package_is_an_app_member() {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        let mut sources = library_sources();
        assert!(is_library_member(&manifest, &sources));
        sources.files[0].path = "src/acme/wsgi.py".to_owned();
        assert!(is_library_member(&manifest, &sources));
        manifest.entry_points.push(console_script("acme.cli:main"));
        assert!(!is_library_member(&manifest, &sources));
    }
}
