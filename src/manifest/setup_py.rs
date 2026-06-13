//! Static `setup.py` manifest extraction.

use std::path::Path;

use super::error::ManifestError;
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

fn extract_keyword_string(body: &str, keyword: &str) -> Option<String> {
    let pos = find_keyword_assignment(body, keyword)?;
    let after = body[pos + keyword.len() + 1..].trim_start();
    next_string_literal(after).map(|(value, _)| value)
}

fn extract_string_list_argument(body: &str, argument: &str) -> Option<LiteralScan> {
    let pos = find_keyword_assignment(body, argument)?;
    let after = &body[pos + argument.len()..];
    let bracket_start = after.find('[')?;
    let list_body = extract_delimited_body(&after[bracket_start..], '[', ']')?;
    Some(scan_string_literals(list_body))
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

fn find_keyword_assignment(body: &str, keyword: &str) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut search_start = 0;
    while let Some(rel) = body[search_start..].find(keyword) {
        let abs = search_start + rel;
        if bytes.get(abs + keyword.len()) == Some(&b'=')
            && (abs == 0 || !is_ident_byte(bytes[abs - 1]))
        {
            return Some(abs);
        }
        search_start = abs + 1;
    }
    None
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn parse_dict_entry(input: &str) -> Option<(String, &str, &str)> {
    let trimmed = input.trim_start_matches([',', ' ', '\n', '\r', '\t']);
    if trimmed.is_empty() {
        return None;
    }

    let (key, after_key) = if let Some(stripped) = trimmed.strip_prefix('"') {
        let (value, remaining) = read_quoted_string(stripped, '"')?;
        (value, remaining)
    } else if let Some(stripped) = trimmed.strip_prefix('\'') {
        let (value, remaining) = read_quoted_string(stripped, '\'')?;
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

fn extract_delimited_body(input: &str, open: char, close: char) -> Option<&str> {
    if !input.starts_with(open) {
        return None;
    }

    let mut depth = 0usize;
    let mut in_string = None;
    let mut escaped = false;

    for (index, ch) in input.char_indices() {
        if let Some(quote) = in_string {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == quote {
                in_string = None;
            }
            continue;
        }

        if ch == '"' || ch == '\'' {
            in_string = Some(ch);
            continue;
        }

        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return Some(&input[1..index]);
            }
        }
    }

    None
}

struct LiteralScan {
    values: Vec<String>,
    complete: bool,
}

fn scan_string_literals(input: &str) -> LiteralScan {
    let mut values = Vec::new();
    let mut rest = input;
    let mut complete = true;

    loop {
        rest = rest.trim_start_matches([',', ' ', '\n', '\r', '\t']);
        if rest.is_empty() {
            break;
        }
        if rest.starts_with('#') {
            rest = rest.split('\n').nth(1).unwrap_or("");
            continue;
        }
        if let Some((literal, remaining)) = next_string_literal(rest) {
            values.push(literal);
            rest = remaining;
        } else {
            complete = false;
            break;
        }
    }

    LiteralScan { values, complete }
}

fn next_string_literal(input: &str) -> Option<(String, &str)> {
    let trimmed = input.trim_start_matches([',', ' ', '\n', '\r', '\t']);
    if let Some(stripped) = trimmed.strip_prefix('"') {
        let (value, remaining) = read_quoted_string(stripped, '"')?;
        return Some((value, remaining));
    }
    if let Some(stripped) = trimmed.strip_prefix('\'') {
        let (value, remaining) = read_quoted_string(stripped, '\'')?;
        return Some((value, remaining));
    }
    None
}

fn read_quoted_string(input: &str, quote: char) -> Option<(String, &str)> {
    // Fast path: no escape before the closing quote, so the value is a
    // plain slice copied in one allocation.
    let special = input.find([quote, '\\'])?;
    if input[special..].starts_with(quote) {
        let end = special + quote.len_utf8();
        return Some((input[..special].to_owned(), &input[end..]));
    }

    let mut value = String::new();
    let mut escaped = false;

    for (index, ch) in input.char_indices() {
        if escaped {
            value.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == quote {
            let end_index = index + ch.len_utf8();
            return Some((value, &input[end_index..]));
        }
        value.push(ch);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_string_literals_skips_inline_comments() {
        let scan = scan_string_literals(
            r#""a", # comment
 "b""#,
        );
        assert!(scan.complete);
        assert_eq!(scan.values, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn scan_string_literals_marks_incomplete_on_unexpected_token() {
        let scan = scan_string_literals(r#""a", broken, "b""#);
        assert!(!scan.complete);
        assert_eq!(scan.values, vec!["a".to_owned()]);
    }

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

        /// Quote `value` as a Python double-quoted string literal.
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

        /// String values without newlines (Python literals here are single-line).
        fn literal_value() -> impl Strategy<Value = String> {
            "[^\\r\\n]{0,40}"
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
            fn read_quoted_string_roundtrips(value in literal_value()) {
                let quoted = python_quote(&value);
                let (read, remaining) = read_quoted_string(&quoted[1..], '"')
                    .expect("quoted literal must read back");
                prop_assert_eq!(read, value);
                prop_assert_eq!(remaining, "");
            }

            #[test]
            fn read_quoted_string_never_panics(input in "\\PC{0,120}") {
                let _ = read_quoted_string(&input, '"');
                let _ = read_quoted_string(&input, '\'');
            }

            #[test]
            fn scan_string_literals_roundtrips(values in prop::collection::vec(literal_value(), 0..8)) {
                let rendered = values
                    .iter()
                    .map(|value| python_quote(value))
                    .collect::<Vec<_>>()
                    .join(", ");
                let scan = scan_string_literals(&rendered);
                prop_assert!(scan.complete);
                prop_assert_eq!(scan.values, values);
            }

            #[test]
            fn scan_string_literals_never_panics(input in "\\PC{0,200}") {
                let scan = scan_string_literals(&input);
                prop_assert!(scan.values.len() <= input.len());
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
