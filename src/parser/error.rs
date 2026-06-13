//! Python parse errors.

use std::path::PathBuf;

/// Fatal errors while reading or parsing a Python file.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// Failed to read source from disk.
    #[error("failed to read `{path}`")]
    Io {
        /// Absolute or root-relative path.
        path: PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
}
