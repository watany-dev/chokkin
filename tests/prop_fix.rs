//! Property-based tests for `--fix` end to end.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeSet;
use std::fs;
use std::process::Command;

use proptest::prelude::*;
use tempfile::TempDir;

fn project(deps: &[String], used: &BTreeSet<String>, eol: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("src/pkg")).expect("mkdir");
    fs::write(root.join("src/pkg/__init__.py"), "").expect("init");
    let imports: String = used.iter().fold(String::new(), |mut acc, name| {
        acc.push_str("import ");
        acc.push_str(name);
        acc.push('\n');
        acc
    });
    fs::write(
        root.join("src/pkg/main.py"),
        format!("{imports}def main():\n    pass\n"),
    )
    .expect("main");
    let quoted: Vec<String> = deps.iter().map(|d| format!("\"{d}\"")).collect();
    fs::write(
        root.join("pyproject.toml"),
        format!(
            "[project]{eol}name = \"pkg\"{eol}version = \"0.1\"{eol}dependencies = [{}]{eol}{eol}[project.scripts]{eol}pkg = \"pkg.main:main\"{eol}",
            quoted.join(", ")
        ),
    )
    .expect("pyproject");
    dir
}

fn run_fix(root: &std::path::Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .arg("--fix")
        .arg("--no-cache")
        .current_dir(root)
        .output()
        .expect("run chokkin");
    assert!(
        output.status.code().is_some_and(|code| code <= 1),
        "chokkin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn remaining_deps(root: &std::path::Path) -> Vec<String> {
    let text = fs::read_to_string(root.join("pyproject.toml")).expect("read");
    let doc: toml::Table = text.parse().expect("pyproject stays valid TOML");
    doc["project"]["dependencies"]
        .as_array()
        .expect("dependencies array")
        .iter()
        .map(|v| v.as_str().expect("string").to_owned())
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// `--fix` removes exactly the unused dependencies from
    /// `[project].dependencies`, however many are unused in the same array,
    /// and keeps the used ones in their original order.
    #[test]
    fn fix_removes_exactly_the_unused_dependencies(
        names in prop::collection::btree_set("zz[a-z]{3,6}", 1..6),
        used_mask in prop::collection::vec(any::<bool>(), 6),
    ) {
        let deps: Vec<String> = names.into_iter().collect();
        let used: BTreeSet<String> = deps
            .iter()
            .zip(used_mask.iter().cycle())
            .filter(|(_, used)| **used)
            .map(|(name, _)| name.clone())
            .collect();
        let dir = project(&deps, &used, "\n");
        run_fix(dir.path());
        let expected: Vec<String> = deps.iter().filter(|d| used.contains(*d)).cloned().collect();
        prop_assert_eq!(remaining_deps(dir.path()), expected);
    }

    /// The fix is idempotent: running `--fix` twice equals running it once.
    #[test]
    fn fix_is_idempotent(names in prop::collection::btree_set("zz[a-z]{3,6}", 1..5)) {
        let deps: Vec<String> = names.into_iter().collect();
        let dir = project(&deps, &BTreeSet::new(), "\n");
        run_fix(dir.path());
        let once = fs::read_to_string(dir.path().join("pyproject.toml")).expect("read");
        run_fix(dir.path());
        let twice = fs::read_to_string(dir.path().join("pyproject.toml")).expect("read");
        prop_assert_eq!(once, twice);
    }
}

/// One `[project].dependencies` entry in the format-variant property.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    suffix: &'static str,
    used: bool,
    inline_comment: bool,
    comment_above: bool,
}

#[derive(Debug, Clone)]
struct Layout {
    entries: Vec<Entry>,
    multiline: bool,
    trailing_comma: bool,
    crlf: bool,
}

fn entry_strategy() -> impl Strategy<Value = (&'static str, bool, bool, bool)> {
    (
        prop::sample::select(vec!["", ">=1", "[x]>=1", "==1.*"]),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
}

/// Distinct names in shuffled order, so declaration order differs from
/// alphabetical order. Each name sits in one bucket only, so one `--fix`
/// reaches the fixed point (ADR 0003 allows more runs otherwise).
fn layout_strategy() -> impl Strategy<Value = Layout> {
    prop::collection::btree_set("zz[a-z]{3,6}", 1..6)
        .prop_flat_map(|names| {
            let len = names.len();
            (
                Just(names.into_iter().collect::<Vec<_>>()).prop_shuffle(),
                prop::collection::vec(entry_strategy(), len),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
            )
        })
        .prop_map(|(names, shapes, multiline, trailing_comma, crlf)| Layout {
            entries: names
                .into_iter()
                .zip(shapes)
                .map(
                    |(name, (suffix, used, inline_comment, comment_above))| Entry {
                        name,
                        suffix,
                        used,
                        inline_comment,
                        comment_above,
                    },
                )
                .collect(),
            multiline,
            trailing_comma,
            crlf,
        })
}

const HEAD: &str = "# top comment\n[project]\nname = \"pkg\"  # keep me\nversion = \"0.1\"\n";
const TAIL: &str = "\n[project.scripts]\npkg = \"pkg.main:main\"\n\n[tool.other]\nkeys = [\"a\", \"b\"]  # untouched\n";

/// Renders the dependencies array; returns it with each entry's own line.
fn render_array(layout: &Layout) -> (String, Vec<String>) {
    let quoted = |entry: &Entry| format!("\"{}{}\"", entry.name, entry.suffix);
    let last = layout.entries.len() - 1;
    if !layout.multiline {
        let items: Vec<String> = layout.entries.iter().map(quoted).collect();
        let comma = if layout.trailing_comma { "," } else { "" };
        return (
            format!("dependencies = [{}{comma}]\n", items.join(", ")),
            Vec::new(),
        );
    }
    let mut text = String::from("dependencies = [\n");
    let mut lines = Vec::new();
    for (index, entry) in layout.entries.iter().enumerate() {
        if entry.comment_above {
            text.push_str("    # above-");
            text.push_str(&entry.name);
            text.push('\n');
        }
        let comma = if index < last || layout.trailing_comma {
            ","
        } else {
            ""
        };
        let comment = if entry.inline_comment {
            format!("  # c-{}", entry.name)
        } else {
            String::new()
        };
        let line = format!("    {}{comma}{comment}", quoted(entry));
        text.push_str(&line);
        text.push('\n');
        lines.push(line);
    }
    text.push_str("]\n");
    (text, lines)
}

/// Runs `--fix` and returns the names in its `applied` lines and in the
/// unused-dependencies report section, in printed order.
fn run_fix_reported(root: &std::path::Path) -> (Vec<String>, Vec<String>) {
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--fix", "--no-cache"])
        .current_dir(root)
        .output()
        .expect("run chokkin");
    assert!(
        output.status.code().is_some_and(|code| code <= 1),
        "chokkin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let applied = text
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("applied CHK002 "))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
        .collect();
    let reported = text
        .lines()
        .skip_while(|line| !line.starts_with("Unused dependencies"))
        .skip(1)
        .take_while(|line| line.starts_with("  "))
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect();
    (applied, reported)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Whatever the array's layout (single or multi-line, trailing comma,
    /// inline and standalone comments, CRLF, version and extra specifiers),
    /// `--fix` drops exactly the unused entries together with their own
    /// comments, keeps every other byte, and reaches its fixed point.
    #[test]
    fn fix_preserves_layout_around_removed_entries(layout in layout_strategy()) {
        let used: BTreeSet<String> = layout
            .entries
            .iter()
            .filter(|entry| entry.used)
            .map(|entry| entry.name.clone())
            .collect();
        let dir = project(&[], &used, "\n");
        let root = dir.path();
        let (array, kept_lines) = render_array(&layout);
        let source = format!("{HEAD}{array}{TAIL}");
        let source = if layout.crlf { source.replace('\n', "\r\n") } else { source };
        fs::write(root.join("pyproject.toml"), source).expect("pyproject");

        let (applied, reported) = run_fix_reported(root);
        let once = fs::read_to_string(root.join("pyproject.toml")).expect("read");

        // #443: fixes apply in report order, one per unused entry.
        prop_assert_eq!(&applied, &reported);
        let unused: BTreeSet<&str> = layout
            .entries
            .iter()
            .filter(|entry| !entry.used)
            .map(|entry| entry.name.as_str())
            .collect();
        prop_assert_eq!(applied.iter().map(String::as_str).collect::<BTreeSet<_>>(), unused);

        let expected: Vec<String> = layout
            .entries
            .iter()
            .filter(|entry| entry.used)
            .map(|entry| format!("{}{}", entry.name, entry.suffix))
            .collect();
        prop_assert_eq!(remaining_deps(root), expected);

        if layout.crlf {
            prop_assert!(!once.replace("\r\n", "").contains('\n'), "{once:?}");
        }
        let plain = once.replace("\r\n", "\n");
        prop_assert!(plain.starts_with(HEAD), "{plain}");
        prop_assert!(plain.ends_with(TAIL), "{plain}");

        // A kept line survives verbatim apart from the comma that removing
        // the entries after it can drop.
        let uncomma = |line: &str| line.replacen("\",", "\"", 1);
        let output_lines: Vec<String> = plain.lines().map(uncomma).collect();
        for (entry, line) in layout.entries.iter().zip(&kept_lines) {
            if entry.used {
                prop_assert!(output_lines.contains(&uncomma(line)), "{line:?} in {plain}");
                if entry.comment_above {
                    let above = format!("    # above-{}", entry.name);
                    prop_assert!(output_lines.contains(&above), "{plain}");
                }
            } else {
                for comment in [format!("# c-{}", entry.name), format!("# above-{}", entry.name)] {
                    prop_assert!(!plain.contains(&comment), "{comment} in {plain}");
                }
            }
        }

        run_fix(root);
        let twice = fs::read_to_string(root.join("pyproject.toml")).expect("read");
        prop_assert_eq!(once, twice);
    }
}
