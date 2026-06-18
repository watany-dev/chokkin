//! Stable machine-readable contract versions (Phase 3 / v0.3).

/// JSON reporter `schema_version` for v0.3 contract stabilization.
pub const JSON_REPORT_SCHEMA_VERSION: &str = "1";

/// Baseline file `schema_version` for v0.3 contract stabilization.
pub const BASELINE_SCHEMA_VERSION: &str = "1";

/// Baseline files without `schema_version` are treated as v0.2 draft (`"0"`).
pub const BASELINE_DRAFT_SCHEMA_VERSION: &str = "0";
