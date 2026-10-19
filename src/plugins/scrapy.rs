//! Scrapy plugin: the settings module `scrapy.cfg` names, the spiders under
//! its `SPIDER_MODULES`, and the components its settings name by path
//! (#401).

use std::collections::BTreeSet;

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::{Ranged, TextSize};

use crate::config::{EntrySpec, PluginId};
use crate::manifest::literals::{assigned_value, parse_module, string_value};
use crate::parser::LineIndex;
use crate::path_util::rel_to_root;
use crate::sources::FileContext;

use super::commands::is_dotted_identifier;
use super::config_text::{ini_value, origin_at};
use super::context::PluginContext;
use super::types::{
    ModuleReference, PluginContribution, PluginEntry, ReferenceOrigin, SymbolReference,
};
use super::util::{module_paths, parse_ini_section, project_dirs};

const SPIDER_MODULES: &str = "SPIDER_MODULES";

/// Where `scrapy genspider` writes a new spider; scrapy never loads it.
const NEWSPIDER_MODULE: &str = "NEWSPIDER_MODULE";

#[must_use]
pub(super) fn extract(ctx: &PluginContext<'_>) -> PluginContribution {
    let root = ctx.root.path.as_path();
    let mut contrib = PluginContribution::empty(PluginId::Scrapy);
    let dirs = project_dirs(ctx);
    for dir in dirs {
        let path = root.join(dir).join("scrapy.cfg");
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let file = rel_to_root(root, &path);
        for key in parse_ini_section(&contents, "settings").keys() {
            let Some((index, value)) = ini_value(&contents, "settings", key) else {
                continue;
            };
            let origin = origin_at(&file, index, "settings");
            settings_module(ctx, &mut contrib, dir, value.trim(), origin);
        }
    }
    // A module several settings name keeps its first origin.
    let mut paths = BTreeSet::new();
    contrib
        .entries
        .retain(|entry| paths.insert(entry.spec.path.clone()));
    contrib
}

/// A `[settings]` value is the settings module scrapy imports, with the
/// config's directory on `sys.path`. One that is not a dotted name is a
/// gap; one that names no project file is left to the import resolver as a
/// module reference.
fn settings_module(
    ctx: &PluginContext<'_>,
    contrib: &mut PluginContribution,
    dir: &str,
    module: &str,
    origin: ReferenceOrigin,
) {
    if !is_dotted_identifier(module) {
        return contrib.push_gap(module, origin);
    }
    let Some((path, context)) = module_paths(ctx, dir, module, false).into_iter().next() else {
        contrib.module_refs.push(ModuleReference {
            module: module.to_owned(),
            origin,
        });
        return;
    };
    if contrib.entries.iter().any(|entry| entry.spec.path == path) {
        return;
    }
    push_entry(contrib, &path, context, origin);
    // Scrapy reads the settings by name.
    contrib.scanned_files.push(path.clone());
    let Ok(contents) = std::fs::read_to_string(ctx.root.path.join(&path)) else {
        return;
    };
    let Some(stmts) = parse_module(&contents) else {
        return;
    };
    let settings = Settings {
        dir,
        file: &path,
        contents: &contents,
        lines: LineIndex::new(&contents),
    };
    spider_modules(ctx, contrib, &stmts, &settings);
    components(ctx, contrib, &stmts, &settings);
}

/// A settings module read for the names it gives scrapy.
struct Settings<'a> {
    /// Directory of `scrapy.cfg`, which scrapy puts on `sys.path`.
    dir: &'a str,
    file: &'a str,
    contents: &'a str,
    lines: LineIndex,
}

impl Settings<'_> {
    fn origin(&self, start: TextSize, label: &str) -> ReferenceOrigin {
        ReferenceOrigin {
            file: self.file.to_owned(),
            line: Some(self.lines.line(start)),
            label: label.to_owned(),
        }
    }
}

fn push_entry(
    contrib: &mut PluginContribution,
    path: &str,
    context: FileContext,
    origin: ReferenceOrigin,
) {
    contrib.entries.push(PluginEntry {
        spec: EntrySpec {
            path: path.to_owned(),
            symbol: None,
        },
        context,
        origin,
    });
}

/// Scrapy walks every module under each `SPIDER_MODULES` package for
/// spider classes. A list or tuple of strings, or one comma-separated
/// string, is read; any other value is a gap.
fn spider_modules(
    ctx: &PluginContext<'_>,
    contrib: &mut PluginContribution,
    stmts: &[Stmt],
    settings: &Settings<'_>,
) {
    let Some(value) = assigned_value(stmts, SPIDER_MODULES) else {
        return;
    };
    let origin = settings.origin(value.start(), SPIDER_MODULES);
    let elts = match value {
        Expr::List(list) => list.elts.as_slice(),
        Expr::Tuple(tuple) => tuple.elts.as_slice(),
        _ => std::slice::from_ref(value),
    };
    let mut modules = Vec::new();
    for elt in elts {
        let Some(text) = string_value(elt) else {
            return contrib.push_gap(&settings.contents[value.range()], origin);
        };
        modules.extend(
            text.split(',')
                .map(str::trim)
                .filter(|module| !module.is_empty())
                .map(str::to_owned),
        );
    }
    let mut seen = BTreeSet::new();
    for module in modules {
        if !is_dotted_identifier(&module) {
            contrib.push_gap(&module, origin.clone());
            continue;
        }
        for (path, context) in module_paths(ctx, settings.dir, &module, true) {
            if seen.insert(path.clone()) {
                push_entry(contrib, &path, context, origin.clone());
                // Scrapy collects the spider classes each module binds.
                contrib.scanned_files.push(path);
            }
        }
    }
}

/// Components a setting names by dotted path (`ITEM_PIPELINES`, the
/// middlewares, `EXTENSIONS`, a `*_CLASS`, ...): every string in a
/// top-level assignment that names a project module or a name in one.
/// Other strings name no project code, so skipping them hides nothing; a
/// component written as a non-literal is not read and stays unreached.
fn components(
    ctx: &PluginContext<'_>,
    contrib: &mut PluginContribution,
    stmts: &[Stmt],
    settings: &Settings<'_>,
) {
    for stmt in stmts {
        let Stmt::Assign(assign) = stmt else {
            continue;
        };
        let Some(Expr::Name(target)) = assign.targets.first() else {
            continue;
        };
        let field = target.id.as_str();
        if [SPIDER_MODULES, NEWSPIDER_MODULE].contains(&field)
            || !field
                .chars()
                .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
        {
            continue;
        }
        let mut strings = Vec::new();
        collect_strings(&assign.value, &mut strings);
        for (text, start) in strings {
            push_component(ctx, contrib, settings, &text, settings.origin(start, field));
        }
    }
}

fn collect_strings(expr: &Expr, out: &mut Vec<(String, TextSize)>) {
    match expr {
        Expr::StringLiteral(_) => out.extend(string_value(expr).map(|text| (text, expr.start()))),
        Expr::Dict(dict) => {
            for item in &dict.items {
                if let Some(key) = &item.key {
                    collect_strings(key, out);
                }
                collect_strings(&item.value, out);
            }
        },
        Expr::List(list) => list.elts.iter().for_each(|elt| collect_strings(elt, out)),
        Expr::Tuple(tuple) => tuple.elts.iter().for_each(|elt| collect_strings(elt, out)),
        _ => {},
    }
}

/// `pkg.mod` or `pkg.mod.Name` for a project module: the module's file is
/// an entry, so a module the layout left out joins the inventory, and
/// `Name` is a symbol scrapy loads.
fn push_component(
    ctx: &PluginContext<'_>,
    contrib: &mut PluginContribution,
    settings: &Settings<'_>,
    text: &str,
    origin: ReferenceOrigin,
) {
    if !is_dotted_identifier(text) || !text.contains('.') {
        return;
    }
    if let Some((path, context)) = module_paths(ctx, settings.dir, text, false)
        .into_iter()
        .next()
    {
        return push_entry(contrib, &path, context, origin);
    }
    if let Some((module, symbol)) = text.rsplit_once('.')
        && let Some((path, context)) = module_paths(ctx, settings.dir, module, false)
            .into_iter()
            .next()
    {
        push_entry(contrib, &path, context, origin.clone());
        contrib.symbol_refs.push(SymbolReference {
            module: module.to_owned(),
            symbol: symbol.to_owned(),
            origin,
        });
    }
}
