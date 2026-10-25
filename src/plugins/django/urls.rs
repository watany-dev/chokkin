//! The URL configurations a `ROOT_URLCONF` loads through `include()` (#1094).

use std::collections::{BTreeSet, VecDeque};

use ruff_python_ast::visitor::{Visitor, walk_expr};
use ruff_python_ast::{Expr, ExprCall};
use ruff_text_size::{Ranged, TextSize};

use crate::manifest::literals::{parse_module, string_value};
use crate::manifest::util::read_to_string;
use crate::parser::LineIndex;
use crate::rules::import_site;

use super::super::context::PluginContext;
use super::super::types::{ModuleReference, PluginContribution, ReferenceOrigin};
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
    /// A string computed at runtime (an f-string, a concatenation, a call),
    /// which may name any module.
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
            for name in URLCONF_NAMES {
                push_used_symbol(contrib, &module, name, &origin);
            }
            contrib.module_refs.push(ModuleReference { module, origin });
            if visited.insert(path.clone()) {
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

/// The `include()` calls of a module, in source order: by name or as the
/// last attribute (`django.urls.include`, `django.conf.urls.include`).
#[derive(Default)]
struct Includes(Vec<Included>);

impl Visitor<'_> for Includes {
    fn visit_expr(&mut self, expr: &Expr) {
        if let Expr::Call(call) = expr
            && is_include(call)
            && let Some(first) = call.arguments.args.first()
        {
            // `include(("pkg.urls", "app_name"))` names the module first.
            let target = match first {
                Expr::Tuple(tuple) => tuple.elts.first(),
                other => Some(other),
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
        Expr::Attribute(attribute) => attribute.attr.as_str() == "include",
        _ => false,
    }
}

/// A string built at runtime: an f-string, `+` or `%` on operands, or a call
/// such as `"{}.urls".format(app)`. Names, attributes, subscripts and list
/// literals are module objects or patterns the import graph follows.
fn is_computed_string(expr: &Expr) -> bool {
    matches!(expr, Expr::FString(_) | Expr::BinOp(_) | Expr::Call(_))
}
