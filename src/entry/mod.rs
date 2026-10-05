//! Entry root construction (pipeline step 8).

mod auto;
mod build;
mod merge;
mod mode;
mod module;
mod script;
mod types;

pub use build::{add_member_manifest_roots, build_entry_roots};
pub use mode::is_library_member;
pub use script::add_script_roots;
pub use types::{EntryOrigin, EntryPlan, EntryRoot, EntryWarning};
