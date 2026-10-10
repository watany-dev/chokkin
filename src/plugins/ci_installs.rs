//! Distributions CI workflows install before running them (#725).
//!
//! `run:` steps that `pip install` a tool and `uses:` steps of known actions
//! provide that tool the way a manifest declaration does. They are read even
//! with the github-actions plugin disabled (R-07), because they only ever
//! suppress CHK008: no usage is added here.

use std::path::{Path, PathBuf};

use super::config_scan::{extract_requirement_name, is_distribution_name, strip_shell_comment};
use super::config_text::leading_spaces;
use super::devtools::{is_workflow_file, workflow_run_commands};

/// Actions that install a distribution's CLI themselves.
const ACTION_PROVIDERS: &[(&str, &str)] = &[
    ("pre-commit/action", "pre-commit"),
    ("j178/prek-action", "prek"),
];

/// Install options whose value is the next word, never a distribution.
const VALUE_OPTIONS: &[&str] = &[
    "-r",
    "--requirement",
    "-c",
    "--constraint",
    "-e",
    "--editable",
    "-i",
    "--index-url",
    "--extra-index-url",
    "-f",
    "--find-links",
    "-t",
    "--target",
    "--prefix",
    "--root",
    "-p",
    "--python",
    "--with",
    "--with-requirements",
    "--group",
    "--extra",
];

/// `.github/workflows/*.{yml,yaml}`, sorted so scans and fingerprints are stable.
pub(super) fn input_paths(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root.join(".github").join("workflows")) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_workflow_file(path))
        .collect();
    paths.sort();
    paths
}

pub(super) fn scan(root: &Path) -> Vec<String> {
    let mut distributions = Vec::new();
    for path in input_paths(root) {
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (_, command) in workflow_run_commands(&contents) {
            distributions.extend(installed_distributions(&command));
        }
        distributions.extend(contents.lines().filter_map(action_distribution));
    }
    distributions
}

/// Distributions a shell command installs with `pip install`, `uv pip
/// install`, `python -m pip install`, `pipx install` or `uv tool install`.
fn installed_distributions(command: &str) -> Vec<String> {
    let command = without_expressions(command).replace("\\\n", " ");
    let mut found = Vec::new();
    let lines = command.lines().map(strip_shell_comment);
    for segment in lines.flat_map(|line| line.split([';', '|', '&'])) {
        let words: Vec<&str> = segment.split_whitespace().collect();
        let Some(install) = words.windows(2).position(|pair| {
            pair[1] == "install"
                && (pair[0] == "pipx"
                    || pair[0].strip_prefix("pip").is_some_and(|version| {
                        version.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
                    })
                    || (pair[0] == "tool"
                        && words.iter().take_while(|word| **word != "tool").last() == Some(&"uv")))
        }) else {
            continue;
        };
        let mut args = words.iter().skip(install + 2);
        while let Some(arg) = args.next() {
            if VALUE_OPTIONS.contains(arg) {
                args.next();
                continue;
            }
            let name = extract_requirement_name(arg.trim_matches(['"', '\'']));
            found.extend(name.filter(|name| is_distribution_name(name)));
        }
    }
    found
}

/// `command` with `${{ ... }}` expressions dropped: their words are unknown
/// until the workflow runs.
fn without_expressions(command: &str) -> String {
    let mut rest = command;
    let mut kept = String::with_capacity(command.len());
    while let Some(start) = rest.find("${{") {
        kept.push_str(&rest[..start]);
        rest = rest[start..]
            .find("}}")
            .map_or("", |end| &rest[start + end + 2..]);
    }
    kept.push_str(rest);
    kept
}

/// `uses: pre-commit/action@v3` → `pre-commit`.
fn action_distribution(line: &str) -> Option<String> {
    let trimmed = line.get(leading_spaces(line)..)?;
    let value = trimmed
        .strip_prefix("- ")
        .unwrap_or(trimmed)
        .strip_prefix("uses:")?
        .trim()
        .trim_matches(['"', '\'']);
    let action = value.split('@').next()?.to_ascii_lowercase();
    ACTION_PROVIDERS
        .iter()
        .find(|(name, _)| *name == action)
        .map(|(_, distribution)| (*distribution).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_commands_name_distributions_without_options_or_files() {
        let cases: &[(&str, &[&str])] = &[
            ("python -Im pip install tox", &["tox"]),
            ("uv pip install --system tox", &["tox"]),
            (
                "pip install -U \"tox>=4\" -r requirements.txt -c c.txt",
                &["tox"],
            ),
            (
                "python3 -m pip install --upgrade pip setuptools",
                &["pip", "setuptools"],
            ),
            ("pipx install nox && nox -s tests", &["nox"]),
            ("uv tool install --with tox-uv tox", &["tox"]),
            ("pip3.12 install -e . build", &["build"]),
            ("pip install ${{ matrix.dep }} ./dist/pkg.whl", &[]),
            ("pip list", &[]),
            ("uv run tool install x", &[]),
            ("pip install wheel\ntox -e py", &["wheel"]),
            (
                "pip install \\\n  tox \\\n  pre-commit",
                &["tox", "pre-commit"],
            ),
            (
                "# pip install black\npip install isort # not black",
                &["isort"],
            ),
        ];
        for (command, expected) in cases {
            assert_eq!(installed_distributions(command), *expected, "{command}");
        }
    }

    #[test]
    fn known_actions_provide_their_distribution() {
        assert_eq!(
            action_distribution("      - uses: pre-commit/action@v3.0.1"),
            Some("pre-commit".to_owned())
        );
        assert_eq!(
            action_distribution("        uses: \"j178/prek-action@v1\""),
            Some("prek".to_owned())
        );
        assert_eq!(
            action_distribution("      - uses: actions/checkout@v4"),
            None
        );
    }

    #[test]
    fn scan_reads_run_steps_and_actions_of_workflow_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workflows = temp.path().join(".github").join("workflows");
        std::fs::create_dir_all(&workflows).expect("create workflows");
        std::fs::write(
            workflows.join("ci.yml"),
            "jobs:\n  t:\n    steps:\n      - uses: pre-commit/action@v3\n      - run: |\n          \
             uv pip install --system tox\n          tox\n",
        )
        .expect("write ci");
        std::fs::write(workflows.join("notes.md"), "- run: pip install black\n").expect("write md");
        let mut found = scan(temp.path());
        found.sort();
        assert_eq!(found, ["pre-commit", "tox"]);
    }
}
