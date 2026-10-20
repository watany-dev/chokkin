//! TOML parsing for chokkin configuration files.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::manifest::{UvSourceKind, uv_source_entries};
use crate::rules::RuleId;

use super::defaults::PartialConfig;
use super::error::ConfigError;
use super::types::{UvWorkspaceHint, is_absolute_path_str};

#[derive(Deserialize)]
struct PyProject {
    #[serde(default)]
    tool: PyProjectTool,
}

#[derive(Default, Deserialize)]
struct PyProjectTool {
    chokkin: Option<PartialConfig>,
    uv: Option<UvTool>,
}

#[derive(Deserialize)]
struct UvTool {
    workspace: Option<UvWorkspace>,
    #[serde(default)]
    sources: toml::Table,
}

#[derive(Deserialize)]
struct UvWorkspace {
    members: Option<UvMembers>,
    exclude: Option<UvMembers>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum UvMembers {
    One(String),
    Many(Vec<String>),
}

impl UvMembers {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(member) => vec![member],
            Self::Many(members) => members,
        }
    }
}

/// Read and parse a standalone `.chokkin.toml` or `chokkin.toml` file.
pub(super) fn parse_standalone_config(path: &Path) -> Result<PartialConfig, ConfigError> {
    let partial = parse_toml::<PartialConfig>(path)?;
    validate(path, &partial)?;
    Ok(partial)
}

/// `[tool.chokkin]` and the uv hints that shape workspace members.
pub(super) struct PyProjectConfig {
    pub partial: PartialConfig,
    pub uv_workspace: Option<UvWorkspaceHint>,
    /// `[tool.uv.sources]` path entries as written, keyed by distribution name;
    /// a marker-scoped array contributes each `path` in order.
    pub uv_path_sources: BTreeMap<String, Vec<String>>,
}

/// Read `[tool.chokkin]` from `pyproject.toml` with its `[tool.uv]` member hints.
pub(super) fn parse_pyproject_config(path: &Path) -> Result<PyProjectConfig, ConfigError> {
    let PyProject {
        tool: PyProjectTool { chokkin, uv },
    } = parse_toml::<PyProject>(path)?;

    let partial = chokkin.unwrap_or_default();
    validate(path, &partial)?;

    let (workspace, sources) = uv.map_or_else(Default::default, |uv| (uv.workspace, uv.sources));
    let mut uv_path_sources: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, kind) in uv_source_entries(&sources) {
        if let UvSourceKind::Path(path) = kind {
            uv_path_sources
                .entry(name.to_owned())
                .or_default()
                .push(path);
        }
    }
    let uv_workspace = workspace.and_then(|workspace| {
        let members = workspace.members.map(UvMembers::into_vec)?;
        (!members.is_empty()).then(|| UvWorkspaceHint {
            members,
            exclude: workspace
                .exclude
                .map(UvMembers::into_vec)
                .unwrap_or_default(),
        })
    });
    Ok(PyProjectConfig {
        partial,
        uv_workspace,
        uv_path_sources,
    })
}

fn parse_toml<T: DeserializeOwned>(path: &Path) -> Result<T, ConfigError> {
    let contents = fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&contents).map_err(|error| ConfigError::InvalidToml {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

/// Checks serde cannot express: root-relative paths, known rule codes and
/// loader function names.
fn validate(path: &Path, config: &PartialConfig) -> Result<(), ConfigError> {
    ensure_relative_all(
        path,
        "entry",
        config
            .entry
            .iter()
            .flatten()
            .map(|entry| entry.path.as_str()),
    )?;
    ensure_relative_all(
        path,
        "project",
        config.project.iter().flatten().map(String::as_str),
    )?;
    ensure_relative_all(
        path,
        "exclude",
        config.exclude.iter().flatten().map(String::as_str),
    )?;
    ensure_relative_all(
        path,
        "vendored",
        config.vendored.iter().flatten().map(String::as_str),
    )?;
    ensure_function_names(path, config.module_loaders.iter().flatten())?;
    ensure_known_rules(
        path,
        "ignore",
        config.ignore.iter().flat_map(BTreeMap::keys),
    )?;
    ensure_known_rules(
        path,
        "severity",
        config.severity.iter().flat_map(BTreeMap::keys),
    )?;
    validate_workspaces(path, config)
}

fn validate_workspaces(path: &Path, config: &PartialConfig) -> Result<(), ConfigError> {
    for (id, workspace) in config.workspaces.iter().flatten() {
        let field = format!("workspaces.{id}");
        if workspace.path.is_empty() {
            return Err(validation_error(
                path,
                format!("{field}.path"),
                "workspace path must not be empty",
            ));
        }
        ensure_relative(path, format!("{field}.path"), &workspace.path)?;
        ensure_relative_all(
            path,
            &format!("{field}.entry"),
            workspace
                .entry
                .iter()
                .flatten()
                .map(|entry| entry.path.as_str()),
        )?;
        ensure_relative_all(
            path,
            &format!("{field}.project"),
            workspace.project.iter().flatten().map(String::as_str),
        )?;
    }
    Ok(())
}

fn ensure_relative_all<'a>(
    path: &Path,
    field: &str,
    values: impl Iterator<Item = &'a str>,
) -> Result<(), ConfigError> {
    for (index, value) in values.enumerate() {
        ensure_relative(path, format!("{field}[{index}]"), value)?;
    }
    Ok(())
}

fn ensure_relative(path: &Path, field: String, value: &str) -> Result<(), ConfigError> {
    if is_absolute_path_str(value) {
        return Err(validation_error(
            path,
            field,
            "path must be relative to the project root",
        ));
    }
    if escapes_root(value) {
        return Err(validation_error(
            path,
            field,
            "path resolves outside the project root",
        ));
    }
    Ok(())
}

/// Whether `..` climbs above the start of `value` (#981): such a path or glob
/// can never name a file chokkin walks, and dropping it silently hid the typo.
/// Glob segments count as one directory each.
fn escapes_root(value: &str) -> bool {
    let mut depth = 0usize;
    for part in value.split(['/', '\\']) {
        match part {
            "" | "." => {},
            ".." => match depth.checked_sub(1) {
                Some(up) => depth = up,
                None => return true,
            },
            _ => depth += 1,
        }
    }
    false
}

fn ensure_known_rules<'a>(
    path: &Path,
    field: &str,
    codes: impl Iterator<Item = &'a String>,
) -> Result<(), ConfigError> {
    for code in codes {
        if RuleId::parse_code(code).is_none_or(|rule| rule.as_code() != code.as_str()) {
            return Err(validation_error(
                path,
                format!("{field}.{code}"),
                "unknown rule code",
            ));
        }
    }
    Ok(())
}

/// A `module_loaders` entry is matched against a callee's last name, so a
/// qualified `pkg.LazyFeature` would never match (#493).
fn ensure_function_names<'a>(
    path: &Path,
    names: impl Iterator<Item = &'a String>,
) -> Result<(), ConfigError> {
    for name in names {
        let identifier = name
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_alphanumeric() || c == '_');
        if !identifier {
            return Err(validation_error(
                path,
                format!("module_loaders.{name}"),
                "must be a function name without its module (`LazyFeature`, not `pkg.LazyFeature`)",
            ));
        }
    }
    Ok(())
}

fn validation_error(path: &Path, field: String, message: &str) -> ConfigError {
    ConfigError::Validation {
        path: path.to_path_buf(),
        field,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_sources_keep_written_names_and_marker_array_order() {
        let contents = concat!(
            "[tool.uv.sources]\n",
            "My_Lib = [\n",
            "  { path = \"lib\", marker = \"sys_platform == 'linux'\" },\n",
            "  { index = \"cpu\" },\n",
            "  { path = \"lib-win\", editable = true },\n",
            "]\n",
            "billing = { workspace = true }\n",
            "bad = \"lib\"\n",
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pyproject.toml");
        fs::write(&path, contents).expect("write pyproject");
        let parsed = parse_pyproject_config(&path).expect("parse");
        assert_eq!(
            parsed.uv_path_sources,
            BTreeMap::from([(
                "My_Lib".to_owned(),
                vec!["lib".to_owned(), "lib-win".to_owned()]
            )])
        );
    }

    #[test]
    fn escapes_root_folds_parent_segments() {
        for inside in ["src/**/*.py", "a/../x", "./a/./b", "a\\..\\b", "**/.."] {
            assert!(!escapes_root(inside), "{inside}");
        }
        for outside in ["..", "../x", "a/../../x", "./../x", "a\\..\\..\\x"] {
            assert!(escapes_root(outside), "{outside}");
        }
    }

    #[test]
    fn rule_code_keys_are_case_sensitive() {
        let path = Path::new(".chokkin.toml");
        let config: PartialConfig = toml::from_str("[ignore]\nchk001 = []").expect("valid config");
        let err = validate(path, &config).unwrap_err();
        assert!(
            matches!(err, ConfigError::Validation { ref field, .. } if field == "ignore.chk001")
        );
        let config: PartialConfig =
            toml::from_str("[severity]\nchk001 = \"off\"").expect("valid config");
        let err = validate(path, &config).unwrap_err();
        assert!(
            matches!(err, ConfigError::Validation { ref field, .. } if field == "severity.chk001")
        );
    }

    #[test]
    fn module_loaders_are_function_names() {
        let path = Path::new(".chokkin.toml");
        let config: PartialConfig =
            toml::from_str("module_loaders = [\"LazyFeature\", \"_load\"]").expect("valid");
        validate(path, &config).expect("function names");
        for name in ["pkg.LazyFeature", "", "1st", "load()"] {
            let config: PartialConfig =
                toml::from_str(&format!("module_loaders = [{name:?}]")).expect("valid toml");
            let err = validate(path, &config).unwrap_err();
            assert!(
                matches!(err, ConfigError::Validation { ref field, .. } if *field == format!("module_loaders.{name}")),
                "{name}"
            );
        }
    }

    #[test]
    fn uv_workspace_hint_reads_exclude() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("pyproject.toml");
        fs::write(
            &path,
            "[tool.uv.workspace]\nmembers = [\"packages/*\"]\nexclude = [\"packages/legacy\"]\n",
        )
        .expect("write");
        let hint = parse_pyproject_config(&path)
            .expect("parse")
            .uv_workspace
            .expect("hint");
        assert_eq!(hint.members, ["packages/*"]);
        assert_eq!(hint.exclude, ["packages/legacy"]);
    }
}
