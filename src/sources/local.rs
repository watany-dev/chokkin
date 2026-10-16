//! The project's local trees: workspace members and `[tool.uv.sources]`
//! entries the checkout provides (#547).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::config::{ChokkinConfig, ResolvedWorkspaceMember};
use crate::manifest::{LoadedManifest, ProjectMetadata, UvSourceKind, UvToolSettings};

use super::layout::infer_layout;
use super::types::DiscoveredFile;

/// Every way the root names a tree of its own checkout, decided once at
/// probe time so resolution and dependency rules ask the same question.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalTrees {
    members: Vec<ResolvedWorkspaceMember>,
    /// Import roots naming a member: its id or directory basename, a uv
    /// `members` glob's basename, or a `[tool.chokkin.workspaces]` path's.
    member_roots: BTreeSet<String>,
    /// Normalized names of `workspace = true` and `path` sources.
    local_sources: BTreeSet<String>,
    /// Import roots a `path` source's tree provides, each with the source
    /// names providing it.
    path_imports: BTreeMap<String, Vec<String>>,
}

impl LocalTrees {
    /// A `path` source's tree is read here, never cached, so it cannot go
    /// stale; a tree without packages falls back to the distribution name.
    #[must_use]
    pub fn new(
        manifest: &LoadedManifest,
        members: &[ResolvedWorkspaceMember],
        config: &ChokkinConfig,
    ) -> Self {
        let members: Vec<ResolvedWorkspaceMember> = members
            .iter()
            .map(|member| ResolvedWorkspaceMember {
                path: member.path.replace('\\', "/"),
                ..member.clone()
            })
            .collect();
        let member_roots = members
            .iter()
            .flat_map(|member| [member.id.as_str(), member_basename(&member.path)])
            .chain(
                manifest
                    .uv_workspace
                    .iter()
                    .flat_map(|hint| &hint.members)
                    .map(|pattern| member_basename(pattern)),
            )
            .chain(
                config
                    .workspaces
                    .values()
                    .map(|workspace| member_basename(&workspace.path)),
            )
            .map(str::to_owned)
            .collect();
        let local_sources = manifest
            .uv
            .sources
            .iter()
            .filter(|source| matches!(source.kind, UvSourceKind::Workspace | UvSourceKind::Path(_)))
            .map(|source| source.name.clone())
            .collect();
        let mut path_imports: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for source in &manifest.uv.sources {
            let UvSourceKind::Path(path) = &source.kind else {
                continue;
            };
            let metadata = ProjectMetadata {
                name: Some(source.name.clone()),
                ..ProjectMetadata::default()
            };
            let (layout, _) = infer_layout(
                &manifest.root.path.join(path),
                &metadata,
                &UvToolSettings::default(),
            );
            let mut packages = layout.packages;
            if packages.is_empty() {
                packages.push(source.name.replace('-', "_"));
            }
            for package in packages {
                let distributions = path_imports.entry(package).or_default();
                if !distributions.contains(&source.name) {
                    distributions.push(source.name.clone());
                }
            }
        }
        Self {
            members,
            member_roots,
            local_sources,
            path_imports,
        }
    }

    /// Resolved workspace members, with `/`-separated paths.
    #[must_use]
    pub(crate) fn members(&self) -> &[ResolvedWorkspaceMember] {
        &self.members
    }

    #[must_use]
    pub(crate) fn names_member(&self, import_root: &str) -> bool {
        self.member_roots.contains(import_root)
    }

    /// Whether the normalized distribution `name` comes from a local
    /// `workspace` or `path` source.
    #[must_use]
    pub(crate) fn is_local_source(&self, name: &str) -> bool {
        self.local_sources.contains(name)
    }

    #[must_use]
    pub(crate) fn path_imports(&self) -> &BTreeMap<String, Vec<String>> {
        &self.path_imports
    }

    /// Modules importable from the root or a member, by [`importable_modules`].
    #[must_use]
    pub(crate) fn modules(&self, files: &[DiscoveredFile]) -> HashMap<String, bool> {
        let roots: HashSet<&str> = std::iter::once("")
            .chain(self.members.iter().map(|member| member.path.as_str()))
            .collect();
        importable_modules(files.iter().map(|file| file.path.as_str()), &roots)
    }
}

/// Dotted modules the files under `roots` provide, imported from a root or
/// from its `src` directory (`providers/x/src/airflow/models.py` gives
/// `airflow` and `airflow.models` under root `providers/x`), each `true` when
/// a file holds that module itself rather than only a submodule. `""` is the
/// tree `paths` are relative to.
///
/// A file below no root, or below a deeper `src`, names no module from
/// there: `tests/fixtures/vendor/poetry/core/` is not importable as
/// `poetry.core`. Neither does a file other than `.py` or `.pyi`.
#[must_use]
pub(crate) fn importable_modules<'a>(
    paths: impl IntoIterator<Item = &'a str>,
    roots: &HashSet<&str>,
) -> HashMap<String, bool> {
    let is_root = |dir: &str| {
        roots.contains(dir)
            || (dir == "src" && roots.contains(""))
            || dir
                .strip_suffix("/src")
                .is_some_and(|parent| roots.contains(parent))
    };
    let mut modules: HashMap<String, bool> = HashMap::new();
    for path in paths {
        let path = path.replace('\\', "/");
        let Some(stem) = path
            .strip_suffix(".py")
            .or_else(|| path.strip_suffix(".pyi"))
        else {
            continue;
        };
        let module = if stem == "__init__" {
            ""
        } else {
            stem.strip_suffix("/__init__").unwrap_or(stem)
        };
        // Roots by hash lookup per ancestor, not a prefix test against every
        // member (#592).
        let starts = std::iter::once(0)
            .filter(|_| is_root(""))
            .chain(
                stem.match_indices('/')
                    .map(|(slash, _)| slash)
                    .filter(|&slash| is_root(&stem[..slash]))
                    .map(|slash| slash + 1),
            )
            .filter(|&start| start < module.len());
        for start in starts {
            let rest = &module[start..];
            for (end, _) in rest.match_indices('/') {
                modules.entry(rest[..end].replace('/', ".")).or_default();
            }
            modules.insert(rest.replace('/', "."), true);
        }
    }
    modules
}

fn member_basename(pattern: &str) -> &str {
    pattern
        .trim_end_matches("/*")
        .trim_end_matches('*')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{UvWorkspaceHint, WorkspaceOverride, default_config};
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::manifest::{LockfileGraph, ManifestSources, UvSource};
    use crate::sources::{FileContext, FileKind};

    fn modules(paths: &[&str], roots: &[&str]) -> HashMap<String, bool> {
        importable_modules(paths.iter().copied(), &roots.iter().copied().collect())
    }

    fn member(id: &str, path: &str) -> ResolvedWorkspaceMember {
        ResolvedWorkspaceMember {
            id: id.to_owned(),
            path: path.to_owned(),
            pyproject_toml: None,
        }
    }

    fn manifest_at(root: &std::path::Path, sources: Vec<UvSource>) -> LoadedManifest {
        LoadedManifest {
            root: ProjectRoot {
                path: root.to_path_buf(),
                marker: RootMarker::PyProjectToml,
            },
            metadata: ProjectMetadata::default(),
            dependencies: Vec::new(),
            constraints: Vec::new(),
            uv: UvToolSettings {
                sources,
                ..UvToolSettings::default()
            },
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: LockfileGraph::default(),
            sources: ManifestSources::default(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn modules_start_at_a_root_or_its_src_and_mark_files() {
        let found = modules(
            &[
                "airflow-core/src/airflow/__init__.py",
                "airflow-core/src/airflow/models/dag.py",
                "airflow-core/tests/utils.py",
                "airflow-core/tests/fixtures/src/vendored/core.py",
                "task-sdk/src/airflow/sdk/__init__.py",
                "airflow-core/notes.ipynb",
            ],
            &["airflow-core"],
        );
        let expected: HashMap<String, bool> = [
            ("airflow", true),
            ("airflow.models", false),
            ("airflow.models.dag", true),
            ("src", false),
            ("src.airflow", true),
            ("src.airflow.models", false),
            ("src.airflow.models.dag", true),
            ("tests", false),
            ("tests.utils", true),
            ("tests.fixtures", false),
            ("tests.fixtures.src", false),
            ("tests.fixtures.src.vendored", false),
            ("tests.fixtures.src.vendored.core", true),
        ]
        .into_iter()
        .map(|(module, file)| (module.to_owned(), file))
        .collect();
        assert_eq!(found, expected);
    }

    /// A directory holding no `.py` / `.pyi` file is not importable, so it
    /// does not keep `poetry.core` first-party.
    #[test]
    fn tree_without_python_files_provides_no_module() {
        let found = modules(&["poetry/core/demo.ipynb", "poetry/README.md"], &[""]);
        assert!(found.is_empty());
    }

    #[test]
    fn tree_root_takes_top_level_src_and_backslashes() {
        let found = modules(&["src\\acme\\__init__.py", "__init__.py"], &[""]);
        assert_eq!(found.get("acme"), Some(&true));
        assert_eq!(found.get("src.acme"), Some(&true));
        assert!(!found.contains_key(""));
        assert!(!found.keys().any(|module| module.ends_with("__init__")));
    }

    /// The resolver's project-wide set is the union of each tree's set, from
    /// the project root and from every member root.
    #[test]
    fn project_modules_are_the_union_of_each_tree() {
        let files: Vec<DiscoveredFile> = [
            "acme/core.py",
            "src/pkg/stub.pyi",
            "packages/api/src/api/views.py",
            "packages\\api\\plugins\\plug\\hooks.py",
            "README.md",
            "tests/fixtures/vendor/poetry/core/__init__.py",
        ]
        .into_iter()
        .map(|path| DiscoveredFile {
            path: path.to_owned(),
            kind: FileKind::Python,
            context: FileContext::Runtime,
        })
        .collect();
        let trees = LocalTrees {
            members: vec![
                member("api", "packages/api"),
                member("plugins", "packages/api/plugins"),
            ],
            ..LocalTrees::default()
        };
        let project = trees.modules(&files);
        let mut union = HashMap::new();
        for root in ["", "packages/api", "packages/api/plugins"] {
            let paths = files.iter().map(|file| file.path.as_str());
            for (module, file) in importable_modules(paths, &HashSet::from([root])) {
                *union.entry(module).or_insert(false) |= file;
            }
        }
        assert_eq!(project, union);
        for module in [
            "acme.core",
            "pkg.stub",
            "api.views",
            "plug.hooks",
            "plugins.plug.hooks",
        ] {
            assert!(project.contains_key(module), "{module}");
        }
        assert!(!project.contains_key("poetry.core"));
    }

    #[test]
    fn member_roots_cover_ids_basenames_uv_globs_and_overrides() {
        let mut manifest = manifest_at(std::path::Path::new("/nonexistent"), Vec::new());
        manifest.uv_workspace = Some(UvWorkspaceHint {
            members: vec!["packages/billing".to_owned(), "libs/*".to_owned()],
            exclude: Vec::new(),
        });
        let mut config = default_config();
        config.workspaces.insert(
            "shipping".to_owned(),
            WorkspaceOverride {
                path: "svc/shipping".to_owned(),
                entry: None,
                project: None,
                mode: None,
            },
        );
        let trees = LocalTrees::new(
            &manifest,
            &[member("api-service", "services\\api")],
            &config,
        );
        for root in ["api-service", "api", "billing", "libs", "shipping"] {
            assert!(trees.names_member(root), "{root}");
        }
        assert!(!trees.names_member("services"));
        assert_eq!(trees.members()[0].path, "services/api");
    }

    #[test]
    fn local_sources_are_workspace_and_path_entries() {
        let source = |name: &str, kind| UvSource {
            name: name.to_owned(),
            kind,
        };
        let manifest = manifest_at(
            &std::env::temp_dir().join("chokkin-no-such-root"),
            vec![
                source("billing", UvSourceKind::Workspace),
                source("my-lib", UvSourceKind::Path("does/not/exist".to_owned())),
                source("torch", UvSourceKind::Other),
            ],
        );
        let trees = LocalTrees::new(&manifest, &[], &default_config());
        assert!(trees.is_local_source("billing"));
        assert!(trees.is_local_source("my-lib"));
        assert!(!trees.is_local_source("torch"));
        // A missing tree falls back to the distribution name.
        assert_eq!(
            trees.path_imports(),
            &BTreeMap::from([("my_lib".to_owned(), vec!["my-lib".to_owned()])])
        );
    }

    #[test]
    fn path_source_picks_the_flat_package_named_after_the_distribution() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        for package in ["aaa_helpers", "my_lib"] {
            let package_dir = dir.path().join("vendor").join(package);
            std::fs::create_dir_all(&package_dir).expect("mkdir");
            std::fs::write(package_dir.join("__init__.py"), "").expect("write");
        }
        let manifest = manifest_at(
            dir.path(),
            vec![UvSource {
                name: "my-lib".to_owned(),
                kind: UvSourceKind::Path("vendor".to_owned()),
            }],
        );
        let trees = LocalTrees::new(&manifest, &[], &default_config());
        assert_eq!(
            trees.path_imports(),
            &BTreeMap::from([("my_lib".to_owned(), vec!["my-lib".to_owned()])])
        );
    }
}
