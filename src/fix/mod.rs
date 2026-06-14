//! Optional automatic fixes (pipeline step 13).

mod apply;
mod containment;
mod error;
mod plan;
mod pyproject;
mod requirements;
mod setup_cfg;
mod types;
mod write;

pub(crate) use apply::apply_fixes_with_workspace;
pub use apply::apply_fixes;
pub use error::FixError;
pub use types::{AppliedFix, FixOptions, FixReport, SkippedFix, SkippedReason};
pub(crate) use types::WorkspaceFixManifest;
