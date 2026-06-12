//! Default configuration and layer merging.

use std::collections::BTreeMap;

use super::types::{
    Confidence, DependencyGroupsConfig, PluginId, ProjectMode, TargetVersion, YokeiConfig,
};

/// Optional dependency group keys for one layer. Missing keys keep lower-priority values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(clippy::struct_field_names)]
pub(super) struct PartialDependencyGroups {
    pub dev_groups: Option<Vec<String>>,
    pub runtime_groups: Option<Vec<String>>,
    pub type_groups: Option<Vec<String>>,
}

impl PartialDependencyGroups {
    /// Overwrite `target` with the groups this layer sets.
    fn apply_to(&self, target: &mut DependencyGroupsConfig) {
        if let Some(dev_groups) = &self.dev_groups {
            target.dev_groups.clone_from(dev_groups);
        }
        if let Some(runtime_groups) = &self.runtime_groups {
            target.runtime_groups.clone_from(runtime_groups);
        }
        if let Some(type_groups) = &self.type_groups {
            target.type_groups.clone_from(type_groups);
        }
    }
}

/// Optional fields for one configuration layer. `Some` values replace the merged
/// result, except `plugins` and `dependencies` which merge per key (§3.1).
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
    pub dependencies: Option<PartialDependencyGroups>,
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
            dependencies.apply_to(&mut config.dependencies);
        }
        if let Some(package_module_map) = &layer.package_module_map {
            config.package_module_map.clone_from(package_module_map);
        }
        if let Some(binary_map) = &layer.binary_map {
            config.binary_map.clone_from(binary_map);
        }
        if let Some(plugins) = &layer.plugins {
            for (plugin, enabled) in plugins {
                config.plugins.insert(*plugin, *enabled);
            }
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
    fn merge_overlays_plugins_onto_defaults() {
        let mut plugins = BTreeMap::new();
        plugins.insert(PluginId::Celery, true);

        let merged = merge_layers(&[PartialConfig {
            plugins: Some(plugins),
            ..PartialConfig::default()
        }]);

        assert_eq!(merged.plugins.len(), PluginId::all().len());
        assert_eq!(merged.plugins.get(&PluginId::Celery), Some(&true));
        assert_eq!(merged.plugins.get(&PluginId::Pytest), Some(&true));
        assert_eq!(merged.plugins.get(&PluginId::Tox), Some(&false));
    }

    #[test]
    fn merge_keeps_default_dependency_groups_for_missing_keys() {
        let merged = merge_layers(&[PartialConfig {
            dependencies: Some(PartialDependencyGroups {
                dev_groups: Some(vec!["dev".to_owned()]),
                ..PartialDependencyGroups::default()
            }),
            ..PartialConfig::default()
        }]);

        assert_eq!(merged.dependencies.dev_groups, vec!["dev".to_owned()]);
        assert_eq!(
            merged.dependencies.runtime_groups,
            default_config().dependencies.runtime_groups
        );
        assert_eq!(
            merged.dependencies.type_groups,
            default_config().dependencies.type_groups
        );
    }
}
