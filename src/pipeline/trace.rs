//! Reachability trace formatting for `--trace`.

use crate::graph::ProjectGraph;
use crate::reachability::{ReachabilityReport, TracePath, TraceStep, trace_to_file};

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
pub fn trace_output(report: &ReachabilityReport, graph: &ProjectGraph, target: &str) -> String {
    let Some(file_id) = file_id_for_trace(graph, target) else {
        return format!("trace: file `{target}` was not discovered in this project\n");
    };
    let Some(path) = trace_to_file(report, file_id) else {
        return format!("trace: `{target}` is not reachable from any entry root\n");
    };
    format_trace(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_dot_slash_and_backslashes() {
        assert_eq!(
            normalize_trace_path(".\\src\\pkg\\mod.py"),
            "src/pkg/mod.py"
        );
    }
}
