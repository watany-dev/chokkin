//! Probe pipeline errors.

use std::io;

use crate::config::ConfigError;
use crate::discovery::DiscoveryError;
use crate::manifest::ManifestError;
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
    /// Invalid CLI invocation.
    #[error("invalid CLI: {0}")]
    Usage(String),
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
            Self::Discovery(_)
                | Self::Config(_)
                | Self::Manifest(_)
                | Self::Sources(_)
                | Self::Usage(_)
        )
    }
}
