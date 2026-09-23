//! Read-only inputs for plugin extractors.

use crate::config::ChokkinConfig;
use crate::discovery::ProjectRoot;
use crate::manifest::LoadedManifest;
use crate::parser::ParseSummary;
use crate::sources::DiscoveredSources;

/// Read-only inputs for plugin extractors.
pub struct PluginContext<'a> {
    /// Discovered project root.
    pub root: &'a ProjectRoot,
    /// Effective chokkin configuration.
    pub config: &'a ChokkinConfig,
    /// Discovered source files.
    pub sources: &'a DiscoveredSources,
    /// Extracted manifest.
    pub manifest: &'a LoadedManifest,
    /// Step 6 parse output.
    pub parse: &'a ParseSummary,
}
