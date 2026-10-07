//! Baseline support for freezing existing issues (Phase 2 / v0.2).

mod store;
mod types;

pub use store::{apply_baseline, write_baseline};
pub(crate) use types::BaselineError;
pub use types::BaselineReport;
