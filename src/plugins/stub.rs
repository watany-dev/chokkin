//! No-op stubs for v0.2 plugins.

use crate::config::PluginId;

use super::context::PluginContext;
use super::types::PluginContribution;
use super::warnings::PluginsWarning;

/// Return an empty contribution for plugins not yet implemented.
pub fn extract(
    plugin: PluginId,
    _ctx: &PluginContext<'_>,
) -> (PluginContribution, Vec<PluginsWarning>) {
    (
        PluginContribution::empty(plugin),
        vec![PluginsWarning::PluginNoOp { plugin }],
    )
}
