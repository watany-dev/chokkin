//! Entry root construction (pipeline step 8).

mod auto;
mod backend;
mod build;
mod main_block;
mod merge;
mod mode;
mod module;
mod script;
mod types;

pub(crate) use backend::add_build_backend_roots;
#[cfg(test)]
pub(crate) use build::LIBRARY_SUBPACKAGE_RULE;
pub(crate) use build::MemberEntryInput;
pub(crate) use build::beside_origin;
pub use build::build_entry_roots;
pub(crate) use build::mark_command_scripts_dev;
pub(crate) use main_block::add_main_block_roots;
pub(crate) use module::resolve_module_to_path;
pub(crate) use script::add_script_roots;
#[cfg(test)]
pub(crate) use types::EntryRoot;
pub use types::{EntryOrigin, EntryPlan, EntryWarning};
