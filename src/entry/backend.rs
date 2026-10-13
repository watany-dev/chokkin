//! PEP 517 build backends as entry roots (#782).

use crate::config::EntrySpec;
use crate::parser::{ParseSummary, ParsedModule};
use crate::sources::{DiscoveredFile, FileContext};

use super::types::{EntryOrigin, EntryPlan, EntryRoot};

/// The hooks PEP 517 requires of every build backend.
const REQUIRED_HOOKS: [&str; 2] = ["build_wheel", "build_sdist"];

/// Register every runtime module that provides the PEP 517 hooks as an entry
/// root of `plan`.
///
/// A frontend imports the module named by a `build-backend` key, so nothing in
/// the project imports it: poetry's `poetry.masonry.api` re-exports
/// `poetry.core`'s hooks for projects that still name it, and an in-tree
/// backend under `backend-path` defines them itself.
pub(crate) fn add_build_backend_roots(
    plan: &mut EntryPlan,
    parse: &ParseSummary,
    files: &[DiscoveredFile],
) {
    let mut added = false;
    for module in parse.modules.iter().filter(|module| provides_hooks(module)) {
        let Some(file) = files.iter().find(|file| file.path == module.path) else {
            continue;
        };
        if file.context != FileContext::Runtime {
            continue;
        }
        if let Some(root) = plan
            .roots
            .iter_mut()
            .find(|root| root.spec.path == module.path)
        {
            if !root.origins.contains(&EntryOrigin::BuildBackend) {
                root.origins.push(EntryOrigin::BuildBackend);
            }
            continue;
        }
        plan.roots.push(EntryRoot {
            spec: EntrySpec {
                path: module.path.clone(),
                symbol: None,
            },
            context: file.context,
            origins: vec![EntryOrigin::BuildBackend],
        });
        added = true;
    }
    if added {
        plan.roots
            .sort_by(|left, right| left.spec.path.cmp(&right.spec.path));
    }
}

/// Whether `module` defines or re-exports (`__all__`) every required hook.
fn provides_hooks(module: &ParsedModule) -> bool {
    REQUIRED_HOOKS.iter().all(|hook| {
        module.symbols.iter().any(|symbol| symbol.name == *hook)
            || module.exports.iter().any(|name| name == hook)
    })
}

#[cfg(test)]
mod tests {
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
            exports: exports.iter().map(|name| (*name).to_owned()).collect(),
            ..ParsedModule::default()
        }
    }

    fn roots_for(modules: Vec<ParsedModule>, files: &[(&str, FileContext)]) -> Vec<EntryRoot> {
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
            roots: Vec::new(),
            warnings: Vec::new(),
            library_members: Vec::new(),
        };
        add_build_backend_roots(&mut plan, &ParseSummary { modules }, &files);
        plan.roots
    }

    #[test]
    fn modules_providing_both_hooks_are_roots() {
        let roots = roots_for(
            vec![
                module(
                    "src/poetry/masonry/api.py",
                    &[],
                    &["build_sdist", "build_wheel"],
                ),
                module("_build/backend.py", &["build_wheel", "build_sdist"], &[]),
            ],
            &[
                ("src/poetry/masonry/api.py", FileContext::Runtime),
                ("_build/backend.py", FileContext::Runtime),
            ],
        );
        let paths: Vec<_> = roots.iter().map(|root| root.spec.path.as_str()).collect();
        assert_eq!(paths, ["_build/backend.py", "src/poetry/masonry/api.py"]);
        assert!(
            roots
                .iter()
                .all(|root| root.origins == [EntryOrigin::BuildBackend])
        );
    }

    #[test]
    fn one_hook_or_a_test_module_is_not_a_root() {
        let roots = roots_for(
            vec![
                module("src/acme/wheel.py", &["build_wheel"], &[]),
                module(
                    "tests/test_backend.py",
                    &["build_wheel", "build_sdist"],
                    &[],
                ),
            ],
            &[
                ("src/acme/wheel.py", FileContext::Runtime),
                ("tests/test_backend.py", FileContext::Test),
            ],
        );
        assert!(roots.is_empty(), "{roots:#?}");
    }
}
