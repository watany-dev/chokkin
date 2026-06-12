//! Errors raised while discovering a Python project root.

use std::path::PathBuf;

/// Failure while walking upward for project root markers.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// No project marker found walking upward from `start`.
    #[error("no project root found from {start}")]
    NotFound {
        /// Directory where the upward walk began.
        start: PathBuf,
    },

    /// `start` does not exist or is not a directory.
    #[error("invalid start path: {path}")]
    InvalidStart {
        /// Rejected start path.
        path: PathBuf,
    },

    /// Filesystem I/O failure during marker probe.
    #[error("failed to read {path}")]
    Io {
        /// Path that triggered the I/O error.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
}
