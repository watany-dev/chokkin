//! First-party ASGI / WSGI apps named by deployment commands (#1004).

use std::collections::{BTreeMap, BTreeSet};

use crate::config::EntrySpec;
use crate::parser::ParseSummary;
use crate::plugins::{PluginHints, parse_server_target};
use crate::sources::{DiscoveredSources, FileContext};

use super::module::{resolve_module_on_disk, resolve_module_to_path};
use super::types::{EntryCandidate, EntryOrigin, EntryPlan, EntryWarning};

pub(crate) fn add_server_roots(
    plan: &mut EntryPlan,
    parse: &ParseSummary,
    plugins: &PluginHints,
    sources: &DiscoveredSources,
    production: bool,
) {
    if plugins.config_server_refs.is_empty()
        && parse
            .modules
            .iter()
            .all(|module| module.uvicorn_apps.is_empty())
    {
        return;
    }
    let contexts: BTreeMap<_, _> = sources
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.context))
        .collect();
    let paths: BTreeSet<_> = sources.files.iter().map(|file| file.path.clone()).collect();
    let commands = plugins.config_server_refs.iter().map(|reference| {
        (
            reference.module.clone(),
            reference.symbol.clone(),
            reference.origin.file.as_str(),
            reference.origin.line,
            reference.origin.label.as_str(),
        )
    });
    let calls = parse.modules.iter().flat_map(|file| {
        file.uvicorn_apps.iter().filter_map(|reference| {
            let (module, symbol) = parse_server_target(&reference.text)?;
            Some((
                module,
                symbol,
                file.path.as_str(),
                Some(reference.line),
                "uvicorn.run",
            ))
        })
    });
    for (module, symbol, file, line, label) in commands.chain(calls) {
        let origin_context = contexts.get(file).copied().unwrap_or(FileContext::Runtime);
        if production && !origin_context.is_included_in_production() {
            continue;
        }
        let Some(path) = resolve_module_to_path(&module, &sources.layout, &paths) else {
            // Unresolved targets run no first-party code. An excluded file,
            // however, has unread imports and takes the usual skipped-entry gap.
            if let Some(path) = resolve_module_on_disk(&module, &sources.layout, &sources.root.path)
                && (!production
                    || crate::sources::assign_file_context(&path).is_included_in_production())
            {
                plan.warnings.push(EntryWarning::MissingEntryPath { path });
            }
            continue;
        };
        let context = if origin_context == FileContext::Runtime {
            contexts
                .get(path.as_str())
                .copied()
                .unwrap_or(FileContext::Runtime)
        } else {
            origin_context
        };
        if production && !context.is_included_in_production() {
            continue;
        }
        let site = line.map_or_else(|| file.to_owned(), |line| format!("{file}:{line}"));
        plan.push_candidate(EntryCandidate {
            spec: EntrySpec {
                path,
                symbol: Some(symbol.clone()),
            },
            context,
            origin: EntryOrigin::SymbolRef {
                module,
                symbol,
                label: format!("{label} ({site})"),
            },
        });
    }
    plan.roots.sort_by(|left, right| left.path.cmp(&right.path));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ProjectMode, TargetVersion};
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::parser::parse_file;
    use crate::sources::{DiscoveredFile, FileKind, LayoutInfo};

    fn plan_for(caller: FileContext, target: FileContext, production: bool) -> EntryPlan {
        let temp = tempfile::tempdir().expect("tempdir");
        let caller_path = match caller {
            FileContext::Runtime => "caller.py",
            FileContext::Test => "tests/test_server.py",
            FileContext::Dev => "scripts/serve.py",
            FileContext::Docs => "docs/serve.py",
        };
        let (target_path, module) = match target {
            FileContext::Runtime => ("acme/api.py", "acme.api"),
            FileContext::Test => ("tests/api.py", "tests.api"),
            FileContext::Dev => ("scripts/api.py", "scripts.api"),
            FileContext::Docs => ("docs/api.py", "docs.api"),
        };
        for (path, text) in [
            (
                caller_path,
                format!("import uvicorn\nuvicorn.run('{module}:app')\n"),
            ),
            (target_path, "app = None\n".to_owned()),
        ] {
            let path = temp.path().join(path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, text).expect("write");
        }
        let sources = DiscoveredSources {
            root: ProjectRoot {
                path: temp.path().to_path_buf(),
                marker: RootMarker::PyProjectToml,
            },
            layout: LayoutInfo::default(),
            files: [(caller_path, caller), (target_path, target)]
                .into_iter()
                .map(|(path, context)| DiscoveredFile {
                    path: path.to_owned(),
                    context,
                    kind: FileKind::Python,
                })
                .collect(),
            effective_globs: Vec::new(),
            warnings: Vec::new(),
        };
        let parse = ParseSummary {
            modules: vec![
                parse_file(
                    &sources.root,
                    caller_path,
                    &sources.layout,
                    caller,
                    &TargetVersion::default_py311(),
                )
                .expect("parse"),
            ],
        };
        let mut plan = EntryPlan {
            mode: ProjectMode::App,
            roots: Vec::new(),
            warnings: Vec::new(),
            library_members: Vec::new(),
        };
        add_server_roots(
            &mut plan,
            &parse,
            &PluginHints::default(),
            &sources,
            production,
        );
        plan
    }

    #[test]
    fn server_roots_preserve_caller_context_before_production_source_filtering() {
        for context in [
            FileContext::Runtime,
            FileContext::Test,
            FileContext::Dev,
            FileContext::Docs,
        ] {
            let plan = plan_for(context, FileContext::Runtime, false);
            assert_eq!(plan.roots.len(), 1);
            assert_eq!(plan.roots[0].context, context);
            assert_eq!(plan.roots[0].path, "acme/api.py");
            let production = plan_for(context, FileContext::Runtime, true);
            assert_eq!(
                production.roots.len(),
                usize::from(context == FileContext::Runtime)
            );
        }
    }

    #[test]
    fn runtime_server_calls_preserve_and_filter_the_target_context() {
        for context in [
            FileContext::Runtime,
            FileContext::Test,
            FileContext::Dev,
            FileContext::Docs,
        ] {
            let plan = plan_for(FileContext::Runtime, context, false);
            assert_eq!(plan.roots.len(), 1);
            assert_eq!(plan.roots[0].context, context);
            let production = plan_for(FileContext::Runtime, context, true);
            assert_eq!(
                production.roots.len(),
                usize::from(context == FileContext::Runtime)
            );
        }
    }
}
