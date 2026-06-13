//! CLI argument parsing (Phase 0 subset — manual parse, no clap).

use std::path::PathBuf;

use crate::config::RuntimeOverrides;

/// Parsed CLI invocation (Phase 0 subset).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CliArgs {
    /// Path to analyze; `None` means current directory.
    pub path: Option<PathBuf>,
    /// Explicit project root override for discovery.
    pub project_root: Option<PathBuf>,
    /// Runtime overrides from flags.
    pub overrides: RuntimeOverrides,
    /// Print help and exit.
    pub help: bool,
    /// Print version and exit.
    pub version: bool,
}

/// Parse CLI arguments from the process argv slice (without program name).
///
/// # Errors
///
/// Returns a usage message when arguments are invalid.
pub fn parse_cli_args(args: Vec<String>) -> Result<CliArgs, String> {
    let mut cli = CliArgs::default();
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                cli.help = true;
            },
            "-V" | "--version" => {
                cli.version = true;
            },
            "--production" => {
                cli.overrides.production = Some(true);
            },
            "--project-root" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--project-root requires a path argument".to_owned())?;
                if value.starts_with('-') {
                    return Err(format!(
                        "--project-root requires a path argument, got `{value}`"
                    ));
                }
                cli.project_root = Some(PathBuf::from(value));
            },
            other if other.starts_with("--project-root=") => {
                let value = other.strip_prefix("--project-root=").unwrap_or_default();
                if value.is_empty() {
                    return Err("--project-root requires a path argument".to_owned());
                }
                cli.project_root = Some(PathBuf::from(value));
            },
            other if other.starts_with('-') => {
                return Err(format!(
                    "unknown option `{other}` — run `yokei --help` for usage"
                ));
            },
            positional => {
                if cli.path.is_some() {
                    return Err(format!(
                        "unexpected extra argument `{positional}` — only one PATH is allowed"
                    ));
                }
                cli.path = Some(PathBuf::from(positional));
            },
        }
    }

    Ok(cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_production_flag() {
        let args = parse_cli_args(vec!["--production".to_owned()]).expect("parse");
        assert_eq!(args.overrides.production, Some(true));
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
    fn rejects_unknown_flag() {
        let err = parse_cli_args(vec!["--unknown".to_owned()]).expect_err("error");
        assert!(err.contains("unknown option"));
    }

    #[test]
    fn rejects_multiple_positional_args() {
        let err = parse_cli_args(vec!["a".to_owned(), "b".to_owned()]).expect_err("error");
        assert!(err.contains("extra argument"));
    }
}
