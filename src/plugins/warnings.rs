//! Non-fatal plugin extraction warnings.

use crate::config::PluginId;

/// Non-fatal conditions during plugin hint extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginsWarning {
    /// Plugin enabled but no recognizable config found.
    PluginNoOp {
        /// Plugin that produced no hints.
        plugin: PluginId,
    },
    /// `settings.py` found but list literals could not be parsed.
    PartialSettingsParse {
        /// Root-relative settings path.
        path: String,
        /// Field names that could not be fully parsed.
        fields: Vec<String>,
    },
    /// `pytest.ini` exists but `[pytest]` section missing.
    PytestConfigUnreadable {
        /// Root-relative config path.
        path: String,
    },
    /// Multiple `settings.py` candidates; first chosen.
    AmbiguousSettings {
        /// Chosen settings path.
        chosen: String,
        /// Other candidate paths.
        candidates: Vec<String>,
    },
    /// Plugin extractor failed non-fatally; analysis continues.
    PluginExtractFailed {
        /// Plugin that failed.
        plugin: PluginId,
        /// Failure detail.
        detail: String,
    },
}
