//! Config / plugin extraction (pipeline step 5).

mod celery;
mod commands;
mod config_scan;
mod config_text;
mod context;
mod devtools;
mod django;
mod doctools;
mod enablers;
mod error;
mod extract;
mod fastapi;
mod flask;
mod plugin_map;
mod pytest;
mod task_files;
mod tool_plugins;
mod types;
mod util;
mod warnings;

pub(crate) use enablers::{EnablerScope, resolve_plugin_activations};
pub use enablers::{PluginActivation, PluginActivationReason};
pub use error::PluginsError;
pub use extract::{PluginExtractRequest, extract_plugin_hints, extract_plugin_hints_with_parse};
pub use types::{
    BinaryUsage, FrameworkUsedGlob, ModuleReference, PluginContribution, PluginEntry, PluginHints,
    ReferenceOrigin, SymbolReference,
};
pub use util::{parse_module_symbol, parse_uvicorn_script_target};
pub use warnings::PluginsWarning;
