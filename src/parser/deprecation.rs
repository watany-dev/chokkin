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
        visitor.visit_module(module.suite());
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
}
