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
        assert_eq!(
            paths("python ${{ matrix.dir }}/gen.py", Fallback::CheckoutPrefix),
            Vec::<Vec<String>>::new()
        );
        assert_eq!(
            paths("python $DIR/gen.py", Fallback::None),
            Vec::<Vec<String>>::new()
        );
        assert_eq!(
            paths("python ../gen.py", Fallback::None),
            Vec::<Vec<String>>::new()
        );
        assert_eq!(
            paths("python /opt/gen.py", Fallback::None),
            Vec::<Vec<String>>::new()
        );
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

    /// Spec §8's Markdown scope (#861): fences open on three or more `` ` ``
    /// or `~` and close on as many of the same, an unclosed fence runs to the
    /// end, and outside fences only single-line `` `code` `` spans are read.
    mod props {
        use super::*;
        use proptest::prelude::*;

        /// A text line: plain parts, odd entries are code spans.
        type TextLine = Vec<&'static str>;

        #[derive(Debug, Clone)]
        enum Block {
            Text(Vec<TextLine>),
            Fence {
                ch: char,
                len: usize,
                indent: &'static str,
                info: &'static str,
                body: Vec<String>,
                /// Extra marker length and indent of the closing line.
                close: Option<(usize, &'static str)>,
            },
        }

        const PLAIN: [&str; 5] = ["run ", "python outside.py", " see ", "", "uv run ~/x.py "];
        const SPANS: [&str; 4] = ["python scripts/a.py", "uv run b.py", "x", "a b"];

        fn text_line() -> impl Strategy<Value = TextLine> {
            prop_oneof![
                4 => spans_line(),
                // Two markers open no fence: `` is an empty span.
                1 => Just(vec!["", "", ""]),
                1 => Just(vec!["~~ two"]),
            ]
        }

        fn spans_line() -> impl Strategy<Value = TextLine> {
            prop::collection::vec(
                (
                    prop::sample::select(PLAIN.to_vec()),
                    prop::sample::select(SPANS.to_vec()),
                ),
                0..3,
            )
            .prop_flat_map(|pairs| {
                prop::sample::select(PLAIN.to_vec()).prop_map(move |last| {
                    let mut line: TextLine = pairs
                        .iter()
                        .flat_map(|(plain, span)| [*plain, *span])
                        .collect();
                    line.push(last);
                    line
                })
            })
        }

        fn fence() -> impl Strategy<Value = Block> {
            (prop::sample::select(vec!['`', '~']), 3usize..6)
                .prop_flat_map(|(ch, len)| {
                    let other = if ch == '`' { '~' } else { '`' };
                    let body_line = prop_oneof![
                        prop::sample::select(vec![
                            "python inside.py",
                            "  uv run tools/b.py",
                            "",
                            "echo `x`",
                        ])
                        .prop_map(str::to_owned),
                        // A shorter marker or the other character stays body.
                        (1..len).prop_map(move |n| ch.to_string().repeat(n)),
                        (3usize..8).prop_map(move |n| other.to_string().repeat(n)),
                    ];
                    (
                        Just(ch),
                        Just(len),
                        prop::sample::select(vec!["", " ", "   "]),
                        prop::sample::select(vec!["", "bash", " sh {.x}"]),
                        prop::collection::vec(body_line, 0..4),
                        proptest::option::of((0usize..3, prop::sample::select(vec!["", "  "]))),
                    )
                })
                .prop_map(|(ch, len, indent, info, body, close)| Block::Fence {
                    ch,
                    len,
                    indent,
                    info,
                    body,
                    close,
                })
        }

        fn document() -> impl Strategy<Value = Vec<Block>> {
            prop::collection::vec(
                prop_oneof![
                    prop::collection::vec(text_line(), 1..3).prop_map(Block::Text),
                    fence(),
                ],
                0..6,
            )
        }

        /// The document's lines and the commands spec §8 reads from them.
        fn render(blocks: &[Block]) -> (Vec<String>, Vec<(usize, String)>) {
            let mut lines: Vec<String> = Vec::new();
            let mut expected = Vec::new();
            let last = blocks.len().saturating_sub(1);
            for (index, block) in blocks.iter().enumerate() {
                match block {
                    Block::Text(text) => {
                        for line in text {
                            expected.extend(
                                line.iter()
                                    .skip(1)
                                    .step_by(2)
                                    .map(|span| (lines.len(), (*span).to_owned())),
                            );
                            lines.push(line.join("`"));
                        }
                    },
                    Block::Fence {
                        ch,
                        len,
                        indent,
                        info,
                        body,
                        close,
                    } => {
                        let marker = ch.to_string().repeat(*len);
                        lines.push(format!("{indent}{marker}{info}"));
                        let start = lines.len();
                        lines.extend(body.iter().cloned());
                        let text: String =
                            body.iter().flat_map(|line| [line.as_str(), "\n"]).collect();
                        expected.push((start, text));
                        // Only the last fence may stay open.
                        let close = close.or((index != last).then_some((0, "")));
                        if let Some((extra, indent)) = close {
                            lines.push(format!("{indent}{}", ch.to_string().repeat(len + extra)));
                        }
                    },
                }
            }
            (lines, expected)
        }

        proptest! {
            /// Fenced bodies and inline spans come out with their lines; the
            /// text around them, a shorter marker and the other fence
            /// character inside a fence do not, and an unclosed last fence
            /// runs to the end.
            #[test]
            fn markdown_commands_match_the_fences(
                blocks in document(),
                crlf in any::<bool>(),
            ) {
                let (lines, expected) = render(&blocks);
                let end = if crlf { "\r\n" } else { "\n" };
                let contents: String = lines.iter().flat_map(|line| [line.as_str(), end]).collect();
                prop_assert_eq!(markdown_commands(&contents), expected);
            }

            /// Any text, unbalanced backticks and fences included, reads
            /// without a panic.
            #[test]
            fn markdown_commands_never_panic(contents in "([`~ a\\n\\r]|é|```|~~~){0,40}") {
                let _ = markdown_commands(&contents);
            }
        }
    }
}
