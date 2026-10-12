//! `.py` files commands run by path (#797).
//!
//! CI workflow `run:` steps, local pre-commit hook `entry:` commands and agent
//! skill docs run scripts no import reaches. They are read even with the
//! github-actions plugin disabled, like `ci_installs`, and only ever add dev
//! entry roots.

use std::path::{Path, PathBuf};

use crate::path_util::rel_to_root;

use super::ci_installs;
use super::commands::{ScriptRun, command_scripts};
use super::config_text::origin_at;
use super::devtools::workflow_run_commands;
use super::types::{ReferenceOrigin, ScriptReference};

const SKILL_DIRS: [&str; 2] = [".agents", ".claude"];
const SKILL_FILE: &str = "SKILL.md";

/// `<dir>/skills/<name>/SKILL.md` for each of [`SKILL_DIRS`], sorted.
pub(super) fn input_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = SKILL_DIRS
        .into_iter()
        .filter_map(|dir| std::fs::read_dir(root.join(dir).join("skills")).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(SKILL_FILE))
        .filter(|path| path.is_file())
        .collect();
    paths.sort();
    paths
}

pub(super) fn scan(root: &Path) -> Vec<ScriptReference> {
    let mut entries = Vec::new();
    for path in ci_installs::input_paths(root) {
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = rel_to_root(root, &path);
        for (index, command) in workflow_run_commands(&contents) {
            let origin = origin_at(&rel, index, "workflow run");
            entries.extend(command_entries(&command, &origin, Fallback::CheckoutPrefix));
        }
    }
    for path in input_paths(root) {
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = rel_to_root(root, &path);
        let skill_dir = rel.rsplit_once('/').map_or("", |(dir, _)| dir);
        for (index, command) in markdown_commands(&contents) {
            let origin = origin_at(&rel, index, "skill doc");
            entries.extend(command_entries(
                &command,
                &origin,
                Fallback::Under(skill_dir),
            ));
        }
    }
    entries
}

/// Where else a command's script may be, tried after the root-relative path.
#[derive(Clone, Copy)]
pub(super) enum Fallback<'a> {
    None,
    /// A workflow may run a script under the `path:` it checked the
    /// repository out to (`python3 fastmcp/tests/x.py`): the path without its
    /// first component, unless a directory is given.
    CheckoutPrefix,
    /// A skill doc may name its bundled scripts relative to its own directory.
    Under(&'a str),
}

pub(super) fn command_entries(
    command: &str,
    origin: &ReferenceOrigin,
    fallback: Fallback<'_>,
) -> Vec<ScriptReference> {
    command_scripts(command)
        .iter()
        .filter_map(|run| {
            let components = script_components(run)?;
            let path = components.join("/");
            let fallback = match fallback {
                Fallback::CheckoutPrefix if run.directory.is_none() => components
                    .get(1..)
                    .filter(|rest| !rest.is_empty())
                    .map(|rest| rest.join("/")),
                Fallback::Under(dir) => Some(format!("{dir}/{path}")),
                _ => None,
            };
            Some(ScriptReference {
                paths: std::iter::once(path).chain(fallback).collect(),
                origin: origin.clone(),
            })
        })
        .collect()
}

/// Components of the script path under its directory, or `None` when it is
/// absolute, leaves the root or holds a substitution. Split on both `/` and
/// `\\` so every platform reads a command the same way.
fn script_components(run: &ScriptRun) -> Option<Vec<&str>> {
    let mut components = Vec::new();
    for part in run.directory.iter().chain([&run.path]) {
        if part.contains(['$', '{', '}', '%', '*', ':']) || part.starts_with(['/', '\\']) {
            return None;
        }
        for component in part.split(['/', '\\']) {
            match component {
                "" | "." => {},
                ".." => return None,
                name => components.push(name),
            }
        }
    }
    Some(components)
}

/// Fenced code blocks and inline code spans of a Markdown document, with the
/// 0-based line each starts on.
fn markdown_commands(contents: &str) -> Vec<(usize, String)> {
    let mut commands = Vec::new();
    let mut open: Option<((char, usize), usize, String)> = None;
    for (index, line) in contents.lines().enumerate() {
        let marker = fence_marker(line);
        if let Some((opening, start, body)) = &mut open {
            if marker.is_some_and(|(ch, len)| ch == opening.0 && len >= opening.1) {
                commands.push((*start, std::mem::take(body)));
                open = None;
            } else {
                body.push_str(line);
                body.push('\n');
            }
        } else if let Some(marker) = marker {
            open = Some((marker, index + 1, String::new()));
        } else {
            commands.extend(
                line.split('`')
                    .skip(1)
                    .step_by(2)
                    .map(|span| (index, span.to_owned())),
            );
        }
    }
    commands.extend(open.map(|(_, start, body)| (start, body)));
    commands
}

fn fence_marker(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let ch = trimmed
        .chars()
        .next()
        .filter(|ch| matches!(ch, '`' | '~'))?;
    let len = trimmed.chars().take_while(|next| *next == ch).count();
    (len >= 3).then_some((ch, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(command: &str, fallback: Fallback<'_>) -> Vec<Vec<String>> {
        let origin = origin_at("x", 0, "test");
        command_entries(command, &origin, fallback)
            .into_iter()
            .map(|entry| entry.paths)
            .collect()
    }

    #[test]
    fn command_entries_resolve_directory_and_fallbacks() {
        assert_eq!(
            paths(
                "python3 fastmcp/tests/downstream/_harness.py",
                Fallback::CheckoutPrefix
            ),
            [[
                "fastmcp/tests/downstream/_harness.py",
                "tests/downstream/_harness.py"
            ]]
        );
        assert_eq!(
            paths("python ./gen.py", Fallback::CheckoutPrefix),
            [["gen.py"]]
        );
        assert_eq!(
            paths(
                "uv run --directory task-sdk -s dev/gen.py",
                Fallback::CheckoutPrefix
            ),
            [["task-sdk/dev/gen.py"]]
        );
        assert_eq!(
            paths("python tools\\gen.py", Fallback::None),
            [["tools/gen.py"]]
        );
        assert_eq!(
            paths(
                "uv run scripts/bump.py",
                Fallback::Under(".agents/skills/r")
            ),
            [["scripts/bump.py", ".agents/skills/r/scripts/bump.py"]]
        );
        assert!(paths("python ${{ matrix.dir }}/gen.py", Fallback::CheckoutPrefix).is_empty());
        assert!(paths("python $DIR/gen.py", Fallback::None).is_empty());
        assert!(paths("python ../gen.py", Fallback::None).is_empty());
        assert!(paths("python /opt/gen.py", Fallback::None).is_empty());
    }

    #[test]
    fn markdown_commands_read_fences_and_inline_spans() {
        let contents = "Run `uv run a.py` first.\n\n```bash\n   uv run b.py \\\n     --flag\n```\nThen `x` and `python c.py`.\n";
        assert_eq!(
            markdown_commands(contents),
            [
                (0, "uv run a.py".to_owned()),
                (3, "   uv run b.py \\\n     --flag\n".to_owned()),
                (6, "x".to_owned()),
                (6, "python c.py".to_owned()),
            ]
        );
    }

    #[test]
    fn markdown_fences_close_on_a_matching_marker() {
        let contents = "````\n```\npython a.py\n```\n````\n~~~\npython b.py\n";
        assert_eq!(
            markdown_commands(contents),
            [
                (1, "```\npython a.py\n```\n".to_owned()),
                (6, "python b.py\n".to_owned()),
            ]
        );
    }

    #[test]
    fn scan_reads_workflows_and_skill_docs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let workflows = root.join(".github").join("workflows");
        std::fs::create_dir_all(&workflows).expect("workflows");
        std::fs::write(
            workflows.join("ci.yml"),
            "jobs:\n  t:\n    steps:\n      - run: python3 repo/tools/check.py\n",
        )
        .expect("workflow");
        let skill = root.join(".claude").join("skills").join("release");
        std::fs::create_dir_all(&skill).expect("skill");
        std::fs::write(skill.join(SKILL_FILE), "```\nuv run tools/bump.py\n```\n")
            .expect("skill doc");

        let entries: Vec<_> = scan(root)
            .into_iter()
            .map(|entry| (entry.paths, entry.origin.file, entry.origin.line))
            .collect();
        assert_eq!(
            entries,
            [
                (
                    vec![
                        "repo/tools/check.py".to_owned(),
                        "tools/check.py".to_owned()
                    ],
                    ".github/workflows/ci.yml".to_owned(),
                    Some(4),
                ),
                (
                    vec![
                        "tools/bump.py".to_owned(),
                        ".claude/skills/release/tools/bump.py".to_owned()
                    ],
                    ".claude/skills/release/SKILL.md".to_owned(),
                    Some(2),
                ),
            ]
        );
    }
}
