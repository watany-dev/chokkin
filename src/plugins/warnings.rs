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
    /// Django is in use but its settings could not be found or read, so what
    /// they would have explained is unknown (#914).
    #[error("plugin: django: {reason}; unused files and dependencies are reported below certain")]
    DjangoSettingsUnread {
        /// Root-relative file the reason points into.
        path: String,
        /// Its line, when the reason names one.
        line: Option<u32>,
        /// What could not be found or read.
        reason: SettingsUnread,
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

/// Why the Django settings could not be found or read (#914).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsUnread {
    /// `DJANGO_SETTINGS_MODULE` names a module that is not a project file.
    #[error("settings module `{module}` set in `{site}` is not a project file")]
    MissingModule {
        /// The module named.
        module: String,
        /// Where it is set, as `path:line`.
        site: String,
    },
    /// `DJANGO_SETTINGS_MODULE` is set in a form that could not be read, and
    /// no `settings.py` was found.
    #[error(
        "`{site}` sets DJANGO_SETTINGS_MODULE in a form that could not be read, and no settings.py was found"
    )]
    UnreadModule {
        /// Where it is set, as `path:line`.
        site: String,
    },
    /// `manage.py` names no settings module and no `settings.py` was found.
    #[error("`manage.py` sets no DJANGO_SETTINGS_MODULE and no settings.py was found")]
    NoSettings,
    /// The settings module could not be read or parsed.
    #[error("settings `{path}` could not be read")]
    Unreadable {
        /// Root-relative settings path.
        path: String,
    },
    /// The settings star-import a module that is not a project file, or one
    /// past the files the plugin follows, which may set a field it reads.
    #[error("settings `{site}` star-imports `{module}`, which was not read")]
    UnfollowedImport {
        /// The module star-imported.
        module: String,
        /// Where, as `path:line`.
        site: String,
    },
    /// The settings call `include()` or `exec()`, which runs a file the
    /// plugin does not read and may set a field it reads (#925).
    #[error("settings `{site}` calls `{call}`, which was not read")]
    UnreadCall {
        /// The call, as `include()`.
        call: String,
        /// Where, as `path:line`.
        site: String,
    },
}
