//! Config / plugin extraction (pipeline step 5).

mod context;
mod django;
mod error;
mod extract;
mod fastapi;
mod pytest;
mod stub;
mod types;
mod util;
mod warnings;

pub use error::PluginsError;
pub use extract::extract_plugin_hints;
pub use types::{
    BinaryUsage, FileContextOverride, FrameworkUsedGlob, ModuleReference, PluginContribution,
    PluginEntry, PluginHints, ReferenceOrigin, SymbolReference,
};
pub use warnings::PluginsWarning;
