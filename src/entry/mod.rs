//! Entry root construction (pipeline step 8).

mod apply;
mod auto;
mod build;
mod error;
mod merge;
mod mode;
mod module;
mod types;

pub use apply::apply_entry_plan;
pub use build::build_entry_roots;
pub use error::EntryError;
pub use types::{EntryOrigin, EntryPlan, EntryRoot, EntryWarning, ResolvedMode};
