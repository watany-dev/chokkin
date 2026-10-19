//! Source file discovery (pipeline step 4).

mod cache;
mod context;
mod dirs;
mod discover;
mod error;
mod glob;
mod layout;
mod local;
mod surface;
mod types;
mod walk;
#[cfg(test)]
mod walk_reference;
mod warnings;

pub(crate) use cache::SourcesCache;
pub(crate) use context::{
    apply_member_context, assign_file_context, assign_layout_file_context, is_test_data_path,
};
pub use discover::discover_sources;
pub(crate) use discover::{Discovery, discover_named_files, discover_sources_cached};
pub use error::SourcesError;
pub(crate) use glob::build_glob_set;
pub(crate) use layout::unpackaged_module_dirs;
pub(crate) use layout::{module_path, path_to_module};
pub use local::LocalTrees;
pub(crate) use local::importable_modules;
pub use surface::PublicSurface;
pub(crate) use surface::member_ships;
pub(crate) use types::MemberLayout;
pub use types::{
    DiscoveredFile, DiscoveredSources, FileContext, FileKind, LayoutInfo, ProjectLayout,
};
pub use warnings::SourcesWarning;
