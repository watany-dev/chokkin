//! Pipeline orchestration errors.

use std::io;

use crate::baseline::BaselineError;
use crate::config::ConfigError;
use crate::discovery::DiscoveryError;
use crate::graph::GraphError;
use crate::manifest::ManifestError;
use crate::parser::ParseError;
use crate::plugins::PluginsError;
use crate::reachability::ReachabilityError;
use crate::sources::SourcesError;

/// Fatal error while running pipeline steps 1–4 for probe output.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// Project root discovery failed.
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    /// Configuration loading failed.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Manifest extraction failed.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// Source file discovery failed.
    #[error(transparent)]
    Sources(#[from] SourcesError),
    /// Start path could not be canonicalized.
    #[error("failed to resolve start path: {0}")]
    StartPath(#[source] io::Error),
}

impl ProbeError {
    /// Whether this error should map to [`crate::ExitStatus::UsageError`].
    #[must_use]
    pub const fn is_usage_error(&self) -> bool {
        matches!(
            self,
            Self::Discovery(_) | Self::Config(_) | Self::Manifest(_) | Self::Sources(_)
        )
    }
}

/// Fatal error while running the full analysis pipeline.
#[derive(Debug, thiserror::Error)]
pub enum AnalyzeError {
    /// Steps 1–4 failed.
    #[error(transparent)]
    Probe(#[from] ProbeError),
    /// Plugin extraction failed.
    #[error(transparent)]
    Plugins(#[from] PluginsError),
    /// Python parsing failed.
    #[error(transparent)]
    Parse(#[from] ParseError),
    /// Graph construction failed.
    #[error(transparent)]
    Graph(#[from] GraphError),
    /// Reachability analysis failed.
    #[error(transparent)]
    Reachability(#[from] ReachabilityError),
    /// Baseline read/write failed.
    #[error(transparent)]
    Baseline(#[from] BaselineError),
}

impl AnalyzeError {
    /// Whether this error should map to [`crate::ExitStatus::UsageError`].
    #[must_use]
    pub const fn is_usage_error(&self) -> bool {
        match self {
            Self::Probe(error) => error.is_usage_error(),
            Self::Plugins(_) | Self::Parse(_) => true,
            Self::Graph(_) | Self::Reachability(_) | Self::Baseline(_) => false,
        }
    }
}
