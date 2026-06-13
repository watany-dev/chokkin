//! Plugin extraction errors.

use std::path::PathBuf;

/// Fatal errors during plugin hint extraction.
#[derive(Debug, thiserror::Error)]
pub enum PluginsError {
    /// Failed to read a configuration file.
    #[error("failed to read `{path}`")]
    Io {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Configuration file could not be parsed.
    #[error("invalid config at `{path}`: {detail}")]
    InvalidConfig {
        /// Root-relative config path.
        path: String,
        /// Human-readable detail.
        detail: String,
    },
}
