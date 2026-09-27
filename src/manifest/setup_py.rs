//! Static `setup.py` manifest extraction.

use std::path::Path;

use ruff_python_ast::{Expr, Keyword, Stmt};

use super::error::ManifestError;
use super::literals::{LiteralScan, parse_module, string_list, string_value};
use super::types::{DeclaredDependency, DependencyContext, ProjectMetadata};
use super::util::{DependencyPush, push_dependency, read_to_string, relative_path};
use super::warnings::ManifestWarning;

/// Partial extraction result from `setup.py`.
#[derive(Debug, Default)]
pub struct SetupPyExtraction {
    /// Project metadata when statically available.
    pub metadata: ProjectMetadata,
    /// Declared dependencies.
    pub dependencies: Vec<DeclaredDependency>,
    /// Non-fatal warnings.
    pub warnings: Vec<ManifestWarning>,
    /// Whether static parsing succeeded.
    pub parsed: bool,
}

/// Extract manifest data from `setup.py` without executing Python.
pub fn extract_setup_py(root: &Path, path: &Path) -> Result<SetupPyExtraction, ManifestError> {
    let contents = read_to_string(path)?;
    let rel = relative_path(root, path);
    let mut result = SetupPyExtraction::default();

    let stmts = parse_module(&contents).unwrap_or_default();
    let Some(keywords) = setup_call_keywords(&stmts) else {
        result
            .warnings
            .push(ManifestWarning::SetupPyNotStatic { file: rel });
        return Ok(result);
    };

    result.metadata.name = keyword_value(keywords, "name").and_then(string_value);
    result.metadata.version = keyword_value(keywords, "version").and_then(string_value);

    let install_requires = keyword_value(keywords, "install_requires").and_then(string_list);
    let extras_require = extract_extras_require(keywords);

    if install_requires.is_none() && extras_require.is_empty() {
        result
            .warnings
            .push(ManifestWarning::SetupPyNotStatic { file: rel });
        return Ok(result);
    }

    result.parsed = true;

    if let Some(scan) = install_requires {
        if !scan.complete {
            result
                .warnings
                .push(ManifestWarning::SetupPyPartiallyStatic {
                    file: rel.clone(),
                    argument: "install_requires".to_owned(),
                });
        }
        for (index, raw) in scan.values.iter().enumerate() {
            push_dependency(DependencyPush {
                dependencies: &mut result.dependencies,
                warnings: &mut result.warnings,
                raw,
                context: DependencyContext::Runtime,
                file: &rel,
                label: format!("install_requires[{index}]"),
                line: None,
            });
        }
    }

    for (extra, scan) in extras_require {
        if !scan.complete {
            result
                .warnings
                .push(ManifestWarning::SetupPyPartiallyStatic {
                    file: rel.clone(),
                    argument: format!("extras_require.{extra}"),
                });
        }
        for (index, raw) in scan.values.iter().enumerate() {
            push_dependency(DependencyPush {
                dependencies: &mut result.dependencies,
                warnings: &mut result.warnings,
                raw,
                context: DependencyContext::SetupExtra(extra.clone()),
                file: &rel,
                label: format!("extras_require.{extra}[{index}]"),
                line: None,
            });
        }
    }

    Ok(result)
}

/// Keywords of the first `setup(...)` call at top level or under an `if`.
fn setup_call_keywords(stmts: &[Stmt]) -> Option<&[Keyword]> {
    stmts.iter().find_map(|stmt| match stmt {
        Stmt::Expr(expr) => match &*expr.value {
            Expr::Call(call) if is_setup(&call.func) => Some(&*call.arguments.keywords),
            _ => None,
        },
        Stmt::If(if_stmt) => setup_call_keywords(&if_stmt.body).or_else(|| {
            if_stmt
                .elif_else_clauses
                .iter()
                .find_map(|clause| setup_call_keywords(&clause.body))
        }),
        _ => None,
    })
}

fn is_setup(func: &Expr) -> bool {
    match func {
        Expr::Name(name) => name.id.as_str() == "setup",
        Expr::Attribute(attribute) => attribute.attr.as_str() == "setup",
        _ => false,
    }
}

fn keyword_value<'a>(keywords: &'a [Keyword], name: &str) -> Option<&'a Expr> {
    keywords
        .iter()
        .find(|keyword| keyword.arg.as_ref().is_some_and(|arg| arg.as_str() == name))
        .map(|keyword| &keyword.value)
}

fn extract_extras_require(keywords: &[Keyword]) -> Vec<(String, LiteralScan)> {
    let Some(Expr::Dict(dict)) = keyword_value(keywords, "extras_require") else {
        return Vec::new();
    };
    dict.items
        .iter()
        .filter_map(|item| Some((string_value(item.key.as_ref()?)?, string_list(&item.value)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_setup_keywords<T>(contents: &str, f: impl FnOnce(&[Keyword]) -> T) -> Option<T> {
        let stmts = parse_module(contents)?;
        setup_call_keywords(&stmts).map(f)
    }

    #[test]
    fn setup_call_under_main_guard_is_found() {
        let contents = "import setuptools\nif __name__ == \"__main__\":\n    setuptools.setup(name=\"acme\")\n";
        let name = with_setup_keywords(contents, |keywords| {
            keyword_value(keywords, "name").and_then(string_value)
        });
        assert_eq!(name.flatten().as_deref(), Some("acme"));
    }

    #[test]
    fn extras_require_keeps_string_key_list_entries() {
        let contents =
            "setup(extras_require={\"test\": [\"pytest\"], \"dev\": \"ruff\", **more})\n";
        let extras = with_setup_keywords(contents, extract_extras_require).expect("setup call");
        assert_eq!(extras.len(), 1);
        assert_eq!(extras[0].0, "test");
        assert_eq!(extras[0].1.values, vec!["pytest".to_owned()]);
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn python_quote(value: &str) -> String {
            let mut quoted = String::with_capacity(value.len() + 2);
            quoted.push('"');
            for ch in value.chars() {
                if ch == '"' || ch == '\\' {
                    quoted.push('\\');
                }
                quoted.push(ch);
            }
            quoted.push('"');
            quoted
        }

        proptest! {
            #[test]
            fn setup_call_keywords_never_panics(input in "\\PC{0,300}") {
                let _ = with_setup_keywords(&input, <[Keyword]>::len);
            }

            #[test]
            fn full_setup_call_roundtrips_install_requires(
                deps in prop::collection::vec("[a-z][a-z0-9-]{0,15}", 0..6),
            ) {
                let rendered = deps
                    .iter()
                    .map(|dep| python_quote(dep))
                    .collect::<Vec<_>>()
                    .join(", ");
                let contents = format!(
                    "from setuptools import setup\nsetup(\n    name=\"acme\",\n    install_requires=[{rendered}],\n)\n"
                );
                let scan = with_setup_keywords(&contents, |keywords| {
                    keyword_value(keywords, "install_requires").and_then(string_list)
                })
                .flatten()
                .expect("install_requires must be found");
                prop_assert!(scan.complete);
                prop_assert_eq!(scan.values, deps);
            }
        }
    }
}
