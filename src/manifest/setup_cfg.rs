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
    let mut current = String::from("default");
    let mut last_key: Option<String> = None;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            trimmed[1..trimmed.len() - 1]
                .trim()
                .clone_into(&mut current);
            sections.entry(current.clone()).or_default();
            last_key = None;
            continue;
        }

        if line.starts_with([' ', '\t']) {
            if let Some(ref key) = last_key {
                append_value(
                    sections.entry(current.clone()).or_default(),
                    key,
                    trimmed,
                    true,
                );
            }
        } else if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim().to_ascii_lowercase();
            append_value(
                sections.entry(current.clone()).or_default(),
                &key,
                value.trim(),
                false,
            );
            last_key = Some(key);
        }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

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

    #[test]
    fn merges_repeated_sections_and_keeps_empty_ones() {
        let contents =
            "[options]\ninstall_requires = a\n[empty]\n[options]\ninstall_requires =\n    b\n";
        let sections = parse_ini_sections(contents);
        assert_eq!(
            sections
                .get("options")
                .and_then(|o| o.get("install_requires"))
                .map(String::as_str),
            Some("a\nb")
        );
        assert!(sections.get("empty").is_some_and(BTreeMap::is_empty));
        assert!(!sections.contains_key("default"));
    }

    mod props {
        use std::fmt::Write as _;

        use super::*;
        use proptest::prelude::*;

        /// INI values that survive `key = value` rendering unchanged: no
        /// newlines, no comment leaders, and no surrounding whitespace.
        fn ini_value() -> impl Strategy<Value = String> {
            "[A-Za-z0-9><=~. _-]{0,30}".prop_map(|value| value.trim().to_owned())
        }

        proptest! {
            #[test]
            fn parse_ini_sections_never_panics(contents in "\\PC{0,400}") {
                let _ = parse_ini_sections(&contents);
            }

            #[test]
            fn parse_ini_sections_roundtrips_flat_keys(
                section in "[a-z][a-z0-9.]{0,15}",
                entries in prop::collection::btree_map("[a-z][a-z0-9_]{0,12}", ini_value(), 0..6),
            ) {
                let mut contents = format!("[{section}]\n");
                for (key, value) in &entries {
                    writeln!(contents, "{key} = {value}").expect("write to string");
                }

                let sections = parse_ini_sections(&contents);
                let parsed = sections.get(&section).expect("section must exist");
                prop_assert_eq!(parsed, &entries);
            }

            #[test]
            fn parse_ini_sections_joins_continuation_lines(
                values in prop::collection::vec("[a-z][a-z0-9>=.-]{0,15}", 1..6),
            ) {
                let mut contents = String::from("[options]\ninstall_requires =\n");
                for value in &values {
                    writeln!(contents, "    {value}").expect("write to string");
                }

                let sections = parse_ini_sections(&contents);
                let requires = sections
                    .get("options")
                    .and_then(|options| options.get("install_requires"))
                    .expect("install_requires must exist");
                prop_assert_eq!(split_requirement_lines(requires).collect::<Vec<_>>(), values);
            }

            #[test]
            fn split_requirement_lines_yields_trimmed_nonempty(value in "\\PC{0,200}") {
                for line in split_requirement_lines(&value) {
                    prop_assert!(!line.is_empty());
                    prop_assert_eq!(line.trim(), line);
                }
            }
        }
    }
}
