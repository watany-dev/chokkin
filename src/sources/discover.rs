//! Source file discovery orchestration.

use crate::config::LoadedConfig;
use crate::discovery::ProjectRoot;
use crate::manifest::LoadedManifest;

use super::error::SourcesError;
use super::glob::{build_glob_set, effective_exclude};
use super::layout::{infer_layout, layout_warnings};
use super::types::DiscoveredSources;
use super::walk::{
    CollectOptions, collect_files, large_project_warning, load_gitignore, validate_entries,
};

/// Discover Python source files under `root` using config and manifest hints.
pub fn discover_sources(
    root: &ProjectRoot,
    config: &LoadedConfig,
    manifest: &LoadedManifest,
) -> Result<DiscoveredSources, SourcesError> {
    let layout = infer_layout(&root.path, &manifest.metadata);
    let effective_globs = if config.effective.project.is_empty() {
        layout.inferred_globs.clone()
    } else {
        config.effective.project.clone()
    };

    let exclude_patterns = effective_exclude(&config.effective.exclude);
    let project_matcher = build_glob_set(&effective_globs)?;
    let exclude_matcher = build_glob_set(&exclude_patterns)?;

    let (gitignore, gitignore_warning) = if config.effective.respect_gitignore {
        load_gitignore(&root.path)
    } else {
        (None, None)
    };

    let collect_options = CollectOptions {
        root: &root.path,
        project_matcher: &project_matcher,
        exclude_matcher: &exclude_matcher,
        exclude_patterns: &exclude_patterns,
        respect_gitignore: config.effective.respect_gitignore,
        gitignore: gitignore.as_ref(),
        production: config.effective.production,
        layout: &layout,
    };
    let (files, walk_warnings) = collect_files(&collect_options)?;

    let mut warnings = validate_entries(&root.path, &config.effective.entry);
    warnings.extend(layout_warnings(&layout));
    warnings.extend(walk_warnings);
    if let Some(warning) = gitignore_warning {
        warnings.push(warning);
    }
    if let Some(warning) = large_project_warning(files.len()) {
        warnings.push(warning);
    }

    Ok(DiscoveredSources {
        root: root.clone(),
        layout,
        effective_globs,
        files,
        warnings,
    })
}
