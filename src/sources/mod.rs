//! Source file discovery (pipeline step 4).

mod context;
mod discover;
mod error;
mod glob;
mod layout;
mod types;
mod walk;
mod warnings;

pub use discover::discover_sources;
pub use error::SourcesError;
pub use types::{
    DiscoveredFile, DiscoveredSources, FileContext, FileKind, LayoutInfo, ProjectLayout,
};
pub use warnings::SourcesWarning;
