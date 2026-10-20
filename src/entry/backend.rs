//! PEP 517 build backends as entry roots (#782).

use crate::config::EntrySpec;
use crate::parser::{ParseSummary, ParsedModule};
use crate::sources::{DiscoveredFile, FileContext};

use super::types::{EntryCandidate, EntryOrigin, EntryPlan};

/// The hooks PEP 517 requires of every build backend.
const REQUIRED_HOOKS: [&str; 2] = ["build_wheel", "build_sdist"];

/// Register every runtime module that provides the PEP 517 hooks as an entry
/// root of `plan`.
///
/// A frontend imports the module named by a `build-backend` key, so nothing in
/// the project imports it: poetry's `poetry.masonry.api` re-exports
/// `poetry.core`'s hooks for projects that still name it. An in-tree backend
/// under `backend-path` is a dev build script instead (`parse_in_tree_backend`).
pub(crate) fn add_build_backend_roots(
    plan: &mut EntryPlan,
    parse: &ParseSummary,
    files: &[DiscoveredFile],
) {
    for module in parse.modules.iter().filter(|module| provides_hooks(module)) {
        let Some(file) = files.iter().find(|file| file.path == module.path) else {
            continue;
        };
        if file.context != FileContext::Runtime {
            continue;
        }
        plan.push_candidate(EntryCandidate {
            spec: EntrySpec {
                path: module.path.clone(),
                symbol: None,
            },
            context: file.context,
            origin: EntryOrigin::BuildBackend,
        });
    }
    plan.roots.sort_by(|left, right| left.path.cmp(&right.path));
}

/// Whether `module` defines or re-exports (`__all__`) every required hook.
fn provides_hooks(module: &ParsedModule) -> bool {
    REQUIRED_HOOKS.iter().all(|hook| {
        module.symbols.iter().any(|symbol| symbol.name == *hook) || module.exports.contains(hook)
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::types::EntryRoot;
    use super::*;
    use crate::config::ProjectMode;
    use crate::parser::{SymbolDef, SymbolKind};
    use crate::sources::FileKind;

    fn module(path: &str, defined: &[&str], exports: &[&str]) -> ParsedModule {
        ParsedModule {
            path: path.to_owned(),
            symbols: defined
                .iter()
                .map(|name| SymbolDef {
                    name: (*name).to_owned(),
                    kind: SymbolKind::Function,
                    line: 1,
                    is_public: true,
                    decorators: Vec::new(),
                    in_type_checking: false,
                    used_in_module: false,
                })
                .collect(),
            exports: crate::parser::Exports::Known(
                exports.iter().map(|name| (*name).to_owned()).collect(),
            ),
            ..ParsedModule::default()
        }
    }

    fn roots_for(
        roots: Vec<EntryRoot>,
        modules: Vec<ParsedModule>,
        files: &[(&str, FileContext)],
    ) -> Vec<EntryRoot> {
        let files: Vec<_> = files
            .iter()
            .map(|(path, context)| DiscoveredFile {
                path: (*path).to_owned(),
                kind: FileKind::Python,
                context: *context,
            })
            .collect();
        let mut plan = EntryPlan {
            mode: ProjectMode::App,
            roots,
            warnings: Vec::new(),
            library_members: Vec::new(),
            distributed_members: Vec::new(),
        };
        add_build_backend_roots(&mut plan, &ParseSummary { modules }, &files);
        plan.roots
    }

    #[test]
    fn one_hook_other_exports_or_a_test_module_is_not_a_root() {
        let roots = roots_for(
            Vec::new(),
            vec![
                module("src/acme/wheel.py", &["build_wheel"], &[]),
                module("src/acme/util.py", &[], &["helper"]),
                module(
                    "tests/test_backend.py",
                    &["build_wheel", "build_sdist"],
                    &[],
                ),
            ],
            &[
                ("src/acme/wheel.py", FileContext::Runtime),
                ("src/acme/util.py", FileContext::Runtime),
                ("tests/test_backend.py", FileContext::Test),
            ],
        );
        assert!(roots.is_empty(), "{roots:#?}");
    }

    #[test]
    fn an_existing_root_gains_the_origin_once() {
        let existing = EntryRoot {
            path: "src/poetry/masonry/api.py".to_owned(),
            symbols: BTreeSet::new(),
            context: FileContext::Runtime,
            origins: vec![EntryOrigin::Config],
        };
        let modules = || {
            vec![
                module(
                    "src/poetry/masonry/api.py",
                    &[],
                    &["build_sdist", "build_wheel"],
                ),
                module("src/acme/backend.py", &["build_wheel", "build_sdist"], &[]),
            ]
        };
        let files = [
            ("src/poetry/masonry/api.py", FileContext::Runtime),
            ("src/acme/backend.py", FileContext::Runtime),
        ];
        let once = roots_for(vec![existing], modules(), &files);
        let roots = roots_for(once, modules(), &files);
        let paths: Vec<_> = roots.iter().map(|root| root.path.as_str()).collect();
        assert_eq!(paths, ["src/acme/backend.py", "src/poetry/masonry/api.py"]);
        assert_eq!(roots[0].origins, [EntryOrigin::BuildBackend]);
        assert_eq!(
            roots[1].origins,
            [EntryOrigin::Config, EntryOrigin::BuildBackend]
        );
    }
}
