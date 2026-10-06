//! Entry root construction (pipeline step 8).

mod auto;
mod build;
mod merge;
mod mode;
mod module;
mod script;
mod types;

pub(crate) use build::add_member_manifest_roots;
pub use build::build_entry_roots;
pub(crate) use mode::is_library_member;
pub(crate) use script::add_script_roots;
#[cfg(test)]
pub(crate) use types::EntryRoot;
pub use types::{EntryOrigin, EntryPlan, EntryWarning};
