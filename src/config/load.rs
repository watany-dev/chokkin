//! Load and validate merged yokei configuration.

use std::path::Path;

use crate::discovery::ProjectRoot;

use super::defaults::merge_layers;
use super::error::ConfigError;
use super::parse::{parse_pyproject_config, parse_standalone_config};
use super::source::discover_config_files;
use super::types::{
    ConfigSources, LoadedConfig, RuntimeOverrides, YokeiConfig, is_absolute_path_str,
};

/// Load and merge yokei configuration for `root`.
///
/// Returns defaults when no config files exist. Never executes Python.
pub fn load_config(root: &ProjectRoot) -> Result<LoadedConfig, ConfigError> {
    let files = discover_config_files(&root.path);
    let mut layers = Vec::new();
    let mut sources = ConfigSources {
        used_defaults: true,
        dot_yokei_toml: files.dot_yokei_toml.clone(),
        yokei_toml: files.yokei_toml.clone(),
        pyproject_tool_yokei: false,
    };

    if let Some(path) = &files.dot_yokei_toml {
        let partial = parse_standalone_config(path)?;
        if partial.has_any_field() {
            layers.push(partial);
        }
    }

    if let Some(path) = &files.yokei_toml {
        let partial = parse_standalone_config(path)?;
        if partial.has_any_field() {
            layers.push(partial);
        }
    }

    let mut uv_workspace = None;
    if let Some(path) = &files.pyproject_toml {
        let (partial, uv_hint) = parse_pyproject_config(path)?;
        uv_workspace = uv_hint;
        if partial.has_any_field() {
            sources.pyproject_tool_yokei = true;
            layers.push(partial);
        }
    }

    let effective = merge_layers(&layers);
    validate_effective(&effective, &root.path)?;

    Ok(LoadedConfig {
        root: root.clone(),
        effective,
        sources,
        uv_workspace,
    })
}

/// Apply CLI/runtime overrides onto a loaded file configuration.
pub fn apply_overrides(config: &mut YokeiConfig, overrides: &RuntimeOverrides) {
    if let Some(production) = overrides.production {
        config.production = production;
    }
    if let Some(confidence) = overrides.confidence_floor {
        config.confidence = confidence;
    }
    let _ = overrides.strict;
}

fn validate_effective(config: &YokeiConfig, root: &Path) -> Result<(), ConfigError> {
    let path = config_validation_path(root);

    for (index, entry) in config.entry.iter().enumerate() {
        validate_relative_path(&path, &format!("entry[{index}]"), &entry.path)?;
        if let Some(symbol) = &entry.symbol
            && symbol.is_empty()
        {
            return Err(ConfigError::Validation {
                path,
                field: format!("entry[{index}]"),
                message: "symbol must not be empty".to_owned(),
            });
        }
    }

    for (index, pattern) in config.project.iter().enumerate() {
        validate_relative_path(&path, &format!("project[{index}]"), pattern)?;
    }

    for (index, pattern) in config.exclude.iter().enumerate() {
        validate_relative_path(&path, &format!("exclude[{index}]"), pattern)?;
    }

    for (id, workspace) in &config.workspaces {
        if workspace.path.is_empty() {
            return Err(ConfigError::Validation {
                path,
                field: format!("workspaces.{id}.path"),
                message: "workspace path must not be empty".to_owned(),
            });
        }
        validate_relative_path(&path, &format!("workspaces.{id}.path"), &workspace.path)?;
        if let Some(entries) = &workspace.entry {
            for (index, entry) in entries.iter().enumerate() {
                validate_relative_path(
                    &path,
                    &format!("workspaces.{id}.entry[{index}]"),
                    &entry.path,
                )?;
            }
        }
        if let Some(patterns) = &workspace.project {
            for (index, pattern) in patterns.iter().enumerate() {
                validate_relative_path(
                    &path,
                    &format!("workspaces.{id}.project[{index}]"),
                    pattern,
                )?;
            }
        }
    }

    Ok(())
}

fn validate_relative_path(path: &Path, field: &str, value: &str) -> Result<(), ConfigError> {
    if is_absolute_path_str(value) {
        return Err(ConfigError::Validation {
            path: path.to_path_buf(),
            field: field.to_owned(),
            message: "path must be relative to the project root".to_owned(),
        });
    }
    Ok(())
}

fn config_validation_path(root: &Path) -> std::path::PathBuf {
    let pyproject = root.join("pyproject.toml");
    if pyproject.is_file() {
        pyproject
    } else if root.join("yokei.toml").is_file() {
        root.join("yokei.toml")
    } else if root.join(".yokei.toml").is_file() {
        root.join(".yokei.toml")
    } else {
        root.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Confidence;
    use crate::config::default_config;

    #[test]
    fn apply_overrides_sets_production() {
        let mut config = default_config();
        apply_overrides(
            &mut config,
            &RuntimeOverrides {
                production: Some(true),
                ..RuntimeOverrides::default()
            },
        );
        assert!(config.production);
    }

    #[test]
    fn apply_overrides_sets_confidence_floor() {
        let mut config = default_config();
        apply_overrides(
            &mut config,
            &RuntimeOverrides {
                confidence_floor: Some(Confidence::Certain),
                ..RuntimeOverrides::default()
            },
        );
        assert_eq!(config.confidence, Confidence::Certain);
    }
}
