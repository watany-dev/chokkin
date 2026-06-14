//! Pipeline orchestration (probe and analyze).

mod analyze;
mod error;
mod probe;
mod trace;
mod warnings;

pub use analyze::{AnalysisReport, AnalyzeOptions, analyze_project};
pub use error::{AnalyzeError, ProbeError};
pub use probe::{ProbeReport, probe_project, write_probe_report};
pub use trace::{format_trace, trace_output};
pub use warnings::{ProbeWarning, write_probe_warnings};
