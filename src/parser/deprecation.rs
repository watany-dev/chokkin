//! Compatibility shims that warn on import that they are deprecated (#778).

use ruff_python_ast::{Expr, Stmt};

use super::types::{ImportKind, ImportRef};

/// Whether a module-level statement calls `warnings.warn(…, <category>)` with
/// a deprecation category, so importing the module itself is deprecated.
///
/// The category is matched by name only (`*Deprecat*` or `Removed*Warning`);
/// a `warn` inside a function deprecates that function, not the module.
pub(super) fn warns_deprecated_on_import(stmts: &[Stmt], imports: &[ImportRef]) -> bool {
    stmts.iter().any(|stmt| {
        let Stmt::Expr(stmt) = stmt else {
            return false;
        };
        let Expr::Call(call) = &*stmt.value else {
            return false;
        };
        imports.iter().any(|import| binds_warn(import, &call.func))
            && call
                .arguments
                .find_argument_value("category", 1)
                .and_then(category_name)
                .is_some_and(is_deprecation_category)
    })
}

fn binds_warn(import: &ImportRef, func: &Expr) -> bool {
    if import.deferred || import.module != "warnings" {
        return false;
    }
    match (import.kind, import.name.as_deref(), func) {
        (ImportKind::Import, None, Expr::Attribute(attribute)) => {
            attribute.attr.as_str() == "warn"
                && matches!(&*attribute.value, Expr::Name(receiver)
                    if receiver.id.as_str() == import.alias.as_deref().unwrap_or("warnings"))
        },
        (ImportKind::ImportFrom, Some("warn"), Expr::Name(name)) => {
            name.id.as_str() == import.alias.as_deref().unwrap_or("warn")
        },
        _ => false,
    }
}

fn category_name(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Name(name) => Some(name.id.as_str()),
        Expr::Attribute(attribute) => Some(attribute.attr.as_str()),
        _ => None,
    }
}

fn is_deprecation_category(name: &str) -> bool {
    name.contains("Deprecat") || (name.starts_with("Removed") && name.ends_with("Warning"))
}

#[cfg(test)]
mod tests {
    use crate::parser::lines::LineIndex;
    use crate::parser::visit::ModuleVisitor;
    use crate::sources::{FileContext, LayoutInfo};

    fn is_shim(source: &str) -> bool {
        let module = ruff_python_parser::parse_module(source).expect("parse");
        let layout = LayoutInfo::default();
        let lines = LineIndex::new(source);
        let mut visitor = ModuleVisitor::new("mod.py", &layout, FileContext::Runtime, &lines);
        visitor.visit_module(source, module.suite());
        visitor.into_parsed().deprecated_shim
    }

    #[test]
    fn module_level_deprecation_warning_marks_a_shim() {
        for source in [
            "import warnings\nwarnings.warn('moved', DeprecationWarning, stacklevel=2)\n",
            "import warnings\nwarnings.warn('moved', category=PendingDeprecationWarning)\n",
            "import warnings as w\nw.warn('moved', DeprecatedImportWarning)\n",
            "from warnings import warn\nwarn('moved', exc.RemovedInAirflow4Warning)\n",
            "from warnings import warn as _warn\n_warn('moved', RemovedInV2Warning)\n",
        ] {
            assert!(is_shim(source), "{source}");
        }
    }

    #[test]
    fn other_warnings_and_function_level_warnings_are_not_shims() {
        for source in [
            // A deprecated function, not a deprecated module.
            "import warnings\ndef old():\n    warnings.warn('old', DeprecationWarning)\n",
            "import warnings\nwarnings.warn('slow', RuntimeWarning)\n",
            "import warnings\nwarnings.warn('moved')\n",
            "import warnings\nwarnings.warn('moved', make_category())\n",
            "warnings.warn('moved', DeprecationWarning)\n",
            "import warnings\nlogger.warn('moved', DeprecationWarning)\n",
            "import warnings\nwarn('moved', DeprecationWarning)\n",
            "def f():\n    import warnings\nwarnings.warn('moved', DeprecationWarning)\n",
            "import warnings\nwarnings.filterwarnings('ignore', category=DeprecationWarning)\n",
        ] {
            assert!(!is_shim(source), "{source}");
        }
    }

    /// Shim detection over every binding of `warn`, category and place of
    /// the call, with layout noise that changes nothing (#861).
    mod props {
        use std::fmt::Write as _;

        use proptest::prelude::*;

        use super::is_shim;

        /// How the module binds `warn`, the callee it then calls, and whether
        /// that callee is the bound `warnings.warn`.
        const BINDINGS: [(&str, &str, bool); 7] = [
            ("import warnings", "warnings.warn", true),
            ("import warnings as w", "w.warn", true),
            ("from warnings import warn", "warn", true),
            ("from warnings import warn as _warn", "_warn", true),
            ("import warnings as w", "warnings.warn", false),
            ("from warnings import warn", "warnings.warn", false),
            ("import logging as warnings", "warnings.warn", false),
        ];

        /// Category expressions and whether their name marks a deprecation.
        const CATEGORIES: [(&str, bool); 10] = [
            ("DeprecationWarning", true),
            ("PendingDeprecationWarning", true),
            ("exc.DeprecatedImportWarning", true),
            ("RemovedInV3Warning", true),
            ("airflow.exceptions.RemovedInAirflow4Warning", true),
            ("FutureWarning", false),
            ("RuntimeWarning", false),
            ("UserWarning", false),
            ("RemovedInV3", false),
            ("make_category()", false),
        ];

        /// Where the call sits: only a module-level statement deprecates the
        /// module itself.
        #[derive(Clone, Copy, Debug)]
        enum Place {
            TopLevel,
            InFunction,
            InIf,
            InClass,
        }

        fn place() -> impl Strategy<Value = Place> {
            prop_oneof![
                Just(Place::TopLevel),
                Just(Place::InFunction),
                Just(Place::InIf),
                Just(Place::InClass),
            ]
        }

        #[derive(Clone, Debug)]
        struct Shim {
            binding: usize,
            /// The import sits in a function, so the module never binds it.
            deferred_import: bool,
            category: usize,
            /// `None` leaves the category out.
            arg: Option<u8>,
            place: Place,
        }

        fn shim() -> impl Strategy<Value = Shim> {
            (
                0..BINDINGS.len(),
                any::<bool>(),
                0..CATEGORIES.len(),
                proptest::option::weighted(0.9, 0..4u8),
                place(),
            )
                .prop_map(|(binding, deferred_import, category, arg, place)| Shim {
                    binding,
                    deferred_import,
                    category,
                    arg,
                    place,
                })
        }

        impl Shim {
            fn expected(&self) -> bool {
                BINDINGS[self.binding].2
                    && !self.deferred_import
                    && self.arg.is_some()
                    && CATEGORIES[self.category].1
                    && matches!(self.place, Place::TopLevel)
            }

            /// The module text; `noise` adds comments, blank lines and line
            /// breaks inside the call's parentheses.
            fn render(&self, noise: &[bool]) -> String {
                let mut noise = noise.iter().cycle().copied();
                let mut next = || noise.next().unwrap_or(false);
                let (import, callee, _) = BINDINGS[self.binding];
                let (category, _) = CATEGORIES[self.category];
                let mut source = String::from("import os\n");
                if self.deferred_import {
                    let _ = writeln!(source, "def setup():\n    {import}");
                } else {
                    let _ = writeln!(source, "{import}");
                }
                if next() {
                    source.push_str("\n# compatibility shim\n");
                }
                let sep = if next() { ",\n    # why\n    " } else { ", " };
                let args = match self.arg {
                    None => "\"moved\"".to_owned(),
                    Some(0) => format!("\"moved\"{sep}{category}"),
                    Some(1) => format!("\"moved\"{sep}{category}{sep}stacklevel=2"),
                    Some(2) => format!("\"moved\"{sep}category={category}"),
                    Some(_) => {
                        format!("message=\"moved\"{sep}category={category}{sep}stacklevel=2")
                    },
                };
                let call = format!("{callee}({args})");
                match self.place {
                    Place::TopLevel => {
                        let _ = writeln!(source, "{call}");
                    },
                    Place::InFunction => {
                        let _ = writeln!(source, "def old():\n    {call}");
                    },
                    Place::InIf => {
                        let _ = writeln!(source, "if os.environ.get(\"WARN\"):\n    {call}");
                    },
                    Place::InClass => {
                        let _ = writeln!(source, "class Old:\n    {call}");
                    },
                }
                if next() {
                    source.push_str("value = 1  # trailing\n");
                }
                source
            }
        }

        proptest! {
            /// A module is a shim exactly when a module-level statement
            /// calls the `warn` it binds at module level with a deprecation
            /// category, whatever the layout around it.
            #[test]
            fn shim_detection_matches_reference_model(
                shim in shim(),
                noise in prop::collection::vec(any::<bool>(), 0..6),
            ) {
                let plain = shim.render(&[]);
                prop_assert_eq!(is_shim(&plain), shim.expected(), "{}", plain);
                let noisy = shim.render(&noise);
                prop_assert_eq!(is_shim(&noisy), shim.expected(), "{}", noisy);
            }
        }
    }
}
