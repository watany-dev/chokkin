//! Property-based tests for what a fresh analysis finds after `--fix`
//! (#1109 §4, ADR 0003): a fix removes the issues it applies and adds none,
//! whatever mix of CHK001 file removals, CHK002 removals and CHK005 moves it
//! applies together, and removing a certain CHK001 file leaves every other
//! finding as it was.
//!
//! An `__init__.py` is never a CHK001 candidate (§11), `--strict` included,
//! so a removal never turns a package into a namespace package; the
//! generator still varies a missing `src/pkg/__init__.py` and a subpackage
//! whose modules all go.

#![expect(
    clippy::expect_used,
    reason = "helpers outside `#[test]` fns abort the test on a failed setup step"
)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command;

use proptest::prelude::*;
use tempfile::TempDir;

/// No name is an affixed form of another, so the loose resolver step never
/// links two of them.
const POOL: [&str; 3] = ["zz-alpha", "zz-bravo", "zz-charlie"];

fn module(name: &str) -> String {
    name.replace('-', "_")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Declared {
    None,
    Runtime,
    Dev,
    Both,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent axis the property test varies"
)]
#[derive(Debug, Clone, Copy)]
struct Dep {
    declared: Declared,
    /// `import` in the app's entry module.
    main: bool,
    /// `import` in `tests/test_app.py`.
    test: bool,
    /// `import` in an orphan module.
    orphan: bool,
    /// `import` in the orphan subpackage's `__init__.py`.
    subpackage: bool,
}

#[derive(Debug, Clone)]
struct Spec {
    deps: Vec<Dep>,
    /// Orphan modules `src/pkg/orphan_<i>.py`; each imports the next one.
    orphans: usize,
    /// `src/pkg/sub/` with `__init__.py` and `x.py`, which nothing imports.
    subpackage: bool,
    /// Whether `main.py` imports `pkg.sub.x`, so `sub/__init__.py` loads.
    sub_used: bool,
    /// Whether `src/pkg/__init__.py` exists; without it `pkg` is a
    /// namespace package.
    package_init: bool,
}

fn dep_strategy() -> impl Strategy<Value = Dep> {
    (
        prop::sample::select(vec![
            Declared::None,
            Declared::Runtime,
            Declared::Dev,
            Declared::Both,
        ]),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(declared, main, test, orphan, subpackage)| Dep {
            declared,
            main,
            test,
            orphan,
            subpackage,
        })
}

fn spec_strategy() -> impl Strategy<Value = Spec> {
    (
        prop::collection::vec(dep_strategy(), POOL.len()),
        0..=2_usize,
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(deps, orphans, subpackage, sub_used, package_init)| Spec {
            deps,
            orphans,
            subpackage,
            sub_used,
            package_init,
        })
}

fn write(root: &Path, file: &str, text: &str) {
    let path = root.join(file);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, text).expect("write");
}

fn imports(spec: &Spec, pick: impl Fn(&Dep) -> bool) -> String {
    POOL.iter()
        .zip(&spec.deps)
        .filter(|(_, dep)| pick(dep))
        .fold(String::new(), |mut text, (name, _)| {
            writeln!(text, "import {}", module(name)).expect("fmt");
            text
        })
}

fn project(spec: &Spec) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    let declared = |kinds: &[Declared]| -> String {
        POOL.iter()
            .zip(&spec.deps)
            .filter(|(_, dep)| kinds.contains(&dep.declared))
            .map(|(name, _)| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    write(
        root,
        "pyproject.toml",
        &format!(
            "[project]\nname = \"pkg\"\nversion = \"0.1.0\"\ndependencies = [{}]\n\n[dependency-groups]\ndev = [{}]\n\n[project.scripts]\npkg = \"pkg.main:main\"\n\n[tool.chokkin]\nmode = \"app\"\n",
            declared(&[Declared::Runtime, Declared::Both]),
            declared(&[Declared::Dev, Declared::Both]),
        ),
    );
    if spec.package_init {
        write(root, "src/pkg/__init__.py", "");
    }
    let mut main = imports(spec, |dep| dep.main);
    if spec.subpackage && spec.sub_used {
        main.push_str("import pkg.sub.x\n");
    }
    main.push_str("\n\ndef main():\n    pass\n");
    write(root, "src/pkg/main.py", &main);
    write(
        root,
        "tests/test_app.py",
        &format!("import pkg.main\n{}", imports(spec, |dep| dep.test)),
    );
    for index in 0..spec.orphans {
        let mut text = imports(spec, |dep| dep.orphan);
        if index + 1 < spec.orphans {
            writeln!(text, "from pkg import orphan_{}", index + 1).expect("fmt");
        }
        write(root, &format!("src/pkg/orphan_{index}.py"), &text);
    }
    if spec.subpackage {
        write(
            root,
            "src/pkg/sub/__init__.py",
            &imports(spec, |dep| dep.subpackage),
        );
        write(root, "src/pkg/sub/x.py", "");
    }
    dir
}

type Key = (String, String);

/// `(code, target)` of every issue.
fn issues(root: &Path) -> BTreeSet<Key> {
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--reporter", "json", "--no-cache"])
        .arg(root)
        .output()
        .expect("run chokkin");
    assert!(
        output.status.code().is_some_and(|code| code <= 1),
        "chokkin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("json reporter output");
    parsed["issues"]
        .as_array()
        .expect("issues array")
        .iter()
        .map(|issue| {
            let field = |name: &str| issue[name].as_str().unwrap_or_default().to_owned();
            (field("code"), field("target"))
        })
        .collect()
}

/// Runs `--fix` with `extra` and returns the `(rule, target)` it applied.
fn fix(root: &Path, extra: &[&str]) -> BTreeSet<Key> {
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--fix", "--no-cache"])
        .args(extra)
        .current_dir(root)
        .output()
        .expect("run chokkin");
    assert!(
        output.status.code().is_some_and(|code| code <= 1),
        "chokkin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("applied "))
        .filter_map(|rest| {
            let mut words = rest.split_whitespace();
            Some((words.next()?.to_owned(), words.next()?.to_owned()))
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(chokkin::internals::proptest_cases(32)))]

    /// After `--fix --allow-remove-files`, a fresh analysis finds no issue
    /// the analysis before it did not, and none of the applied ones.
    #[test]
    fn fix_adds_no_finding(spec in spec_strategy()) {
        let dir = project(&spec);
        let root = dir.path();
        let before = issues(root);
        let applied = fix(root, &["--allow-remove-files"]);
        let after = issues(root);
        let applied_keys: BTreeSet<Key> = before
            .iter()
            .filter(|(code, target)| {
                applied.iter().any(|(rule, name)| rule == code && target == name)
            })
            .cloned()
            .collect();
        prop_assert_eq!(applied_keys.len(), applied.len(), "applied {:?} of {:?}", applied, before);
        let new: Vec<&Key> = after.difference(&before).collect();
        prop_assert!(new.is_empty(), "new {:?} after applying {:?} to {:#?}", new, applied, spec);
        let kept: Vec<&Key> = after.intersection(&applied_keys).collect();
        prop_assert!(kept.is_empty(), "{:?} still reported after the fix", kept);
    }

    /// Removing the certain CHK001 files alone changes no other finding: the
    /// remaining files reach the same files and declare the same use.
    #[test]
    fn file_removal_keeps_every_other_finding(spec in spec_strategy()) {
        let dir = project(&spec);
        let root = dir.path();
        // Settle the manifest first, so only the file removal differs.
        fix(root, &[]);
        let before = issues(root);
        let applied = fix(root, &["--allow-remove-files"]);
        prop_assert!(applied.iter().all(|(rule, _)| rule == "CHK001"), "{:?}", applied);
        let after = issues(root);
        let expected: BTreeSet<Key> = before
            .iter()
            .filter(|key| !applied.contains(key))
            .cloned()
            .collect();
        prop_assert_eq!(after, expected, "after removing {:?} from {:#?}", applied, spec);
    }
}
