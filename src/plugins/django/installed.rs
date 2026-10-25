//! The apps `INSTALLED_APPS` lists, as one settings file builds it (#1127).

use std::collections::BTreeSet;

use ruff_python_ast::visitor::{Visitor, walk_expr, walk_stmt};
use ruff_python_ast::{Expr, ExprCall, Operator, Stmt};
use ruff_text_size::{Ranged, TextSize};

use crate::manifest::literals::string_value;

const FIELD: &str = "INSTALLED_APPS";

/// List methods that only remove or read, so they install nothing.
const NON_ADDING: &[&str] = &[
    "remove", "pop", "clear", "index", "count", "copy", "sort", "reverse",
];

/// Names a list is followed through before it counts as not read.
const MAX_DEPTH: usize = 8;

/// What a settings file adds to `INSTALLED_APPS`.
#[derive(Default)]
pub(super) struct Apps {
    /// Each app string and where it starts.
    pub values: Vec<(String, TextSize)>,
    /// Where the first addition not read starts.
    pub unread: Option<TextSize>,
}

/// Every app the file adds to `INSTALLED_APPS`, in any scope: assigned
/// (`=`, `: list =`), `+=`, `.append()`, `.insert()` and `.extend()`. A value
/// is read when it is a string, or a list or tuple of them, joined with `+`,
/// unpacked with `*`, wrapped in `list()` / `tuple()`, or a name the file
/// builds the same way (`LOCAL_APPS`). An earlier value of `INSTALLED_APPS`
/// itself is what the file has already added. Anything else is unread, and
/// so is a subscript or unpacking store. Removals are not followed, so an app
/// a file takes out still counts as installed: that hides no unused file.
/// `None` when the file adds nothing.
pub(super) fn read(stmts: &[Stmt]) -> Option<Apps> {
    let mut apps = Apps::default();
    let mut reader = Reader {
        stmts,
        seen: BTreeSet::from([FIELD.to_owned()]),
        apps: &mut apps,
    };
    let found = reader.name(FIELD, 0);
    found.then_some(apps)
}

struct Reader<'a, 'b> {
    stmts: &'a [Stmt],
    /// Names already read, so a list built from itself ends.
    seen: BTreeSet<String>,
    apps: &'b mut Apps,
}

impl Reader<'_, '_> {
    /// Read what the file adds to `name`; `false` when it adds nothing.
    fn name(&mut self, name: &str, depth: usize) -> bool {
        let mut additions = Additions {
            name,
            found: Vec::new(),
        };
        additions.visit_body(self.stmts);
        let found = !additions.found.is_empty();
        for addition in additions.found {
            match addition {
                Addition::Sequence(expr) => self.sequence(expr, depth),
                Addition::Element(expr) => self.element(expr),
                Addition::Unread(start) => self.unread(start),
            }
        }
        found
    }

    fn sequence(&mut self, expr: &Expr, depth: usize) {
        match expr {
            Expr::List(list) => list
                .elts
                .iter()
                .for_each(|elt| self.element_or_starred(elt, depth)),
            Expr::Tuple(tuple) => tuple
                .elts
                .iter()
                .for_each(|elt| self.element_or_starred(elt, depth)),
            Expr::BinOp(binop) if binop.op == Operator::Add => {
                self.sequence(&binop.left, depth);
                self.sequence(&binop.right, depth);
            },
            Expr::Name(name) => {
                let id = name.id.as_str();
                if self.seen.contains(id) {
                    return;
                }
                if depth >= MAX_DEPTH {
                    self.unread(name.start());
                    return;
                }
                self.seen.insert(id.to_owned());
                if !self.name(id, depth + 1) {
                    self.unread(name.start());
                }
            },
            Expr::Call(call) if is_conversion(call) => {
                self.sequence(&call.arguments.args[0], depth);
            },
            _ => self.unread(expr.start()),
        }
    }

    fn element_or_starred(&mut self, expr: &Expr, depth: usize) {
        if let Expr::Starred(starred) = expr {
            self.sequence(&starred.value, depth);
        } else {
            self.element(expr);
        }
    }

    fn element(&mut self, expr: &Expr) {
        match string_value(expr) {
            Some(value) => self.apps.values.push((value, expr.start())),
            None => self.unread(expr.start()),
        }
    }

    fn unread(&mut self, start: TextSize) {
        let first = self.apps.unread.get_or_insert(start);
        *first = (*first).min(start);
    }
}

/// `list(x)` or `tuple(x)`, which hold what `x` holds.
fn is_conversion(call: &ExprCall) -> bool {
    matches!(&*call.func, Expr::Name(func) if matches!(func.id.as_str(), "list" | "tuple"))
        && call.arguments.args.len() == 1
        && call.arguments.keywords.is_empty()
        && !matches!(call.arguments.args[0], Expr::Starred(_))
}

enum Addition<'a> {
    /// A value whose items are added.
    Sequence(&'a Expr),
    /// A value added as one item.
    Element(&'a Expr),
    /// An addition not read, where it starts.
    Unread(TextSize),
}

/// The places a file adds to `name`.
struct Additions<'a, 'n> {
    name: &'n str,
    found: Vec<Addition<'a>>,
}

impl<'a> Additions<'a, '_> {
    fn is_name(&self, expr: &Expr) -> bool {
        matches!(expr, Expr::Name(name) if name.id.as_str() == self.name)
    }

    /// Whether `target` stores into `name` other than by plain assignment.
    fn stores_into(&self, target: &Expr) -> bool {
        match target {
            Expr::Subscript(subscript) => self.is_name(&subscript.value),
            Expr::Tuple(tuple) => tuple.elts.iter().any(|elt| self.stores_into_any(elt)),
            Expr::List(list) => list.elts.iter().any(|elt| self.stores_into_any(elt)),
            Expr::Starred(starred) => self.stores_into_any(&starred.value),
            _ => false,
        }
    }

    fn stores_into_any(&self, target: &Expr) -> bool {
        self.is_name(target) || self.stores_into(target)
    }

    fn method(&mut self, call: &'a ExprCall) {
        let Expr::Attribute(attribute) = &*call.func else {
            return;
        };
        if !self.is_name(&attribute.value) {
            return;
        }
        let args = &call.arguments.args;
        let plain = call.arguments.keywords.is_empty()
            && !args.iter().any(|arg| matches!(arg, Expr::Starred(_)));
        let addition = match (attribute.attr.as_str(), args.len()) {
            (method, _) if NON_ADDING.contains(&method) => return,
            ("append", 1) if plain => Addition::Element(&args[0]),
            ("insert", 2) if plain => Addition::Element(&args[1]),
            ("extend", 1) if plain => Addition::Sequence(&args[0]),
            _ => Addition::Unread(call.start()),
        };
        self.found.push(addition);
    }
}

impl<'a> Visitor<'a> for Additions<'a, '_> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::Assign(assign) => {
                if assign.targets.iter().any(|target| self.is_name(target)) {
                    self.found.push(Addition::Sequence(&assign.value));
                }
                if assign.targets.iter().any(|target| self.stores_into(target)) {
                    self.found.push(Addition::Unread(assign.start()));
                }
            },
            Stmt::AnnAssign(assign) if self.is_name(&assign.target) => {
                if let Some(value) = &assign.value {
                    self.found.push(Addition::Sequence(value));
                }
            },
            Stmt::AugAssign(assign) if self.is_name(&assign.target) => {
                self.found.push(if assign.op == Operator::Add {
                    Addition::Sequence(&assign.value)
                } else {
                    Addition::Unread(assign.start())
                });
            },
            Stmt::AugAssign(assign) if self.stores_into(&assign.target) => {
                self.found.push(Addition::Unread(assign.start()));
            },
            _ => {},
        }
        walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Call(call) = expr {
            self.method(call);
        }
        walk_expr(self, expr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::literals::parse_module;

    fn apps(source: &str) -> Option<(Vec<String>, bool)> {
        let stmts = parse_module(source).expect("parse");
        read(&stmts).map(|apps| {
            (
                apps.values.into_iter().map(|(value, _)| value).collect(),
                apps.unread.is_some(),
            )
        })
    }

    #[test]
    fn reads_each_addition() {
        let source = "\
BASE = ('a',)
INSTALLED_APPS = [*BASE, 'b']
INSTALLED_APPS += ['c']
INSTALLED_APPS.append('d')
INSTALLED_APPS.insert(0, 'e')
INSTALLED_APPS.extend(list(EXTRA))
EXTRA = ['f']
EXTRA += ['g']
INSTALLED_APPS.remove('a')
";
        assert_eq!(
            apps(source),
            Some((
                ["a", "b", "c", "d", "e", "f", "g"]
                    .map(str::to_owned)
                    .to_vec(),
                false
            ))
        );
    }

    #[test]
    fn a_list_built_from_itself_ends() {
        assert_eq!(
            apps("A = B\nB = A + ['x']\nINSTALLED_APPS = A\n"),
            Some((vec!["x".to_owned()], false))
        );
    }

    #[test]
    fn other_forms_are_unread() {
        for source in [
            "INSTALLED_APPS = build()",
            "INSTALLED_APPS = ['a', name]",
            "INSTALLED_APPS = BASE",
            "INSTALLED_APPS = []\nINSTALLED_APPS += build()",
            "INSTALLED_APPS = []\nINSTALLED_APPS.append(name)",
            "INSTALLED_APPS = []\nINSTALLED_APPS.extend(*more)",
            "INSTALLED_APPS = []\nINSTALLED_APPS.add('a')",
            "INSTALLED_APPS = []\nINSTALLED_APPS[0:0] = ['a']",
            "INSTALLED_APPS = []\nINSTALLED_APPS *= 2",
            "first, *INSTALLED_APPS = build()",
        ] {
            assert_eq!(
                apps(source).map(|(_, unread)| unread),
                Some(true),
                "{source}"
            );
        }
    }

    #[test]
    fn a_file_that_only_reads_it_adds_nothing() {
        assert_eq!(apps("if 'a' in INSTALLED_APPS:\n    DEBUG = True\n"), None);
    }
}
