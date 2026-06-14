//! Optional automatic fixes (pipeline step 13).

mod apply;
mod error;
mod plan;
mod pyproject;
mod requirements;
mod setup_cfg;
mod types;
mod write;

pub use apply::apply_fixes;
pub use error::FixError;
pub use types::{AppliedFix, FixOptions, FixReport, SkippedFix, SkippedReason};
