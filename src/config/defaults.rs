//! Default configuration and layer merging.

use std::collections::BTreeMap;

use super::types::{
    Confidence, DependencyGroupsConfig, PluginId, ProjectMode, TargetVersion, YokeiConfig,
};

/// Optional fields for one configuration layer. `Some` values replace the merged result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct PartialConfig {
    pub entry: Option<Vec<super::types::EntrySpec>>,
    pub project: Option<Vec<String>>,
    pub mode: Option<ProjectMode>,
    pub production: Option<bool>,
    pub target_version: Option<TargetVersion>,
    pub respect_gitignore: Option<bool>,
    pub confidence: Option<Confidence>,
    pub exclude: Option<Vec<String>>,
    pub dependencies: Option<DependencyGroupsConfig>,
    pub package_module_map: Option<BTreeMap<String, Vec<String>>>,
    pub binary_map: Option<BTreeMap<String, String>>,
    pub plugins: Option<BTreeMap<PluginId, bool>>,
    pub ignore: Option<BTreeMap<String, Vec<String>>>,
    pub workspaces: Option<BTreeMap<String, super::types::WorkspaceOverride>>,
}

impl PartialConfig {
    /// Returns true when this layer sets at least one field.
    #[must_use]
    pub fn has_any_field(&self) -> bool {
        self.entry.is_some()
            || self.project.is_some()
            || self.mode.is_some()
            || self.production.is_some()
            || self.target_version.is_some()
            || self.respect_gitignore.is_some()
            || self.confidence.is_some()
            || self.exclude.is_some()
            || self.dependencies.is_some()
            || self.package_module_map.is_some()
            || self.binary_map.is_some()
            || self.plugins.is_some()
            || self.ignore.is_some()
            || self.workspaces.is_some()
    }
}

/// Hardcoded zero-config defaults (§5).
#[must_use]
pub fn default_config() -> YokeiConfig {
    let mut plugins = BTreeMap::new();
    for plugin in PluginId::all() {
        let enabled = matches!(
            plugin,
            PluginId::Pytest | PluginId::Django | PluginId::Fastapi
        );
        plugins.insert(*plugin, enabled);
    }

    YokeiConfig {
        entry: Vec::new(),
        project: Vec::new(),
        mode: ProjectMode::Auto,
        production: false,
        target_version: TargetVersion::default_py311(),
        respect_gitignore: true,
        confidence: Confidence::Likely,
        exclude: vec![
            ".venv/**".to_owned(),
            "build/**".to_owned(),
            "dist/**".to_owned(),
            "**/__pycache__/**".to_owned(),
        ],
        dependencies: DependencyGroupsConfig {
            dev_groups: vec![
                "dev".to_owned(),
                "test".to_owned(),
                "tests".to_owned(),
                "lint".to_owned(),
                "docs".to_owned(),
            ],
            runtime_groups: vec!["server".to_owned(), "worker".to_owned()],
            type_groups: vec!["types".to_owned(), "typing".to_owned(), "mypy".to_owned()],
        },
        package_module_map: BTreeMap::new(),
        binary_map: BTreeMap::new(),
        plugins,
        ignore: BTreeMap::new(),
        workspaces: BTreeMap::new(),
    }
}

/// Merge configuration layers from lowest to highest priority.
#[must_use]
pub fn merge_layers(layers: &[PartialConfig]) -> YokeiConfig {
    let mut config = default_config();

    for layer in layers {
        if let Some(entry) = &layer.entry {
            config.entry.clone_from(entry);
        }
        if let Some(project) = &layer.project {
            config.project.clone_from(project);
        }
        if let Some(mode) = layer.mode {
            config.mode = mode;
        }
        if let Some(production) = layer.production {
            config.production = production;
        }
        if let Some(target_version) = &layer.target_version {
            config.target_version = target_version.clone();
        }
        if let Some(respect_gitignore) = layer.respect_gitignore {
            config.respect_gitignore = respect_gitignore;
        }
        if let Some(confidence) = layer.confidence {
            config.confidence = confidence;
        }
        if let Some(exclude) = &layer.exclude {
            config.exclude.clone_from(exclude);
        }
        if let Some(dependencies) = &layer.dependencies {
            config.dependencies = dependencies.clone();
        }
        if let Some(package_module_map) = &layer.package_module_map {
            config.package_module_map.clone_from(package_module_map);
        }
        if let Some(binary_map) = &layer.binary_map {
            config.binary_map.clone_from(binary_map);
        }
        if let Some(plugins) = &layer.plugins {
            config.plugins.clone_from(plugins);
        }
        if let Some(ignore) = &layer.ignore {
            config.ignore.clone_from(ignore);
        }
        if let Some(workspaces) = &layer.workspaces {
            config.workspaces.clone_from(workspaces);
        }
    }

    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_partial_has_no_fields() {
        assert!(!PartialConfig::default().has_any_field());
    }

    #[test]
    fn partial_with_mode_has_field() {
        assert!(
            PartialConfig {
                mode: Some(ProjectMode::App),
                ..PartialConfig::default()
            }
            .has_any_field()
        );
    }

    #[test]
    fn merge_replaces_whole_plugin_map() {
        let mut plugins = BTreeMap::new();
        plugins.insert(PluginId::Celery, true);

        let merged = merge_layers(&[PartialConfig {
            plugins: Some(plugins),
            ..PartialConfig::default()
        }]);

        assert_eq!(merged.plugins.len(), 1);
        assert_eq!(merged.plugins.get(&PluginId::Celery), Some(&true));
        assert!(!merged.plugins.contains_key(&PluginId::Pytest));
    }
}
