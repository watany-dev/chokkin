//! Parse Python source into import references.

use rustpython_parser::ast::{self, ExceptHandler, Ranged, Stmt};
use rustpython_parser::source_code::RandomLocator;
use rustpython_parser::{Parse, ParseError as RpParseError};

use crate::config::TargetVersion;
use crate::discovery::ProjectRoot;

use super::error::ParseError;
use super::types::{ImportKind, ImportRef, ParseDiagnostic, ParseSeverity, ParsedModule};

/// Parse one `.py` file under `root` (static only; never executes Python).
///
/// Syntax errors are recorded in [`ParsedModule::diagnostics`]; the function still
/// returns `Ok` unless the file cannot be read.
///
/// # Errors
///
/// Returns [`ParseError::Io`] when the file cannot be read.
#[allow(clippy::no_effect_underscore_binding)]
pub fn parse_file(
    root: &ProjectRoot,
    path: &str,
    target: TargetVersion,
) -> Result<ParsedModule, ParseError> {
    let _unused = target; // Step 6 will gate syntax features by target version.
    let absolute = root.path.join(path);
    let source = std::fs::read_to_string(&absolute).map_err(|source| ParseError::Io {
        path: absolute,
        source,
    })?;

    let mut parsed = ParsedModule {
        path: path.to_owned(),
        imports: Vec::new(),
        diagnostics: Vec::new(),
    };

    let mut locator = RandomLocator::new(&source);
    match ast::Suite::parse(&source, path) {
        Ok(stmts) => {
            for stmt in stmts {
                collect_imports(&stmt, &mut locator, &mut parsed.imports);
            }
        },
        Err(error) => parsed
            .diagnostics
            .push(syntax_diagnostic(path, &mut locator, &error)),
    }

    Ok(parsed)
}

fn syntax_diagnostic(
    path: &str,
    locator: &mut RandomLocator<'_>,
    error: &RpParseError,
) -> ParseDiagnostic {
    let line = locator.locate(error.offset).row.get();
    ParseDiagnostic {
        line,
        message: format!("syntax error in `{path}`: {error}"),
        severity: ParseSeverity::Error,
    }
}

fn line_number<R: Ranged>(node: &R, locator: &mut RandomLocator<'_>) -> u32 {
    locator.locate(node.start()).row.get()
}

#[allow(clippy::too_many_lines)]
fn collect_imports(stmt: &Stmt, locator: &mut RandomLocator<'_>, imports: &mut Vec<ImportRef>) {
    match stmt {
        Stmt::Import(import) => {
            let line = line_number(import, locator);
            for alias in &import.names {
                imports.push(ImportRef {
                    module: alias.name.to_string(),
                    line,
                    kind: ImportKind::Import,
                });
            }
        },
        Stmt::ImportFrom(import_from) => {
            if import_from.level.is_some_and(|level| level.to_u32() > 0) {
                return;
            }
            let Some(module) = import_from.module.as_ref() else {
                return;
            };
            let line = line_number(import_from, locator);
            imports.push(ImportRef {
                module: module.to_string(),
                line,
                kind: ImportKind::ImportFrom,
            });
        },
        Stmt::FunctionDef(function) => {
            for inner in &function.body {
                collect_imports(inner, locator, imports);
            }
        },
        Stmt::AsyncFunctionDef(function) => {
            for inner in &function.body {
                collect_imports(inner, locator, imports);
            }
        },
        Stmt::ClassDef(class) => {
            for inner in &class.body {
                collect_imports(inner, locator, imports);
            }
        },
        Stmt::If(if_stmt) => {
            for inner in &if_stmt.body {
                collect_imports(inner, locator, imports);
            }
            for inner in &if_stmt.orelse {
                collect_imports(inner, locator, imports);
            }
        },
        Stmt::Try(try_stmt) => {
            for inner in &try_stmt.body {
                collect_imports(inner, locator, imports);
            }
            for handler in &try_stmt.handlers {
                let ExceptHandler::ExceptHandler(handler) = handler;
                for inner in &handler.body {
                    collect_imports(inner, locator, imports);
                }
            }
            for inner in &try_stmt.orelse {
                collect_imports(inner, locator, imports);
            }
            for inner in &try_stmt.finalbody {
                collect_imports(inner, locator, imports);
            }
        },
        Stmt::With(with_stmt) => {
            for inner in &with_stmt.body {
                collect_imports(inner, locator, imports);
            }
        },
        Stmt::AsyncWith(with_stmt) => {
            for inner in &with_stmt.body {
                collect_imports(inner, locator, imports);
            }
        },
        Stmt::Match(match_stmt) => {
            for case in &match_stmt.cases {
                for inner in &case.body {
                    collect_imports(inner, locator, imports);
                }
            }
        },
        _ => {},
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};

    fn write_temp_py(dir: &Path, name: &str, contents: &str) -> ProjectRoot {
        fs::write(dir.join(name), contents).expect("write");
        ProjectRoot {
            path: dir.to_path_buf(),
            marker: RootMarker::PyProjectToml,
            start: dir.to_path_buf(),
        }
    }

    #[test]
    fn parses_simple_import() {
        let temp = TempDir::new().expect("tempdir");
        let root = write_temp_py(
            temp.path(),
            "sample.py",
            "import os\nfrom sys import version\n",
        );
        let parsed = parse_file(&root, "sample.py", TargetVersion::default_py311()).expect("parse");
        assert_eq!(parsed.imports.len(), 2);
        assert!(parsed.diagnostics.is_empty());
    }

    #[test]
    fn syntax_error_becomes_diagnostic() {
        let temp = TempDir::new().expect("tempdir");
        let root = write_temp_py(temp.path(), "broken.py", "def broken(:\n");
        let parsed = parse_file(&root, "broken.py", TargetVersion::default_py311()).expect("parse");
        assert!(parsed.imports.is_empty());
        assert_eq!(parsed.diagnostics.len(), 1);
    }
}
