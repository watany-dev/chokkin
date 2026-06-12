//! TOML parsing for yokei configuration files.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use toml::Value;

use super::defaults::{PartialConfig, PartialDependencyGroups};
use super::error::ConfigError;
use super::types::{
    Confidence, EntrySpec, PluginId, ProjectMode, TargetVersion, UvWorkspaceHint,
    WorkspaceOverride, is_absolute_path_str,
};

const WORKSPACE_KEYS: &[&str] = &["path", "entry", "project", "mode"];

const TOP_LEVEL_KEYS: &[&str] = &[
    "entry",
    "project",
    "mode",
    "production",
    "target_version",
    "respect_gitignore",
    "confidence",
    "exclude",
    "dependencies",
    "package_module_map",
    "binary_map",
    "plugins",
    "ignore",
    "workspaces",
];

/// Read and parse a standalone `.yokei.toml` or `yokei.toml` file.
pub fn parse_standalone_config(path: &Path) -> Result<PartialConfig, ConfigError> {
    let contents = read_to_string(path)?;
    let table = parse_table(path, &contents)?;
    partial_from_table(path, &table)
}

/// Read `[tool.yokei]` from `pyproject.toml` and optional `[tool.uv.workspace]` hint.
pub fn parse_pyproject_config(
    path: &Path,
) -> Result<(PartialConfig, Option<UvWorkspaceHint>), ConfigError> {
    let contents = read_to_string(path)?;
    let table = parse_table(path, &contents)?;
    let uv_workspace = parse_uv_workspace(path, &table)?;

    let Some(tool_table) = table.get("tool").and_then(Value::as_table) else {
        return Ok((PartialConfig::default(), uv_workspace));
    };

    let Some(yokei_value) = tool_table.get("yokei") else {
        return Ok((PartialConfig::default(), uv_workspace));
    };

    let yokei_table = value_as_table(path, yokei_value, "tool.yokei")?;
    let partial = partial_from_table(path, yokei_table)?;
    Ok((partial, uv_workspace))
}

fn read_to_string(path: &Path) -> Result<String, ConfigError> {
    fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn parse_table(path: &Path, contents: &str) -> Result<toml::Table, ConfigError> {
    toml::from_str(contents).map_err(|error| ConfigError::InvalidToml {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn parse_uv_workspace(
    path: &Path,
    table: &toml::Table,
) -> Result<Option<UvWorkspaceHint>, ConfigError> {
    let Some(tool_table) = table.get("tool").and_then(Value::as_table) else {
        return Ok(None);
    };
    let Some(uv_table) = tool_table.get("uv").and_then(Value::as_table) else {
        return Ok(None);
    };
    let Some(workspace_table) = uv_table.get("workspace").and_then(Value::as_table) else {
        return Ok(None);
    };
    let Some(members_value) = workspace_table.get("members") else {
        return Ok(None);
    };

    let members = match members_value {
        Value::Array(items) => items
            .iter()
            .map(|item| value_as_string(path, item, "tool.uv.workspace.members"))
            .collect::<Result<Vec<_>, _>>()?,
        Value::String(member) => vec![member.clone()],
        _ => {
            return Err(ConfigError::Validation {
                path: path.to_path_buf(),
                field: "tool.uv.workspace.members".to_owned(),
                message: "expected string or array of strings".to_owned(),
            });
        },
    };

    if members.is_empty() {
        return Ok(None);
    }

    Ok(Some(UvWorkspaceHint { members }))
}

fn partial_from_table(path: &Path, table: &toml::Table) -> Result<PartialConfig, ConfigError> {
    reject_unknown_top_level_keys(path, table)?;

    Ok(PartialConfig {
        entry: parse_optional_entry_list(path, table.get("entry"), "entry")?,
        project: parse_optional_path_list(path, table.get("project"), "project")?,
        mode: parse_optional_mode(path, table.get("mode"))?,
        production: parse_optional_bool(path, table.get("production"), "production")?,
        target_version: parse_optional_target_version(path, table.get("target_version"))?,
        respect_gitignore: parse_optional_bool(
            path,
            table.get("respect_gitignore"),
            "respect_gitignore",
        )?,
        confidence: parse_optional_confidence(path, table.get("confidence"))?,
        exclude: parse_optional_path_list(path, table.get("exclude"), "exclude")?,
        dependencies: parse_optional_dependencies(path, table.get("dependencies"))?,
        package_module_map: parse_optional_string_list_map(
            path,
            table.get("package_module_map"),
            "package_module_map",
        )?,
        binary_map: parse_optional_string_map(path, table.get("binary_map"), "binary_map")?,
        plugins: parse_optional_plugins(path, table.get("plugins"))?,
        ignore: parse_optional_ignore(path, table.get("ignore"))?,
        workspaces: parse_optional_workspaces(path, table.get("workspaces"))?,
    })
}

fn reject_unknown_top_level_keys(path: &Path, table: &toml::Table) -> Result<(), ConfigError> {
    for key in table.keys() {
        if !TOP_LEVEL_KEYS.contains(&key.as_str()) {
            return Err(ConfigError::UnknownKey {
                path: path.to_path_buf(),
                key: key.clone(),
            });
        }
    }
    Ok(())
}

fn parse_optional_entry_list(
    path: &Path,
    value: Option<&Value>,
    field: &'static str,
) -> Result<Option<Vec<EntrySpec>>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let items = value_as_string_array(path, value, field)?;
    let mut entries = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        let field_name = format!("{field}[{index}]");
        let parsed = EntrySpec::parse(&item).map_err(|message| ConfigError::Validation {
            path: path.to_path_buf(),
            field: field_name.clone(),
            message: message.to_owned(),
        })?;
        ensure_relative_path(path, &field_name, &parsed.path)?;
        entries.push(parsed);
    }
    Ok(Some(entries))
}

/// Parse an optional array of root-relative path or glob strings.
fn parse_optional_path_list(
    path: &Path,
    value: Option<&Value>,
    field: &'static str,
) -> Result<Option<Vec<String>>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let items = value_as_string_array(path, value, field)?;
    for (index, item) in items.iter().enumerate() {
        ensure_relative_path(path, &format!("{field}[{index}]"), item)?;
    }
    Ok(Some(items))
}

fn ensure_relative_path(path: &Path, field: &str, value: &str) -> Result<(), ConfigError> {
    if is_absolute_path_str(value) {
        return Err(ConfigError::Validation {
            path: path.to_path_buf(),
            field: field.to_owned(),
            message: "path must be relative to the project root".to_owned(),
        });
    }
    Ok(())
}

fn parse_optional_string_list(
    path: &Path,
    value: Option<&Value>,
    field: &'static str,
) -> Result<Option<Vec<String>>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    Ok(Some(value_as_string_array(path, value, field)?))
}

fn parse_optional_mode(
    path: &Path,
    value: Option<&Value>,
) -> Result<Option<ProjectMode>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let raw = value_as_string(path, value, "mode")?;
    let mode = ProjectMode::parse(&raw).ok_or_else(|| ConfigError::Validation {
        path: path.to_path_buf(),
        field: "mode".to_owned(),
        message: format!("expected auto, app, or library; got {raw}"),
    })?;
    Ok(Some(mode))
}

fn parse_optional_confidence(
    path: &Path,
    value: Option<&Value>,
) -> Result<Option<Confidence>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let raw = value_as_string(path, value, "confidence")?;
    let confidence = Confidence::parse(&raw).ok_or_else(|| ConfigError::Validation {
        path: path.to_path_buf(),
        field: "confidence".to_owned(),
        message: format!("expected certain, likely, or maybe; got {raw}"),
    })?;
    Ok(Some(confidence))
}

fn parse_optional_target_version(
    path: &Path,
    value: Option<&Value>,
) -> Result<Option<TargetVersion>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let raw = value_as_string(path, value, "target_version")?;
    let target = TargetVersion::parse(&raw).ok_or_else(|| ConfigError::Validation {
        path: path.to_path_buf(),
        field: "target_version".to_owned(),
        message: format!("expected py3XX form; got {raw}"),
    })?;
    Ok(Some(target))
}

fn parse_optional_bool(
    path: &Path,
    value: Option<&Value>,
    field: &'static str,
) -> Result<Option<bool>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| ConfigError::Validation {
            path: path.to_path_buf(),
            field: field.to_owned(),
            message: "expected boolean".to_owned(),
        })
}

const DEPENDENCY_GROUP_KEYS: &[&str] = &["dev_groups", "runtime_groups", "type_groups"];

fn parse_optional_dependencies(
    path: &Path,
    value: Option<&Value>,
) -> Result<Option<PartialDependencyGroups>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let table = value_as_table(path, value, "dependencies")?;
    for key in table.keys() {
        if !DEPENDENCY_GROUP_KEYS.contains(&key.as_str()) {
            return Err(ConfigError::UnknownKey {
                path: path.to_path_buf(),
                key: format!("dependencies.{key}"),
            });
        }
    }
    Ok(Some(PartialDependencyGroups {
        dev_groups: parse_optional_string_list(
            path,
            table.get("dev_groups"),
            "dependencies.dev_groups",
        )?,
        runtime_groups: parse_optional_string_list(
            path,
            table.get("runtime_groups"),
            "dependencies.runtime_groups",
        )?,
        type_groups: parse_optional_string_list(
            path,
            table.get("type_groups"),
            "dependencies.type_groups",
        )?,
    }))
}

fn parse_optional_string_list_map(
    path: &Path,
    value: Option<&Value>,
    field: &'static str,
) -> Result<Option<BTreeMap<String, Vec<String>>>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let table = value_as_table(path, value, field)?;
    let mut map = BTreeMap::new();
    for (key, item) in table {
        let values = value_as_string_array(path, item, field)?;
        map.insert(key.clone(), values);
    }
    Ok(Some(map))
}

fn parse_optional_string_map(
    path: &Path,
    value: Option<&Value>,
    field: &'static str,
) -> Result<Option<BTreeMap<String, String>>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let table = value_as_table(path, value, field)?;
    let mut map = BTreeMap::new();
    for (key, item) in table {
        map.insert(key.clone(), value_as_string(path, item, field)?);
    }
    Ok(Some(map))
}

fn parse_optional_plugins(
    path: &Path,
    value: Option<&Value>,
) -> Result<Option<BTreeMap<PluginId, bool>>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let table = value_as_table(path, value, "plugins")?;
    let mut map = BTreeMap::new();
    for (key, item) in table {
        let Some(plugin) = PluginId::from_key(key) else {
            return Err(ConfigError::UnknownKey {
                path: path.to_path_buf(),
                key: format!("plugins.{key}"),
            });
        };
        let enabled = item.as_bool().ok_or_else(|| ConfigError::Validation {
            path: path.to_path_buf(),
            field: format!("plugins.{key}"),
            message: "expected boolean".to_owned(),
        })?;
        map.insert(plugin, enabled);
    }
    Ok(Some(map))
}

fn parse_optional_ignore(
    path: &Path,
    value: Option<&Value>,
) -> Result<Option<BTreeMap<String, Vec<String>>>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let table = value_as_table(path, value, "ignore")?;
    let mut map = BTreeMap::new();
    for (key, item) in table {
        if !is_valid_ignore_rule(key) {
            return Err(ConfigError::Validation {
                path: path.to_path_buf(),
                field: format!("ignore.{key}"),
                message: "unknown rule code".to_owned(),
            });
        }
        map.insert(key.clone(), value_as_string_array(path, item, "ignore")?);
    }
    Ok(Some(map))
}

fn parse_optional_workspaces(
    path: &Path,
    value: Option<&Value>,
) -> Result<Option<BTreeMap<String, WorkspaceOverride>>, ConfigError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let table = value_as_table(path, value, "workspaces")?;
    let mut map = BTreeMap::new();
    for (id, item) in table {
        let workspace_table = value_as_table(path, item, "workspaces")?;
        for key in workspace_table.keys() {
            if !WORKSPACE_KEYS.contains(&key.as_str()) {
                return Err(ConfigError::UnknownKey {
                    path: path.to_path_buf(),
                    key: format!("workspaces.{id}.{key}"),
                });
            }
        }

        let path_value = workspace_table
            .get("path")
            .ok_or_else(|| ConfigError::Validation {
                path: path.to_path_buf(),
                field: format!("workspaces.{id}.path"),
                message: "required key path is missing".to_owned(),
            })?;
        let member_path = value_as_string(path, path_value, "workspaces.path")?;
        if member_path.is_empty() {
            return Err(ConfigError::Validation {
                path: path.to_path_buf(),
                field: format!("workspaces.{id}.path"),
                message: "workspace path must not be empty".to_owned(),
            });
        }
        ensure_relative_path(path, &format!("workspaces.{id}.path"), &member_path)?;

        let entry =
            parse_optional_entry_list(path, workspace_table.get("entry"), "workspaces.entry")?;
        let project =
            parse_optional_path_list(path, workspace_table.get("project"), "workspaces.project")?;
        let mode = parse_optional_mode(path, workspace_table.get("mode"))?;

        map.insert(
            id.clone(),
            WorkspaceOverride {
                path: member_path,
                entry,
                project,
                mode,
            },
        );
    }
    Ok(Some(map))
}

fn is_valid_ignore_rule(code: &str) -> bool {
    matches!(
        code,
        "YOK001"
            | "YOK002"
            | "YOK003"
            | "YOK004"
            | "YOK005"
            | "YOK006"
            | "YOK007"
            | "YOK008"
            | "YOK009"
            | "YOK010"
    )
}

fn value_as_table<'a>(
    path: &Path,
    value: &'a Value,
    field: &'static str,
) -> Result<&'a toml::Table, ConfigError> {
    value.as_table().ok_or_else(|| ConfigError::Validation {
        path: path.to_path_buf(),
        field: field.to_owned(),
        message: "expected table".to_owned(),
    })
}

fn value_as_string(path: &Path, value: &Value, field: &str) -> Result<String, ConfigError> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| ConfigError::Validation {
            path: path.to_path_buf(),
            field: field.to_owned(),
            message: "expected string".to_owned(),
        })
}

fn value_as_string_array(
    path: &Path,
    value: &Value,
    field: &str,
) -> Result<Vec<String>, ConfigError> {
    let array = value.as_array().ok_or_else(|| ConfigError::Validation {
        path: path.to_path_buf(),
        field: field.to_owned(),
        message: "expected array".to_owned(),
    })?;
    array
        .iter()
        .map(|item| value_as_string(path, item, field))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ignore_rules_accept_yok001_through_yok010() {
        for code in 1..=10 {
            let rule = format!("YOK{code:03}");
            assert!(is_valid_ignore_rule(&rule), "expected {rule} to be valid");
        }
        assert!(!is_valid_ignore_rule("YOK000"));
        assert!(!is_valid_ignore_rule("YOK011"));
        assert!(!is_valid_ignore_rule("YOK099"));
    }
}
