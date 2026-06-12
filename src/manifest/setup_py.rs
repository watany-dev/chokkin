//! Static `setup.py` manifest extraction.

use std::path::Path;

use super::error::ManifestError;
use super::pep508_util::parse_requirement;
use super::types::{DeclaredDependency, DependencyContext, DependencyOrigin, ProjectMetadata};
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

    if let Some(name) = extract_keyword_string(&contents, "name") {
        result.metadata.name = Some(name);
    }
    if let Some(version) = extract_keyword_string(&contents, "version") {
        result.metadata.version = Some(version);
    }

    let install_requires = extract_string_list_argument(&contents, "install_requires");
    let extras_require = extract_extras_require(&contents);

    if install_requires.is_none() && extras_require.is_empty() {
        result
            .warnings
            .push(ManifestWarning::SetupPyNotStatic { file: rel.clone() });
        return Ok(result);
    }

    result.parsed = true;

    if let Some(requires) = install_requires {
        for (index, raw) in requires.iter().enumerate() {
            push_setup_dependency(
                &mut result.dependencies,
                &mut result.warnings,
                raw,
                DependencyContext::Runtime,
                &rel,
                &format!("install_requires[{index}]"),
            );
        }
    }

    for (extra, requires) in extras_require {
        for (index, raw) in requires.iter().enumerate() {
            push_setup_dependency(
                &mut result.dependencies,
                &mut result.warnings,
                raw,
                DependencyContext::SetupExtra(extra.clone()),
                &rel,
                &format!("extras_require.{extra}[{index}]"),
            );
        }
    }

    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn push_setup_dependency(
    dependencies: &mut Vec<DeclaredDependency>,
    warnings: &mut Vec<ManifestWarning>,
    raw: &str,
    context: DependencyContext,
    file: &str,
    label: &str,
) {
    let origin = DependencyOrigin {
        file: file.to_owned(),
        line: None,
        label: label.to_owned(),
    };
    match parse_requirement(raw, context, origin) {
        Ok(dep) => dependencies.push(dep),
        Err(warning) => warnings.push(warning),
    }
}

fn extract_keyword_string(contents: &str, keyword: &str) -> Option<String> {
    let needle = format!("{keyword}=");
    let start = contents.find(&needle)?;
    let after = contents[start + needle.len()..].trim_start();
    next_string_literal(after).map(|(value, _)| value)
}

fn extract_string_list_argument(contents: &str, argument: &str) -> Option<Vec<String>> {
    let start = contents.find(argument)?;
    let after = &contents[start + argument.len()..];
    let bracket_start = after.find('[')?;
    let list_body = extract_bracket_body(&after[bracket_start..])?;
    Some(extract_string_literals(list_body))
}

fn extract_extras_require(contents: &str) -> Vec<(String, Vec<String>)> {
    let Some(start) = contents.find("extras_require") else {
        return Vec::new();
    };
    let after = &contents[start + "extras_require".len()..];
    let Some(bracket_start) = after.find('{') else {
        return Vec::new();
    };
    let Some(body) = extract_brace_body(&after[bracket_start..]) else {
        return Vec::new();
    };

    let mut extras = Vec::new();
    let mut rest = body;
    while let Some((key, value, remaining)) = parse_dict_entry(rest) {
        extras.push((key, extract_string_literals(value)));
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
        let end = stripped.find('"')?;
        (&stripped[..end], &stripped[end + 1..])
    } else if let Some(stripped) = trimmed.strip_prefix('\'') {
        let end = stripped.find('\'')?;
        (&stripped[..end], &stripped[end + 1..])
    } else {
        return None;
    };

    let after_colon = after_key.split_once(':')?.1;
    let after_colon = after_colon.trim_start();
    let bracket_start = after_colon.find('[')?;
    let list_body = extract_bracket_body(&after_colon[bracket_start..])?;
    let consumed = bracket_start + list_body.len() + 2;
    let remaining = &after_colon[consumed.min(after_colon.len())..];
    Some((key.to_owned(), list_body, remaining))
}

fn extract_bracket_body(input: &str) -> Option<&str> {
    extract_delimited_body(input, '[', ']')
}

fn extract_brace_body(input: &str) -> Option<&str> {
    extract_delimited_body(input, '{', '}')
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

fn extract_string_literals(input: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = input;
    while let Some((literal, remaining)) = next_string_literal(rest) {
        values.push(literal);
        rest = remaining;
    }
    values
}

fn next_string_literal(input: &str) -> Option<(String, &str)> {
    let trimmed = input.trim_start_matches([',', ' ', '\n', '\r', '\t']);
    if let Some(stripped) = trimmed.strip_prefix('"') {
        let end = stripped.find('"')?;
        return Some((stripped[..end].to_owned(), &stripped[end + 1..]));
    }
    if let Some(stripped) = trimmed.strip_prefix('\'') {
        let end = stripped.find('\'')?;
        return Some((stripped[..end].to_owned(), &stripped[end + 1..]));
    }
    None
}

fn read_to_string(path: &Path) -> Result<String, ManifestError> {
    std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).map_or_else(
        |_| "setup.py".to_owned(),
        |p| p.to_string_lossy().replace('\\', "/"),
    )
}
