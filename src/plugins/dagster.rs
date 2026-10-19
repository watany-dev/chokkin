//! Dagster plugin: the code locations `[tool.dagster]`, a `dg` project and
//! `workspace.yaml` name, and the modules that build `Definitions` (#401).

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::Path;

use crate::config::{EntrySpec, PluginId};
use crate::parser::ParsedModule;
use crate::path_util::rel_to_root;
use crate::sources::FileContext;

use super::commands::is_dotted_identifier;
use super::config_text::{leading_spaces, origin_at, toml_key_line, yaml_scalar};
use super::context::PluginContext;
use super::types::{ModuleReference, PluginContribution, PluginEntry, ReferenceOrigin};
use super::util::{file_context, is_py_path, join_lexical, module_paths, project_dirs};

/// What dagster builds a code location from.
const DEFINITION_NAMES: [&str; 3] = ["Definitions", "repository", "definitions"];

/// `load_from` kinds and the key that names their target when nested.
const LOAD_KINDS: [(&str, &str); 3] = [
    ("python_file", "relative_path"),
    ("python_module", "module_name"),
    ("python_package", "package_name"),
];

#[must_use]
pub(super) fn extract(ctx: &PluginContext<'_>) -> PluginContribution {
    let root = ctx.root.path.as_path();
    let mut locations = Locations {
        ctx,
        contrib: PluginContribution::empty(PluginId::Dagster),
        seen: BTreeSet::new(),
    };
    let dirs = project_dirs(ctx);
    for dir in dirs {
        locations.pyproject(dir);
        let path = root.join(dir).join("workspace.yaml");
        if let Ok(contents) = std::fs::read_to_string(&path) {
            let file = rel_to_root(root, &path);
            locations.load_from(&Workspace {
                dir,
                file: &file,
                lines: contents.lines().collect(),
            });
        }
    }
    for module in &ctx.parse.modules {
        if let Some(line) = definitions_line(module)
            && let Some(file) = ctx
                .sources
                .python_files()
                .find(|file| file.path == module.path)
        {
            let origin = ReferenceOrigin {
                file: module.path.clone(),
                line: Some(line),
                label: "dagster definitions".to_owned(),
            };
            locations.push_entry(&module.path, file.context, origin);
        }
    }
    // Dagster collects the definitions each module binds.
    let mut contrib = locations.contrib;
    contrib.scanned_files = contrib
        .entries
        .iter()
        .map(|entry| entry.spec.path.clone())
        .collect();
    contrib
}

/// The code locations found so far, each file once.
struct Locations<'a, 'c> {
    ctx: &'a PluginContext<'c>,
    contrib: PluginContribution,
    seen: BTreeSet<String>,
}

/// A `workspace.yaml` and the directory its paths are read from.
struct Workspace<'a> {
    dir: &'a str,
    file: &'a str,
    lines: Vec<&'a str>,
}

impl Locations<'_, '_> {
    /// `[tool.dagster] module_name` and a `dg` project's modules, from
    /// `pyproject.toml` (`[tool.dg.project]`) or `dg.toml` (`[project]`). A
    /// `dg` project loads every module under `defs_module` and builds its
    /// location from `code_location_target_module`. A value that is not a
    /// dotted name is a gap.
    fn pyproject(&mut self, dir: &str) {
        let root = self.ctx.root.path.as_path();
        for (name, table_path) in [
            ("pyproject.toml", "tool.dagster"),
            ("pyproject.toml", "tool.dg.project"),
            ("dg.toml", "project"),
        ] {
            let path = root.join(dir).join(name);
            let Some((text, table)) = read_table(&path, table_path) else {
                continue;
            };
            let file = rel_to_root(root, &path);
            let origin = |key: &str| ReferenceOrigin {
                file: file.clone(),
                line: toml_key_line(&text, table_path, key),
                label: format!("{table_path}.{key}"),
            };
            if table_path == "tool.dagster" {
                let modules = toml_strings(table.get("module_name"));
                let Some(modules) = modules else {
                    self.contrib
                        .push_gap(&table_text(&table, "module_name"), origin("module_name"));
                    continue;
                };
                for module in modules {
                    if is_dotted_identifier(&module) {
                        self.push_module(dir, &module, origin("module_name"));
                    } else {
                        self.contrib.push_gap(&module, origin("module_name"));
                    }
                }
                continue;
            }
            let root_module = table.get("root_module").and_then(toml::Value::as_str);
            let setting = |key: &str, suffix: &str| {
                table
                    .get(key)
                    .map(|value| (value.as_str().map(str::to_owned), true))
                    .or_else(|| {
                        root_module.map(|module| (Some(format!("{module}.{suffix}")), false))
                    })
            };
            if let Some((defs, explicit)) = setting("defs_module", "defs") {
                match defs {
                    Some(defs) if is_dotted_identifier(&defs) => {
                        for (path, context) in module_paths(self.ctx, dir, &defs, true) {
                            self.push_entry(&path, context, origin("defs_module"));
                        }
                    },
                    _ if explicit => {
                        self.contrib
                            .push_gap(&table_text(&table, "defs_module"), origin("defs_module"));
                    },
                    _ => {},
                }
            }
            let key = "code_location_target_module";
            if let Some((target, explicit)) = setting(key, "definitions") {
                match target {
                    Some(target)
                        if is_dotted_identifier(&target)
                            && (explicit
                                || !module_paths(self.ctx, dir, &target, false).is_empty()) =>
                    {
                        self.push_module(dir, &target, origin(key));
                    },
                    _ if explicit => self.contrib.push_gap(&table_text(&table, key), origin(key)),
                    _ => {},
                }
            }
        }
    }

    /// The items of `workspace.yaml`'s `load_from` list. `python_file` is a
    /// path from the file's directory, `python_module` and `python_package`
    /// a module, each written inline or under its nested key. `grpc_server`
    /// runs code elsewhere; any other kind, a flow sequence or mapping, a
    /// line that is no list item, an alias or a template is not read, so it
    /// is a gap.
    fn load_from(&mut self, workspace: &Workspace<'_>) {
        let lines = &workspace.lines;
        let Some(start) = lines
            .iter()
            .position(|line| line.trim_end() == "load_from:" || line.starts_with("load_from: "))
        else {
            return;
        };
        // A flow sequence or an alias on the key's own line is not read.
        let inline = yaml_scalar(&lines[start]["load_from:".len()..]);
        if !inline.is_empty() {
            let origin = origin_at(workspace.file, start, "load_from");
            return self.contrib.push_gap(inline, origin);
        }
        let base_indent = leading_spaces(lines[start]);
        let mut index = start + 1;
        while let Some(line) = lines.get(index) {
            let text = line.trim();
            if text.is_empty() || text.starts_with('#') {
                index += 1;
                continue;
            }
            let indent = leading_spaces(line);
            if indent < base_indent || (indent == base_indent && !text.starts_with('-')) {
                break;
            }
            // A line that is no list item is a mapping dagster rejects.
            let Some(item) = text.strip_prefix('-').map(str::trim_start) else {
                let origin = origin_at(workspace.file, index, "load_from");
                self.contrib.push_gap(text, origin);
                index += 1;
                continue;
            };
            let mut end = index + 1;
            while lines.get(end).is_some_and(|next| {
                let next_text = next.trim();
                next_text.is_empty() || next_text.starts_with('#') || leading_spaces(next) > indent
            }) {
                end += 1;
            }
            self.load_item(workspace, index..end, item);
            index = end;
        }
    }

    fn load_item(&mut self, workspace: &Workspace<'_>, lines: Range<usize>, item: &str) {
        let origin = origin_at(workspace.file, lines.start, "load_from");
        let Some((kind, inline)) = item.split_once(':') else {
            return self.contrib.push_gap(item, origin);
        };
        let kind = kind.trim();
        if kind == "grpc_server" {
            return;
        }
        let Some((_, nested_key)) = LOAD_KINDS.iter().find(|(name, _)| *name == kind) else {
            return self.contrib.push_gap(item, origin);
        };
        let inline = yaml_scalar(inline);
        let (value, origin) = if inline.is_empty() {
            let nested = (lines.start + 1..lines.end).find_map(|line| {
                let text = workspace.lines[line].trim_start();
                let value = text.strip_prefix(nested_key)?.strip_prefix(':')?;
                Some((
                    yaml_scalar(value),
                    origin_at(workspace.file, line, nested_key),
                ))
            });
            let Some(nested) = nested else {
                return self.contrib.push_gap(item, origin);
            };
            nested
        } else {
            (inline, origin)
        };
        if kind == "python_file" {
            return self.push_file(workspace.dir, value, origin);
        }
        if is_dotted_identifier(value) {
            self.push_module(workspace.dir, value, origin);
        } else {
            self.contrib.push_gap(value, origin);
        }
    }

    /// A `python_file` from the workspace's directory. A file that does not
    /// exist hides no project file, so it is neither an entry nor a gap.
    fn push_file(&mut self, dir: &str, value: &str, origin: ReferenceOrigin) {
        let path = (is_py_path(value) && !value.contains(['{', '$', '*']))
            .then(|| join_lexical(dir, value))
            .flatten();
        let Some(path) = path else {
            return self.contrib.push_gap(value, origin);
        };
        if !self.ctx.root.path.join(&path).is_file() {
            return;
        }
        let context = file_context(self.ctx, &path);
        self.push_entry(&path, context, origin);
    }

    /// The file dagster imports for `module`, found from `dir` as a code
    /// location's working directory; a module with no project file is left
    /// to the import resolver as a module reference.
    fn push_module(&mut self, dir: &str, module: &str, origin: ReferenceOrigin) {
        let paths = module_paths(self.ctx, dir, module, false);
        if paths.is_empty() {
            self.contrib.module_refs.push(ModuleReference {
                module: module.to_owned(),
                origin,
            });
            return;
        }
        for (path, context) in paths {
            self.push_entry(&path, context, origin.clone());
        }
    }

    fn push_entry(&mut self, path: &str, context: FileContext, origin: ReferenceOrigin) {
        if self.seen.insert(path.to_owned()) {
            self.contrib.entries.push(PluginEntry {
                spec: EntrySpec {
                    path: path.to_owned(),
                    symbol: None,
                },
                context,
                origin,
            });
        }
    }
}

fn read_table(path: &Path, table_path: &str) -> Option<(String, toml::Table)> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc = toml::from_str::<toml::Table>(&text).ok()?;
    let table = table_path
        .split('.')
        .try_fold(&doc, |table, key| table.get(key)?.as_table())?
        .clone();
    Some((text, table))
}

/// A string or an array of strings; `None` for any other value.
fn toml_strings(value: Option<&toml::Value>) -> Option<Vec<String>> {
    let Some(value) = value else {
        return Some(Vec::new());
    };
    if let Some(text) = value.as_str() {
        return Some(vec![text.to_owned()]);
    }
    value
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_owned))
        .collect()
}

fn table_text(table: &toml::Table, key: &str) -> String {
    table
        .get(key)
        .map_or_else(String::new, toml::Value::to_string)
}

/// The first line binding a dagster definitions name: imported from
/// `dagster` (a decorator too), or read off an imported `dagster`.
fn definitions_line(module: &ParsedModule) -> Option<u32> {
    let mut aliases = BTreeSet::new();
    let mut lines = Vec::new();
    for import in &module.imports {
        match import.import_target() {
            Some(("dagster", name)) if DEFINITION_NAMES.contains(&name) => lines.push(import.line),
            None if import.module == "dagster" => aliases.extend(import.bound_name()),
            _ => {},
        }
    }
    lines.extend(
        module
            .attribute_accesses
            .iter()
            .filter(|access| {
                aliases.contains(access.receiver.as_str())
                    && DEFINITION_NAMES.contains(&access.name.as_str())
            })
            .map(|access| access.line),
    );
    lines.into_iter().min()
}
