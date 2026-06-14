//! Issue reporter types and traits (pipeline step 12).

mod compact;
mod default;
mod format;
mod github;
mod json;
mod markdown;
mod sarif;
mod traits;
mod types;

pub use compact::CompactReporter;
pub use default::{DefaultReporter, config_label_from_sources};
pub use format::format_subject;
pub use github::GithubReporter;
pub use json::JsonReporter;
pub use markdown::MarkdownReporter;
pub use sarif::SarifReporter;
pub use traits::Reporter;
pub use types::{RenderContext, ReporterId};

use crate::rules::IssueReport;

/// Render an issue report with the selected built-in reporter.
#[must_use]
pub fn render_issues(id: ReporterId, report: &IssueReport, context: &RenderContext) -> String {
    match id {
        ReporterId::Default => DefaultReporter.render(report, context),
        ReporterId::Compact => CompactReporter.render(report, context),
        ReporterId::Json => JsonReporter.render(report, context),
        ReporterId::Markdown => MarkdownReporter.render(report, context),
        ReporterId::Github => GithubReporter.render(report, context),
        ReporterId::Sarif => SarifReporter.render(report, context),
    }
}
