//! Reporter identifiers and render context (pipeline step 12 / Phase 1 CLI).

use crate::config::ProjectMode;
use crate::entry::ResolvedMode;

/// Built-in reporter identifiers (§2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReporterId {
    /// Human-readable grouped output.
    #[default]
    Default,
    /// One line per issue.
    Compact,
    /// Machine-readable JSON.
    Json,
    /// Markdown summary for CI.
    Markdown,
    /// GitHub Actions workflow annotations.
    Github,
    /// SARIF v2.1.0 for code scanning.
    Sarif,
}

impl ReporterId {
    /// Parse a `--reporter` flag value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "default" => Some(Self::Default),
            "compact" => Some(Self::Compact),
            "json" => Some(Self::Json),
            "markdown" => Some(Self::Markdown),
            "github" => Some(Self::Github),
            "sarif" => Some(Self::Sarif),
            _ => None,
        }
    }

    /// Stable CLI identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Compact => "compact",
            Self::Json => "json",
            Self::Markdown => "markdown",
            Self::Github => "github",
            Self::Sarif => "sarif",
        }
    }
}

/// Context passed to reporters alongside an [`crate::rules::IssueReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderContext {
    /// Project name from manifest metadata when available.
    pub project_name: Option<String>,
    /// Resolved analysis mode.
    pub mode: ResolvedMode,
    /// Effective `production` flag for the run.
    pub production: bool,
    /// chokkin version string.
    pub version: &'static str,
    /// Primary config file label for the header.
    pub config_label: Option<String>,
}

impl RenderContext {
    /// Borrow the effective project mode.
    #[must_use]
    pub fn project_mode(&self) -> ProjectMode {
        self.mode.mode
    }
}
