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
pub(crate) use types::WorkspaceFixManifest;
pub(crate) use types::{FixOptions, FixReport};
pub(crate) use write::atomic_write;
