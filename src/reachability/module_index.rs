//! First-party module name → file index.

use std::collections::HashMap;

use crate::graph::{FileId, ProjectGraph};
use crate::parser::ParseSummary;
use crate::plugins::{SysPathImport, SysPathImporter};
use crate::resolver::PytestImportPaths;
use crate::sources::{DiscoveredSources, path_to_module, shadows};

/// Maps dotted module names to project files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleIndex {
    module_to_file: HashMap<String, FileId>,
    path_to_file: HashMap<String, FileId>,
    pytest: PytestImportPaths,
}

impl ModuleIndex {
    /// Build a module index from discovered sources and the project graph.
    #[must_use]
    pub(crate) fn build(graph: &ProjectGraph, sources: &DiscoveredSources) -> Self {
        let mut module_to_file = HashMap::new();
        let mut path_to_file = HashMap::new();
        for (file_id, file) in graph.files() {
            if let Some(module) = path_to_module(&file.path, &sources.layout) {
                // Between directories the first file in the graph stands in
                // for `sys.path` order.
                module_to_file
                    .entry(module)
                    .and_modify(|held: &mut FileId| {
                        if graph
                            .file(*held)
                            .is_some_and(|held| shadows(&file.path, &held.path))
                        {
                            *held = file_id;
                        }
                    })
                    .or_insert(file_id);
            }
            path_to_file.insert(file.path.clone(), file_id);
        }
        Self {
            module_to_file,
            path_to_file,
            pytest: PytestImportPaths::build(sources),
        }
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
        let mut ids = HashMap::new();
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
