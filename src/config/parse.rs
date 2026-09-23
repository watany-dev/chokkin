//! TOML parsing for chokkin configuration files.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde::de::DeserializeOwned;

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
}

#[derive(Deserialize)]
struct UvWorkspace {
    members: Option<UvMembers>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum UvMembers {
    One(String),
    Many(Vec<String>),
}

/// Read and parse a standalone `.chokkin.toml` or `chokkin.toml` file.
pub fn parse_standalone_config(path: &Path) -> Result<PartialConfig, ConfigError> {
    let partial = parse_toml::<PartialConfig>(path)?;
    validate(path, &partial)?;
    Ok(partial)
}

/// Read `[tool.chokkin]` from `pyproject.toml` and optional `[tool.uv.workspace]` hint.
pub fn parse_pyproject_config(
    path: &Path,
) -> Result<(PartialConfig, Option<UvWorkspaceHint>), ConfigError> {
    let PyProject {
        tool: PyProjectTool { chokkin, uv },
    } = parse_toml::<PyProject>(path)?;

    let partial = chokkin.unwrap_or_default();
    validate(path, &partial)?;

    let uv_workspace = uv
        .and_then(|uv| uv.workspace)
        .and_then(|workspace| workspace.members)
        .map(|members| match members {
            UvMembers::One(member) => vec![member],
            UvMembers::Many(members) => members,
        })
        .filter(|members| !members.is_empty())
        .map(|members| UvWorkspaceHint { members });
    Ok((partial, uv_workspace))
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

/// Checks serde cannot express: root-relative paths and known rule codes.
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
    Ok(())
}

fn ensure_known_rules<'a>(
    path: &Path,
    field: &str,
    codes: impl Iterator<Item = &'a String>,
) -> Result<(), ConfigError> {
    for code in codes {
        if !is_valid_ignore_rule(code) {
            return Err(validation_error(
                path,
                format!("{field}.{code}"),
                "unknown rule code",
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

fn is_valid_ignore_rule(code: &str) -> bool {
    matches!(
        code,
        "CHK001"
            | "CHK002"
            | "CHK003"
            | "CHK004"
            | "CHK005"
            | "CHK006"
            | "CHK007"
            | "CHK008"
            | "CHK009"
            | "CHK010"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ignore_rules_accept_chk001_through_chk010() {
        for code in 1..=10 {
            let rule = format!("CHK{code:03}");
            assert!(is_valid_ignore_rule(&rule), "expected {rule} to be valid");
        }
        assert!(!is_valid_ignore_rule("CHK000"));
        assert!(!is_valid_ignore_rule("CHK011"));
        assert!(!is_valid_ignore_rule("CHK099"));
    }
}
