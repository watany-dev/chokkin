//! PEP 723 scripts as entry roots.

use crate::config::EntrySpec;
use crate::manifest::InlineScript;
use crate::sources::{DiscoveredSources, FileContext};

use super::types::{EntryCandidate, EntryOrigin, EntryPlan};

/// Register every PEP 723 script as an entry root of `plan`.
///
/// A script is run directly, so files it imports are reachable even when no
/// other entry reaches them.
pub(crate) fn add_script_roots(
    plan: &mut EntryPlan,
    scripts: &[InlineScript],
    sources: &DiscoveredSources,
    production: bool,
) {
    for script in scripts {
        let context = sources
            .files
            .iter()
            .find(|file| file.path == script.path)
            .map_or(FileContext::Runtime, |file| file.context);
        if production && !context.is_included_in_production() {
            continue;
        }
        plan.push_candidate(EntryCandidate {
            spec: EntrySpec {
                path: script.path.clone(),
                symbol: None,
            },
            context,
            origin: EntryOrigin::Script,
        });
    }
    plan.roots.sort_by(|left, right| left.path.cmp(&right.path));
}

#[cfg(test)]
mod tests {
    use crate::config::ProjectMode;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::sources::{DiscoveredFile, FileKind, LayoutInfo};

    use super::*;

    fn script(path: &str) -> InlineScript {
        InlineScript {
            path: path.to_owned(),
            dependencies: Vec::new(),
            requires_python: None,
            target_version: None,
            invalid_requirements: Vec::new(),
        }
    }

    fn roots(production: bool) -> Vec<(String, FileContext)> {
        let file = |path: &str, context| DiscoveredFile {
            path: path.to_owned(),
            kind: FileKind::Python,
            context,
        };
        let sources = DiscoveredSources {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
            },
            layout: LayoutInfo::default(),
            effective_globs: Vec::new(),
            files: vec![
                file("tools/run.py", FileContext::Runtime),
                file("tests/gen.py", FileContext::Test),
                file("scripts/dev.py", FileContext::Dev),
            ],
            warnings: Vec::new(),
        };
        let mut plan = EntryPlan {
            mode: ProjectMode::App,
            roots: Vec::new(),
            warnings: Vec::new(),
            library_members: Vec::new(),
            distributed_members: Vec::new(),
        };
        let scripts = [
            script("tests/gen.py"),
            script("tools/run.py"),
            script("scripts/dev.py"),
            script("undiscovered.py"),
        ];
        add_script_roots(&mut plan, &scripts, &sources, production);
        plan.roots
            .into_iter()
            .map(|root| (root.path, root.context))
            .collect()
    }

    /// Each script takes its own file's context, and `--production` drops
    /// the scripts outside runtime context (#1136).
    #[test]
    fn production_keeps_only_runtime_scripts() {
        let runtime = |path: &str| (path.to_owned(), FileContext::Runtime);
        assert_eq!(
            roots(false),
            [
                ("scripts/dev.py".to_owned(), FileContext::Dev),
                ("tests/gen.py".to_owned(), FileContext::Test),
                runtime("tools/run.py"),
                runtime("undiscovered.py"),
            ]
        );
        assert_eq!(
            roots(true),
            [runtime("tools/run.py"), runtime("undiscovered.py")]
        );
    }
}
