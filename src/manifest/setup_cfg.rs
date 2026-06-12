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
        for (index, raw) in split_requirement_lines(requires).iter().enumerate() {
            push_dependency(DependencyPush {
                dependencies: &mut result.dependencies,
                warnings: &mut result.warnings,
                raw,
                context: DependencyContext::Runtime,
                file: &rel,
                label: &format!("options.install_requires[{index}]"),
                line: None,
            });
        }
    }

    if let Some(extras) = sections.get("options.extras_require") {
        for (extra, requires) in extras {
            if extra.is_empty() {
                continue;
            }
            for (index, raw) in split_requirement_lines(requires).iter().enumerate() {
                push_dependency(DependencyPush {
                    dependencies: &mut result.dependencies,
                    warnings: &mut result.warnings,
                    raw,
                    context: DependencyContext::SetupExtra(extra.clone()),
                    file: &rel,
                    label: &format!("options.extras_require.{extra}[{index}]"),
                    line: None,
                });
            }
        }
    }

    Ok(result)
}

fn split_requirement_lines(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn parse_ini_sections(
    contents: &str,
) -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>> {
    use std::collections::BTreeMap;

    let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut current = String::from("default");

    let mut last_key: Option<String> = None;

    for line in contents.lines() {
        if line.trim().is_empty() || line.trim().starts_with('#') || line.trim().starts_with(';') {
            continue;
        }

        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            trimmed[1..trimmed.len() - 1]
                .trim()
                .clone_into(&mut current);
            sections.entry(current.clone()).or_default();
            last_key = None;
            continue;
        }

        let section = sections.entry(current.clone()).or_default();
        if line.starts_with([' ', '\t']) {
            if let Some(ref key) = last_key {
                section
                    .entry(key.clone())
                    .and_modify(|existing| {
                        if !existing.is_empty() {
                            existing.push('\n');
                        }
                        existing.push_str(trimmed);
                    })
                    .or_insert_with(|| trimmed.to_owned());
            }
        } else if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            section
                .entry(key.clone())
                .and_modify(|existing| {
                    if !value.is_empty() {
                        if !existing.is_empty() {
                            existing.push('\n');
                        }
                        existing.push_str(value);
                    }
                })
                .or_insert_with(|| value.to_owned());
            last_key = Some(key);
        }
    }

    sections
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
