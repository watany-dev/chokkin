//! Plugin hint extraction orchestration.

use crate::VERSION;
use crate::cache::{
    CacheKeyContext, CacheOptions, ScanCacheKey, ScanInputFingerprints, stable_hex_hash,
};
use crate::config::{LoadedConfig, PluginId};
use crate::discovery::ProjectRoot;
use crate::manifest::LoadedManifest;
use crate::sources::DiscoveredSources;

use super::config_scan;
use super::celery;
use super::context::PluginContext;
use super::devtools;
use super::django;
use super::doctools;
use super::error::PluginsError;
use super::fastapi;
use super::flask;
use super::pytest;
use super::types::PluginHints;

/// Extract framework hints from tool configuration (§6 step 5).
pub fn extract_plugin_hints(
    root: &ProjectRoot,
    config: &LoadedConfig,
    sources: &DiscoveredSources,
    manifest: &LoadedManifest,
) -> Result<PluginHints, PluginsError> {
    extract_plugin_hints_with_cache(root, config, sources, manifest, None)
}

/// Extract framework hints, optionally caching generic config scan results.
pub fn extract_plugin_hints_with_cache(
    root: &ProjectRoot,
    config: &LoadedConfig,
    sources: &DiscoveredSources,
    manifest: &LoadedManifest,
    cache: Option<&CacheOptions>,
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
            PluginId::Flask => flask::extract(&ctx),
            PluginId::Celery => celery::extract(&ctx),
            PluginId::Tox | PluginId::Nox | PluginId::PreCommit | PluginId::GithubActions => {
                devtools::extract(*plugin, &ctx)
            }
            PluginId::Sphinx | PluginId::MkDocs | PluginId::Alembic => {
                doctools::extract(*plugin, &ctx)
            }
        };
        warnings.extend(plugin_warnings);
        contributions.push(contrib);
    }

    let scan = cached_config_scan(&ctx, config, manifest, cache)?;

    Ok(PluginHints {
        contributions,
        config_binary_usages: scan.binary_usages,
        config_used_distributions: scan.used_distributions,
        warnings,
    })
}

fn cached_config_scan(
    ctx: &PluginContext<'_>,
    config: &LoadedConfig,
    manifest: &LoadedManifest,
    cache: Option<&CacheOptions>,
) -> Result<config_scan::ConfigScanResult, PluginsError> {
    let Some(cache) = cache.filter(|cache| cache.enabled) else {
        return Ok(config_scan::scan_config(ctx));
    };
    let key = config_scan_cache_key(config, manifest)?;
    if let Some(scan) = cache
        .read_scan_payload(ctx.root.path.as_path(), &key)
        .map_err(|source| PluginsError::Io {
            path: cache.scan_entry_path(ctx.root.path.as_path(), &key),
            source,
        })?
    {
        return Ok(scan);
    }

    let scan = config_scan::scan_config(ctx);
    cache
        .write_scan_payload(ctx.root.path.as_path(), key, &scan)
        .map_err(|source| PluginsError::Io {
            path: cache.directory_path(ctx.root.path.as_path()),
            source,
        })?;
    Ok(scan)
}

fn config_scan_cache_key(
    config: &LoadedConfig,
    manifest: &LoadedManifest,
) -> Result<ScanCacheKey, PluginsError> {
    let inputs = ScanInputFingerprints::collect(
        config.root.path.as_path(),
        &config.sources,
        &manifest.sources,
    )
    .map_err(|source| PluginsError::Io {
        path: config.root.path.clone(),
        source,
    })?;
    let target = config
        .effective
        .target_version
        .clone()
        .unwrap_or_else(crate::config::TargetVersion::default_py311);
    Ok(ScanCacheKey {
        context: CacheKeyContext {
            chokkin_version: VERSION.to_owned(),
            config_hash: stable_hex_hash(format!("{:?}", config.effective).as_bytes()),
            manifest_hash: stable_hex_hash(format!("{:?}", manifest.sources).as_bytes()),
            target_version: target.as_str().to_owned(),
            unit_version: "config-scan-v1".to_owned(),
        },
        inputs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheOptions;

    #[test]
    fn disabled_plugins_are_skipped() {
        let root_path = std::env::temp_dir().join("chokkin-plugins-empty");
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
                dot_chokkin_toml: None,
                chokkin_toml: None,
                pyproject_tool_chokkin: false,
            },
            uv_workspace: None,
            workspace_members: Vec::new(),
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

    #[test]
    fn config_scan_uses_disk_cache_payload() {
        let root_path = std::env::temp_dir().join(format!(
            "chokkin-plugin-scan-cache-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root_path);
        std::fs::create_dir_all(&root_path).expect("create root");
        std::fs::write(
            root_path.join("pyproject.toml"),
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[tool.mypy]\nstrict = true\n",
        )
        .expect("write pyproject");
        let root = ProjectRoot {
            path: root_path.clone(),
            marker: crate::discovery::RootMarker::PyProjectToml,
            start: root_path.clone(),
        };
        let loaded = LoadedConfig {
            root: root.clone(),
            effective: crate::default_config(),
            sources: crate::config::ConfigSources {
                used_defaults: true,
                dot_chokkin_toml: None,
                chokkin_toml: None,
                pyproject_tool_chokkin: false,
            },
            uv_workspace: None,
            workspace_members: Vec::new(),
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
            sources: crate::manifest::ManifestSources {
                pyproject_toml: true,
                ..crate::manifest::ManifestSources::default()
            },
            warnings: Vec::new(),
        };
        let cache = CacheOptions::default();

        let first = extract_plugin_hints_with_cache(
            &loaded.root,
            &loaded,
            &sources,
            &manifest,
            Some(&cache),
        )
        .expect("first extract");
        let key = config_scan_cache_key(&loaded, &manifest).expect("cache key");
        let cached: config_scan::ConfigScanResult = cache
            .read_scan_payload(&root_path, &key)
            .expect("read payload")
            .expect("payload hit");
        let second = extract_plugin_hints_with_cache(
            &loaded.root,
            &loaded,
            &sources,
            &manifest,
            Some(&cache),
        )
        .expect("second extract");

        assert!(
            first
                .config_binary_usages
                .iter()
                .any(|usage| usage.binary == "mypy")
        );
        assert_eq!(cached.binary_usages, first.config_binary_usages);
        assert_eq!(second.config_binary_usages, first.config_binary_usages);
        let _ = std::fs::remove_dir_all(root_path);
    }
}
