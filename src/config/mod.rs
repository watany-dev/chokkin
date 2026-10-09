//! Configuration loading (pipeline step 2).

mod defaults;
mod error;
mod load;
mod parse;
mod types;
mod workspace;

pub use defaults::default_config;
pub use error::ConfigError;
pub(crate) use load::apply_overrides;
pub use load::load_config;
pub(crate) use load::load_root_config;
#[cfg(test)]
pub(crate) use types::WorkspaceOverride;
pub use types::{
    ChokkinConfig, Confidence, ConfigSources, EntrySpec, LoadedConfig, PluginId, ProjectMode,
    RuntimeOverrides, SeverityLevel, TargetVersion,
};
pub(crate) use types::{DependencyGroupsConfig, ResolvedWorkspaceMember, UvWorkspaceHint};
pub(crate) use workspace::nested_projects;
