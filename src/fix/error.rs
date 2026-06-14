//! Fix error types.

use thiserror::Error;

/// Errors that prevent fix application entirely.
#[derive(Debug, Error)]
pub enum FixError {
    /// I/O failure while reading or writing a manifest file.
    #[error("failed to edit {path}: {source}")]
    Io {
        /// Manifest path.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// `pyproject.toml` could not be parsed for editing.
    #[error("invalid TOML in {path}: {detail}")]
    InvalidToml {
        /// Manifest path.
        path: String,
        /// Parse detail.
        detail: String,
    },
    /// Requested fix mode is not supported in v0.1.
    #[error("{detail}")]
    Unsupported {
        /// Explanation.
        detail: String,
    },
}
