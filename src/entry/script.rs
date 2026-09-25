//! PEP 723 scripts as entry roots.

use crate::config::EntrySpec;
use crate::manifest::InlineScript;
use crate::sources::{DiscoveredSources, FileContext};

use super::types::{EntryOrigin, EntryPlan, EntryRoot};

/// Register every PEP 723 script as an entry root of `plan`.
///
/// A script is run directly, so files it imports are reachable even when no
/// other entry reaches them.
pub fn add_script_roots(
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
        if let Some(root) = plan
            .roots
            .iter_mut()
            .find(|root| root.spec.path == script.path)
        {
            if !root.origins.contains(&EntryOrigin::Script) {
                root.origins.push(EntryOrigin::Script);
            }
            continue;
        }
        plan.roots.push(EntryRoot {
            spec: EntrySpec {
                path: script.path.clone(),
                symbol: None,
            },
            context,
            origins: vec![EntryOrigin::Script],
        });
    }
    plan.roots
        .sort_by(|left, right| left.spec.path.cmp(&right.spec.path));
}
