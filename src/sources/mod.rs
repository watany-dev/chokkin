//! Source file discovery (pipeline step 4).

mod context;
mod discover;
mod error;
mod glob;
mod layout;
mod types;
mod walk;
mod warnings;

pub use context::assign_file_context;
pub use discover::discover_sources;
pub use error::SourcesError;
pub use glob::build_glob_set;
pub(crate) use layout::infer_layout;
pub use layout::path_to_module;
pub use types::{
    DiscoveredFile, DiscoveredSources, FileContext, FileKind, LayoutInfo, ProjectLayout,
};
pub use warnings::SourcesWarning;
