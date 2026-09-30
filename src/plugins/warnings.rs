//! Non-fatal plugin extraction warnings.

use crate::config::PluginId;

/// Non-fatal conditions during plugin hint extraction.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PluginsWarning {
    /// `settings.py` found but list literals could not be parsed.
    #[error("plugin: partial Django settings parse at `{path}` (fields: {})", .fields.join(", "))]
    PartialSettingsParse {
        /// Root-relative settings path.
        path: String,
        /// Field names that could not be fully parsed.
        fields: Vec<String>,
    },
    /// `pytest.ini` exists but `[pytest]` section missing.
    #[error("plugin: unreadable pytest config `{path}`")]
    PytestConfigUnreadable {
        /// Root-relative config path.
        path: String,
    },
    /// Multiple `settings.py` candidates; first chosen.
    #[error("plugin: ambiguous Django settings; chose `{chosen}` from {candidates:?}")]
    AmbiguousSettings {
        /// Chosen settings path.
        chosen: String,
        /// Other candidate paths.
        candidates: Vec<String>,
    },
    /// Plugin extractor failed non-fatally; analysis continues.
    #[error("plugin: `{}` extraction failed: {detail}", .plugin.as_key())]
    PluginExtractFailed {
        /// Plugin that failed.
        plugin: PluginId,
        /// Failure detail.
        detail: String,
    },
}
