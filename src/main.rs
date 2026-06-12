//! CLI entry point. Logic belongs in the library; this file only handles
//! argument dispatch and process exit.

// The CLI layer is the one place allowed to print and exit.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;

use yokei::ExitStatus;

const USAGE: &str = "\
yokei - Find unused files, dependencies, and public symbols in Python projects

Usage: yokei [OPTIONS]

Options:
  -h, --help     Print help
  -V, --version  Print version

yokei is in the design phase; the analyzer is not implemented yet.
See https://github.com/watany-dev/yokei for the specification and roadmap.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            println!("{USAGE}");
            ExitCode::from(ExitStatus::Success.code())
        },
        Some("-V" | "--version") => {
            println!("yokei {}", yokei::VERSION);
            ExitCode::from(ExitStatus::Success.code())
        },
        _ => {
            eprintln!(
                "yokei {}: the analyzer is not implemented yet.",
                yokei::VERSION
            );
            eprintln!("Run `yokei --help` for details.");
            ExitCode::from(ExitStatus::UsageError.code())
        },
    }
}
