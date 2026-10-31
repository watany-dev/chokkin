//! First-party module name → file index.

use rustc_hash::FxHashMap as HashMap;

use crate::graph::{FileId, ProjectGraph};
use crate::parser::ParseSummary;
use crate::path_util::join_rel;
use crate::plugins::{SysPathImport, SysPathImporter};
use crate::resolver::PytestImportPaths;
use crate::sources::{DiscoveredSources, path_to_module, shadows};

/// Maps dotted module names to project files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleIndex {
    module_to_file: HashMap<String, FileId>,
    path_to_file: HashMap<String, FileId>,
    /// Modules named only through a symlink inside the root, each with the
    /// link it goes through and the link's target (#1001).
    through_symlink: HashMap<String, (String, String)>,
    pytest: PytestImportPaths,
}

impl ModuleIndex {
    /// Build a module index from discovered sources and the project graph.
    #[must_use]
    pub(crate) fn build(graph: &ProjectGraph, sources: &DiscoveredSources) -> Self {
        Self::with_pytest(graph, sources, PytestImportPaths::build(sources))
    }

    /// [`Self::build`] with the pytest search paths already built from
    /// `sources`: building them reads every member's `pyproject.toml` (#1326).
    #[must_use]
    pub(crate) fn with_pytest(
        graph: &ProjectGraph,
        sources: &DiscoveredSources,
        pytest: PytestImportPaths,
    ) -> Self {
        let mut module_to_file = HashMap::default();
        let mut path_to_file = HashMap::default();
        for (file_id, file) in graph.files() {
            if let Some(module) = path_to_module(&file.path, &sources.layout) {
                insert_module(graph, &mut module_to_file, module, &file.path, file_id);
            }
            path_to_file.insert(file.path.clone(), file_id);
        }
        let mut through_symlink = HashMap::default();
        for (module, (file_id, link)) in symlinked_modules(graph, sources) {
            if let std::collections::hash_map::Entry::Vacant(slot) =
                module_to_file.entry(module.clone())
            {
                slot.insert(file_id);
                through_symlink.insert(module, link);
            }
        }
        Self {
            module_to_file,
            path_to_file,
            through_symlink,
            pytest,
        }
    }

    /// The symlink inside the root that `module` is named through, and its
    /// target, when the file it resolves to has no such name at its own path
    /// (#1001).
    #[must_use]
    pub(crate) fn symlink_for(&self, module: &str) -> Option<(&str, &str)> {
        self.through_symlink
            .get(module)
            .map(|(link, target)| (link.as_str(), target.as_str()))
    }

    /// Also resolve imports from directories the files' `sys.path` edits add.
    /// A module an import names that is one of the project's own is imported
    /// by that name elsewhere too, so it takes none of them.
    #[must_use]
    pub(crate) fn with_sys_path_hints(
        mut self,
        sources: &DiscoveredSources,
        parse: &ParseSummary,
        imports: &[SysPathImport],
    ) -> Self {
        let imports: Vec<SysPathImport> = imports
            .iter()
            .filter(|import| match &import.importer {
                SysPathImporter::File(_) => true,
                SysPathImporter::Named(module) => !self.module_to_file.contains_key(module),
            })
            .cloned()
            .collect();
        self.pytest = self
            .pytest
            .with_sys_path_hints(&sources.root.path, parse, &imports);
        self
    }

    /// Resolve a dotted module name to a first-party file id.
    #[must_use]
    pub(crate) fn resolve(&self, module: &str) -> Option<FileId> {
        self.module_to_file.get(module).copied()
    }

    /// Resolve `module` as imported from the file at `from_path`: pytest's
    /// `sys.path` entries for a test file come first, as prepend mode puts
    /// them ahead of everything else; a module beside a script or in a
    /// directory a `sys.path` edit adds comes last.
    #[must_use]
    pub(crate) fn resolve_from(&self, from_path: &str, module: &str) -> Option<FileId> {
        let file_at = |path: &str| self.path_to_file.get(path).copied();
        self.pytest
            .resolve(from_path, module)
            .and_then(file_at)
            .or_else(|| self.resolve(module))
            .or_else(|| {
                self.pytest
                    .resolve_fallback(from_path, module)
                    .and_then(file_at)
            })
    }

    /// Modules strictly inside package `prefix`, sorted by name.
    #[must_use]
    pub(crate) fn under(&self, prefix: &str) -> Vec<(&str, FileId)> {
        let mut modules: Vec<_> = self
            .module_to_file
            .iter()
            .filter(|(module, _)| {
                module
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'))
            })
            .map(|(module, file_id)| (module.as_str(), *file_id))
            .collect();
        modules.sort_unstable_by_key(|(module, _)| *module);
        modules
    }
}

/// Hold `file_id` for `module` unless a file already holds it. Between
/// directories the first file in the graph stands in for `sys.path` order.
fn insert_module(
    graph: &ProjectGraph,
    modules: &mut HashMap<String, FileId>,
    module: String,
    path: &str,
    file_id: FileId,
) {
    modules
        .entry(module)
        .and_modify(|held: &mut FileId| {
            if graph
                .file(*held)
                .is_some_and(|held| shadows(path, &held.path))
            {
                *held = file_id;
            }
        })
        .or_insert(file_id);
}

/// The module names files have at the paths symlinks inside the root give
/// them (`acme/compat -> _impl/compat` names `acme/_impl/compat/core.py`
/// `acme.compat.core`), each with the file, the link and its target. A name
/// a file has at its own path takes precedence, so the caller adds only the
/// new ones.
fn symlinked_modules(
    graph: &ProjectGraph,
    sources: &DiscoveredSources,
) -> HashMap<String, (FileId, (String, String))> {
    let layout = &sources.layout;
    // Module → file, its path through the link, and the link with its target.
    let mut modules: HashMap<String, (FileId, String, (&String, &String))> = HashMap::default();
    for (link, target) in &layout.symlinks {
        for (file_id, file) in graph.files() {
            let path = if file.path == *target {
                link.clone()
            } else if target.is_empty() {
                join_rel(link, &file.path)
            } else {
                let Some(rest) = file
                    .path
                    .strip_prefix(target.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
                else {
                    continue;
                };
                join_rel(link, rest)
            };
            let Some(module) = path_to_module(&path, layout) else {
                continue;
            };
            modules
                .entry(module)
                .and_modify(|held| {
                    if shadows(&path, &held.1) {
                        *held = (file_id, path.clone(), (link, target));
                    }
                })
                .or_insert((file_id, path, (link, target)));
        }
    }
    modules
        .into_iter()
        .map(|(module, (file_id, _, (link, target)))| {
            (module, (file_id, (link.clone(), target.clone())))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::graph::{FileNode, ProjectGraph};
    use crate::sources::{FileContext, FileKind, LayoutInfo};

    #[test]
    fn module_index_resolves_registered_file() {
        let mut graph = ProjectGraph::new(ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        });
        let file_id = graph
            .intern_file(FileNode {
                path: "src/acme/foo.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("file");
        let sources = DiscoveredSources {
            root: graph.root.clone(),
            layout: LayoutInfo::new("src", vec!["acme".to_owned()]),
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let index = ModuleIndex::build(&graph, &sources);
        assert_eq!(index.resolve("acme.foo"), Some(file_id));
        let added = graph
            .intern_file(FileNode {
                path: "src/acme/bar.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("new file");
        let updated = ModuleIndex::build(&graph, &sources);
        assert_eq!(updated.resolve("acme.bar"), Some(added));
        assert_eq!(
            updated.under("acme"),
            vec![("acme.bar", added), ("acme.foo", file_id)]
        );
        assert_eq!(updated.under("acme.foo"), []);
        assert_eq!(updated.under("acm"), []);
    }

    /// `CPython`'s path finder tries `foo/__init__.py` before `foo.py` in the
    /// same directory, so the package is what `import foo` loads, whichever
    /// file the graph holds first (#1108).
    #[test]
    fn a_package_shadows_the_module_beside_it() {
        for paths in [
            ["src/acme/foo.py", "src/acme/foo/__init__.py"],
            ["src/acme/foo/__init__.py", "src/acme/foo.py"],
        ] {
            let mut graph = ProjectGraph::new(ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
            });
            for path in paths {
                graph
                    .intern_file(FileNode {
                        path: path.to_owned(),
                        context: FileContext::Runtime,
                        kind: FileKind::Python,
                    })
                    .expect("file");
            }
            let sources = DiscoveredSources {
                root: graph.root.clone(),
                layout: LayoutInfo::new("src", vec!["acme".to_owned()]),
                effective_globs: Vec::new(),
                files: Vec::new(),
                warnings: Vec::new(),
            };
            let index = ModuleIndex::build(&graph, &sources);
            assert_eq!(
                index.resolve("acme.foo"),
                graph.file_id("src/acme/foo/__init__.py"),
                "{paths:?}"
            );
        }
    }

    /// A file behind a symlink inside the root has the module name its path
    /// through the link gives, too; a name a file has at its own path wins
    /// (#1001).
    #[test]
    fn modules_resolve_through_symlinks_inside_the_root() {
        let mut graph = ProjectGraph::new(ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        });
        let mut intern = |path: &str| {
            graph
                .intern_file(FileNode {
                    path: path.to_owned(),
                    context: FileContext::Runtime,
                    kind: FileKind::Python,
                })
                .expect("file")
        };
        let core = intern("acme/_impl/compat/core.py");
        let package = intern("acme/_impl/compat/__init__.py");
        let real = intern("acme/real.py");
        let own = intern("acme/own.py");
        let mut layout = LayoutInfo::new("", vec!["acme".to_owned()]);
        layout.symlinks = [
            ("acme/compat", "acme/_impl/compat"),
            ("acme/alias.py", "acme/real.py"),
            // A file already named `acme.own` keeps its name.
            ("acme/own.py", "acme/real.py"),
        ]
        .into_iter()
        .map(|(link, target)| (link.to_owned(), target.to_owned()))
        .collect();
        let sources = DiscoveredSources {
            root: graph.root.clone(),
            layout,
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let index = ModuleIndex::build(&graph, &sources);
        assert_eq!(index.resolve("acme.compat.core"), Some(core));
        assert_eq!(index.resolve("acme.compat"), Some(package));
        assert_eq!(index.resolve("acme.alias"), Some(real));
        assert_eq!(index.resolve("acme.own"), Some(own));
        assert_eq!(index.resolve("acme._impl.compat.core"), Some(core));
        assert_eq!(
            index.symlink_for("acme.compat.core"),
            Some(("acme/compat", "acme/_impl/compat"))
        );
        assert_eq!(index.symlink_for("acme._impl.compat.core"), None);
        assert_eq!(index.symlink_for("acme.own"), None);
    }

    #[test]
    fn modules_of_the_project_take_no_named_sys_path_hints() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let mut graph = ProjectGraph::new(ProjectRoot {
            path: temp.path().to_path_buf(),
            marker: RootMarker::PyProjectToml,
        });
        let files = [
            "docs/conf.py",
            "exts/docroles.py",
            "exts/yaml_utils.py",
            "src/acme/__init__.py",
            "src/acme/sphinxext.py",
        ];
        let mut ids = HashMap::default();
        for path in files {
            let id = graph
                .intern_file(FileNode {
                    path: path.to_owned(),
                    context: FileContext::Runtime,
                    kind: FileKind::Python,
                })
                .expect("file");
            ids.insert(path, id);
        }
        let sources = DiscoveredSources {
            root: graph.root.clone(),
            layout: LayoutInfo::new("src", vec!["acme".to_owned()]),
            effective_globs: Vec::new(),
            files: files
                .iter()
                .map(|path| crate::sources::DiscoveredFile {
                    path: (*path).to_owned(),
                    kind: FileKind::Python,
                    context: FileContext::Runtime,
                })
                .collect(),
            warnings: Vec::new(),
        };
        let parse = ParseSummary {
            modules: vec![crate::parser::ParsedModule {
                path: "docs/conf.py".to_owned(),
                sys_path_hints: vec!["exts".to_owned(), "src".to_owned()],
                ..Default::default()
            }],
        };
        let named = |module: &str| SysPathImport {
            importer: SysPathImporter::Named(module.to_owned()),
            imported: "docs/conf.py".to_owned(),
        };
        let index = ModuleIndex::build(&graph, &sources).with_sys_path_hints(
            &sources,
            &parse,
            &[named("docroles"), named("acme.sphinxext")],
        );
        assert_eq!(
            index.resolve_from("exts/docroles.py", "yaml_utils"),
            Some(ids["exts/yaml_utils.py"])
        );
        // The project's own package imports it by that name too.
        assert_eq!(
            index.resolve_from("src/acme/sphinxext.py", "yaml_utils"),
            None
        );
    }
}
