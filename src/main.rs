//! CLI entry point. Logic belongs in the library; this file only handles
//! argument dispatch and process exit.

#![allow(clippy::print_stdout, clippy::print_stderr)]
#![allow(clippy::multiple_crate_versions)] // pep508_rs depends on thiserror 1.x

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use chokkin::{
    AnalysisReport, CliArgs, ExitStatus, FixReport, RenderContext, RuntimeOverrides, VERSION,
    analyze_project, config_label_from_sources, explain_issue, format_subject, init_project,
    parse_cli_args, probe_project, render_issues, trace_output, write_probe_report,
    write_probe_warnings,
};

const USAGE: &str = "\
chokkin - Find unused files, dependencies, and public symbols in Python projects

Usage: chokkin [OPTIONS] [PATH]

Arguments:
  [PATH]  Directory to analyze (default: current directory)

Options:
  -h, --help              Print help
  -V, --version           Print version
      --production        Analyze runtime context only (exclude test/docs/dev files)
      --strict            Enable strict analysis policies
      --no-exit-code      Report issues but return exit code 0
      --include <RULES>   Only emit these rule codes (comma-separated CHK00x)
      --exclude <RULES>   Suppress these rule codes (comma-separated CHK00x)
      --reporter <ID>     Output reporter: default, compact, json, markdown, github, sarif
      --confidence <LVL>  Minimum confidence: certain, likely, maybe
      --explain <SEL>     Explain an issue (e.g. CHK002:boto3)
      --trace <PATH>      Show reachability trace to a file
      --fix               Apply safe automatic manifest fixes
      --dry-run           Preview fixes without writing files (requires --fix)
      --allow-remove-files Allow --fix to remove unreachable project files
      --add-missing       Add unambiguous missing dependencies when safe
      --baseline <PATH>   Suppress issues already recorded in a baseline file
      --update-baseline   Write current issues to --baseline
      --no-cache          Disable cache reads and writes
      --probe             Run probe mode (pipeline steps 1-4 only)
      --init              Append starter [tool.chokkin] config to pyproject.toml
      --project-root PATH Override project root discovery start directory

See https://github.com/watany-dev/chokkin for the specification and roadmap.";

fn main() -> ExitCode {
    let args = match parse_cli_args(std::env::args().skip(1).collect()) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("Run `chokkin --help` for usage.");
            return ExitCode::from(ExitStatus::UsageError.code());
        },
    };

    if args.help {
        println!("{USAGE}");
        return ExitCode::from(ExitStatus::Success.code());
    }

    if args.version {
        println!("chokkin {VERSION}");
        return ExitCode::from(ExitStatus::Success.code());
    }

    if let Err(message) = args.validate() {
        eprintln!("{message}");
        return ExitCode::from(ExitStatus::UsageError.code());
    }

    let start = args.path.as_deref().unwrap_or_else(|| Path::new("."));
    let overrides = args.runtime_overrides();

    if args.init {
        return run_init(start, args.project_root.as_deref(), &overrides);
    }

    if args.probe {
        return run_probe(start, args.project_root.as_deref(), &overrides);
    }

    match analyze_project(
        start,
        args.project_root.as_deref(),
        &overrides,
        args.analyze_options(),
    ) {
        Ok(report) => run_analysis(&args, report),
        Err(error) if error.is_usage_error() => {
            eprintln!("{error}");
            ExitCode::from(ExitStatus::UsageError.code())
        },
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(ExitStatus::InternalError.code())
        },
    }
}

fn run_init(start: &Path, project_root: Option<&Path>, overrides: &RuntimeOverrides) -> ExitCode {
    match init_project(start, project_root, overrides) {
        Ok(report) => {
            println!("wrote starter [tool.chokkin] to {}", report.path.display());
            ExitCode::from(ExitStatus::Success.code())
        },
        Err(error) if error.is_usage_error() => {
            eprintln!("{error}");
            ExitCode::from(ExitStatus::UsageError.code())
        },
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(ExitStatus::InternalError.code())
        },
    }
}

fn run_probe(start: &Path, project_root: Option<&Path>, overrides: &RuntimeOverrides) -> ExitCode {
    match probe_project(start, project_root, overrides) {
        Ok(report) => {
            let stdout_ok = write_probe_report(&report, &mut std::io::stdout());
            let stderr_ok = write_probe_warnings(&report.warnings, &mut std::io::stderr());
            if stdout_ok.is_err() || stderr_ok.is_err() {
                return ExitCode::from(ExitStatus::InternalError.code());
            }
            ExitCode::from(ExitStatus::Success.code())
        },
        Err(error) if error.is_usage_error() => {
            eprintln!("{error}");
            ExitCode::from(ExitStatus::UsageError.code())
        },
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(ExitStatus::InternalError.code())
        },
    }
}

fn run_analysis(args: &CliArgs, report: AnalysisReport) -> ExitCode {
    if let Err(error) = write_probe_warnings(&report.probe.warnings, &mut std::io::stderr()) {
        let _ = error;
        return ExitCode::from(ExitStatus::InternalError.code());
    }
    if let Err(error) = write_probe_warnings(&report.warnings, &mut std::io::stderr()) {
        let _ = error;
        return ExitCode::from(ExitStatus::InternalError.code());
    }

    if let Some(selector) = &args.explain {
        match explain_issue(&report.issues, selector) {
            Some(text) => {
                if writeln!(std::io::stderr(), "{text}").is_err() {
                    return ExitCode::from(ExitStatus::InternalError.code());
                }
            },
            None => {
                let _ = writeln!(
                    std::io::stderr(),
                    "explain: no matching issue for `{selector}`"
                );
            },
        }
    }

    if let Some(target) = &args.trace {
        let text = trace_output(&report.reachability, &report.graph, target);
        if writeln!(std::io::stderr(), "{text}").is_err() {
            return ExitCode::from(ExitStatus::InternalError.code());
        }
    }

    let context = RenderContext {
        project_name: report.probe.manifest.metadata.name.clone(),
        mode: report.entry_mode,
        production: report.probe.effective_config.production,
        version: VERSION,
        config_label: Some(config_label_from_sources(&report.probe.config_sources)),
    };

    let output = render_issues(args.reporter_id(), &report.issues, &context);
    if print_stdout(&output).is_err() {
        return ExitCode::from(ExitStatus::InternalError.code());
    }

    if let Some(fix_report) = &report.fix
        && write_fix_report(fix_report, &mut std::io::stderr()).is_err()
    {
        return ExitCode::from(ExitStatus::InternalError.code());
    }

    if let Some(baseline) = &report.baseline
        && write_baseline_report(baseline, &mut std::io::stderr()).is_err()
    {
        return ExitCode::from(ExitStatus::InternalError.code());
    }

    ExitCode::from(report.issues.exit_status.code())
}

fn print_stdout(text: &str) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }
    Ok(())
}

fn write_fix_report(report: &FixReport, out: &mut impl Write) -> std::io::Result<()> {
    if report.applied.is_empty() && report.skipped.is_empty() && report.reminders.is_empty() {
        return Ok(());
    }
    writeln!(out, "Fixes:")?;
    for fix in &report.applied {
        writeln!(
            out,
            "  applied {} {} in {} — {}",
            fix.rule.as_code(),
            format_subject(&fix.subject),
            fix.file,
            fix.description
        )?;
    }
    for skipped in &report.skipped {
        writeln!(
            out,
            "  skipped {} {} — {:?}: {}",
            skipped.rule.as_code(),
            format_subject(&skipped.subject),
            skipped.reason,
            skipped.detail
        )?;
    }
    for reminder in &report.reminders {
        writeln!(out, "  reminder: {reminder}")?;
    }
    Ok(())
}

fn write_baseline_report(
    report: &chokkin::BaselineReport,
    out: &mut impl Write,
) -> std::io::Result<()> {
    let path = report.path.as_deref().unwrap_or("(unknown)");
    if report.written > 0 {
        writeln!(out, "Baseline: wrote {} issues to {path}", report.written)?;
    } else if report.suppressed > 0 {
        writeln!(
            out,
            "Baseline: suppressed {} existing issues from {path}",
            report.suppressed
        )?;
    }
    Ok(())
}
