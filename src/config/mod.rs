//! Configuration loading (pipeline step 2).

mod defaults;
mod error;
mod load;
mod parse;
mod source;
mod types;

pub use defaults::default_config;
pub use error::ConfigError;
pub use load::{apply_overrides, load_config};
pub use types::{
    Confidence, ConfigSources, DependencyGroupsConfig, EntrySpec, LoadedConfig, PluginId,
    ProjectMode, RuntimeOverrides, TargetVersion, UvWorkspaceHint, WorkspaceOverride, YokeiConfig,
};
