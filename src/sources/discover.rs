//! Source file discovery orchestration.

use std::collections::BTreeSet;
use std::path::Path;

use crate::config::LoadedConfig;
use crate::discovery::ProjectRoot;
use crate::manifest::LoadedManifest;

use super::cache::SourcesCache;
use super::error::SourcesError;
use super::glob::{build_glob_set, effective_exclude};
use super::layout::{
    ENTRY_TREE_GLOBS, PytestConfigs, infer_layout, pytest_python_file_globs, pytest_testpaths,
    unmatched_testpaths,
};
use super::types::{DiscoveredFile, DiscoveredSources, LayoutInfo};
use super::walk::{
    CollectOptions, Collected, collect_files, large_project_warning, matches_source,
    root_gitignore_warning, validate_entries,
};
use super::warnings::SourcesWarning;

/// Discover Python source files under `root` using config and manifest hints.
pub fn discover_sources(
    root: &ProjectRoot,
    config: &LoadedConfig,
    manifest: &LoadedManifest,
) -> Result<DiscoveredSources, SourcesError> {
    discover_sources_cached(root, config, manifest, &SourcesCache::default())
        .map(|discovery| discovery.sources)
}

/// What [`discover_sources_cached`] found besides the inventory.
#[derive(Debug)]
pub(crate) struct Discovery {
    pub sources: DiscoveredSources,
    /// Under `production`, the test files left out of `sources`, sorted by
    /// path (#844).
    pub held_tests: Vec<DiscoveredFile>,
    /// Command files the walk found, root-relative and sorted: Dockerfiles
    /// below the root (#1061), shell script and supervisor config candidates
    /// (#1240).
    pub command_files: Vec<String>,
}

/// [`discover_sources`] reusing the ignore files and globs that earlier calls
/// sharing `cache` parsed: a workspace probe walks member trees the root walk
/// already covered.
pub(crate) fn discover_sources_cached(
    root: &ProjectRoot,
    config: &LoadedConfig,
    manifest: &LoadedManifest,
    cache: &SourcesCache,
) -> Result<Discovery, SourcesError> {
    let configs = PytestConfigs::default();
    let (mut layout, layout_warning) =
        infer_layout(&root.path, &manifest.metadata, &manifest.uv, &configs);
    // Only inferred globs take testpaths: the parse cache keys on the globs,
    // so testpaths outside them would change file contexts unnoticed.
    let mut testpath_warnings = Vec::new();
    let (test_globs, effective_globs) = if config.effective.project.is_empty() {
        testpath_warnings = unmatched_testpaths(&root.path, &layout, &configs)
            .into_iter()
            .map(|entry| SourcesWarning::TestpathsUnmatched { entry })
            .collect();
        let test_globs = pytest_test_globs(&root.path, &layout, &configs);
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

    let gitignore_warning = config
        .effective
        .respect_gitignore
        .then(|| root_gitignore_warning(&root.path, cache))
        .flatten();

    let collect_options = CollectOptions {
        root: &root.path,
        project_matcher: &project_matcher,
        exclude_matcher: &exclude_matcher,
        test_matcher: &test_matcher,
        exclude_patterns: &exclude_patterns,
        respect_gitignore: config.effective.respect_gitignore,
        production: config.effective.production,
        cache,
        dirs: None,
    };
    let mut collected = collect_files(&collect_options)?;
    if config.effective.project.is_empty() {
        drop_member_entry_trees(
            &mut collected,
            &effective_globs,
            &test_matcher,
            config,
            cache,
        )?;
    }
    let files = collected.files;
    layout.package_dirs = files
        .iter()
        .chain(&collected.held_tests)
        .filter_map(|file| file.path.strip_suffix("/__init__.py").map(str::to_owned))
        .collect();
    layout.symlinks = collected.symlinks;

    let mut warnings = validate_entries(&root.path, &config.effective.entry);
    warnings.extend(layout_warning);
    warnings.extend(testpath_warnings);
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
        command_files: collected.command_files,
    })
}

/// The globs of the files pytest collects as tests: its `testpaths` trees
/// and the files its `python_files` names.
fn pytest_test_globs(root: &Path, layout: &LayoutInfo, configs: &PytestConfigs) -> Vec<String> {
    pytest_testpaths(root, layout, configs)
        .iter()
        .map(|path| globset::escape(path))
        .flat_map(|path| [format!("{path}/**/*.{{py,pyi,ipynb}}"), path])
        .chain(pytest_python_file_globs(root, layout, configs))
        .collect()
}

/// The files at `paths` (root-relative) that discovery would collect if the
/// project globs named them: the same exclusions, ignore rules and symlink
/// policy, without walking the rest of the tree (#932). A path that is
/// excluded, ignored or missing is left out; the caller decides what that
/// means. Contexts come from the path, and `production` drops none. The walk
/// warnings are dropped: the root walk met the same directories and links.
pub(crate) fn discover_named_files(
    config: &LoadedConfig,
    paths: &BTreeSet<String>,
) -> Result<Vec<DiscoveredFile>, SourcesError> {
    let cache = SourcesCache::default();
    let globs: Vec<String> = paths.iter().map(|path| globset::escape(path)).collect();
    let dirs: BTreeSet<String> = paths
        .iter()
        .flat_map(|path| {
            path.match_indices('/')
                .map(|(end, _)| path[..end].to_owned())
        })
        .collect();
    let exclude_patterns = effective_exclude(&config.effective.exclude);
    let project_matcher = cache.glob_set(&globs)?;
    let exclude_matcher = cache.glob_set(&exclude_patterns)?;
    let collected = collect_files(&CollectOptions {
        root: &config.root.path,
        project_matcher: &project_matcher,
        exclude_matcher: &exclude_matcher,
        test_matcher: &globset::GlobSet::empty(),
        exclude_patterns: &exclude_patterns,
        respect_gitignore: config.effective.respect_gitignore,
        production: false,
        cache: &cache,
        dirs: Some(&dirs),
    })?;
    Ok(collected.files)
}

/// Drop the files inside a workspace member that only [`ENTRY_TREE_GLOBS`]
/// reached: the member keeps its own layout, and the root never read them
/// before those globs (#1023). Files a pytest `testpaths` entry covers stay,
/// since the root read them as tests before.
fn drop_member_entry_trees(
    collected: &mut Collected,
    effective_globs: &[String],
    test_matcher: &globset::GlobSet,
    config: &LoadedConfig,
    cache: &SourcesCache,
) -> Result<(), SourcesError> {
    let members: Vec<&Path> = config
        .workspace_members
        .iter()
        .filter(|member| !member.path.is_empty())
        .map(|member| Path::new(member.path.as_str()))
        .collect();
    if members.is_empty() {
        return Ok(());
    }
    let own_globs: Vec<String> = effective_globs
        .iter()
        .filter(|glob| !ENTRY_TREE_GLOBS.contains(&glob.as_str()))
        .cloned()
        .collect();
    let own_matcher = cache.glob_set(&own_globs)?;
    let keep = |file: &DiscoveredFile| {
        matches_source(&own_matcher, &file.path)
            || matches_source(test_matcher, &file.path)
            || !members
                .iter()
                .any(|member| Path::new(&file.path).starts_with(member))
    };
    collected.files.retain(keep);
    collected.held_tests.retain(keep);
    Ok(())
}

/// Whether the root's walk reaches `path` only through [`ENTRY_TREE_GLOBS`],
/// so [`drop_member_entry_trees`] leaves it out on purpose when it sits in a
/// workspace member (#1023). `effective_globs` are the root's inferred globs;
/// with explicit `project` globs nothing is dropped and this does not apply.
/// A member entry there is not one the root skipped (#1140).
pub(crate) fn only_entry_trees_reach(effective_globs: &[String], path: &str) -> bool {
    let (trees, own): (Vec<String>, Vec<String>) = effective_globs
        .iter()
        .cloned()
        .partition(|glob| ENTRY_TREE_GLOBS.contains(&glob.as_str()));
    // Discovery already compiled these globs; one that fails here reaches
    // nothing, so the entry stays a skipped one.
    let matches = |globs: &[String]| build_glob_set(globs).is_ok_and(|set| set.is_match(path));
    matches(&trees) && !matches(&own)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_entry_trees_reach_needs_no_own_glob_to_match() {
        let globs: Vec<String> = ["src/**/*.{py,pyi,ipynb}", "tests/**/*.{py,pyi,ipynb}"]
            .into_iter()
            .chain(ENTRY_TREE_GLOBS)
            .map(str::to_owned)
            .collect();
        assert!(only_entry_trees_reach(&globs, "examples/demo/demo.py"));
        assert!(only_entry_trees_reach(&globs, "pkgs/a/nb.ipynb"));
        assert!(!only_entry_trees_reach(&globs, "src/lib/cli.py"));
        assert!(!only_entry_trees_reach(&globs, "pkgs/a/cli.py"));
        let flat = ["**/*.{py,pyi,ipynb}".to_owned()];
        assert!(!only_entry_trees_reach(&flat, "examples/demo/demo.py"));
    }
}
