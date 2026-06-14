//! `setup.cfg` dependency line edits (limited sections).

use super::error::FixError;
use super::requirements::remove_dependency_line;

/// Remove a dependency from `setup.cfg` `install_requires` or extras.
pub fn remove_dependency(path: &std::path::Path, distribution: &str) -> Result<String, FixError> {
    let rel = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("setup.cfg");
    let contents = std::fs::read_to_string(path).map_err(|source| FixError::Io {
        path: rel.to_owned(),
        source,
    })?;

    let mut removed = false;
    let mut output: Vec<String> = Vec::new();
    let mut in_install_requires = false;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed == "[options]" {
            in_install_requires = true;
            output.push(line.to_owned());
            continue;
        }
        if trimmed.starts_with('[') {
            in_install_requires = false;
        }
        if in_install_requires
            && trimmed.starts_with("install_requires")
            && line_contains_dependency(trimmed, distribution)
        {
            let updated = remove_from_multiline_value(trimmed, distribution);
            if updated.is_empty() {
                removed = true;
                continue;
            }
            output.push(updated);
            removed = true;
            continue;
        }
        output.push(line.to_owned());
    }

    if !removed {
        return remove_dependency_line(path, distribution, None);
    }

    let mut updated = output.join("\n");
    if contents.ends_with('\n') {
        updated.push('\n');
    }
    std::fs::write(path, updated).map_err(|source| FixError::Io {
        path: rel.to_owned(),
        source,
    })?;
    Ok(format!("removed `{distribution}` from {rel}"))
}

fn line_contains_dependency(line: &str, distribution: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains(&distribution.to_ascii_lowercase())
}

fn remove_from_multiline_value(line: &str, distribution: &str) -> String {
    let Some((key, value)) = line.split_once('=') else {
        return line.to_owned();
    };
    let entries: Vec<_> = value
        .split(',')
        .map(str::trim)
        .filter(|entry| {
            !entry.is_empty()
                && !entry
                    .to_ascii_lowercase()
                    .starts_with(&distribution.to_ascii_lowercase())
        })
        .collect();
    if entries.is_empty() {
        String::new()
    } else {
        format!("{key} = {}", entries.join(",\n    "))
    }
}
