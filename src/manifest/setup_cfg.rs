//! `setup.cfg` manifest extraction.

use std::path::Path;

use super::error::ManifestError;
use super::types::{DeclaredDependency, DependencyContext, ProjectMetadata};
use super::util::{DependencyPush, push_dependency, read_to_string, relative_path};
use super::warnings::ManifestWarning;

/// Partial extraction result from `setup.cfg`.
#[derive(Debug, Default)]
pub struct SetupCfgExtraction {
    /// Project metadata.
    pub metadata: ProjectMetadata,
    /// Declared dependencies.
    pub dependencies: Vec<DeclaredDependency>,
    /// Non-fatal warnings.
    pub warnings: Vec<ManifestWarning>,
}

/// Extract manifest data from `setup.cfg`.
pub fn extract_setup_cfg(root: &Path, path: &Path) -> Result<SetupCfgExtraction, ManifestError> {
    let contents = read_to_string(path)?;
    let rel = relative_path(root, path);
    let mut result = SetupCfgExtraction::default();

    let sections = parse_ini_sections(&contents);
    if let Some(metadata) = sections.get("metadata") {
        result.metadata.name = metadata.get("name").cloned();
        result.metadata.version = metadata.get("version").cloned();
    }

    if let Some(options) = sections.get("options")
        && let Some(requires) = options.get("install_requires")
    {
        for (index, raw) in split_requirement_lines(requires).enumerate() {
            push_dependency(DependencyPush {
                dependencies: &mut result.dependencies,
                warnings: &mut result.warnings,
                raw,
                context: DependencyContext::Runtime,
                file: &rel,
                label: format!("options.install_requires[{index}]"),
                line: None,
            });
        }
    }

    if let Some(extras) = sections.get("options.extras_require") {
        for (extra, requires) in extras {
            if extra.is_empty() {
                continue;
            }
            for (index, raw) in split_requirement_lines(requires).enumerate() {
                push_dependency(DependencyPush {
                    dependencies: &mut result.dependencies,
                    warnings: &mut result.warnings,
                    raw,
                    context: DependencyContext::SetupExtra(extra.clone()),
                    file: &rel,
                    label: format!("options.extras_require.{extra}[{index}]"),
                    line: None,
                });
            }
        }
    }

    Ok(result)
}

fn split_requirement_lines(value: &str) -> impl Iterator<Item = &str> {
    value.lines().map(str::trim).filter(|line| !line.is_empty())
}

fn parse_ini_sections(
    contents: &str,
) -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>> {
    use std::collections::BTreeMap;

    let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    // Lines accumulate into a local section map so the hot loop never
    // re-resolves the current section name; the map is merged into
    // `sections` when a new header starts or input ends.
    let mut current = String::from("default");
    let mut declared = false;
    let mut section: BTreeMap<String, String> = BTreeMap::new();
    let mut last_key: Option<String> = None;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if declared || !section.is_empty() {
                flush_section(&mut sections, &current, std::mem::take(&mut section));
            }
            trimmed[1..trimmed.len() - 1]
                .trim()
                .clone_into(&mut current);
            declared = true;
            last_key = None;
            continue;
        }

        if line.starts_with([' ', '\t']) {
            if let Some(ref key) = last_key {
                append_value(&mut section, key, trimmed, true);
            }
        } else if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim().to_ascii_lowercase();
            append_value(&mut section, &key, value.trim(), false);
            last_key = Some(key);
        }
    }

    if declared || !section.is_empty() {
        flush_section(&mut sections, &current, section);
    }
    sections
}

/// Append `value` to `key` in `section`, newline-joining duplicates.
/// Continuation lines (`continuation`) append even when empty-valued
/// entries exist, matching key=value duplicate handling otherwise.
fn append_value(
    section: &mut std::collections::BTreeMap<String, String>,
    key: &str,
    value: &str,
    continuation: bool,
) {
    if let Some(existing) = section.get_mut(key) {
        if continuation || !value.is_empty() {
            if !existing.is_empty() {
                existing.push('\n');
            }
            existing.push_str(value);
        }
    } else {
        section.insert(key.to_owned(), value.to_owned());
    }
}

fn flush_section(
    sections: &mut std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    name: &str,
    section: std::collections::BTreeMap<String, String>,
) {
    let Some(existing) = sections.get_mut(name) else {
        sections.insert(name.to_owned(), section);
        return;
    };
    for (key, value) in section {
        if let Some(slot) = existing.get_mut(&key) {
            if !value.is_empty() {
                if !slot.is_empty() {
                    slot.push('\n');
                }
                slot.push_str(&value);
            }
        } else {
            existing.insert(key, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiline_install_requires() {
        let contents = r"[options]
install_requires =
    requests
    flask>=1.0
";
        let sections = parse_ini_sections(contents);
        let options = sections.get("options").expect("options section");
        let requires = options.get("install_requires").expect("install_requires");
        assert!(requires.contains("requests"), "requires={requires:?}");
        assert!(requires.contains("flask>=1.0"), "requires={requires:?}");
    }
}
