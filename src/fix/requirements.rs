//! `requirements*.txt` line-based edits.

use crate::manifest::normalize_distribution_name;

use super::error::FixError;
use super::write::{atomic_write, read_manifest};

/// Remove a dependency line from a requirements file by line number or name match.
pub fn remove_dependency_line(
    path: &std::path::Path,
    distribution: &str,
    line: Option<u32>,
) -> Result<String, FixError> {
    let (rel, contents) = read_manifest(path, "requirements.txt")?;

    if contents.lines().any(|line| line.contains("--hash=")) {
        return Err(FixError::Unsupported {
            detail: "hash-pinned requirements files cannot be auto-edited".to_owned(),
        });
    }

    let target = normalize_distribution_name(distribution);
    let mut removed = false;
    let mut output = Vec::new();

    for (index, raw_line) in contents.lines().enumerate() {
        let line_no = u32::try_from(index + 1).unwrap_or(u32::MAX);
        if line.is_some_and(|expected| expected == line_no) || line_name_matches(raw_line, &target)
        {
            removed = true;
            continue;
        }
        output.push(raw_line);
    }

    if !removed {
        return Err(FixError::Unsupported {
            detail: format!("dependency `{distribution}` not found in {rel}"),
        });
    }

    let mut updated = output.join("\n");
    if contents.ends_with('\n') {
        updated.push('\n');
    }

    atomic_write(path, &updated)?;
    Ok(format!("removed `{distribution}` from {rel}"))
}

fn line_name_matches(line: &str, distribution: &str) -> bool {
    let trimmed = strip_comment(line).trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return false;
    }
    let name = trimmed
        .split(['[', ';', '#', ' '])
        .next()
        .unwrap_or(trimmed);
    let normalized = normalize_distribution_name(
        name.split(['=', '<', '>', '!', '~', '['])
            .next()
            .unwrap_or(name),
    );
    normalized == distribution
}

fn strip_comment(line: &str) -> &str {
    line.split_once('#').map_or(line, |(before, _)| before)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn removes_matching_requirements_line() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("requirements.txt");
        std::fs::write(&path, "boto3>=1.0\nrequests>=2.0\n").expect("write");
        remove_dependency_line(&path, "boto3", None).expect("remove");
        let updated = std::fs::read_to_string(&path).expect("read");
        assert!(!updated.contains("boto3"));
        assert!(updated.contains("requests"));
    }
}
