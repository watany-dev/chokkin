//! Reachability analysis errors.

use thiserror::Error;

/// Fatal reachability analysis failure.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReachabilityError {
    /// A framework-used glob pattern could not be compiled.
    #[error("invalid framework glob `{pattern}`: {reason}")]
    InvalidFrameworkGlob {
        /// Glob pattern string.
        pattern: String,
        /// Compilation failure reason.
        reason: String,
    },
}
