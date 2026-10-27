//! Distributions CI workflows install before running them (#725).
//!
//! `run:` steps that `pip install` a tool and `uses:` steps of known actions
//! provide that tool the way a manifest declaration does. They are read even
//! with the github-actions plugin disabled (R-07), because they only ever
//! suppress CHK008: no usage is added here.

use std::path::{Path, PathBuf};

use super::config_scan::{extract_requirement_name, is_distribution_name, strip_shell_comment};
use super::config_text::{leading_spaces, origin_at};
use super::devtools::{is_workflow_file, workflow_run_commands};
use super::types::RequirementsRef;
use crate::path_util::rel_to_root;

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

/// The distributions the workflows install, and the requirements files
/// their `run:` steps install from, which are read from the repository root
/// the steps start in.
pub(super) fn scan(root: &Path) -> (Vec<String>, Vec<RequirementsRef>) {
    let mut distributions = Vec::new();
    let mut requirements = Vec::new();
    for path in input_paths(root) {
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = rel_to_root(root, &path);
        for (index, command) in workflow_run_commands(&contents) {
            let found = installs(&command);
            distributions.extend(found.distributions);
            requirements.extend(
                found
                    .requirement_files
                    .into_iter()
                    .map(|file| RequirementsRef {
                        path: file,
                        origin: origin_at(&rel, index, "workflow run"),
                        missing_ok: true,
                    }),
            );
        }
        distributions.extend(contents.lines().filter_map(action_distribution));
    }
    (distributions, requirements)
}

/// What the install commands in a shell command name.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Installs {
    /// Distributions named on the command line.
    pub(super) distributions: Vec<String>,
    /// Requirements files `-r` / `--requirement` name, as written (#1273).
    /// `-c` files only constrain versions, so they install nothing.
    pub(super) requirement_files: Vec<String>,
}

/// The distributions and requirements files `pip install`, `uv pip
/// install`, `python -m pip install`, `pipx install` or `uv tool install`
/// name in `command`.
pub(super) fn installs(command: &str) -> Installs {
    let command = without_expressions(command).replace("\\\n", " ");
    let mut found = Installs::default();
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
        let pip = words[install] != "pipx" && words[install] != "tool";
        let mut args = words
            .iter()
            .map(|word| word.trim_matches(['"', '\'']))
            .skip(install + 2);
        while let Some(arg) = args.next() {
            if let Some(file) = requirement_option(arg, &mut args) {
                if pip && !file.is_empty() {
                    found.requirement_files.push(file);
                }
                continue;
            }
            if VALUE_OPTIONS.contains(&arg) {
                args.next();
                continue;
            }
            let name = extract_requirement_name(arg);
            found
                .distributions
                .extend(name.filter(|name| is_distribution_name(name)));
        }
    }
    found
}

/// The file a pip `-r` option names, from its own word (`-rfile`,
/// `--requirement=file`) or the next one, empty when the option ends the
/// command; `None` when `arg` is no `-r` option.
pub(super) fn requirement_option<'a>(
    arg: &str,
    rest: &mut impl Iterator<Item = &'a str>,
) -> Option<String> {
    if matches!(arg, "-r" | "--requirement") {
        return Some(rest.next().unwrap_or_default().to_owned());
    }
    let inline = arg
        .strip_prefix("--requirement=")
        .or_else(|| arg.strip_prefix("-r"))
        .filter(|file| !file.is_empty() && !file.starts_with('-'))?;
    Some(inline.to_owned())
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

    /// An inline `-r` value is the file; an empty one, or one that is
    /// another option, is no `-r` option.
    #[test]
    fn requirement_option_reads_inline_values_only_when_they_name_a_file() {
        let none = &mut std::iter::empty();
        assert_eq!(
            requirement_option("-rci/a.txt", none),
            Some("ci/a.txt".to_owned())
        );
        assert_eq!(requirement_option("--requirement=", none), None);
        assert_eq!(requirement_option("-r-x", none), None);
        assert_eq!(requirement_option("-r", none), Some(String::new()));
    }

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
            assert_eq!(installs(command).distributions, *expected, "{command}");
        }
    }

    #[test]
    fn pip_install_commands_name_their_requirement_files() {
        let cases: &[(&str, &[&str])] = &[
            (
                "pip install -r requirements/dev.txt -c c.txt",
                &["requirements/dev.txt"],
            ),
            (
                "uv pip install --requirement=ci/a.txt -rci/b.txt",
                &["ci/a.txt", "ci/b.txt"],
            ),
            (
                "python -m pip install \"-r\" 'docs/requirements.txt'",
                &["docs/requirements.txt"],
            ),
            ("pipx install -r x.txt tox", &[]),
            ("pip install -r", &[]),
        ];
        for (command, expected) in cases {
            assert_eq!(installs(command).requirement_files, *expected, "{command}");
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
        let (mut found, requirements) = scan(temp.path());
        found.sort();
        assert_eq!(found, ["pre-commit", "tox"]);
        assert!(requirements.is_empty());
    }
}
