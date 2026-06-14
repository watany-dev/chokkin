//! Baseline file data structures and errors.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Baseline file schema written by `--update-baseline`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineFile {
    /// chokkin version that generated the file.
    pub chokkin_version: String,
    /// Generation time as `unix:<seconds>` in the v0.2 draft schema.
    pub generated_at: String,
    /// Frozen issue entries.
    pub issues: Vec<BaselineEntry>,
}

/// One frozen issue fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineEntry {
    /// Stable issue fingerprint.
    pub fingerprint: String,
    /// Rule code, duplicated for reviewability.
    pub code: String,
    /// Stable target identifier used by the fingerprint.
    pub target: String,
}

/// Result of applying or updating a baseline.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BaselineReport {
    /// Baseline file path relative to the analysis root when available.
    pub path: Option<String>,
    /// Issues suppressed because their fingerprint was present.
    pub suppressed: u32,
    /// Issues written by `--update-baseline`.
    pub written: u32,
}

/// Fatal baseline read/write error.
#[derive(Debug, Error)]
pub enum BaselineError {
    /// Baseline path escapes the project root.
    #[error("baseline path `{path}` must stay inside the project root")]
    OutsideRoot {
        /// User-provided path.
        path: String,
    },
    /// I/O failure.
    #[error("failed to access baseline `{path}`: {source}")]
    Io {
        /// Baseline path.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// JSON parse or serialization failure.
    #[error("invalid baseline `{path}`: {detail}")]
    Json {
        /// Baseline path.
        path: String,
        /// Parse/serialization detail.
        detail: String,
    },
}
