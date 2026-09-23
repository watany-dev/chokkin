//! Issue reporters (pipeline step 12).

mod compact;
mod default;
mod format;
mod github;
mod json;
mod markdown;
mod sarif;
mod types;

pub use default::config_label_from_sources;
pub use format::format_subject;
pub use types::{RenderContext, ReporterId};

use crate::rules::IssueReport;

/// Render an issue report with the selected built-in reporter.
#[must_use]
pub fn render_issues(id: ReporterId, report: &IssueReport, context: &RenderContext) -> String {
    match id {
        ReporterId::Default => default::render(report, context),
        ReporterId::Compact => compact::render(report, context),
        ReporterId::Json => json::render(report, context),
        ReporterId::Markdown => markdown::render(report, context),
        ReporterId::Github => github::render(report),
        ReporterId::Sarif => sarif::render(report, context),
    }
}
