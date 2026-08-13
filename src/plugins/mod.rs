//! Config / plugin extraction (pipeline step 5).

mod celery;
mod config_scan;
mod context;
mod devtools;
mod django;
mod doctools;
mod error;
mod extract;
mod fastapi;
mod flask;
mod pytest;
mod types;
mod util;
mod warnings;

pub use error::PluginsError;
pub use extract::{extract_plugin_hints, extract_plugin_hints_with_cache};
pub use types::{
    BinaryUsage, FileContextOverride, FrameworkUsedGlob, ModuleReference, PluginContribution,
    PluginEntry, PluginHints, ReferenceOrigin, SymbolReference,
};
pub use util::{parse_module_symbol, parse_uvicorn_script_target};
pub use warnings::PluginsWarning;
