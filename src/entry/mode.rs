//! `mode = auto` resolution (§8).

use crate::config::{ChokkinConfig, ProjectMode};
use crate::manifest::LoadedManifest;
use crate::resolver::import_root;
use crate::sources::{DiscoveredSources, FileContext, LayoutInfo};

use super::auto::detect_auto_entries;
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
        resolve_project_mode(&config, manifest, &library_sources(), candidates)
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
        assert!(!is_library_member(&manifest, &sources));
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
