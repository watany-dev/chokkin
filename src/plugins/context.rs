//! Read-only inputs for plugin extractors.

use crate::config::YokeiConfig;
use crate::discovery::ProjectRoot;
use crate::manifest::LoadedManifest;
use crate::sources::DiscoveredSources;

/// Read-only inputs for plugin extractors.
pub struct PluginContext<'a> {
    /// Discovered project root.
    pub root: &'a ProjectRoot,
    /// Effective yokei configuration.
    #[allow(dead_code)] // reserved for plugin-specific config overrides
    pub config: &'a YokeiConfig,
    /// Discovered source files.
    pub sources: &'a DiscoveredSources,
    /// Extracted manifest.
    pub manifest: &'a LoadedManifest,
    // Step 6+: `parsed_files: Option<&ParsedFileCache>`
}
