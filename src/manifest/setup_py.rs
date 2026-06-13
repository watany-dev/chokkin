//! Static `setup.py` manifest extraction.

use std::path::Path;

use super::error::ManifestError;
use super::literals::{
    LiteralScan, extract_delimited_body, extract_keyword_string, extract_string_list_argument,
    find_keyword_assignment, scan_string_literals,
};
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

    let Some(body) = setup_call_body(&contents) else {
        result
            .warnings
            .push(ManifestWarning::SetupPyNotStatic { file: rel });
        return Ok(result);
    };

    if let Some(name) = extract_keyword_string(body, "name") {
        result.metadata.name = Some(name);
    }
    if let Some(version) = extract_keyword_string(body, "version") {
        result.metadata.version = Some(version);
    }

    let install_requires = extract_string_list_argument(body, "install_requires");
    let extras_require = extract_extras_require(body);

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

fn setup_call_body(contents: &str) -> Option<&str> {
    let setup_pos = contents.find("setup(")?;
    let after_setup = &contents[setup_pos + "setup".len()..];
    extract_delimited_body(after_setup, '(', ')')
}

fn extract_extras_require(body: &str) -> Vec<(String, LiteralScan)> {
    let Some(pos) = find_keyword_assignment(body, "extras_require") else {
        return Vec::new();
    };
    let after = &body[pos + "extras_require".len()..];
    let Some(bracket_start) = after.find('{') else {
        return Vec::new();
    };
    let Some(dict_body) = extract_delimited_body(&after[bracket_start..], '{', '}') else {
        return Vec::new();
    };

    let mut extras = Vec::new();
    let mut rest = dict_body;
    while let Some((key, value, remaining)) = parse_dict_entry(rest) {
        extras.push((key, scan_string_literals(value)));
        rest = remaining;
    }
    extras
}

fn parse_dict_entry(input: &str) -> Option<(String, &str, &str)> {
    let trimmed = input.trim_start_matches([',', ' ', '\n', '\r', '\t']);
    if trimmed.is_empty() {
        return None;
    }

    let (key, after_key) = if let Some(stripped) = trimmed.strip_prefix('"') {
        let (value, remaining) = super::literals::read_quoted_string(stripped, '"')?;
        (value, remaining)
    } else if let Some(stripped) = trimmed.strip_prefix('\'') {
        let (value, remaining) = super::literals::read_quoted_string(stripped, '\'')?;
        (value, remaining)
    } else {
        return None;
    };

    let after_colon = after_key.split_once(':')?.1;
    let after_colon = after_colon.trim_start();
    let bracket_start = after_colon.find('[')?;
    let list_body = extract_delimited_body(&after_colon[bracket_start..], '[', ']')?;
    let consumed = bracket_start + list_body.len() + 2;
    let remaining = &after_colon[consumed.min(after_colon.len())..];
    Some((key, list_body, remaining))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_search_ignores_filename_false_positive() {
        let body = r#"name="acme", filename="name=ignored""#;
        assert_eq!(
            extract_keyword_string(body, "name").as_deref(),
            Some("acme")
        );
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
            fn extract_delimited_body_never_panics(input in "\\PC{0,200}") {
                for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
                    if let Some(body) = extract_delimited_body(&input, open, close) {
                        prop_assert!(input[1..].contains(body));
                    }
                }
            }

            #[test]
            fn setup_call_body_never_panics(input in "\\PC{0,300}") {
                let _ = setup_call_body(&input);
            }

            #[test]
            fn extract_keyword_string_finds_rendered_name(name in "[A-Za-z0-9._-]{1,30}") {
                let body = format!("name={}, version=\"1.0\"", python_quote(&name));
                prop_assert_eq!(extract_keyword_string(&body, "name"), Some(name));
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
                let body = setup_call_body(&contents).expect("setup call must be found");
                let scan = extract_string_list_argument(body, "install_requires")
                    .expect("install_requires must be found");
                prop_assert!(scan.complete);
                prop_assert_eq!(scan.values, deps);
            }
        }
    }
}
