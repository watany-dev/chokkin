//! Config / plugin extraction (pipeline step 5).

mod alembic;
mod celery;
mod ci_installs;
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
pub(crate) use enablers::{PluginActivation, PluginActivationReason};
pub(crate) use error::PluginsError;
pub use extract::{PluginExtractRequest, extract_plugin_hints_with_parse};
pub(crate) use pytest::{PytestImportSettings, import_settings as pytest_import_settings};
#[cfg(test)]
pub(crate) use types::FrameworkUsedGlob;
pub(crate) use types::{BinaryUsage, ModuleReference, ReferenceOrigin};
pub use types::{PluginContribution, PluginHints};
pub(crate) use util::{parse_module_symbol, parse_uvicorn_script_target};
pub use warnings::PluginsWarning;
