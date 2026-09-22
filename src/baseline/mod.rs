//! Baseline support for freezing existing issues (Phase 2 / v0.2).

mod store;
mod types;

pub use store::{apply_baseline, apply_baseline_with_overrides, write_baseline};
pub use types::{BaselineEntry, BaselineError, BaselineFile, BaselineReport};
