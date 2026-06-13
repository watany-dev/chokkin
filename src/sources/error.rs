//! Source file discovery errors.

use std::path::PathBuf;

/// Fatal errors during source file discovery.
#[derive(Debug, thiserror::Error)]
pub enum SourcesError {
    /// A glob pattern could not be compiled.
    #[error("invalid glob pattern `{pattern}`: {reason}")]
    InvalidGlob {
        /// The invalid pattern string.
        pattern: String,
        /// Compiler error message.
        reason: String,
    },

    /// Filesystem failure while walking the project tree.
    #[error("failed to read project root `{path}`")]
    Io {
        /// Project root path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}
