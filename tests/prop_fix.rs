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
