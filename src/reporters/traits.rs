//! Reporter trait for issue output (Phase 1 CLI implementations).

use crate::rules::IssueReport;

use super::types::RenderContext;

/// Renders an [`IssueReport`] for CLI or CI output.
pub trait Reporter {
    /// Format the issue report as a string.
    fn render(&self, report: &IssueReport, context: &RenderContext) -> String;
}
