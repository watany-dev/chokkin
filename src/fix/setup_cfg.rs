//! `setup.cfg` dependency line edits (limited sections).

use crate::manifest::normalize_distribution_name;

use super::error::FixError;
use super::requirements::remove_dependency_line;
use super::write::atomic_write;

/// Remove a dependency from `setup.cfg` `install_requires` or extras.
#[allow(clippy::too_many_lines)]
pub fn remove_dependency(path: &std::path::Path, distribution: &str) -> Result<String, FixError> {
    let rel = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("setup.cfg");
    let contents = std::fs::read_to_string(path).map_err(|source| FixError::Io {
        path: rel.to_owned(),
        source,
    })?;

    let target = normalize_distribution_name(distribution);
    let mut removed = false;
    let mut output: Vec<String> = Vec::new();
    let mut in_options = false;
    let mut collecting_install_requires = false;
    let mut pending_entries: Vec<String> = Vec::new();

    for line in contents.lines() {
        let trimmed = line.trim();

        if trimmed == "[options]" {
            flush_install_requires(
                &mut output,
                &mut pending_entries,
                &mut collecting_install_requires,
                &mut removed,
                &target,
            );
            in_options = true;
            output.push(line.to_owned());
            continue;
        }

        if trimmed.starts_with('[') {
            flush_install_requires(
                &mut output,
                &mut pending_entries,
                &mut collecting_install_requires,
                &mut removed,
                &target,
            );
            in_options = false;
            output.push(line.to_owned());
            continue;
        }

        if in_options && trimmed.starts_with("install_requires") {
            collecting_install_requires = true;
            if let Some((_, value)) = trimmed.split_once('=') {
                for entry in value.split(',') {
                    let entry = entry.trim();
                    if !entry.is_empty() {
                        pending_entries.push(entry.to_owned());
                    }
                }
            }
            continue;
        }

        if collecting_install_requires && (line.starts_with(' ') || line.starts_with('\t')) {
            if !trimmed.is_empty() {
                pending_entries.push(trimmed.to_owned());
            }
            continue;
        }

        if collecting_install_requires {
            flush_install_requires(
                &mut output,
                &mut pending_entries,
                &mut collecting_install_requires,
                &mut removed,
                &target,
            );
        }

        output.push(line.to_owned());
    }

    flush_install_requires(
        &mut output,
        &mut pending_entries,
        &mut collecting_install_requires,
        &mut removed,
        &target,
    );

    if !removed {
        return remove_dependency_line(path, distribution, None);
    }

    let mut updated = output.join("\n");
    if contents.ends_with('\n') {
        updated.push('\n');
    }
    atomic_write(path, &updated)?;
    Ok(format!("removed `{distribution}` from {rel}"))
}

fn flush_install_requires(
    output: &mut Vec<String>,
    pending_entries: &mut Vec<String>,
    collecting: &mut bool,
    removed: &mut bool,
    target: &str,
) {
    if !*collecting {
        return;
    }
    *collecting = false;
    if pending_entries.is_empty() {
        return;
    }

    let kept: Vec<String> = pending_entries
        .iter()
        .filter(|entry| {
            let keep = !requirement_matches_distribution(entry, target);
            if !keep {
                *removed = true;
            }
            keep
        })
        .cloned()
        .collect();
    pending_entries.clear();

    if kept.is_empty() {
        return;
    }
    if kept.len() == 1 {
        output.push(format!("install_requires = {}", kept[0]));
    } else {
        output.push("install_requires =".to_owned());
        for entry in kept {
            output.push(format!("    {entry}"));
        }
    }
}

fn requirement_matches_distribution(entry: &str, distribution: &str) -> bool {
    let trimmed = entry.split('#').next().unwrap_or(entry).trim();
    if trimmed.is_empty() {
        return false;
    }
    let name = trimmed.split(['[', ';', ' ']).next().unwrap_or(trimmed);
    let name = name.split(['=', '<', '>', '!', '~']).next().unwrap_or(name);
    normalize_distribution_name(name) == distribution
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn removes_exact_distribution_only() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("setup.cfg");
        std::fs::write(
            &path,
            "[options]\ninstall_requires =\n    bar>=1.0\n    foobar>=2.0\n",
        )
        .expect("write");
        remove_dependency(&path, "bar").expect("remove");
        let updated = std::fs::read_to_string(&path).expect("read");
        assert!(updated.contains("foobar>=2.0"));
        assert!(!updated.lines().any(|line| line.trim().starts_with("bar>=")));
    }

    #[test]
    fn does_not_remove_prefix_match() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("setup.cfg");
        std::fs::write(
            &path,
            "[options]\ninstall_requires = requests, requests-oauthlib\n",
        )
        .expect("write");
        remove_dependency(&path, "requests").expect("remove");
        let updated = std::fs::read_to_string(&path).expect("read");
        assert!(!updated.contains("requests,"));
        assert!(updated.contains("requests-oauthlib"));
    }
}
