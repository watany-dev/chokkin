//! Files with a `__main__` block as entry roots (#1026).

use crate::config::EntrySpec;
use crate::parser::ParseSummary;
use crate::sources::DiscoveredFile;

use super::types::{EntryCandidate, EntryOrigin, EntryPlan};

pub(crate) const MAIN_BLOCK_RULE: &str = "main-block";

/// Register every file with a module-level `if __name__ == "__main__":`
/// block as an entry root of `plan`.
///
/// Such a file is written to be run as a script (`python path/to/file.py`),
/// so what it imports is used even when nothing imports the file: fastmcp's
/// `contrib/mcp_mixin/example.py`, airflow's `gunicorn_config.py`. The rule
/// reads the file's content, not its name, so it applies at any depth; it
/// joins after mode resolution, which reads file names only, so a deep
/// `app.py` with a block does not make a library an app. A guard inside a
/// function or class body says nothing about how the file is run. A file
/// the parser could not read, or a guard it does not recognize
/// (`__name__ in (…)`), is no entry: the file stays an orphan, so a miss is
/// a report, not a silence.
pub(crate) fn add_main_block_roots(
    plan: &mut EntryPlan,
    parse: &ParseSummary,
    files: &[DiscoveredFile],
    production: bool,
) {
    for module in parse.modules.iter().filter(|module| module.has_main_block) {
        let Some(file) = files.iter().find(|file| file.path == module.path) else {
            continue;
        };
        if production && !file.context.is_included_in_production() {
            continue;
        }
        plan.push_candidate(EntryCandidate {
            spec: EntrySpec {
                path: module.path.clone(),
                symbol: None,
            },
            context: file.context,
            origin: EntryOrigin::Auto {
                rule: MAIN_BLOCK_RULE.to_owned(),
            },
        });
    }
    plan.roots.sort_by(|left, right| left.path.cmp(&right.path));
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::types::EntryRoot;
    use super::*;
    use crate::config::ProjectMode;
    use crate::parser::ParsedModule;
    use crate::sources::{FileContext, FileKind};

    fn module(path: &str, has_main_block: bool) -> ParsedModule {
        ParsedModule {
            path: path.to_owned(),
            has_main_block,
            ..ParsedModule::default()
        }
    }

    fn roots_for(
        roots: Vec<EntryRoot>,
        modules: Vec<ParsedModule>,
        files: &[(&str, FileContext)],
        production: bool,
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
            member_mode_reasons: Vec::new(),
        };
        add_main_block_roots(&mut plan, &ParseSummary { modules }, &files, production);
        plan.roots
    }

    #[test]
    fn files_with_a_main_block_become_roots_in_their_own_context() {
        let roots = roots_for(
            Vec::new(),
            vec![
                module("src/acme/deep/example.py", true),
                module("src/acme/lib.py", false),
                module("tests/smoke.py", true),
                module("gone.py", true),
            ],
            &[
                ("src/acme/deep/example.py", FileContext::Runtime),
                ("src/acme/lib.py", FileContext::Runtime),
                ("tests/smoke.py", FileContext::Test),
            ],
            false,
        );
        let summary: Vec<_> = roots
            .iter()
            .map(|root| (root.path.as_str(), root.context, root.origins.clone()))
            .collect();
        let origin = || {
            vec![EntryOrigin::Auto {
                rule: "main-block".to_owned(),
            }]
        };
        assert_eq!(
            summary,
            [
                ("src/acme/deep/example.py", FileContext::Runtime, origin()),
                ("tests/smoke.py", FileContext::Test, origin()),
            ]
        );
    }

    #[test]
    fn production_drops_blocks_outside_runtime_code() {
        let roots = roots_for(
            Vec::new(),
            vec![
                module("tests/smoke.py", true),
                module("src/acme/x.py", true),
            ],
            &[
                ("tests/smoke.py", FileContext::Test),
                ("src/acme/x.py", FileContext::Runtime),
            ],
            true,
        );
        let paths: Vec<_> = roots.iter().map(|root| root.path.as_str()).collect();
        assert_eq!(paths, ["src/acme/x.py"]);
    }

    #[test]
    fn an_existing_root_gains_the_origin() {
        let existing = EntryRoot {
            path: "src/acme/cli.py".to_owned(),
            symbols: BTreeSet::new(),
            context: FileContext::Runtime,
            origins: vec![EntryOrigin::Config],
        };
        let roots = roots_for(
            vec![existing],
            vec![module("src/acme/cli.py", true)],
            &[("src/acme/cli.py", FileContext::Runtime)],
            false,
        );
        assert_eq!(roots.len(), 1);
        assert_eq!(
            roots[0].origins,
            [
                EntryOrigin::Config,
                EntryOrigin::Auto {
                    rule: "main-block".to_owned()
                }
            ]
        );
    }
}
