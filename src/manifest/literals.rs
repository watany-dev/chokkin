//! Shared static extraction of Python string literals and list literals.

/// Result of scanning a Python list literal for string elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiteralScan {
    /// Extracted string values in source order.
    pub values: Vec<String>,
    /// `false` when parsing stopped before the closing bracket.
    pub complete: bool,
}

/// Extract a keyword-assigned string list from a `setup()` call body.
pub fn extract_string_list_argument(body: &str, argument: &str) -> Option<LiteralScan> {
    let pos = find_keyword_assignment(body, argument)?;
    let after = &body[pos + argument.len()..];
    let bracket_start = after.find('[')?;
    let list_body = extract_delimited_body(&after[bracket_start..], '[', ']')?;
    Some(scan_string_literals(list_body))
}

/// Extract a keyword-assigned string from a `setup()` call body.
pub fn extract_keyword_string(body: &str, keyword: &str) -> Option<String> {
    let pos = find_keyword_assignment(body, keyword)?;
    let after = body[pos + keyword.len() + 1..].trim_start();
    next_string_literal(after).map(|(value, _)| value)
}

/// Extract uppercase-assigned list literals from Python source (e.g. Django settings).
pub fn extract_python_list_literals(
    contents: &str,
    field_names: &[&str],
) -> std::collections::BTreeMap<String, LiteralScan> {
    let mut out = std::collections::BTreeMap::new();
    for field in field_names {
        if let Some(scan) = extract_uppercase_list_literal(contents, field) {
            out.insert((*field).to_owned(), scan);
        }
    }
    out
}

/// Extract a list literal assigned to a Python name.
pub fn extract_python_list_assignment(contents: &str, field_name: &str) -> Option<LiteralScan> {
    let pos = find_python_assignment(contents, field_name)?;
    let after = contents[pos + field_name.len()..].trim_start();
    let after = after.strip_prefix('=')?;
    let after = after.trim_start();
    let bracket_start = after.find('[')?;
    let list_body = extract_delimited_body(&after[bracket_start..], '[', ']')?;
    Some(scan_string_literals(list_body))
}

/// Extract a single uppercase-assigned string literal (e.g. `ROOT_URLCONF`).
pub fn extract_python_string_assignment(contents: &str, field_name: &str) -> Option<String> {
    let pos = find_python_assignment(contents, field_name)?;
    let after = contents[pos + field_name.len()..].trim_start();
    let after = after.strip_prefix('=')?;
    next_string_literal(after.trim_start()).map(|(value, _)| value)
}

fn extract_uppercase_list_literal(contents: &str, field_name: &str) -> Option<LiteralScan> {
    extract_python_list_assignment(contents, field_name)
}

fn find_python_assignment(contents: &str, field_name: &str) -> Option<usize> {
    let bytes = contents.as_bytes();
    let mut search_start = 0;
    while let Some(rel) = contents[search_start..].find(field_name) {
        let abs = search_start + rel;
        let mut index = abs + field_name.len();
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if bytes.get(index) == Some(&b'=') && (abs == 0 || !is_ident_byte(bytes[abs - 1])) {
            return Some(abs);
        }
        search_start = abs + 1;
    }
    None
}

/// Find `keyword =` at statement boundaries inside a delimited body.
pub fn find_keyword_assignment(body: &str, keyword: &str) -> Option<usize> {
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

/// Extract the inner body of a balanced `open`/`close` delimiter pair.
pub fn extract_delimited_body(input: &str, open: char, close: char) -> Option<&str> {
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

/// Scan comma-separated string literals inside a list body.
pub fn scan_string_literals(input: &str) -> LiteralScan {
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

/// Read the next Python string literal from `input`.
pub fn next_string_literal(input: &str) -> Option<(String, &str)> {
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

/// Read a quoted Python string starting after the opening quote.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn read_quoted_string(input: &str, quote: char) -> Option<(String, &str)> {
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
    fn extract_python_list_finds_installed_apps() {
        let contents = r#"
INSTALLED_APPS = [
    "django.contrib.admin",
    "myapp",
]
"#;
        let lists = extract_python_list_literals(contents, &["INSTALLED_APPS"]);
        let scan = lists.get("INSTALLED_APPS").expect("found");
        assert!(scan.complete);
        assert_eq!(
            scan.values,
            vec!["django.contrib.admin".to_owned(), "myapp".to_owned()]
        );
    }

    #[test]
    fn extract_python_list_assignment_finds_lowercase_name() {
        let contents = r#"
extensions = [
    "sphinx.ext.autodoc",
    "myst_parser",
]
"#;
        let scan = extract_python_list_assignment(contents, "extensions").expect("found");
        assert!(scan.complete);
        assert_eq!(
            scan.values,
            vec!["sphinx.ext.autodoc".to_owned(), "myst_parser".to_owned()]
        );
    }

    #[test]
    fn extract_python_string_finds_root_urlconf() {
        let contents = r#"ROOT_URLCONF = "myproject.urls""#;
        assert_eq!(
            extract_python_string_assignment(contents, "ROOT_URLCONF").as_deref(),
            Some("myproject.urls")
        );
    }
}
