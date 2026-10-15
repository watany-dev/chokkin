//! Source file discovery orchestration.

use crate::config::LoadedConfig;
use crate::discovery::ProjectRoot;
use crate::manifest::LoadedManifest;

use super::cache::SourcesCache;
use super::error::SourcesError;
use super::glob::effective_exclude;
use super::layout::{infer_layout, pytest_testpaths};
use super::types::{DiscoveredFile, DiscoveredSources};
use super::walk::{
    CollectOptions, WalkRecord, collect_files, large_project_warning, load_gitignore, record_files,
    replay_files, validate_entries,
};

/// Discover Python source files under `root` using config and manifest hints.
pub fn discover_sources(
    root: &ProjectRoot,
    config: &LoadedConfig,
    manifest: &LoadedManifest,
) -> Result<DiscoveredSources, SourcesError> {
    discover_sources_cached(
        root,
        config,
        manifest,
        &SourcesCache::default(),
        WalkMode::Walk,
    )
    .map(|discovery| discovery.sources)
}

/// What [`discover_sources_cached`] found besides the inventory.
#[derive(Debug)]
pub(crate) struct Discovery {
    pub sources: DiscoveredSources,
    /// Under `production`, the test files left out of `sources`, sorted by
    /// path (#844).
    pub held_tests: Vec<DiscoveredFile>,
    /// The walk, under [`WalkMode::Record`].
    pub record: Option<WalkRecord>,
}

/// How [`discover_sources_cached`] reads the tree.
#[derive(Debug, Clone, Copy)]
pub(crate) enum WalkMode<'a> {
    /// Walk it.
    Walk,
    /// Walk it and keep the walk, for the member roots below (#843).
    Record,
    /// Answer from an earlier walk of a root above, where it can (#843).
    Replay(&'a WalkRecord),
}

/// [`discover_sources`] reusing the ignore files and globs that earlier calls
/// sharing `cache` parsed: a workspace probe walks member trees the root walk
/// already covered.
pub(crate) fn discover_sources_cached(
    root: &ProjectRoot,
    config: &LoadedConfig,
    manifest: &LoadedManifest,
    cache: &SourcesCache,
    mode: WalkMode<'_>,
) -> Result<Discovery, SourcesError> {
    let (layout, layout_warning) = infer_layout(&root.path, &manifest.metadata, &manifest.uv);
    // Only inferred globs take testpaths: the parse cache keys on the globs,
    // so testpaths outside them would change file contexts unnoticed.
    let (test_globs, effective_globs) = if config.effective.project.is_empty() {
        let test_globs: Vec<String> = pytest_testpaths(&root.path, &layout)
            .iter()
            .map(|path| globset::escape(path))
            .flat_map(|path| [format!("{path}/**/*.{{py,pyi,ipynb}}"), path])
            .collect();
        let mut globs = layout.inferred_globs.clone();
        let fresh = test_globs
            .iter()
            .filter(|glob| !layout.inferred_globs.contains(glob));
        globs.extend(fresh.cloned());
        (test_globs, globs)
    } else {
        (Vec::new(), config.effective.project.clone())
    };

    let exclude_patterns = effective_exclude(&config.effective.exclude);
    let project_matcher = cache.glob_set(&effective_globs)?;
    let exclude_matcher = cache.glob_set(&exclude_patterns)?;
    let test_matcher = cache.glob_set(&test_globs)?;

    let (gitignore, gitignore_warning) = if config.effective.respect_gitignore {
        load_gitignore(&root.path, cache)
    } else {
        (None, None)
    };

    let collect_options = CollectOptions {
        root: &root.path,
        project_matcher: &project_matcher,
        exclude_matcher: &exclude_matcher,
        test_matcher: &test_matcher,
        exclude_patterns: &exclude_patterns,
        respect_gitignore: config.effective.respect_gitignore,
        gitignore: gitignore.as_deref(),
        production: config.effective.production,
        cache,
    };
    let (collected, record) = match mode {
        WalkMode::Walk => (collect_files(&collect_options)?, None),
        WalkMode::Record => {
            let (collected, record) = record_files(&collect_options)?;
            (collected, Some(record))
        },
        WalkMode::Replay(record) => (replay_files(&collect_options, record)?, None),
    };
    let files = collected.files;

    let mut warnings = validate_entries(&root.path, &config.effective.entry);
    warnings.extend(layout_warning);
    warnings.extend(collected.warnings);
    if let Some(warning) = gitignore_warning {
        warnings.push(warning);
    }
    if let Some(warning) = large_project_warning(files.len()) {
        warnings.push(warning);
    }

    Ok(Discovery {
        sources: DiscoveredSources {
            root: root.clone(),
            layout,
            effective_globs,
            files,
            warnings,
        },
        held_tests: collected.held_tests,
        record,
    })
}
