//! Resolver errors.

/// Fatal errors during import resolution.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// Internal invariant violated.
    #[error("resolver invariant: {detail}")]
    Invariant {
        /// Human-readable detail.
        detail: String,
    },
}
