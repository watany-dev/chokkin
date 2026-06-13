//! Entry root construction errors.

/// Fatal errors while building entry roots.
#[derive(Debug, thiserror::Error)]
pub enum EntryError {
    /// An internal invariant was violated.
    #[error("entry plan invariant violated: {detail}")]
    Invariant {
        /// Human-readable detail.
        detail: String,
    },
}
