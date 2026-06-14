//! Plugin hint extraction orchestration.

use crate::config::{LoadedConfig, PluginId};
use crate::discovery::ProjectRoot;
use crate::manifest::LoadedManifest;
use crate::sources::DiscoveredSources;

use super::config_scan;
use super::context::PluginContext;
use super::django;
use super::error::PluginsError;
use super::fastapi;
use super::pytest;
use super::stub;
use super::types::PluginHints;

/// Extract framework hints from tool configuration (§6 step 5).
pub fn extract_plugin_hints(
    root: &ProjectRoot,
    config: &LoadedConfig,
    sources: &DiscoveredSources,
    manifest: &LoadedManifest,
) -> Result<PluginHints, PluginsError> {
    let ctx = PluginContext {
        root,
        config: &config.effective,
        sources,
        manifest,
    };
    let mut contributions = Vec::new();
    let mut warnings = Vec::new();

    for plugin in PluginId::all() {
        let enabled = config
            .effective
            .plugins
            .get(plugin)
            .copied()
            .unwrap_or(false);
        if !enabled {
            continue;
        }
        let (contrib, plugin_warnings) = match plugin {
            PluginId::Pytest => pytest::extract(&ctx),
            PluginId::Django => django::extract(&ctx),
            PluginId::Fastapi => fastapi::extract(&ctx),
            _ => stub::extract(*plugin, &ctx),
        };
        warnings.extend(plugin_warnings);
        contributions.push(contrib);
    }

    let scan = config_scan::scan_config(&ctx);

    Ok(PluginHints {
        contributions,
        config_binary_usages: scan.binary_usages,
        config_used_distributions: scan.used_distributions,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_plugins_are_skipped() {
        let root_path = std::env::temp_dir().join("yokei-plugins-empty");
        let _ = std::fs::create_dir_all(&root_path);
        let root = ProjectRoot {
            path: root_path.clone(),
            marker: crate::discovery::RootMarker::PyProjectToml,
            start: root_path,
        };
        let mut config = crate::default_config();
        config.plugins.insert(PluginId::Pytest, false);
        config.plugins.insert(PluginId::Django, false);
        config.plugins.insert(PluginId::Fastapi, false);
        let loaded = LoadedConfig {
            root: root.clone(),
            effective: config,
            sources: crate::config::ConfigSources {
                used_defaults: true,
                dot_yokei_toml: None,
                yokei_toml: None,
                pyproject_tool_yokei: false,
            },
            uv_workspace: None,
        };
        let sources = DiscoveredSources {
            root: root.clone(),
            layout: crate::sources::LayoutInfo {
                layout: crate::sources::ProjectLayout::Unknown,
                packages: Vec::new(),
                inferred_globs: Vec::new(),
                flat_candidates: Vec::new(),
                ambiguous_flat_resolution: false,
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let manifest = LoadedManifest {
            root,
            metadata: crate::manifest::ProjectMetadata::default(),
            dependencies: Vec::new(),
            constraints: Vec::new(),
            uv_workspace: None,
            entry_points: Vec::new(),
            lockfile: crate::manifest::LockfileGraph::default(),
            sources: crate::manifest::ManifestSources::default(),
            warnings: Vec::new(),
        };
        let hints = extract_plugin_hints(&loaded.root, &loaded, &sources, &manifest)
            .expect("extract hints");
        assert!(hints.contributions.is_empty());
    }
}
