//! CLI entry point. Logic belongs in the library; this file only handles
//! argument dispatch and process exit.

#![allow(clippy::print_stdout, clippy::print_stderr)]
#![allow(clippy::multiple_crate_versions)] // pep508_rs depends on thiserror 1.x

use std::path::Path;
use std::process::ExitCode;

use yokei::{ExitStatus, parse_cli_args, probe_project, write_probe_report, write_probe_warnings};

const USAGE: &str = "\
yokei - Find unused files, dependencies, and public symbols in Python projects

Usage: yokei [OPTIONS] [PATH]

Arguments:
  [PATH]  Directory to analyze (default: current directory)

Options:
  -h, --help              Print help
  -V, --version           Print version
      --production        Analyze runtime context only (exclude test/docs/dev files)
      --project-root PATH Override project root discovery start directory

Probe mode runs pipeline steps 1–4 and prints project/manifest/source summary.
Full unused dependency and file analysis is not implemented yet.

See https://github.com/watany-dev/yokei for the specification and roadmap.";

fn main() -> ExitCode {
    let args = match parse_cli_args(std::env::args().skip(1).collect()) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("Run `yokei --help` for usage.");
            return ExitCode::from(ExitStatus::UsageError.code());
        },
    };

    if args.help {
        println!("{USAGE}");
        return ExitCode::from(ExitStatus::Success.code());
    }

    if args.version {
        println!("yokei {}", yokei::VERSION);
        return ExitCode::from(ExitStatus::Success.code());
    }

    let start = args.path.as_deref().unwrap_or_else(|| Path::new("."));

    match probe_project(start, args.project_root.as_deref(), &args.overrides) {
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
