//! Attach parsed import edges to the project graph.

use crate::parser::ParsedModule;

use super::error::GraphError;
use super::types::{GraphEdge, ModuleOrigin, ProjectGraph};

/// Attach import edges from a parsed module onto the graph.
///
/// # Errors
///
/// Returns [`GraphError::Invariant`] when `file_id` is not registered.
pub fn add_parsed_imports(
    graph: &mut ProjectGraph,
    file_id: super::types::FileId,
    parsed: &ParsedModule,
) -> Result<(), GraphError> {
    if !graph.file_id(&parsed.path).is_some_and(|id| id == file_id) {
        return Err(GraphError::Invariant {
            detail: format!("file id does not match parsed path `{}`", parsed.path),
        });
    }

    push_module_import_edges(
        graph,
        file_id,
        parsed.imports.iter().map(|i| (i.module.as_str(), i.line)),
    );
    push_module_import_edges(
        graph,
        file_id,
        parsed
            .dynamic_imports
            .iter()
            .map(|i| (i.module.as_str(), i.line)),
    );

    Ok(())
}

fn push_module_import_edges<'a>(
    graph: &mut ProjectGraph,
    file_id: super::types::FileId,
    imports: impl IntoIterator<Item = (&'a str, u32)>,
) {
    for (module, line) in imports {
        if module.is_empty() {
            continue;
        }
        let module_id = graph.intern_module(module.to_owned(), ModuleOrigin::Unknown);
        graph.push_edge(GraphEdge::FileImportsModule {
            file: file_id,
            module: module_id,
            line,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::parser::{DynamicImport, ImportContext, ImportKind, ImportRef, ParsedModule};
    use crate::sources::{FileContext, FileKind};

    #[test]
    fn adds_import_edges_for_static_and_dynamic_imports() {
        let mut graph = ProjectGraph::new(ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
        });
        let file_id = graph
            .intern_file(super::super::types::FileNode {
                path: "app.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("file");
        let parsed = ParsedModule {
            path: "app.py".to_owned(),
            imports: vec![
                import_ref("os", 1),
                import_ref("", 2),
                import_ref("json", 3),
            ],
            dynamic_imports: vec![
                DynamicImport {
                    module: "plugins.a".to_owned(),
                    line: 5,
                },
                DynamicImport {
                    module: String::new(),
                    line: 6,
                },
                DynamicImport {
                    module: "os".to_owned(),
                    line: 7,
                },
            ],
            dynamic_import_prefixes: Vec::new(),
            attribute_accesses: Vec::new(),
            symbols: Vec::new(),
            exports: Vec::new(),
            used_import_bindings: Vec::new(),
            ignores: Vec::new(),
            has_opaque_dynamic_import: false,
            runs_python_file: false,
            shell_commands: Vec::new(),
            decorator_sites: Vec::new(),
            diagnostics: Vec::new(),
            skipped: false,
        };
        add_parsed_imports(&mut graph, file_id, &parsed).expect("edges");

        let names = ["os", "json", "plugins.a"];
        let ids: Vec<_> = names
            .iter()
            .map(|name| graph.module_id(name).expect("module interned"))
            .collect();
        let edge = |module, line| GraphEdge::FileImportsModule {
            file: file_id,
            module,
            line,
        };
        assert_eq!(
            graph.edges(),
            [
                edge(ids[0], 1),
                edge(ids[1], 3),
                edge(ids[2], 5),
                edge(ids[0], 7),
            ]
        );
        assert_eq!(graph.module_count(), names.len());
        assert!(graph.module_id("").is_none());
        for id in ids {
            assert_eq!(
                graph.module(id).map(|node| node.origin),
                Some(ModuleOrigin::Unknown)
            );
        }
    }

    fn import_ref(module: &str, line: u32) -> ImportRef {
        ImportRef {
            module: module.to_owned(),
            name: None,
            alias: None,
            line,
            kind: ImportKind::Import,
            context: ImportContext::Runtime,
            optional: false,
            platform_guarded: false,
            relative_level: 0,
        }
    }
}
