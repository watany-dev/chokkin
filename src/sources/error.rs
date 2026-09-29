//! Source file discovery errors.

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
}
