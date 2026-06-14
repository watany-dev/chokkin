//! CLI argument parsing (Phase 1 — clap).

use std::path::PathBuf;

use clap::Parser;

use crate::config::{Confidence, RuntimeOverrides};
use crate::fix::FixOptions;
use crate::pipeline::AnalyzeOptions;
use crate::reporters::ReporterId;

/// Parsed CLI invocation.
#[derive(Debug, Clone, Parser, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)]
#[command(
    name = "chokkin",
    about = "Find unused files, dependencies, and public symbols in Python projects",
    disable_help_flag = true,
    disable_version_flag = true
)]
pub struct CliArgs {
    /// Directory to analyze (default: current directory).
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Override project root discovery start directory.
    #[arg(long, value_name = "PATH")]
    pub project_root: Option<PathBuf>,

    /// Analyze runtime context only (exclude test/docs/dev files).
    #[arg(long)]
    pub production: bool,

    /// Enable strict analysis policies.
    #[arg(long)]
    pub strict: bool,

    /// Report issues but return exit code 0.
    #[arg(long)]
    pub no_exit_code: bool,

    /// Only emit issues for these rule codes (comma-separated `CHK00x`).
    #[arg(long, value_delimiter = ',')]
    pub include: Option<Vec<String>>,

    /// Suppress issues for these rule codes (comma-separated `CHK00x`).
    #[arg(long, value_delimiter = ',')]
    pub exclude: Option<Vec<String>>,

    /// Output reporter (`default`, `compact`, `json`, `markdown`).
    #[arg(long, value_parser = parse_reporter)]
    pub reporter: Option<ReporterId>,

    /// Minimum confidence for emitted issues (`certain`, `likely`, `maybe`).
    #[arg(long, value_parser = parse_confidence)]
    pub confidence: Option<Confidence>,

    /// Explain a specific issue (e.g. `CHK002:boto3`).
    #[arg(long, value_name = "SELECTOR")]
    pub explain: Option<String>,

    /// Show reachability trace to a file path.
    #[arg(long, value_name = "PATH")]
    pub trace: Option<String>,

    /// Apply safe automatic fixes to manifest files.
    #[arg(long)]
    pub fix: bool,

    /// Preview fixes without writing files (requires `--fix`).
    #[arg(long)]
    pub dry_run: bool,

    /// Run probe mode (pipeline steps 1–4 only).
    #[arg(long)]
    pub probe: bool,

    /// Print help and exit.
    #[arg(short = 'h', long = "help")]
    pub help: bool,

    /// Print version and exit.
    #[arg(short = 'V', long = "version")]
    pub version: bool,
}

/// Parse CLI arguments from the process argv slice (without program name).
///
/// # Errors
///
/// Returns a usage message when arguments are invalid.
pub fn parse_cli_args(args: Vec<String>) -> Result<CliArgs, String> {
    let command_line = std::iter::once("chokkin".to_owned()).chain(args);
    CliArgs::try_parse_from(command_line).map_err(|err| err.to_string())
}

impl CliArgs {
    /// Build runtime configuration overrides from CLI flags.
    #[must_use]
    pub fn runtime_overrides(&self) -> RuntimeOverrides {
        RuntimeOverrides {
            production: self.production.then_some(true),
            strict: self.strict.then_some(true),
            confidence_floor: self.confidence,
            no_exit_code: self.no_exit_code.then_some(true),
            include_rules: self.include.clone(),
            exclude_rules: self.exclude.clone(),
        }
    }

    /// Selected reporter, defaulting to human-readable output.
    #[must_use]
    pub fn reporter_id(&self) -> ReporterId {
        self.reporter.unwrap_or_default()
    }

    /// Analysis options including optional fix behaviour.
    #[must_use]
    pub fn analyze_options(&self) -> AnalyzeOptions {
        AnalyzeOptions {
            fix_enabled: self.fix,
            fix: FixOptions {
                dry_run: self.dry_run,
                ..FixOptions::default()
            },
        }
    }

    /// Validate flag combinations.
    pub fn validate(&self) -> Result<(), String> {
        if self.dry_run && !self.fix {
            return Err("`--dry-run` requires `--fix`".to_owned());
        }
        Ok(())
    }
}

fn parse_reporter(value: &str) -> Result<ReporterId, String> {
    ReporterId::parse(value).ok_or_else(|| format!("unknown reporter `{value}`"))
}

fn parse_confidence(value: &str) -> Result<Confidence, String> {
    Confidence::parse(value).ok_or_else(|| format!("unknown confidence `{value}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_production_flag() {
        let args = parse_cli_args(vec!["--production".to_owned()]).expect("parse");
        assert!(args.production);
        assert_eq!(args.runtime_overrides().production, Some(true));
    }

    #[test]
    fn parses_project_root_and_path() {
        let args = parse_cli_args(vec![
            "--project-root".to_owned(),
            "/tmp/root".to_owned(),
            "subdir".to_owned(),
        ])
        .expect("parse");
        assert_eq!(
            args.project_root.as_deref(),
            Some(std::path::Path::new("/tmp/root"))
        );
        assert_eq!(args.path.as_deref(), Some(std::path::Path::new("subdir")));
    }

    #[test]
    fn parses_reporter_and_strict() {
        let args = parse_cli_args(vec![
            "--reporter".to_owned(),
            "json".to_owned(),
            "--strict".to_owned(),
        ])
        .expect("parse");
        assert_eq!(args.reporter, Some(ReporterId::Json));
        assert!(args.strict);
    }

    #[test]
    fn rejects_unknown_flag() {
        let err = parse_cli_args(vec!["--unknown".to_owned()]).expect_err("error");
        assert!(err.contains("unknown"));
    }

    #[test]
    fn rejects_multiple_positional_args() {
        let err = parse_cli_args(vec!["a".to_owned(), "b".to_owned()]).expect_err("error");
        assert!(err.contains("unexpected"));
    }

    #[test]
    fn dry_run_requires_fix() {
        let args = parse_cli_args(vec!["--dry-run".to_owned()]).expect("parse");
        assert!(args.validate().is_err());
    }
}
