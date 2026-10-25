use std::collections::BTreeSet;

use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use crate::manifest::literals::string_value;
use crate::parser::LineIndex;
use crate::plugins::commands::is_dotted_identifier;
use crate::plugins::types::{ModuleReference, ReferenceOrigin};

use super::{LIST_FIELDS, Lookup};

// `*` selects container values, never dict keys. Non-import options such as
// storage bucket names must not become third-party import evidence (#1003).
const IMPORT_PATHS: &[(&str, &[&str])] = &[
    ("AUTHENTICATION_BACKENDS", &["*"]),
    ("PASSWORD_HASHERS", &["*"]),
    ("DATABASE_ROUTERS", &["*"]),
    ("DEFAULT_FILE_STORAGE", &[]),
    ("STATICFILES_STORAGE", &[]),
    ("EMAIL_BACKEND", &[]),
    ("SESSION_ENGINE", &[]),
    ("TEST_RUNNER", &[]),
    ("FORM_RENDERER", &[]),
    ("CSRF_FAILURE_VIEW", &[]),
    ("AUTH_PASSWORD_VALIDATORS", &["*", "NAME"]),
    ("STORAGES", &["*", "BACKEND"]),
    ("TEMPLATES", &["*", "BACKEND"]),
    ("TEMPLATES", &["*", "OPTIONS", "context_processors", "*"]),
    ("TEMPLATES", &["*", "OPTIONS", "libraries", "*"]),
    ("TEMPLATES", &["*", "OPTIONS", "builtins", "*"]),
    ("LOGGING", &["handlers", "*", "class"]),
    ("LOGGING", &["handlers", "*", "()"]),
    ("LOGGING", &["filters", "*", "()"]),
    ("LOGGING", &["formatters", "*", "()"]),
];

pub(super) fn extract(
    lookup: &Lookup,
    stmts: &[Stmt],
    file: &str,
    lines: &LineIndex,
) -> (Vec<ModuleReference>, Vec<ReferenceOrigin>) {
    let mut references = Vec::new();
    let mut unread = Vec::new();
    let mut assignments = Assignments(|target: &Expr, value: &Expr, direct: bool| {
        let Some((field, prefix)) = setting_target(target) else {
            return;
        };
        // These fields already have their own extraction and warning policy.
        if LIST_FIELDS.contains(&field)
            || matches!(
                field,
                "INSTALLED_APPS" | "ROOT_URLCONF" | "WSGI_APPLICATION" | "ASGI_APPLICATION"
            )
        {
            return;
        }
        let mut seen = BTreeSet::new();
        let mut add = |expr: &Expr| {
            if let Some(module) = string_value(expr)
                && seen.insert(expr.start())
            {
                references.push(ModuleReference {
                    module,
                    origin: ReferenceOrigin {
                        file: file.to_owned(),
                        line: Some(lines.line(expr.start())),
                        label: field.to_owned(),
                    },
                });
            }
        };
        let mut complete = true;
        for (_, path) in IMPORT_PATHS.iter().filter(|(name, _)| *name == field) {
            if !direct {
                complete = false;
                continue;
            }
            let Some(prefix) = &prefix else {
                complete = false;
                continue;
            };
            if prefix.len() <= path.len()
                && prefix
                    .iter()
                    .zip(*path)
                    .all(|(key, part)| *part == "*" || key == part)
            {
                complete &= read_path(value, &path[prefix.len()..], &mut add);
            }
        }
        FirstPartyValues {
            modules: &lookup.modules,
            add: &mut add,
        }
        .visit_expr(value);
        if !complete {
            unread.push(ReferenceOrigin {
                file: file.to_owned(),
                line: Some(lines.line(value.start())),
                label: field.to_owned(),
            });
        }
    });
    assignments.visit_body(stmts);
    (references, unread)
}

// Assignments in module-level branches can configure Django too; function,
// class and lambda bodies do not populate the module's settings namespace.
struct Assignments<F>(F);

impl<'a, F: FnMut(&'a Expr, &'a Expr, bool)> Assignments<F> {
    fn assign(&mut self, target: &'a Expr, value: &'a Expr, direct: bool) {
        let items = match target {
            Expr::Tuple(tuple) => &tuple.elts,
            Expr::List(list) => &list.elts,
            _ => {
                (self.0)(target, value, direct);
                return;
            },
        };
        // Unpacking can set import fields, but the RHS is not their value.
        // Keep first-party evidence and mark known fields unread.
        for item in items {
            self.assign(item, value, false);
        }
    }
}

impl<'a, F: FnMut(&'a Expr, &'a Expr, bool)> Visitor<'a> for Assignments<F> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::Assign(assign) => {
                for target in &assign.targets {
                    self.assign(target, &assign.value, true);
                }
            },
            Stmt::AnnAssign(assign) => {
                if let Some(value) = &assign.value {
                    self.assign(&assign.target, value, true);
                }
            },
            Stmt::AugAssign(assign) => {
                self.assign(&assign.target, &assign.value, true);
            },
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {},
            _ => walk_stmt(self, stmt),
        }
    }
}

fn setting_target(target: &Expr) -> Option<(&str, Option<Vec<String>>)> {
    match target {
        Expr::Name(name) => Some((name.id.as_str(), Some(Vec::new()))),
        Expr::Subscript(subscript) => {
            let (field, prefix) = setting_target(&subscript.value)?;
            let key = string_value(&subscript.slice).or_else(|| {
                matches!(&*subscript.slice, Expr::NumberLiteral(_)).then(|| "*".to_owned())
            });
            Some((
                field,
                prefix.zip(key).map(|(mut prefix, key)| {
                    prefix.push(key);
                    prefix
                }),
            ))
        },
        // Attribute writes change another object, not the settings namespace.
        _ => None,
    }
}

// An unread container or import slot may hide a project file or dependency;
// retain its readable siblings and report a gap rather than assuming absence.
fn read_path(expr: &Expr, path: &[&str], add: &mut impl FnMut(&Expr)) -> bool {
    let Some((head, tail)) = path.split_first() else {
        if matches!(expr, Expr::StringLiteral(_)) {
            add(expr);
            return true;
        }
        return false;
    };
    match expr {
        Expr::List(list) if *head == "*" => list
            .elts
            .iter()
            .fold(true, |complete, item| read_path(item, tail, add) & complete),
        Expr::Tuple(tuple) if *head == "*" => tuple
            .elts
            .iter()
            .fold(true, |complete, item| read_path(item, tail, add) & complete),
        Expr::Dict(dict) => dict.items.iter().fold(true, |complete, item| {
            let Some(key) = item.key.as_ref().and_then(string_value) else {
                return false;
            };
            if *head == "*" || key == *head {
                read_path(&item.value, tail, add) & complete
            } else {
                complete
            }
        }),
        _ => false,
    }
}

struct FirstPartyValues<'a, F> {
    modules: &'a BTreeSet<String>,
    add: F,
}

impl<F: FnMut(&Expr)> Visitor<'_> for FirstPartyValues<'_, F> {
    fn visit_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::StringLiteral(literal) => {
                let target = literal.value.to_str();
                if is_dotted_identifier(target)
                    && target.contains('.')
                    && (self.modules.contains(target)
                        || target
                            .rsplit_once('.')
                            .is_some_and(|(module, _)| self.modules.contains(module)))
                {
                    (self.add)(expr);
                }
                // Other strings name no first-party module, so ignoring them
                // cannot hide a project file or assert third-party usage.
            },
            Expr::Dict(dict) => {
                for item in &dict.items {
                    self.visit_expr(&item.value);
                }
            },
            Expr::Lambda(_) => {},
            _ => walk_expr(self, expr),
        }
    }
}
