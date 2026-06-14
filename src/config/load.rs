//! Load and merge chokkin configuration layers.
//!
//! Value validation happens at parse time so errors point at the file that
//! actually contains the offending value.

use crate::discovery::ProjectRoot;

use super::defaults::merge_layers;
use super::error::ConfigError;
use super::parse::{parse_pyproject_config, parse_standalone_config};
use super::source::discover_config_files;
use super::types::{ChokkinConfig, ConfigSources, LoadedConfig, RuntimeOverrides};
use super::workspace::resolve_workspace_members;

/// Load and merge chokkin configuration for `root`.
///
/// Returns defaults when no config files exist. Never executes Python.
pub fn load_config(root: &ProjectRoot) -> Result<LoadedConfig, ConfigError> {
    let files = discover_config_files(&root.path);
    let mut layers = Vec::new();
    let mut sources = ConfigSources {
        used_defaults: true,
        dot_chokkin_toml: files.dot_chokkin_toml.clone(),
        chokkin_toml: files.chokkin_toml.clone(),
        pyproject_tool_chokkin: false,
    };

    if let Some(path) = &files.dot_chokkin_toml {
        let partial = parse_standalone_config(path)?;
        if partial.has_any_field() {
            layers.push(partial);
        }
    }

    if let Some(path) = &files.chokkin_toml {
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
            sources.pyproject_tool_chokkin = true;
            layers.push(partial);
        }
    }

    let effective = merge_layers(&layers);
    let workspace_members = resolve_workspace_members(root, &effective, uv_workspace.as_ref())?;

    Ok(LoadedConfig {
        root: root.clone(),
        effective,
        sources,
        uv_workspace,
        workspace_members,
    })
}

/// Apply CLI/runtime overrides onto a loaded file configuration.
pub fn apply_overrides(config: &mut ChokkinConfig, overrides: &RuntimeOverrides) {
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

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn any_confidence() -> impl Strategy<Value = Confidence> {
            prop_oneof![
                Just(Confidence::Certain),
                Just(Confidence::Likely),
                Just(Confidence::Maybe),
            ]
        }

        proptest! {
            #[test]
            fn overrides_replace_only_set_fields(
                production in proptest::option::of(proptest::bool::ANY),
                confidence_floor in proptest::option::of(any_confidence()),
                strict in proptest::option::of(proptest::bool::ANY),
            ) {
                let baseline = default_config();
                let mut config = baseline.clone();
                apply_overrides(
                    &mut config,
                    &RuntimeOverrides {
                        production,
                        strict,
                        confidence_floor,
                        ..RuntimeOverrides::default()
                    },
                );

                prop_assert_eq!(
                    config.production,
                    production.unwrap_or(baseline.production)
                );
                prop_assert_eq!(
                    config.confidence,
                    confidence_floor.unwrap_or(baseline.confidence)
                );

                // Everything else stays untouched.
                config.production = baseline.production;
                config.confidence = baseline.confidence;
                prop_assert_eq!(config, baseline);
            }

            #[test]
            fn apply_overrides_is_idempotent(
                production in proptest::option::of(proptest::bool::ANY),
                confidence_floor in proptest::option::of(any_confidence()),
            ) {
                let overrides = RuntimeOverrides {
                    production,
                    strict: None,
                    confidence_floor,
                    ..RuntimeOverrides::default()
                };
                let mut once = default_config();
                apply_overrides(&mut once, &overrides);
                let mut twice = once.clone();
                apply_overrides(&mut twice, &overrides);
                prop_assert_eq!(once, twice);
            }
        }
    }
}
