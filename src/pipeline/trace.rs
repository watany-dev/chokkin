//! Reachability trace formatting for `--trace`.

use std::fmt::Write;

use crate::entry::{EntryOrigin, EntryPlan};
use crate::graph::{FileId, GraphEdge, ProjectGraph};
use crate::reachability::{
    ModuleIndex, ReachabilityReport, TracePath, TraceStep, UnreachableReason, trace_to_file,
};
use crate::sources::DiscoveredSources;

/// Normalize a user-supplied path for graph lookup.
pub fn normalize_trace_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_owned()
}

/// Find a file id for a root-relative trace target.
pub fn file_id_for_trace(graph: &ProjectGraph, target: &str) -> Option<crate::graph::FileId> {
    let normalized = normalize_trace_path(target);
    graph.file_id(&normalized)
}

/// Format a reachability trace as a tree for CLI output.
#[must_use]
pub fn format_trace(path: &TracePath) -> String {
    let mut out = String::from("Trace path:\n");
    for (index, step) in path.steps.iter().enumerate() {
        let prefix = if index + 1 == path.steps.len() {
            "└─ "
        } else {
            "├─ "
        };
        out.push_str(prefix);
        out.push_str(&format_step(step));
        out.push('\n');
    }
    out
}

fn format_step(step: &TraceStep) -> String {
    match step {
        TraceStep::Entry { label, .. } => format!("entry {label}"),
        TraceStep::File { path, .. } => format!("file {path}"),
        TraceStep::Import { module, line } => format!("import {module} (line {line})"),
        TraceStep::PluginRef { module, label } => format!("plugin ref {module} ({label})"),
        TraceStep::DynamicImport { module, line } => {
            format!("dynamic import {module} (line {line})")
        },
    }
}

/// Build trace output for a target path, or an error message when unreachable.
#[must_use]
pub fn trace_output(
    report: &ReachabilityReport,
    graph: &ProjectGraph,
    entry: &EntryPlan,
    sources: &DiscoveredSources,
    target: &str,
) -> String {
    let Some(file_id) = file_id_for_trace(graph, target) else {
        return format!("trace: file `{target}` was not discovered in this project\n");
    };
    if let Some(path) = trace_to_file(report, file_id) {
        return format_trace(&path);
    }
    format_negative_trace(report, graph, entry, sources, target, file_id)
}

#[allow(clippy::too_many_arguments)]
fn format_negative_trace(
    report: &ReachabilityReport,
    graph: &ProjectGraph,
    entry: &EntryPlan,
    sources: &DiscoveredSources,
    target: &str,
    file_id: FileId,
) -> String {
    let mut out = format!("Negative trace for {target}:\n\n");
    let _ = write!(
        out,
        "  reason: {}\n\n",
        unreachable_reason_label(report, target)
    );

    out.push_str("  entry roots analyzed:\n");
    if entry.roots.is_empty() {
        out.push_str("    (none)\n");
    } else {
        for (index, root) in entry.roots.iter().enumerate() {
            let prefix = if index + 1 == entry.roots.len() {
                "└─ "
            } else {
                "├─ "
            };
            let origin = root
                .origins
                .first()
                .map_or_else(|| "entry".to_owned(), format_entry_origin);
            let _ = writeln!(out, "    {prefix}{} ({origin})", root.spec.path);
        }
    }

    let importers = collect_incoming_imports(graph, sources, file_id);
    if !importers.is_empty() {
        out.push_str("\n  incoming imports (all unreachable):\n");
        for (index, (path, line, module)) in importers.iter().enumerate() {
            let prefix = if index + 1 == importers.len() {
                "└─ "
            } else {
                "├─ "
            };
            let importer_reachable = graph
                .file_id(path)
                .is_some_and(|id| report.reachable.contains(&id));
            let suffix = if importer_reachable {
                String::new()
            } else {
                " (file also unreachable)".to_owned()
            };
            let _ = writeln!(out, "    {prefix}{path}:{line} imports {module}{suffix}");
        }
    }

    out.push('\n');
    out.push_str("  suggestion: add to [tool.chokkin].entry or verify import chain\n");
    out
}

fn unreachable_reason_label(report: &ReachabilityReport, target: &str) -> String {
    let Some(file) = report
        .unreachable
        .iter()
        .find(|candidate| candidate.path == target)
    else {
        return "not reachable from any entry root".to_owned();
    };

    file.reasons
        .iter()
        .copied()
        .map(format_unreachable_reason)
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_unreachable_reason(reason: UnreachableReason) -> String {
    match reason {
        UnreachableReason::NotReachable => "not reachable from any entry root".to_owned(),
        UnreachableReason::ExcludedInit => "excluded __init__.py".to_owned(),
        UnreachableReason::ExcludedStub => "excluded stub".to_owned(),
        UnreachableReason::ExcludedTestContext => "excluded test context".to_owned(),
        UnreachableReason::ExcludedProductionContext => "excluded in production".to_owned(),
        UnreachableReason::FrameworkUsed => "framework-used".to_owned(),
    }
}

fn format_entry_origin(origin: &EntryOrigin) -> String {
    match origin {
        EntryOrigin::Config => "config".to_owned(),
        EntryOrigin::Manifest { name, group } => format!("manifest: [{group}].{name}"),
        EntryOrigin::Plugin { plugin, label } => format!("plugin: {} ({label})", plugin.as_key()),
        EntryOrigin::Auto { rule } => format!("auto: {rule}"),
        EntryOrigin::SymbolRef { label, .. } => format!("symbol: {label}"),
    }
}

fn collect_incoming_imports(
    graph: &ProjectGraph,
    sources: &DiscoveredSources,
    target: FileId,
) -> Vec<(String, u32, String)> {
    let module_index = ModuleIndex::build(graph, sources);
    let mut importers = Vec::new();

    for edge in graph.edges() {
        let GraphEdge::FileImportsModule { file, module, line } = edge else {
            continue;
        };
        let Some(module_node) = graph.module(*module) else {
            continue;
        };
        if module_index.resolve(&module_node.name) != Some(target) {
            continue;
        }
        let Some(file_node) = graph.file(*file) else {
            continue;
        };
        importers.push((file_node.path.clone(), *line, module_node.name.clone()));
    }

    importers.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    importers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EntrySpec;
    use crate::config::{Confidence, ProjectMode};
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::entry::{EntryPlan, EntryRoot, ResolvedMode};
    use crate::graph::{FileNode, ModuleOrigin, add_parsed_imports};
    use crate::parser::ParsedModule;
    use crate::reachability::UnreachableFile;
    use crate::resolver::ResolveConfidence;
    use crate::sources::{
        DiscoveredFile, DiscoveredSources, FileContext, FileKind, LayoutInfo, ProjectLayout,
    };

    fn src_layout() -> LayoutInfo {
        LayoutInfo {
            layout: ProjectLayout::Src,
            packages: vec!["acme".to_owned()],
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        }
    }

    fn sources_with(paths: &[&str]) -> DiscoveredSources {
        let layout = src_layout();
        DiscoveredSources {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
                start: std::env::temp_dir(),
            },
            layout: layout.clone(),
            effective_globs: Vec::new(),
            files: paths
                .iter()
                .map(|path| DiscoveredFile {
                    path: (*path).to_owned(),
                    kind: FileKind::Python,
                    context: crate::sources::assign_file_context(path, &layout),
                })
                .collect(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn normalize_strips_dot_slash_and_backslashes() {
        assert_eq!(
            normalize_trace_path(".\\src\\pkg\\mod.py"),
            "src/pkg/mod.py"
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn negative_trace_lists_entry_roots_and_incoming_imports() {
        let sources = sources_with(&[
            "src/acme/cli.py",
            "src/acme/legacy.py",
            "src/acme/old_api.py",
        ]);
        let mut graph = ProjectGraph::new(sources.root.clone());
        for file in &sources.files {
            let _ = graph.intern_file(FileNode {
                path: file.path.clone(),
                context: file.context,
                kind: file.kind,
            });
        }
        let cli_id = graph.file_id("src/acme/cli.py").expect("cli");
        let legacy_id = graph.file_id("src/acme/legacy.py").expect("legacy");
        let old_api_id = graph.file_id("src/acme/old_api.py").expect("old api");

        let _legacy_module =
            graph.intern_module("acme.legacy".to_owned(), ModuleOrigin::FirstParty);
        add_parsed_imports(
            &mut graph,
            old_api_id,
            &ParsedModule {
                path: "src/acme/old_api.py".to_owned(),
                imports: vec![crate::parser::ImportRef {
                    module: "acme.legacy".to_owned(),
                    name: None,
                    alias: None,
                    line: 3,
                    kind: crate::parser::ImportKind::Import,
                    context: crate::parser::ImportContext::Runtime,
                    optional: false,
                    platform_guarded: false,
                    relative_level: 0,
                }],
                dynamic_imports: Vec::new(),
                symbols: Vec::new(),
                exports: Vec::new(),
                ignores: Vec::new(),
                has_opaque_dynamic_import: false,
                diagnostics: Vec::new(),
            },
        )
        .expect("imports");

        let mut report = ReachabilityReport::empty();
        report.reachable.insert(cli_id);
        report.unreachable = vec![
            UnreachableFile {
                file: legacy_id,
                path: "src/acme/legacy.py".to_owned(),
                reasons: vec![UnreachableReason::NotReachable],
                max_confidence: Confidence::Certain,
            },
            UnreachableFile {
                file: old_api_id,
                path: "src/acme/old_api.py".to_owned(),
                reasons: vec![UnreachableReason::NotReachable],
                max_confidence: Confidence::Certain,
            },
        ];

        let entry = EntryPlan {
            mode: ResolvedMode {
                mode: ProjectMode::App,
                confidence: ResolveConfidence::Certain,
            },
            roots: vec![EntryRoot {
                spec: EntrySpec {
                    path: "src/acme/cli.py".to_owned(),
                    symbol: None,
                },
                context: FileContext::Runtime,
                origins: vec![EntryOrigin::Manifest {
                    name: "cli".to_owned(),
                    group: "console".to_owned(),
                }],
            }],
            warnings: Vec::new(),
        };

        let output = format_negative_trace(
            &report,
            &graph,
            &entry,
            &sources,
            "src/acme/legacy.py",
            legacy_id,
        );
        assert!(output.contains("Negative trace for src/acme/legacy.py"));
        assert!(output.contains("entry roots analyzed:"));
        assert!(output.contains("src/acme/cli.py (manifest: [console].cli)"));
        assert!(output.contains("incoming imports (all unreachable):"));
        assert!(output.contains("src/acme/old_api.py:3 imports acme.legacy"));
    }
}
