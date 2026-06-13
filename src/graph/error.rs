//! Graph construction errors.

/// Errors while building or updating the project graph.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// The same file path was registered twice.
    #[error("duplicate file path `{path}`")]
    DuplicateFile {
        /// Root-relative path.
        path: String,
    },

    /// An internal graph invariant was violated.
    #[error("graph invariant violated: {detail}")]
    Invariant {
        /// Human-readable detail.
        detail: String,
    },
}
