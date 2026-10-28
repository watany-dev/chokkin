//! Reachability trace formatting for `--trace`.

use std::fmt::Write;
use std::path::{Component, Path, PathBuf};

use crate::entry::{EntryOrigin, EntryPlan};
use crate::graph::{FileId, GraphEdge, ProjectGraph};
use crate::reachability::{ModuleIndex, ReachabilityReport, TracePath, TraceStep, trace_to_file};
use crate::reporters::escape_control;
use crate::sources::DiscoveredSources;

/// Normalize a user-supplied path for graph lookup.
pub(super) fn normalize_trace_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_owned()
}

/// Why a `--trace` target named no discovered file.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum TraceMiss {
    /// The path resolves to somewhere outside the project root.
    OutsideRoot,
    /// The path is under the root (or could not be placed) but no discovered
    /// file has it.
    NotDiscovered,
}

/// Find a file id for a trace target.
///
/// The root-relative spelling is tried first, as it is the documented form.
/// Otherwise an absolute path, or one relative to `cwd`, is made relative to
/// `root` and looked up again (#1157). An absolute path outside the root is
/// reported as such rather than as undiscovered.
pub(super) fn file_id_for_trace(
    graph: &ProjectGraph,
    root: &Path,
    cwd: Option<&Path>,
    target: &str,
) -> Result<FileId, TraceMiss> {
    let normalized = normalize_trace_path(target);
    if let Some(id) = graph.file_id(&normalized) {
        return Ok(id);
    }
    let candidate = Path::new(&normalized);
    let absolute = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else if let Some(cwd) = cwd {
        cwd.join(candidate)
    } else {
        return Err(TraceMiss::NotDiscovered);
    };
    let Some(relative) = root_relative(root, &absolute) else {
        // A relative target was first read as root-relative, so only an
        // absolute one is surely outside the root.
        return Err(if candidate.is_absolute() {
            TraceMiss::OutsideRoot
        } else {
            TraceMiss::NotDiscovered
        });
    };
    graph.file_id(&relative).ok_or(TraceMiss::NotDiscovered)
}

/// `path` relative to `root`, `/`-joined as graph paths are, or `None` when it
/// is not under `root`. Several spellings of each side are tried so that a
/// symlinked prefix (macOS `/tmp`, Windows `\\?\` prefixes) still matches:
/// the lexical one, the canonical one, and for `path` its canonical parent
/// joined with its file name, which covers a file that does not exist and a
/// discovered file that is itself a symlink out of the root.
fn root_relative(root: &Path, path: &Path) -> Option<String> {
    let roots = [
        Some(lexical_normalize(root)),
        std::fs::canonicalize(root).ok(),
    ];
    let lexical = lexical_normalize(path);
    let via_parent = lexical
        .parent()
        .zip(lexical.file_name())
        .and_then(|(parent, name)| Some(std::fs::canonicalize(parent).ok()?.join(name)));
    let paths = [
        Some(lexical.clone()),
        via_parent,
        std::fs::canonicalize(&lexical).ok(),
    ];
    paths.iter().flatten().find_map(|path| {
        roots
            .iter()
            .flatten()
            .find_map(|root| relative_parts(root, path))
    })
}

/// The `/`-joined components of `path` below `root`, if all are plain names.
fn relative_parts(root: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in rest.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_owned()),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

/// Drop `.` and resolve `..` against earlier components without touching the
/// filesystem.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {},
            Component::ParentDir => {
                if !out.pop() {
                    out.push(component);
                }
            },
            other => out.push(other),
        }
    }
    out
}

/// Format a reachability trace as a tree for CLI output.
#[must_use]
pub(super) fn format_trace(path: &TracePath) -> String {
    let mut out = String::from("Trace path:\n");
    for (index, step) in path.steps.iter().enumerate() {
        let prefix = if index + 1 == path.steps.len() {
            "└─ "
        } else {
            "├─ "
        };
        out.push_str(prefix);
        out.push_str(&escape_control(&format_step(step)));
        out.push('\n');
    }
    out
}

fn format_step(step: &TraceStep) -> String {
    match step {
        TraceStep::File { path, .. } => format!("file {path}"),
        TraceStep::Import { module, line } => format!("import {module} (line {line})"),
        TraceStep::PluginRef { module, label } => format!("plugin ref {module} ({label})"),
        TraceStep::DynamicImport { module, line } => {
            format!("dynamic import {module} (line {line})")
        },
    }
}

/// Build trace output for a target path, or an error message when unreachable.
///
/// `target` may be relative to the project root, relative to `cwd`, or
/// absolute; see `file_id_for_trace`.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "takes one output per earlier pipeline phase plus the caller's cwd; a bundle struct would only rename them"
)]
pub fn trace_output(
    report: &ReachabilityReport,
    graph: &ProjectGraph,
    entry: &EntryPlan,
    sources: &DiscoveredSources,
    target: &str,
    cwd: Option<&Path>,
) -> String {
    let root = &sources.root.path;
    let file_id = match file_id_for_trace(graph, root, cwd, target) {
        Ok(file_id) => file_id,
        Err(TraceMiss::OutsideRoot) => {
            return format!(
                "trace: `{}` is outside the project root `{}`; \
                 pass a path inside it, relative to the root\n",
                escape_control(target),
                escape_control(&root.display().to_string())
            );
        },
        Err(TraceMiss::NotDiscovered) => {
            return format!(
                "trace: file `{}` was not discovered in this project\n  \
                 (paths are relative to the project root `{}`, \
                 or absolute / relative to the current directory)\n",
                escape_control(target),
                escape_control(&root.display().to_string())
            );
        },
    };
    if let Some(path) = trace_to_file(report, file_id) {
        return format_trace(&path);
    }
    format_negative_trace(report, graph, entry, sources, target, file_id)
}

#[expect(
    clippy::too_many_arguments,
    reason = "takes one output per earlier pipeline phase; a bundle struct would only rename them"
)]
fn format_negative_trace(
    report: &ReachabilityReport,
    graph: &ProjectGraph,
    entry: &EntryPlan,
    sources: &DiscoveredSources,
    target: &str,
    file_id: FileId,
) -> String {
    let mut out = format!("Negative trace for {}:\n\n", escape_control(target));
    out.push_str("  reason: not reachable from any entry root\n");
    let confidence_reason = report
        .unreachable
        .iter()
        .chain(&report.unread)
        .find(|file| file.file == file_id)
        .and_then(|file| file.confidence_reason.as_deref());
    if let Some(confidence_reason) = confidence_reason {
        let _ = writeln!(out, "  {}", escape_control(confidence_reason));
    }
    out.push('\n');

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
            let line = format!("{} ({origin})", root.path);
            let _ = writeln!(out, "    {prefix}{}", escape_control(&line));
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
            let import = format!("{path}:{line} imports {module}");
            let _ = writeln!(out, "    {prefix}{}{suffix}", escape_control(&import));
        }
    }

    out.push('\n');
    out.push_str("  suggestion: add to [tool.chokkin].entry or verify import chain\n");
    out
}

fn format_entry_origin(origin: &EntryOrigin) -> String {
    match origin {
        EntryOrigin::Config => "config".to_owned(),
        EntryOrigin::Manifest { name, group } => format!("manifest: [{group}].{name}"),
        EntryOrigin::Plugin { plugin, label } => format!("plugin: {} ({label})", plugin.as_key()),
        EntryOrigin::Auto { rule } => format!("auto: {rule}"),
        EntryOrigin::SymbolRef { label, .. } => format!("symbol: {label}"),
        EntryOrigin::Script => "script: PEP 723".to_owned(),
        EntryOrigin::Command { label } => format!("command: {label}"),
        EntryOrigin::BuildBackend => "build backend: PEP 517".to_owned(),
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
        let Some(file_node) = graph.file(*file) else {
            continue;
        };
        if module_index.resolve_from(&file_node.path, &module_node.name) != Some(target) {
            continue;
        }
        importers.push((file_node.path.clone(), *line, module_node.name.clone()));
    }

    importers.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    importers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Confidence, ProjectMode};
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::entry::{EntryPlan, EntryRoot};
    use crate::graph::{FileNode, ModuleOrigin, add_parsed_imports};
    use crate::parser::ParsedModule;
    use crate::reachability::UnreachableFile;
    use crate::sources::{DiscoveredFile, DiscoveredSources, FileContext, FileKind, LayoutInfo};
    use std::collections::BTreeSet;

    fn src_layout() -> LayoutInfo {
        LayoutInfo::new("src", vec!["acme".to_owned()])
    }

    fn sources_with(paths: &[&str]) -> DiscoveredSources {
        let layout = src_layout();
        DiscoveredSources {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
            },
            layout,
            effective_globs: Vec::new(),
            files: paths
                .iter()
                .map(|path| DiscoveredFile {
                    path: (*path).to_owned(),
                    kind: FileKind::Python,
                    context: crate::sources::assign_file_context(path),
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
    fn root_relative_maps_paths_under_the_root_only() {
        let root = Path::new("/nonexistent-chokkin-root/proj");
        assert_eq!(
            root_relative(root, &root.join("src/acme/x.py")).as_deref(),
            Some("src/acme/x.py")
        );
        assert_eq!(
            root_relative(root, &root.join("src/../src/./acme/x.py")).as_deref(),
            Some("src/acme/x.py")
        );
        assert_eq!(root_relative(root, &root.join("../other/x.py")), None);
        assert_eq!(root_relative(root, Path::new("/elsewhere/x.py")), None);
    }

    /// A root reached through a symlinked directory still matches, for an
    /// existing file and for a missing one (macOS `/tmp` is such a link).
    #[cfg(unix)]
    #[test]
    fn root_relative_sees_through_a_symlinked_prefix() {
        let temp = tempfile::tempdir().expect("tempdir");
        let real = std::fs::canonicalize(temp.path())
            .expect("canonical")
            .join("real");
        std::fs::create_dir_all(real.join("src")).expect("mkdir");
        std::fs::write(real.join("src/a.py"), "").expect("write");
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        assert_eq!(
            root_relative(&real, &link.join("src/a.py")).as_deref(),
            Some("src/a.py")
        );
        assert_eq!(
            root_relative(&real, &link.join("src/missing.py")).as_deref(),
            Some("src/missing.py")
        );
    }

    #[test]
    fn file_id_for_trace_resolves_cwd_relative_and_absolute_targets() {
        let sources = sources_with(&["src/acme/legacy.py"]);
        let mut graph = ProjectGraph::new(sources.root);
        let id = graph
            .intern_file(FileNode {
                path: "src/acme/legacy.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("intern");
        let root = Path::new("/nonexistent-chokkin-root/proj");
        let acme = root.join("src/acme");
        let lookup =
            |cwd: Option<&Path>, target: &str| file_id_for_trace(&graph, root, cwd, target);
        assert_eq!(lookup(None, "src/acme/legacy.py"), Ok(id));
        assert_eq!(lookup(Some(&acme), "legacy.py"), Ok(id));
        assert_eq!(
            lookup(
                Some(root.parent().expect("parent")),
                "proj/src/acme/legacy.py"
            ),
            Ok(id)
        );
        assert_eq!(
            lookup(Some(&acme), "missing.py"),
            Err(TraceMiss::NotDiscovered)
        );
        assert_eq!(lookup(None, "legacy.py"), Err(TraceMiss::NotDiscovered));
        assert_eq!(
            lookup(Some(&acme), "../../../x.py"),
            Err(TraceMiss::NotDiscovered)
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one scenario end to end; splitting it would scatter its fixture and assertions"
    )]
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
                    deferred: false,
                    relative_level: 0,
                    fallback_for: Vec::new(),
                    version_minors: None,
                    called_before: Vec::new(),
                    tested_before: Vec::new(),
                }],
                ..ParsedModule::default()
            },
        )
        .expect("imports");

        let mut report = ReachabilityReport::default();
        report.reachable.insert(cli_id);
        report.unreachable = vec![
            UnreachableFile {
                file: legacy_id,
                path: "src/acme/legacy.py".to_owned(),
                max_confidence: Confidence::Certain,
                mode: ProjectMode::App,
                confidence_reason: None,
                test_fixture: false,
                selected_by: None,
            },
            UnreachableFile {
                file: old_api_id,
                path: "src/acme/old_api.py".to_owned(),
                max_confidence: Confidence::Certain,
                mode: ProjectMode::App,
                confidence_reason: None,
                test_fixture: false,
                selected_by: None,
            },
        ];

        let entry = EntryPlan {
            mode: ProjectMode::App,
            library_members: Vec::new(),
            distributed_members: Vec::new(),
            roots: vec![EntryRoot {
                path: "src/acme/cli.py".to_owned(),
                symbols: BTreeSet::new(),
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
