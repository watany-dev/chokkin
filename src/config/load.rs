//! Load and merge yokei configuration layers.
//!
//! Value validation happens at parse time so errors point at the file that
//! actually contains the offending value.

use crate::discovery::ProjectRoot;

use super::defaults::merge_layers;
use super::error::ConfigError;
use super::parse::{parse_pyproject_config, parse_standalone_config};
use super::source::discover_config_files;
use super::types::{ConfigSources, LoadedConfig, RuntimeOverrides, YokeiConfig};

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
