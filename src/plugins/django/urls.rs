//! The URL configurations a `ROOT_URLCONF` loads through `include()` (#1094).

use std::collections::{BTreeSet, VecDeque};

use ruff_python_ast::visitor::{Visitor, walk_expr};
use ruff_python_ast::{Expr, ExprCall};
use ruff_text_size::{Ranged, TextSize};

use crate::entry::resolve_module_to_path;
use crate::manifest::literals::{parse_module, string_value};
use crate::manifest::util::read_to_string;
use crate::parser::LineIndex;
use crate::rules::import_site;
use crate::sources::{FileContext, path_to_module};

use super::super::context::PluginContext;
use super::super::types::{ModuleReference, PluginContribution, PluginEntry, ReferenceOrigin};
use super::super::util::push_used_symbol;
use super::super::warnings::{PluginsWarning, SettingsUnread};
use super::{Lookup, abs_path, module_path, push_unread};

/// Origin label of a URL configuration another one includes.
const INCLUDE_LABEL: &str = "URLconf include()";

/// Names Django reads from every URL configuration it loads.
const URLCONF_NAMES: &[&str] = &["urlpatterns", "app_name"];

/// Names Django reads from the root URL configuration only.
const ROOT_URLCONF_NAMES: &[&str] = &["handler400", "handler403", "handler404", "handler500"];

/// What the first argument of an `include()` names.
enum Included {
    /// A module by its dotted name, and where the literal starts.
    Module(String, TextSize),
    /// A string computed at runtime ([`is_computed_string`]), which may name
    /// any module.
    Unread(TextSize),
}

/// Follow `include("pkg.urls")` from the root URL configuration `root_urls`: each
/// literal that names a project file is a module reference, and its own
/// includes are followed in turn. A non-string argument
/// (`include(router.urls)`, `include(module)`, `include([...])`) is a value
/// the import graph already follows, and a name that is no project file (a
/// third-party app's `"allauth.urls"`) adds nothing. A string computed at
/// runtime is a gap: the configuration it names, and what that imports, are
/// unknown (ADR 0005). Calls are found anywhere in the module, function
/// bodies included, since following one too many only hides a finding.
/// Django reads [`URLCONF_NAMES`] from each of them by name, so those are
/// used.
pub(super) fn follow(
    ctx: &PluginContext<'_>,
    lookup: &Lookup,
    root_urls: &str,
    contrib: &mut PluginContribution,
    warnings: &mut Vec<PluginsWarning>,
) {
    let root = ctx.root.path.as_path();
    let mut queue = VecDeque::from([root_urls.to_owned()]);
    let mut visited = BTreeSet::from([root_urls.to_owned()]);
    let mut unread = false;
    while let Some(rel) = queue.pop_front() {
        // The parse stage reports a discovered file it cannot read or parse.
        let Some(contents) = read_to_string(&abs_path(root, &rel)).ok() else {
            continue;
        };
        let Some(stmts) = parse_module(&contents) else {
            continue;
        };
        let lines = LineIndex::new(&contents);
        let mut calls = Includes::default();
        calls.visit_body(&stmts);
        for included in calls.0 {
            let (module, start) = match included {
                Included::Module(module, start) => (module, start),
                Included::Unread(start) => {
                    // One gap explains them all; more would only repeat it.
                    if !unread {
                        unread = true;
                        let line = Some(lines.line(start));
                        let reason = SettingsUnread::UnreadInclude {
                            site: import_site(&rel, line),
                        };
                        push_unread(contrib, warnings, rel.clone(), line, reason);
                    }
                    continue;
                },
            };
            let Some(path) =
                module_path(root, &module, ctx, lookup).filter(|path| lookup.known.contains(path))
            else {
                continue;
            };
            let origin = ReferenceOrigin {
                file: rel.clone(),
                line: Some(lines.line(start)),
                label: INCLUDE_LABEL.to_owned(),
            };
            let first = visited.insert(path.clone());
            // A file found only under the setter's import directory is not
            // what the name resolves to in the project, so it is an entry.
            if first
                && resolve_module_to_path(&module, &ctx.sources.layout, &lookup.known).as_ref()
                    != Some(&path)
            {
                contrib.entries.push(PluginEntry {
                    spec: crate::config::EntrySpec {
                        path: path.clone(),
                        symbol: None,
                    },
                    context: FileContext::Runtime,
                    origin: origin.clone(),
                });
            }
            let named =
                path_to_module(&path, &ctx.sources.layout).unwrap_or_else(|| module.clone());
            for name in URLCONF_NAMES {
                push_used_symbol(contrib, &named, name, &origin);
            }
            contrib.module_refs.push(ModuleReference { module, origin });
            if first {
                queue.push_back(path);
            }
        }
    }
}

/// Django reads [`URLCONF_NAMES`] and [`ROOT_URLCONF_NAMES`] from the root
/// URL configuration `module` by name.
pub(super) fn root_names_used(
    contrib: &mut PluginContribution,
    module: &str,
    origin: &ReferenceOrigin,
) {
    for name in URLCONF_NAMES.iter().chain(ROOT_URLCONF_NAMES) {
        push_used_symbol(contrib, module, name, origin);
    }
}

/// The `include()` calls of a module, in source order: by name, or as an
/// attribute of a `urls` module (`django.urls.include`, `urls.include`,
/// `django.conf.urls.include`), so a template engine's `.include()` is not
/// one.
#[derive(Default)]
struct Includes(Vec<Included>);

impl Visitor<'_> for Includes {
    fn visit_expr(&mut self, expr: &Expr) {
        if let Expr::Call(call) = expr
            && is_include(call)
        {
            // Django's parameter is `arg`; a star argument may be anything.
            let first = call.arguments.args.first().or_else(|| {
                call.arguments
                    .keywords
                    .iter()
                    .find(|keyword| {
                        keyword
                            .arg
                            .as_ref()
                            .is_some_and(|arg| arg.as_str() == "arg")
                    })
                    .map(|keyword| &keyword.value)
            });
            // `include(("pkg.urls", "app_name"))` names the module first.
            let target = match first {
                Some(Expr::Tuple(tuple)) => tuple.elts.first(),
                other => other,
            };
            if let Some(target) = target {
                if let Some(module) = string_value(target) {
                    self.0.push(Included::Module(module, target.start()));
                } else if is_computed_string(target) {
                    self.0.push(Included::Unread(target.start()));
                }
            }
        }
        walk_expr(self, expr);
    }
}

fn is_include(call: &ExprCall) -> bool {
    match &*call.func {
        Expr::Name(name) => name.id.as_str() == "include",
        Expr::Attribute(attribute) => {
            let receiver = match &*attribute.value {
                Expr::Name(name) => name.id.as_str(),
                Expr::Attribute(parent) => parent.attr.as_str(),
                _ => "",
            };
            attribute.attr.as_str() == "include" && receiver == "urls"
        },
        _ => false,
    }
}

/// A string built at runtime: an f-string, `+` or `%` with a string operand,
/// a `str.format()` call on a literal, or a star argument. Names,
/// attributes, subscripts, list literals, other calls (`router.get_urls()`,
/// `i18n_patterns(...)`) and `+` of pattern lists are module objects or
/// patterns the import graph follows.
fn is_computed_string(expr: &Expr) -> bool {
    let is_string = |expr: &Expr| matches!(expr, Expr::StringLiteral(_) | Expr::FString(_));
    match expr {
        Expr::FString(_) | Expr::Starred(_) => true,
        Expr::BinOp(op) => is_string(&op.left) || is_string(&op.right),
        Expr::Call(call) => matches!(
            &*call.func,
            Expr::Attribute(attribute) if attribute.attr.as_str() == "format" && is_string(&attribute.value)
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The modules `source` includes, and `None` for each computed name.
    fn included(source: &str) -> Vec<Option<String>> {
        let stmts = parse_module(source).expect("parse");
        let mut calls = Includes::default();
        calls.visit_body(&stmts);
        calls
            .0
            .into_iter()
            .map(|included| match included {
                Included::Module(module, _) => Some(module),
                Included::Unread(_) => None,
            })
            .collect()
    }

    #[test]
    fn include_is_read_by_name_and_on_a_urls_module() {
        assert_eq!(
            included(
                "include('a.urls')\nurls.include(arg=('b.urls', 'b'))\ndjango.urls.include('c.urls')\njinja.include('d.html')\n"
            ),
            [
                Some("a.urls".to_owned()),
                Some("b.urls".to_owned()),
                Some("c.urls".to_owned())
            ]
        );
    }

    #[test]
    fn only_a_computed_string_is_unread() {
        assert_eq!(
            included(
                "include(f'{a}.urls')\ninclude('a.' + b)\ninclude('{}.urls'.format(a))\ninclude(*args)\ninclude(router.urls)\ninclude(get_urls())\ninclude(a + b)\ninclude([])\n"
            ),
            [None, None, None, None]
        );
    }
}
