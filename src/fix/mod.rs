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

pub use apply::apply_fixes;
pub(crate) use apply::apply_fixes_with_workspace;
pub use error::FixError;
pub(crate) use types::WorkspaceFixManifest;
pub use types::{AppliedFix, FixOptions, FixReport, SkippedFix, SkippedReason};
