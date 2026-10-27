//! `mode = auto` resolution (§8).

use std::collections::{BTreeMap, BTreeSet};

use crate::config::{ChokkinConfig, ProjectMode};
use crate::manifest::{
    DeclaredDependency, DependencyContext, LoadedManifest, normalize_distribution_name,
};
use crate::resolver::import_root;
use crate::sources::{DiscoveredSources, FileContext, LayoutInfo, PublicSurface};

use super::auto::detect_auto_entries;
use super::build::parse_manifest_target;
use super::module::resolve_module_to_path;
use super::types::EntryCandidate;

const APP_ENTRY_FILE_NAMES: &[&str] = &["manage.py", "asgi.py", "wsgi.py", "app.py"];

/// Resolve effective project mode from config, manifest, and discovered entries.
#[must_use]
pub(super) fn resolve_project_mode(
    config: &ChokkinConfig,
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    candidates: &[EntryCandidate],
) -> ProjectMode {
    if config.mode != ProjectMode::Auto {
        return config.mode;
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

/// Whether a member that a metapackage root distributes ships as a library
/// (#1060): its console script into its own package is then a CLI shipped
/// with the library, as for a root (#652). Its mode stays app (CHK006 /
/// CHK007 keep app severity); only its wheel's orphans are library orphans.
#[must_use]
pub(super) fn is_distributed_library(
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
) -> bool {
    !has_clear_app_signals(manifest, sources, &detect_auto_entries(sources), true)
}

/// The distributions an auto-mode root ships as its content, by PEP 503
/// name, each with the first requirement that names it (#1060).
///
/// Only a metapackage counts: a root whose wheel target ships no file
/// (`bypass-selection`, as for CHK002 in #529) and that shows no app signal
/// of its own. fastmcp's root ships nothing but `fastmcp-slim`, so that
/// member is the library its users install. airflow's root is a metapackage
/// too, but its own `apache-airflow` console script makes it an app, so it
/// distributes nothing and `airflow-core` is scored as any app member. A
/// root that ships code and depends on its member is an app using a
/// library, not one distributing it, and a dependency group is never
/// distributed.
#[must_use]
pub(super) fn metapackage_distributions<'a>(
    config: &ChokkinConfig,
    manifest: &'a LoadedManifest,
    sources: &DiscoveredSources,
    candidates: &[EntryCandidate],
) -> BTreeMap<String, &'a DeclaredDependency> {
    let mut distributions = BTreeMap::new();
    if config.mode != ProjectMode::Auto
        || !ships_no_code(manifest, sources)
        || has_clear_app_signals(manifest, sources, candidates, true)
    {
        return distributions;
    }
    for dep in &manifest.dependencies {
        if matches!(dep.context, DependencyContext::Group(_)) {
            continue;
        }
        distributions
            .entry(normalize_distribution_name(&dep.name))
            .or_insert(dep);
    }
    distributions
}

/// A declared wheel target that matches no discovered file: a metapackage
/// (#529).
fn ships_no_code(manifest: &LoadedManifest, sources: &DiscoveredSources) -> bool {
    manifest
        .metadata
        .wheel_targets
        .as_ref()
        .is_some_and(|targets| PublicSurface::resolve(Some(targets), &sources.files).is_none())
}

/// A `wsgi.py` / `app.py` module of the project's own package (werkzeug)
/// is a library feature, not an app signal; nor is a test fixture's
/// `app.py`. With `own_cli_is_library`, neither is a CLI shipped inside the
/// package (`django-admin`, `httpx`) (#652); a GUI script always is. A
/// script whose target maps to no discovered file is no signal either way:
/// chokkin could not read where it points, which is a gap, not an app
/// (#1275).
fn has_clear_app_signals(
    manifest: &LoadedManifest,
    sources: &DiscoveredSources,
    candidates: &[EntryCandidate],
    own_cli_is_library: bool,
) -> bool {
    let layout = &sources.layout;
    let known_paths: BTreeSet<String> =
        sources.files.iter().map(|file| file.path.clone()).collect();
    let maps_to_source = |target: &str| {
        parse_manifest_target(target).is_some_and(|(module, _)| {
            resolve_module_to_path(&module, layout, &known_paths).is_some()
        })
    };
    if manifest.entry_points.iter().any(|entry| {
        (entry.group == "gui"
            || (entry.group == "console"
                && !(own_cli_is_library && targets_own_package(&entry.target, &layout.packages))))
            && maps_to_source(&entry.target)
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

/// A named distribution that ships a package of its own, or whose declared
/// wheel targets ship a discovered file: a single-module distribution
/// (`py_modules = ["six"]`) has no package directory (#1275).
fn is_library_project(manifest: &LoadedManifest, sources: &DiscoveredSources) -> bool {
    if !names_distribution(manifest) {
        return false;
    }

    if PublicSurface::resolve(manifest.metadata.wheel_targets.as_ref(), &sources.files).is_some() {
        return true;
    }

    if sources.layout.packages.is_empty() {
        return false;
    }

    sources.files.iter().any(|file| {
        sources.layout.packages.iter().any(|package| {
            file.path == format!("{}/__init__.py", sources.layout.package_dir(package))
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
    use crate::sources::{DiscoveredFile, DiscoveredSources, FileContext, FileKind, LayoutInfo};

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
            layout: LayoutInfo::new("src", vec!["acme".to_owned()]),
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
        let mode = resolve_project_mode(&config, &empty_manifest(), &library_sources(), &[]);
        assert_eq!(mode, ProjectMode::Library);
    }

    #[test]
    fn library_mode_for_lib_package_root() {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        let mut config = default_config();
        config.mode = ProjectMode::Auto;
        let mut sources = library_sources();
        sources.layout = LayoutInfo::new("lib", vec!["acme".to_owned()]);
        sources.files[0].path = "lib/acme/__init__.py".to_owned();
        let mode = resolve_project_mode(&config, &manifest, &sources, &[]);
        assert_eq!(mode, ProjectMode::Library);
    }

    #[test]
    fn root_package_outside_the_layout_does_not_make_a_library() {
        // The layout ships `src/acme`; a stray root `acme/` is not it.
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        let mut sources = library_sources();
        sources.files[0].path = "acme/__init__.py".to_owned();
        assert!(!is_library_project(&manifest, &sources));
    }

    #[test]
    fn library_mode_without_app_signals() {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        let mut config = default_config();
        config.mode = ProjectMode::Auto;
        let mode = resolve_project_mode(&config, &manifest, &library_sources(), &[]);
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

    /// A discovered file for each entry point's target, as a project that
    /// ships its scripts has: a target mapping to no file is no app signal
    /// (#1275).
    fn add_target_files(sources: &mut DiscoveredSources, manifest: &LoadedManifest) {
        for entry in &manifest.entry_points {
            let Some((module, _)) = parse_manifest_target(&entry.target) else {
                continue;
            };
            let path = crate::path_util::join_rel(
                &sources.layout.package_root,
                &format!("{}.py", module.replace('.', "/")),
            );
            if !sources.files.iter().any(|file| file.path == path) {
                sources.files.push(DiscoveredFile {
                    context: crate::sources::assign_file_context(&path),
                    path,
                    kind: FileKind::Python,
                });
            }
        }
        sources
            .files
            .sort_by(|left, right| left.path.cmp(&right.path));
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
        let mut sources = library_sources();
        add_target_files(&mut sources, manifest);
        resolve_project_mode(&config, manifest, &sources, candidates)
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
    fn script_mapping_to_no_file_is_no_app_signal() {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        manifest
            .entry_points
            .push(console_script("acme_server.cli:main"));
        let mut gui = console_script("acme_gui:main");
        gui.group = "gui".to_owned();
        manifest.entry_points.push(gui);
        let mut config = default_config();
        config.mode = ProjectMode::Auto;
        let mode = resolve_project_mode(&config, &manifest, &library_sources(), &[]);
        assert_eq!(mode, ProjectMode::Library);
    }

    #[test]
    fn single_module_distribution_is_a_library() {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("six".to_owned());
        let mut sources = library_sources();
        sources.layout = LayoutInfo::new("", Vec::new());
        sources.files[0].path = "six.py".to_owned();
        let mut config = default_config();
        config.mode = ProjectMode::Auto;
        assert_eq!(
            resolve_project_mode(&config, &manifest, &sources, &[]),
            ProjectMode::App
        );
        manifest.metadata.wheel_targets = Some(crate::manifest::WheelTargets {
            source: "setup.py".to_owned(),
            paths: vec!["six.py".to_owned()],
            find: Vec::new(),
        });
        assert_eq!(
            resolve_project_mode(&config, &manifest, &sources, &[]),
            ProjectMode::Library
        );
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
        let mode = resolve_project_mode(&config, &empty_manifest(), &library_sources(), &[]);
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
        let mode = resolve_project_mode(&config, &manifest, &library_sources(), &candidates);
        assert_eq!(mode, ProjectMode::App);
    }

    #[test]
    fn workspace_root_shipping_its_package_is_a_library() {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        manifest.uv_workspace = Some(crate::config::UvWorkspaceHint {
            members: vec!["dev/lint".to_owned(), "plugins/one".to_owned()],
            exclude: Vec::new(),
        });
        assert_eq!(auto_mode(&manifest, &[]), ProjectMode::Library);
        manifest.metadata.name = None;
        assert_eq!(auto_mode(&manifest, &[]), ProjectMode::App);
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
        add_target_files(&mut sources, &manifest);
        assert!(!is_library_member(&manifest, &sources));
    }

    #[test]
    fn distributed_member_cli_in_its_own_package_ships_with_the_library() {
        let mut manifest = empty_manifest();
        let mut sources = library_sources();
        manifest.entry_points.push(console_script("acme.cli:main"));
        add_target_files(&mut sources, &manifest);
        assert!(is_distributed_library(&manifest, &sources));
        manifest.entry_points[0].target = "tools.cli:main".to_owned();
        add_target_files(&mut sources, &manifest);
        assert!(!is_distributed_library(&manifest, &sources));
        manifest.entry_points[0].target = "acme.cli:main".to_owned();
        sources.files[0].path = "manage.py".to_owned();
        assert!(!is_distributed_library(&manifest, &sources));
    }

    fn requirement(name: &str, context: DependencyContext) -> DeclaredDependency {
        DeclaredDependency {
            name: name.to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context,
            origin: crate::manifest::DependencyOrigin {
                file: "pyproject.toml".to_owned(),
                line: Some(7),
                label: "project.dependencies".to_owned(),
            },
            opaque: false,
            included_via: Vec::new(),
            unportable_key: None,
        }
    }

    /// fastmcp's root: a hatch wheel with `bypass-selection`, so its targets
    /// match no file, over a member `slim/` that holds the code.
    fn metapackage_root() -> (LoadedManifest, DiscoveredSources) {
        let mut manifest = empty_manifest();
        manifest.metadata.name = Some("acme".to_owned());
        manifest.metadata.wheel_targets = Some(crate::manifest::WheelTargets {
            source: "tool.hatch.build.targets.wheel".to_owned(),
            paths: Vec::new(),
            find: Vec::new(),
        });
        manifest.dependencies = vec![
            requirement(
                "acme-slim",
                DependencyContext::OptionalExtra("all".to_owned()),
            ),
            requirement("acme-slim", DependencyContext::Runtime),
            requirement("acme-remote", DependencyContext::Group("dev".to_owned())),
            requirement("httpx", DependencyContext::Runtime),
        ];
        let mut sources = library_sources();
        sources.layout = LayoutInfo::new(".", Vec::new());
        sources.files[0].path = "slim/acme/__init__.py".to_owned();
        (manifest, sources)
    }

    fn distributions(
        config: &crate::config::ChokkinConfig,
        manifest: &LoadedManifest,
        sources: &DiscoveredSources,
        candidates: &[EntryCandidate],
    ) -> Vec<(String, DependencyContext)> {
        metapackage_distributions(config, manifest, sources, candidates)
            .into_iter()
            .map(|(name, dep)| (name, dep.context.clone()))
            .collect()
    }

    #[test]
    fn a_metapackage_root_distributes_its_non_group_requirements() {
        let mut config = default_config();
        config.mode = ProjectMode::Auto;
        let (manifest, sources) = metapackage_root();
        assert_eq!(
            distributions(&config, &manifest, &sources, &[]),
            [
                (
                    "acme-slim".to_owned(),
                    DependencyContext::OptionalExtra("all".to_owned())
                ),
                ("httpx".to_owned(), DependencyContext::Runtime),
            ]
        );
    }

    #[test]
    fn only_an_auto_mode_metapackage_without_app_signals_distributes() {
        let mut config = default_config();
        config.mode = ProjectMode::Auto;
        let (manifest, sources) = metapackage_root();

        let mut own_cli = manifest.clone();
        own_cli.entry_points.push(console_script("acme.cli:main"));
        let mut cli_sources = sources.clone();
        add_target_files(&mut cli_sources, &own_cli);
        assert!(distributions(&config, &own_cli, &cli_sources, &[]).is_empty());
        let app_file = [auto_candidate("app.py")];
        assert!(distributions(&config, &manifest, &sources, &app_file).is_empty());

        let mut ships_code = manifest.clone();
        if let Some(targets) = ships_code.metadata.wheel_targets.as_mut() {
            targets.paths.push("slim/acme".to_owned());
        }
        assert!(distributions(&config, &ships_code, &sources, &[]).is_empty());
        let mut no_targets = manifest.clone();
        no_targets.metadata.wheel_targets = None;
        assert!(distributions(&config, &no_targets, &sources, &[]).is_empty());

        config.mode = ProjectMode::App;
        assert!(distributions(&config, &manifest, &sources, &[]).is_empty());
    }

    /// Where a file sits relative to the project's own package `acme`.
    #[derive(Debug, Clone, Copy)]
    enum Place {
        /// Directly under the project root (`manage.py`).
        Root,
        /// Directly in the package directory (`src/acme/wsgi.py`).
        PackageTop,
        /// Deeper in the package directory (`src/acme/web/app.py`).
        PackageNested,
        Outside,
    }

    /// Directories an app-named file is put in (`PKG` is the package
    /// directory, whether or not the layout lists `acme`), with the §10
    /// context of a file there.
    const PLACES: &[(&str, FileContext, Place)] = &[
        ("", FileContext::Runtime, Place::Root),
        ("PKG", FileContext::Runtime, Place::PackageTop),
        ("PKG/web", FileContext::Runtime, Place::PackageNested),
        // `acme_web` only shares a prefix with `acme`.
        ("PKG_web", FileContext::Runtime, Place::Outside),
        ("deploy", FileContext::Runtime, Place::Outside),
        ("tests/apps", FileContext::Test, Place::Outside),
        ("docs", FileContext::Docs, Place::Outside),
        ("scripts", FileContext::Dev, Place::Outside),
        ("examples", FileContext::Dev, Place::Outside),
    ];

    /// `main.py` is auto-detected like the others but names no app.
    const FILE_NAMES: &[(&str, bool)] = &[
        ("manage.py", true),
        ("asgi.py", true),
        ("wsgi.py", true),
        ("app.py", true),
        ("main.py", false),
    ];

    /// Entry points as (group, target, whether the target's top module is
    /// `acme`). A plugin group is never an app signal.
    const ENTRIES: &[(&str, &str, bool)] = &[
        ("console", "acme.cli:main", true),
        ("console", "acme", true),
        ("console", " acme.cli : main", true),
        ("console", "acme_cli:main", false),
        ("console", "tools.run:main", false),
        ("gui", "acme.gui:main", true),
        ("gui", "tools.gui:main", false),
        ("acme.plugins", "tools.run:main", false),
    ];

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Package {
        Absent,
        WithoutInit,
        WithInit,
    }

    #[derive(Debug, Clone, Copy)]
    struct ModeCase {
        name: bool,
        dynamic_name: bool,
        src_layout: bool,
        package: Package,
        entry: Option<(&'static str, &'static str, bool)>,
        file: Option<(
            &'static str,
            bool,
            &'static (&'static str, FileContext, Place),
        )>,
    }

    impl ModeCase {
        fn path(&self) -> Option<String> {
            let (file_name, _, (dir, _, _)) = self.file?;
            let package_dir = if self.src_layout { "src/acme" } else { "acme" };
            let dir = dir.replace("PKG", package_dir);
            Some(if dir.is_empty() {
                file_name.to_owned()
            } else {
                format!("{dir}/{file_name}")
            })
        }

        fn manifest(&self) -> LoadedManifest {
            let mut manifest = empty_manifest();
            manifest.metadata.name = self.name.then(|| "acme".to_owned());
            manifest.sources.setup_py_dynamic_name = self.dynamic_name;
            manifest.entry_points = self
                .entry
                .iter()
                .map(|(group, target, _)| {
                    let mut entry = console_script(target);
                    (*group).clone_into(&mut entry.group);
                    entry
                })
                .collect();
            manifest
        }

        fn sources(&self) -> DiscoveredSources {
            let mut sources = library_sources();
            let packages = if self.package == Package::Absent {
                Vec::new()
            } else {
                vec!["acme".to_owned()]
            };
            sources.layout = LayoutInfo::new(if self.src_layout { "src" } else { "" }, packages);
            let init = (self.package == Package::WithInit)
                .then(|| format!("{}/__init__.py", sources.layout.package_dir("acme")));
            sources.files = init
                .into_iter()
                .chain(self.path())
                .map(|path| DiscoveredFile {
                    context: crate::sources::assign_file_context(&path),
                    path,
                    kind: FileKind::Python,
                })
                .collect();
            add_target_files(&mut sources, &self.manifest());
            sources
        }

        /// Whether the file is an app signal (§8) when it is an entry: an
        /// app file name, in runtime context, outside the package. With
        /// `detected_only`, only auto-detection can make it an entry, as for
        /// a member or a root whose `entry` config does not name it.
        fn file_signal(&self, detected_only: bool) -> bool {
            let Some((_, app_name, (_, context, place))) = self.file else {
                return false;
            };
            let listed = self.package != Package::Absent;
            let (in_package, auto) = match *place {
                Place::Root => (false, true),
                Place::PackageTop => (listed, listed),
                Place::PackageNested => (listed, false),
                Place::Outside => (false, false),
            };
            app_name && *context == FileContext::Runtime && !in_package && (auto || !detected_only)
        }

        /// Whether the entry point is an app signal (§8): a GUI script
        /// always is; a console script is unless, at the root, it targets
        /// the project's own package.
        fn entry_signal(&self, member: bool) -> bool {
            self.entry.is_some_and(|(group, _, targets_acme)| {
                group == "gui"
                    || (group == "console"
                        && (member || !(targets_acme && self.package != Package::Absent)))
            })
        }

        fn names_distribution(&self) -> bool {
            self.name || self.dynamic_name
        }

        /// The §8 table for the workspace root.
        fn root_model(&self, detected_only: bool) -> ProjectMode {
            if self.entry_signal(false) || self.file_signal(detected_only) {
                ProjectMode::App
            } else if self.names_distribution() && self.package == Package::WithInit {
                ProjectMode::Library
            } else {
                ProjectMode::App
            }
        }

        /// §8 for a workspace member: a name and no app signal, with or
        /// without a package.
        fn member_model(&self) -> bool {
            self.names_distribution() && !self.entry_signal(true) && !self.file_signal(true)
        }
    }

    #[test]
    fn mode_follows_the_spec_table() {
        let files =
            std::iter::once(None).chain(FILE_NAMES.iter().flat_map(|(name, app)| {
                PLACES.iter().map(move |place| Some((*name, *app, place)))
            }));
        let files: Vec<_> = files.collect();
        let entries: Vec<_> = std::iter::once(None)
            .chain(ENTRIES.iter().copied().map(Some))
            .collect();
        let names = [(false, false), (true, false), (false, true)];
        let layouts = names.into_iter().flat_map(|(name, dynamic_name)| {
            [false, true].map(|src_layout| (name, dynamic_name, src_layout))
        });
        let mut checked = 0;
        for (name, dynamic_name, src_layout) in layouts {
            for package in [Package::Absent, Package::WithoutInit, Package::WithInit] {
                for &entry in &entries {
                    for &file in &files {
                        let case = ModeCase {
                            name,
                            dynamic_name,
                            src_layout,
                            package,
                            entry,
                            file,
                        };
                        check_mode_case(&case);
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 3 * 2 * 3 * 9 * (1 + 5 * 9));
    }

    fn check_mode_case(case: &ModeCase) {
        let (manifest, sources, path) = (case.manifest(), case.sources(), case.path());
        let candidates: Vec<EntryCandidate> =
            path.as_deref().into_iter().map(auto_candidate).collect();
        let mut config = default_config();
        for mode in [ProjectMode::App, ProjectMode::Library] {
            config.mode = mode;
            assert_eq!(
                resolve_project_mode(&config, &manifest, &sources, &candidates),
                mode,
                "explicit mode: {case:?}"
            );
        }
        config.mode = ProjectMode::Auto;
        // The file as an `entry` config names it, then as only
        // auto-detection finds it.
        assert_eq!(
            resolve_project_mode(&config, &manifest, &sources, &candidates),
            case.root_model(false),
            "root: {case:?} at {path:?}"
        );
        assert_eq!(
            resolve_project_mode(&config, &manifest, &sources, &detect_auto_entries(&sources)),
            case.root_model(true),
            "root, detected only: {case:?} at {path:?}"
        );
        assert_eq!(
            is_library_member(&manifest, &sources),
            case.member_model(),
            "member: {case:?} at {path:?}"
        );
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        const PACKAGES: &[&str] = &["acme", "acme_web", "tools"];
        const TARGETS: &[&str] = &[
            "acme.cli:main",
            "acme",
            "acme_web.main:run",
            "acme_cli:main",
            "tools.run:main",
        ];
        const FILES: &[&str] = &[
            "manage.py",
            "app.py",
            "wsgi.py",
            "deploy/asgi.py",
            "PKG/wsgi.py",
            "PKG/web/app.py",
            "tests/apps/wsgi.py",
            "acme_web/app.py",
            "scripts/run.py",
        ];

        /// The inputs mode resolution reads: the distribution name, package
        /// directories (with or without `__init__.py`), entry points and the
        /// files auto-entry detection sees.
        #[derive(Debug, Clone)]
        struct Inputs {
            name: bool,
            dynamic_name: bool,
            src_layout: bool,
            packages: Vec<(String, bool)>,
            entry_points: Vec<(bool, String)>,
            files: Vec<String>,
        }

        impl Inputs {
            fn package_root(&self) -> &'static str {
                if self.src_layout { "src" } else { "" }
            }

            fn file(&self, path: &str) -> String {
                let root = self.package_root();
                let prefix = if root.is_empty() {
                    String::new()
                } else {
                    format!("{root}/")
                };
                path.replace("PKG/", &format!("{prefix}acme/"))
            }

            fn manifest(&self) -> LoadedManifest {
                let mut manifest = empty_manifest();
                manifest.metadata.name = self.name.then(|| "acme".to_owned());
                manifest.sources.setup_py_dynamic_name = self.dynamic_name;
                manifest.entry_points = self
                    .entry_points
                    .iter()
                    .map(|(gui, target)| {
                        let mut entry = console_script(target);
                        if *gui {
                            entry.group = "gui".to_owned();
                        }
                        entry
                    })
                    .collect();
                manifest
            }

            fn sources(&self) -> DiscoveredSources {
                let mut sources = library_sources();
                sources.layout.package_root = self.package_root().to_owned();
                sources.layout.packages =
                    self.packages.iter().map(|(name, _)| name.clone()).collect();
                let layout = sources.layout.clone();
                sources.files = self
                    .packages
                    .iter()
                    .filter(|(_, init)| *init)
                    .map(|(name, _)| format!("{}/__init__.py", layout.package_dir(name)))
                    .chain(self.files.iter().map(|path| self.file(path)))
                    .map(|path| DiscoveredFile {
                        context: crate::sources::assign_file_context(&path),
                        path,
                        kind: FileKind::Python,
                    })
                    .collect();
                add_target_files(&mut sources, &self.manifest());
                sources
            }

            fn resolve(&self, mode: ProjectMode) -> ProjectMode {
                let mut config = default_config();
                config.mode = mode;
                let candidates: Vec<_> = self
                    .files
                    .iter()
                    .map(|path| auto_candidate(&self.file(path)))
                    .collect();
                resolve_project_mode(&config, &self.manifest(), &self.sources(), &candidates)
            }

            fn library_member(&self) -> bool {
                is_library_member(&self.manifest(), &self.sources())
            }
        }

        fn pick(pool: &'static [&'static str]) -> impl Strategy<Value = String> {
            prop::sample::select(pool).prop_map(str::to_owned)
        }

        fn inputs() -> impl Strategy<Value = Inputs> {
            (
                any::<bool>(),
                prop::bool::weighted(0.2),
                any::<bool>(),
                prop::collection::btree_map(pick(PACKAGES), any::<bool>(), 0..3),
                prop::collection::vec((prop::bool::weighted(0.2), pick(TARGETS)), 0..3),
                prop::collection::vec(pick(FILES), 0..3),
            )
                .prop_map(
                    |(name, dynamic_name, src_layout, packages, entry_points, files)| Inputs {
                        name,
                        dynamic_name,
                        src_layout,
                        packages: packages.into_iter().collect(),
                        entry_points,
                        files,
                    },
                )
        }

        /// Changes that each add one app signal: a GUI script, a console
        /// script outside every package, or a runtime app file at the root.
        #[derive(Debug, Clone)]
        enum AppSignal {
            Gui(String),
            OutsideCli,
            RootFile(String),
        }

        fn app_signal() -> impl Strategy<Value = AppSignal> {
            prop_oneof![
                pick(TARGETS).prop_map(AppSignal::Gui),
                Just(AppSignal::OutsideCli),
                pick(&["manage.py", "app.py", "asgi.py", "wsgi.py"]).prop_map(AppSignal::RootFile),
            ]
        }

        fn add_app_signal(inputs: &mut Inputs, signal: &AppSignal) {
            match signal {
                AppSignal::Gui(target) => inputs.entry_points.push((true, target.clone())),
                AppSignal::OutsideCli => {
                    inputs
                        .entry_points
                        .push((false, "zz_server.cli:main".to_owned()));
                },
                AppSignal::RootFile(path) => inputs.files.push(path.clone()),
            }
        }

        /// Changes that each add one library signal: a declared or
        /// unreadable distribution name, or a package with `__init__.py`.
        #[derive(Debug, Clone)]
        enum LibrarySignal {
            Name,
            DynamicName,
            Package(String),
        }

        fn library_signal() -> impl Strategy<Value = LibrarySignal> {
            prop_oneof![
                Just(LibrarySignal::Name),
                Just(LibrarySignal::DynamicName),
                pick(PACKAGES).prop_map(LibrarySignal::Package),
            ]
        }

        fn add_library_signal(inputs: &mut Inputs, signal: &LibrarySignal) {
            match signal {
                LibrarySignal::Name => inputs.name = true,
                LibrarySignal::DynamicName => inputs.dynamic_name = true,
                LibrarySignal::Package(name) => {
                    inputs.packages.retain(|(package, _)| package != name);
                    inputs.packages.push((name.clone(), true));
                },
            }
        }

        /// App below library, so "never flips to library" is `<=`.
        fn rank(mode: ProjectMode) -> u8 {
            match mode {
                ProjectMode::App => 0,
                ProjectMode::Library => 1,
                ProjectMode::Auto => panic!("auto never resolves to itself"),
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(crate::internals::proptest_cases(256)))]

            /// An explicit `mode` wins over every other input.
            #[test]
            fn explicit_mode_wins(inputs in inputs()) {
                for mode in [ProjectMode::App, ProjectMode::Library] {
                    prop_assert_eq!(inputs.resolve(mode), mode);
                }
            }

            /// One more app signal resolves the project, and the member, to app.
            #[test]
            fn app_signal_never_leans_to_library(
                inputs in inputs(),
                signal in app_signal(),
            ) {
                let mut with = inputs;
                add_app_signal(&mut with, &signal);
                prop_assert_eq!(with.resolve(ProjectMode::Auto), ProjectMode::App);
                prop_assert!(!with.library_member());
            }

            /// One more library signal never turns a library into an app: a
            /// new package can only take app signals into the package.
            #[test]
            fn library_signal_never_leans_to_app(
                inputs in inputs(),
                signal in library_signal(),
            ) {
                let mut with = inputs.clone();
                add_library_signal(&mut with, &signal);
                prop_assert!(
                    rank(with.resolve(ProjectMode::Auto)) >= rank(inputs.resolve(ProjectMode::Auto))
                );
                prop_assert!(with.library_member() || !inputs.library_member());
            }

            /// The order of entry points, packages and files does not matter.
            #[test]
            fn mode_ignores_input_order(inputs in inputs()) {
                let mut reversed = inputs.clone();
                reversed.packages.reverse();
                reversed.entry_points.reverse();
                reversed.files.reverse();
                prop_assert_eq!(
                    reversed.resolve(ProjectMode::Auto),
                    inputs.resolve(ProjectMode::Auto)
                );
                prop_assert_eq!(reversed.library_member(), inputs.library_member());
            }
        }
    }
}
