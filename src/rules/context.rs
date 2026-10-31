//! Borrowed inputs shared by dependency and symbol analysis.
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::OnceLock;

use crate::{
    graph::{FileId, ProjectGraph},
    parser::ParseSummary,
    reachability::{ModuleIndex, ReachabilityReport, module_and_parents},
    resolver::{PytestImportPaths, ResolutionIndex},
    rules::deps::file_context,
    sources::{DiscoveredSources, FileContext, PublicSurface, is_test_harness_name},
};

/// Earlier pipeline outputs read by dependency (step 10) and symbol (step 11) rules.
pub struct RuleContext<'a> {
    /// Step 7 import resolution.
    pub resolution: &'a ResolutionIndex,
    /// Step 9 reachability.
    pub reachability: &'a ReachabilityReport,
    /// Project graph after resolution and entry wiring.
    pub graph: &'a ProjectGraph,
    /// Discovered source files.
    pub sources: &'a DiscoveredSources,
    /// Step 6 parse output.
    pub parse: &'a ParseSummary,
    /// Files the root wheel ships; `None` without wheel targets or when they
    /// match no file (R-05). Resolved once per run.
    pub surface: Option<&'a PublicSurface>,
    /// What the rules derive from the inputs above once per run.
    pub cache: &'a RuleCache,
}

/// Values several rules derive from the same [`RuleContext`] inputs.
///
/// Each is built on first use and then shared (#1326). A context made from
/// another for a narrower resolution shares its cache: nothing here reads the
/// resolution.
#[derive(Debug, Default)]
pub struct RuleCache {
    pytest: Option<PytestImportPaths>,
    name_loaded: OnceLock<HashSet<FileId>>,
}

impl RuleCache {
    /// A cache starting from the pytest search paths import resolution built
    /// from the same sources, before any `sys.path` hint.
    #[must_use]
    pub(crate) fn with_pytest(pytest: PytestImportPaths) -> Self {
        Self {
            pytest: Some(pytest),
            name_loaded: OnceLock::new(),
        }
    }
}

impl<'a> RuleContext<'a> {
    /// Test-context files that reachable code loads only by a module name in
    /// a string (`import_module("tests.x")`, a loader API's `"tests.x:attr"`),
    /// never by an `import` statement or a framework glob, and that are not
    /// named like a test module or conftest. uvicorn's
    /// `tests/importer/raise_import_error.py` is one: a fixture a test loads
    /// to see what happens, often that its import fails (#1223).
    pub(crate) fn name_loaded_test_files(&self) -> HashSet<&'a str> {
        self.cache
            .name_loaded
            .get_or_init(|| self.find_name_loaded_test_files())
            .iter()
            .filter_map(|id| self.graph.file(*id))
            .map(|node| node.path.as_str())
            .collect()
    }

    fn find_name_loaded_test_files(&self) -> HashSet<FileId> {
        let pytest = self
            .cache
            .pytest
            .clone()
            .unwrap_or_else(|| PytestImportPaths::build(self.sources));
        let index = ModuleIndex::with_pytest(self.graph, self.sources, pytest);
        let reached: Vec<_> = self
            .parse
            .modules
            .iter()
            .filter(|module| {
                self.graph
                    .file_id(&module.path)
                    .is_some_and(|id| self.reachability.reachable.contains(&id))
            })
            .collect();
        // A loader name may end in attributes: the longest prefix that names
        // a file is the one loaded.
        let mut by_name: HashSet<FileId> = reached
            .iter()
            .flat_map(|module| {
                let from = module.path.as_str();
                let index = &index;
                module.dynamic_imports.iter().filter_map(move |dynamic| {
                    std::iter::successors(Some(dynamic.module.as_str()), |name| {
                        name.rsplit_once('.').map(|(head, _)| head)
                    })
                    .find_map(|name| index.resolve_from(from, name))
                })
            })
            .filter(|id| {
                !self.reachability.framework_used.contains(id)
                    && self.graph.file(*id).is_some_and(|node| {
                        file_context(&node.path, self.sources) == FileContext::Test
                            && !is_test_harness_name(file_name(&node.path))
                    })
            })
            .collect();
        if by_name.is_empty() {
            return HashSet::new();
        }
        // An import statement can only reach a file through a name ending in
        // its module name; resolving just those keeps this cheap.
        let stems: HashSet<&str> = by_name
            .iter()
            .filter_map(|id| self.graph.file(*id))
            .filter_map(|node| module_stem(&node.path))
            .collect();
        let names = |name: &str| stems.contains(name.rsplit('.').next().unwrap_or(name));
        for module in &reached {
            let from = module.path.as_str();
            for import in &module.imports {
                let submodule = import
                    .name
                    .as_deref()
                    .filter(|name| stems.contains(name))
                    .map(|name| format!("{}.{name}", import.module));
                let statement = module_and_parents(&import.module)
                    .filter(|name| names(name))
                    .chain(submodule.as_deref());
                for id in statement.filter_map(|name| index.resolve_from(from, name)) {
                    by_name.remove(&id);
                }
            }
        }
        by_name
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// The last segment of the module names a file answers to: its stem, or its
/// directory's name for a package `__init__`.
fn module_stem(path: &str) -> Option<&str> {
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    if stem == "__init__" {
        return dir.rsplit('/').next().filter(|dir| !dir.is_empty());
    }
    Some(stem)
}

/// Settings specific to dependency rules; symbol rules do not consume them.
pub struct DependencyRuleContext<'a> {
    /// Shared rule inputs.
    pub rules: &'a RuleContext<'a>,
    /// Effective chokkin configuration.
    pub config: &'a crate::config::ChokkinConfig,
    /// Strict mode (`--strict`).
    pub strict: bool,
    /// Root-relative file → normalized names the separate environment it
    /// runs in installs: a build script's `[build-system].requires` (#735),
    /// a subtree requirements file's names (#1340).
    pub build_scripts: &'a BTreeMap<String, BTreeSet<String>>,
    /// The root's workspace members and local `[tool.uv.sources]` trees.
    pub local: &'a crate::sources::LocalTrees,
}

#[cfg(test)]
mod tests {
    use super::module_stem;

    #[test]
    fn module_stem_is_the_last_segment_a_file_answers_to() {
        assert_eq!(
            module_stem("tests/importer/bad_module.py"),
            Some("bad_module")
        );
        assert_eq!(module_stem("tests/importer/__init__.py"), Some("importer"));
        assert_eq!(module_stem("broken.py"), Some("broken"));
        assert_eq!(module_stem("__init__.py"), None);
    }
}
