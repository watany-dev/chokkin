#![allow(clippy::multiple_crate_versions)] // pep508_rs depends on thiserror 1.x

//! `yokei` finds unused files, dependencies, and public symbols in Python
//! projects by building a project-wide reachability graph.
//!
//! Early implementation: pipeline steps 1–3 ([`discovery`], [`config`],
//! [`manifest`]) are available as library APIs. The CLI analyzer and issue
//! reporting are not wired yet. See `docs/dev/spec.ja.md` for the full
//! specification.

pub mod config;
pub mod discovery;
pub mod manifest;

pub use config::{
    Confidence, ConfigError, ConfigSources, DependencyGroupsConfig, EntrySpec, LoadedConfig,
    PluginId, ProjectMode, RuntimeOverrides, TargetVersion, UvWorkspaceHint, WorkspaceOverride,
    YokeiConfig, apply_overrides, default_config, load_config,
};
pub use discovery::{DiscoveryError, ProjectRoot, RootMarker, discover_project_root};
pub use manifest::{
    DeclaredDependency, DependencyContext, DependencyOrigin, EntryPointDecl, LoadedManifest,
    LockfileGraph, ManifestError, ManifestSources, ManifestWarning, ProjectMetadata,
    extract_manifest, resolve_target_version,
};

/// The version of yokei, taken from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Exit codes reported by the CLI, fixed for CI usage.
///
/// ```text
/// 0: no reportable issues
/// 1: issues found
/// 2: CLI/config error
/// 3: internal error
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitStatus {
    /// No reportable issues.
    Success = 0,
    /// Reportable issues were found.
    IssuesFound = 1,
    /// Invalid CLI invocation or configuration.
    UsageError = 2,
    /// Unexpected internal failure.
    InternalError = 3,
}

impl ExitStatus {
    /// Returns the numeric process exit code.
    pub fn code(self) -> u8 {
        self as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_cargo_manifest() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(ExitStatus::Success.code(), 0);
        assert_eq!(ExitStatus::IssuesFound.code(), 1);
        assert_eq!(ExitStatus::UsageError.code(), 2);
        assert_eq!(ExitStatus::InternalError.code(), 3);
    }
}
