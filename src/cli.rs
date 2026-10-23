//! CLI argument parsing (Phase 1 — clap).

use std::path::PathBuf;

use clap::Parser;

use crate::cache::CacheOptions;
use crate::config::{Confidence, RuntimeOverrides};
use crate::fix::FixOptions;
use crate::pipeline::AnalyzeOptions;
use crate::reporters::ReporterId;
use crate::rules::RuleId;

/// Parsed CLI invocation.
#[derive(Debug, Clone, Parser, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent CLI flag"
)]
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

    /// Output reporter (`default`, `compact`, `json`, `markdown`, `github`, `sarif`).
    #[arg(long, value_parser = parse_reporter)]
    pub reporter: Option<ReporterId>,

    /// Minimum confidence for emitted issues (`certain`, `likely`, `maybe`).
    #[arg(long, value_parser = parse_confidence)]
    pub confidence: Option<Confidence>,

    /// Explain a specific issue (e.g. `CHK002:boto3`).
    #[arg(long, value_name = "SELECTOR", value_parser = parse_explain_selector)]
    pub explain: Option<String>,

    /// Show reachability trace to a file path.
    #[arg(long, value_name = "PATH")]
    pub trace: Option<String>,

    /// Apply safe automatic fixes to manifest files.
    #[arg(long)]
    pub fix: bool,

    /// Preview fixes without writing files (requires `--fix`).
    #[arg(long, requires = "fix")]
    pub dry_run: bool,

    /// Allow `--fix` to remove unreachable project files.
    #[arg(long, requires = "fix")]
    pub allow_remove_files: bool,

    /// Allow `--fix` to add missing dependency declarations when unambiguous.
    #[arg(long, requires = "fix")]
    pub add_missing: bool,

    /// Suppress issues already recorded in this baseline file.
    #[arg(long, value_name = "PATH")]
    pub baseline: Option<PathBuf>,

    /// Write the current issue set to the baseline file.
    #[arg(long, requires = "baseline")]
    pub update_baseline: bool,

    /// Disable cache reads and writes.
    #[arg(long)]
    pub no_cache: bool,

    /// Do not treat nested `pyproject.toml` files as workspace members.
    #[arg(long)]
    pub no_auto_workspace: bool,

    /// Run probe mode (pipeline steps 1–4 only).
    ///
    /// Probe stops before issues exist, so flags that filter, report, fix, or
    /// baseline issues would be ignored; reject them instead (#1128).
    #[arg(long, conflicts_with_all = [
        "fix", "dry_run", "allow_remove_files", "add_missing", "strict", "no_exit_code",
        "include", "exclude", "reporter", "confidence", "baseline", "update_baseline",
        "explain", "trace",
    ])]
    pub probe: bool,

    /// Append a starter `[tool.chokkin]` config to pyproject.toml.
    #[arg(long, conflicts_with_all = [
        "probe", "fix", "dry_run", "allow_remove_files", "add_missing", "production",
        "strict", "no_exit_code", "include", "exclude", "reporter", "confidence",
        "baseline", "update_baseline", "no_cache", "no_auto_workspace", "explain", "trace",
    ])]
    pub init: bool,

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
            no_auto_workspace: self.no_auto_workspace.then_some(true),
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
                allow_remove_files: self.allow_remove_files,
                add_missing: self.add_missing,
            },
            baseline: self.baseline.clone(),
            update_baseline: self.update_baseline,
            cache: if self.no_cache {
                CacheOptions::disabled()
            } else {
                CacheOptions::default()
            },
        }
    }
}

fn parse_reporter(value: &str) -> Result<ReporterId, String> {
    ReporterId::parse(value).ok_or_else(|| format!("unknown reporter `{value}`"))
}

fn parse_confidence(value: &str) -> Result<Confidence, String> {
    Confidence::parse(value).ok_or_else(|| format!("unknown confidence `{value}`"))
}

/// A selector is `CODE:KEY` with a known rule code; anything else is a usage
/// error rather than a selector that never matches (#1159).
fn parse_explain_selector(value: &str) -> Result<String, String> {
    match value.split_once(':') {
        Some((code, key)) if !key.is_empty() => match RuleId::parse_code(code) {
            Some(_) => Ok(value.to_owned()),
            None => Err(format!("unknown rule code `{code}` in selector `{value}`")),
        },
        _ => Err(format!(
            "selector `{value}` is not `CODE:KEY` (e.g. `CHK002:boto3`)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explain_selector_needs_a_known_rule_code_and_a_key() {
        let parse = |selector: &str| {
            parse_cli_args(vec!["--explain".to_owned(), selector.to_owned()])
                .map(|args| args.explain)
        };
        assert_eq!(parse("CHK002:boto3"), Ok(Some("CHK002:boto3".to_owned())));
        assert_eq!(
            parse("chk006:acme.utils:f"),
            Ok(Some("chk006:acme.utils:f".to_owned()))
        );
        for selector in ["boto3", "CHK002:"] {
            let error = parse(selector).expect_err(selector);
            assert!(error.contains("is not `CODE:KEY`"), "{selector}: {error}");
        }
        for selector in ["CHK999:boto3", ":boto3"] {
            let error = parse(selector).expect_err(selector);
            assert!(error.contains("unknown rule code"), "{selector}: {error}");
        }
    }

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
    fn parses_v02_reporters() {
        let github =
            parse_cli_args(vec!["--reporter".to_owned(), "github".to_owned()]).expect("parse");
        let sarif =
            parse_cli_args(vec!["--reporter".to_owned(), "sarif".to_owned()]).expect("parse");
        assert_eq!(github.reporter, Some(ReporterId::Github));
        assert_eq!(sarif.reporter, Some(ReporterId::Sarif));
    }

    #[test]
    fn parses_init_flag() {
        let args = parse_cli_args(vec!["--init".to_owned()]).expect("parse");
        assert!(args.init);
    }

    #[test]
    fn rejects_init_probe_combination() {
        let err = parse_cli_args(vec!["--init".to_owned(), "--probe".to_owned()])
            .expect_err("invalid combination");
        assert!(err.contains("--init"));
    }

    #[test]
    fn rejects_probe_with_issue_flags() {
        let cases: &[&[&str]] = &[
            &["--fix"],
            &["--fix", "--dry-run"],
            &["--fix", "--allow-remove-files"],
            &["--fix", "--add-missing"],
            &["--strict"],
            &["--no-exit-code"],
            &["--include", "CHK001"],
            &["--exclude", "CHK001"],
            &["--reporter", "json"],
            &["--confidence", "certain"],
            &["--baseline", "b.json"],
            &["--baseline", "b.json", "--update-baseline"],
            &["--explain", "CHK002:boto3"],
            &["--trace", "src/a.py"],
        ];
        for flags in cases {
            let args = std::iter::once("--probe")
                .chain(flags.iter().copied())
                .map(str::to_owned)
                .collect();
            let err = parse_cli_args(args).expect_err("invalid combination");
            assert!(err.contains("--probe"), "{flags:?}: {err}");
        }
    }

    #[test]
    fn accepts_probe_with_discovery_flags() {
        let args = parse_cli_args(
            [
                "--probe",
                "--production",
                "--no-cache",
                "--no-auto-workspace",
                "--project-root",
                "root",
                "sub",
            ]
            .map(str::to_owned)
            .to_vec(),
        )
        .expect("parse");
        assert!(args.probe && args.production && args.no_auto_workspace);
    }

    #[test]
    fn rejects_init_with_analysis_flag() {
        let err = parse_cli_args(vec!["--init".to_owned(), "--no-cache".to_owned()])
            .expect_err("invalid combination");
        assert!(err.contains("--init"));
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
        let err = parse_cli_args(vec!["--dry-run".to_owned()]).expect_err("missing fix");
        assert!(err.contains("--fix"));
    }

    #[test]
    fn parses_allow_remove_files() {
        let args = parse_cli_args(vec!["--fix".to_owned(), "--allow-remove-files".to_owned()])
            .expect("parse");
        assert!(args.allow_remove_files);
        assert!(args.analyze_options().fix.allow_remove_files);
    }

    #[test]
    fn allow_remove_files_requires_fix() {
        let err = parse_cli_args(vec!["--allow-remove-files".to_owned()]).expect_err("missing fix");
        assert!(err.contains("--fix"));
    }

    #[test]
    fn parses_add_missing() {
        let args =
            parse_cli_args(vec!["--fix".to_owned(), "--add-missing".to_owned()]).expect("parse");
        assert!(args.add_missing);
        assert!(args.analyze_options().fix.add_missing);
    }

    #[test]
    fn add_missing_requires_fix() {
        let err = parse_cli_args(vec!["--add-missing".to_owned()]).expect_err("missing fix");
        assert!(err.contains("--fix"));
    }

    #[test]
    fn update_baseline_requires_baseline_path() {
        let err =
            parse_cli_args(vec!["--update-baseline".to_owned()]).expect_err("missing baseline");
        assert!(err.contains("--baseline"));
    }

    #[test]
    fn parses_no_cache() {
        let args = parse_cli_args(vec!["--no-cache".to_owned()]).expect("parse");
        assert!(args.no_cache);
        assert!(!args.analyze_options().cache.enabled);
    }

    #[test]
    fn parses_no_auto_workspace() {
        let args = parse_cli_args(vec!["--no-auto-workspace".to_owned()]).expect("parse");
        assert_eq!(args.runtime_overrides().no_auto_workspace, Some(true));
        let args = parse_cli_args(Vec::new()).expect("parse");
        assert_eq!(args.runtime_overrides().no_auto_workspace, None);
    }
}
