//! A notebook that installs a distribution declares it for its own imports
//! (#991).
//!
//! `%pip install X` puts `X` into the kernel the notebook runs in, so the
//! notebook's import of `X` needs no project declaration: its CHK003, CHK004
//! and CHK005 are dropped, and so is the CHK010 of an import the notebook
//! installs by name. The declaration stays inside the file, like a PEP 723
//! block's, and is not a use: a project declaration of `X` is not used by it.
//! An argument that names no distribution is the `UnreadNotebookInstall`
//! gap instead.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::manifest::normalize_distribution_name;
use crate::parser::{ParseSummary, ParsedModule};
use crate::resolver::ResolvedImport;

/// The distributions each notebook installs.
pub(crate) struct NotebookInstalls<'a> {
    by_file: FxHashMap<&'a str, (&'a ParsedModule, FxHashSet<&'a str>)>,
}

impl<'a> NotebookInstalls<'a> {
    pub(crate) fn new(parse: &'a ParseSummary) -> Self {
        let by_file = parse
            .modules
            .iter()
            .filter_map(|module| {
                let installed: FxHashSet<&str> = module
                    .notebook_installs
                    .iter()
                    .filter_map(|install| install.distribution.as_deref())
                    .collect();
                (!installed.is_empty()).then_some((module.path.as_str(), (module, installed)))
            })
            .collect();
        Self { by_file }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.by_file.is_empty()
    }

    /// Whether the importing notebook installs the distribution the import
    /// resolved to, or one its module names (`llama-index-llms-openai` for
    /// `llama_index.llms.openai`).
    pub(crate) fn provides(&self, import: &ResolvedImport) -> bool {
        let Some((_, installed)) = self.by_file.get(import.file.as_str()) else {
            return false;
        };
        import
            .distribution
            .iter()
            .map(|name| normalize_distribution_name(name))
            .chain(module_names(&import.full_module))
            .any(|name| installed.contains(name.as_str()))
    }

    /// Whether the notebook `file` installs a distribution that a module the
    /// unresolved import of `root` at `line` names: the module as the
    /// statement there wrote it, or the root alone.
    pub(crate) fn provides_unresolved(&self, file: &str, root: &str, line: Option<u32>) -> bool {
        let Some((module, installed)) = self.by_file.get(file) else {
            return false;
        };
        let written = module
            .imports
            .iter()
            .filter(|import| Some(import.line) == line && import_root(&import.module) == root)
            .map(|import| import.module.as_str());
        written
            .chain([root])
            .flat_map(module_names)
            .any(|name| installed.contains(name.as_str()))
    }
}

fn import_root(module: &str) -> &str {
    module.split('.').next().unwrap_or(module)
}

/// Each dotted prefix of `module` as a distribution name, longest first:
/// `a.b_c` gives `a-b-c` and `a`.
fn module_names(module: &str) -> impl Iterator<Item = String> + '_ {
    module
        .match_indices('.')
        .map(|(at, _)| &module[..at])
        .chain([module])
        .filter(|prefix| !prefix.is_empty())
        .map(normalize_distribution_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_names_are_the_dotted_prefixes_normalized() {
        assert_eq!(
            module_names("llama_index.llms.openai").collect::<Vec<_>>(),
            ["llama-index", "llama-index-llms", "llama-index-llms-openai"]
        );
        assert_eq!(module_names("dotenv").collect::<Vec<_>>(), ["dotenv"]);
    }
}
