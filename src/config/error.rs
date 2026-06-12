//! Errors raised while loading yokei configuration.

use std::path::PathBuf;

/// Failure while reading or validating configuration files.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Filesystem I/O failure while reading a config file.
    #[error("failed to read {path}")]
    Io {
        /// Path that triggered the I/O error.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// TOML syntax or type error in a config file.
    #[error("invalid TOML in {path}: {message}")]
    InvalidToml {
        /// Config file path.
        path: PathBuf,
        /// Human-readable parse error.
        message: String,
    },

    /// Semantic validation failed for a config value.
    #[error("invalid config at {path}.{field}: {message}")]
    Validation {
        /// Config file path.
        path: PathBuf,
        /// Dotted field path, e.g. `entry[0]`.
        field: String,
        /// Human-readable validation error.
        message: String,
    },

    /// Unknown key in a yokei config table such as `[tool.yokei]` or `plugins`.
    #[error("unknown config key {key} in {path}")]
    UnknownKey {
        /// Config file path.
        path: PathBuf,
        /// Unrecognized key name.
        key: String,
    },
}
