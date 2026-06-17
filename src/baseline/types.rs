//! Baseline file data structures and errors.

use serde::{Deserialize, Serialize};

use crate::schema::{BASELINE_DRAFT_SCHEMA_VERSION, BASELINE_SCHEMA_VERSION};

/// Baseline file schema written by `--update-baseline`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineFile {
    /// Baseline schema version (`"1"` in v0.3; omitted files are v0.2 draft).
    #[serde(default = "default_baseline_schema_version")]
    pub schema_version: String,
    /// chokkin version that generated the file.
    pub chokkin_version: String,
    /// Generation time as `unix:<seconds>` in the v0.2 draft schema.
    pub generated_at: String,
    /// Frozen issue entries.
    pub issues: Vec<BaselineEntry>,
}

fn default_baseline_schema_version() -> String {
    BASELINE_DRAFT_SCHEMA_VERSION.to_owned()
}

/// Current baseline schema version written by chokkin v0.3+.
#[must_use]
pub fn current_baseline_schema_version() -> &'static str {
    BASELINE_SCHEMA_VERSION
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
#[derive(Debug, thiserror::Error)]
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
