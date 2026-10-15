//! Entry root construction (pipeline step 8).

mod auto;
mod backend;
mod build;
mod merge;
mod mode;
mod module;
mod script;
mod types;

pub(crate) use backend::add_build_backend_roots;
#[cfg(test)]
pub(crate) use build::LIBRARY_SUBPACKAGE_RULE;
pub(crate) use build::add_member_manifest_roots;
pub use build::build_entry_roots;
pub(crate) use build::mark_command_scripts_dev;
pub(crate) use mode::is_library_member;
pub(crate) use module::resolve_module_to_path;
pub(crate) use script::add_script_roots;
#[cfg(test)]
pub(crate) use types::EntryRoot;
pub use types::{EntryOrigin, EntryPlan, EntryWarning};
