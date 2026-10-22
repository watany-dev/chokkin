//! Property-based tests for the analysis cache end to end (§19): after any
//! run, editing the project and running again with the cache reports what a
//! `--no-cache` run of the edited project reports.
//!
//! The edits reach every input the cache keys on besides the sources: the
//! manifest, a `requirements` file's `-r` include, `.gitignore`, and a
//! standalone config file. Each state is written over the previous one, so a
//! file that does not change keeps its mtime and the warm run may reuse it.

#![expect(
    clippy::expect_used,
    reason = "helpers outside `#[test]` fns abort the test on a failed setup step"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use proptest::prelude::*;
use tempfile::TempDir;

const DISTRIBUTIONS: [&str; 4] = ["requests", "rich", "click", "pyyaml"];
const MODULES: [&str; 5] = ["requests", "rich", "click", "yaml", "zzmissing"];
const IGNORED: [&str; 3] = ["legacy/", "src/pkg/b.py", "extra.txt"];
const CONFIGS: [&str; 4] = [
    "exclude = [\"legacy/**\"]\n",
    "confidence = \"maybe\"\n",
    "[severity]\nCHK002 = \"off\"\n",
    "[package_module_map]\n\"click\" = [\"zzmissing\"]\n",
];
const HELPERS: [&str; 3] = ["", "import rich\n", "import click\nimport zzmissing\n"];

/// Each field is one input an edit replaces on its own, so the files it
/// leaves alone keep their mtime.
#[derive(Debug, Clone)]
struct State {
    /// `[project] dependencies`, when the key is there.
    pyproject: Option<BTreeSet<usize>>,
    /// `requirements.txt`.
    requirements: Option<BTreeSet<usize>>,
    /// `extra.txt`, which `requirements.txt` includes with `-r`.
    include: Option<BTreeSet<usize>>,
    imports: BTreeSet<usize>,
    /// What `src/pkg/a.py` imports, when `main.py` imports it.
    helper: Option<usize>,
    /// `src/pkg/b.py` and `legacy/old.py`, which nothing imports.
    orphans: (bool, bool),
    gitignore: Option<BTreeSet<usize>>,
    config: Option<usize>,
}

const FIELDS: usize = 8;

fn set(len: usize) -> impl Strategy<Value = BTreeSet<usize>> {
    prop::collection::btree_set(0..len, 0..=len)
}

fn state_strategy() -> impl Strategy<Value = State> {
    let distributions = || prop::option::of(set(DISTRIBUTIONS.len()));
    (
        (distributions(), distributions(), distributions()),
        set(MODULES.len()),
        prop::option::of(0..HELPERS.len()),
        (any::<bool>(), any::<bool>()),
        prop::option::of(set(IGNORED.len())),
        prop::option::of(0..CONFIGS.len()),
    )
        .prop_map(
            |((pyproject, requirements, include), imports, helper, orphans, gitignore, config)| {
                State {
                    pyproject,
                    requirements,
                    include,
                    imports,
                    helper,
                    orphans,
                    gitignore,
                    config,
                }
            },
        )
}

/// `state` with field `field` taken from `from`.
fn edited(state: &State, field: usize, from: &State) -> State {
    let mut next = state.clone();
    match field {
        0 => next.pyproject.clone_from(&from.pyproject),
        1 => next.requirements.clone_from(&from.requirements),
        2 => next.include.clone_from(&from.include),
        3 => next.imports.clone_from(&from.imports),
        4 => next.helper = from.helper,
        5 => next.orphans = from.orphans,
        6 => next.gitignore.clone_from(&from.gitignore),
        _ => next.config = from.config,
    }
    next
}

fn lines<'a>(indices: &BTreeSet<usize>, names: &[&'a str]) -> Vec<&'a str> {
    indices.iter().map(|&index| names[index]).collect()
}

/// Every file of the project in `state`, by root-relative path.
fn files(state: &State) -> BTreeMap<&'static str, String> {
    let mut files = BTreeMap::new();
    let dependencies = state.pyproject.as_ref().map_or_else(String::new, |names| {
        let quoted: Vec<String> = lines(names, &DISTRIBUTIONS)
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect();
        format!("dependencies = [{}]\n", quoted.join(", "))
    });
    if state.requirements.is_some() || state.include.is_some() {
        let mut text = state
            .requirements
            .as_ref()
            .map_or_else(Vec::new, |names| lines(names, &DISTRIBUTIONS));
        if state.include.is_some() {
            text.push("-r extra.txt");
        }
        files.insert("requirements.txt", text.join("\n") + "\n");
    }
    if let Some(include) = &state.include {
        files.insert(
            "extra.txt",
            lines(include, &DISTRIBUTIONS).join("\n") + "\n",
        );
    }
    files.insert(
        "pyproject.toml",
        format!(
            "[project]\nname = \"pkg\"\nversion = \"0.1.0\"\n{dependencies}\n\
             [project.scripts]\npkg = \"pkg.main:run\"\n"
        ),
    );
    let mut main: Vec<String> = lines(&state.imports, &MODULES)
        .iter()
        .map(|module| format!("import {module}"))
        .collect();
    if let Some(helper) = state.helper {
        main.push("import pkg.a".to_owned());
        files.insert("src/pkg/a.py", HELPERS[helper].to_owned());
    }
    main.push("\n\ndef run():\n    pass\n".to_owned());
    files.insert("src/pkg/main.py", main.join("\n"));
    files.insert("src/pkg/__init__.py", String::new());
    if state.orphans.0 {
        files.insert("src/pkg/b.py", "import rich\n".to_owned());
    }
    if state.orphans.1 {
        files.insert("legacy/old.py", "import requests\n".to_owned());
    }
    if let Some(ignored) = &state.gitignore {
        files.insert(".gitignore", lines(ignored, &IGNORED).join("\n") + "\n");
    }
    if let Some(config) = state.config {
        files.insert("chokkin.toml", CONFIGS[config].to_owned());
    }
    files
}

/// Rewrite the project from `before` to `after`, leaving unchanged files alone.
fn apply(
    root: &Path,
    before: &BTreeMap<&'static str, String>,
    after: &BTreeMap<&'static str, String>,
) {
    for path in before.keys().filter(|path| !after.contains_key(*path)) {
        fs::remove_file(root.join(path)).expect("remove");
    }
    for (path, text) in after {
        if before.get(path) == Some(text) {
            continue;
        }
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, text).expect("write");
    }
}

/// The JSON report and exit code of one run.
fn run(root: &Path, cache: bool) -> (serde_json::Value, Option<i32>) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_chokkin"));
    command.args(["--reporter", "json"]);
    if !cache {
        command.arg("--no-cache");
    }
    let output = command.arg(root).output().expect("run chokkin");
    assert!(
        output.status.code().is_some_and(|code| code <= 1),
        "chokkin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = serde_json::from_slice(&output.stdout).expect("json reporter output");
    (report, output.status.code())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(chokkin::internals::proptest_cases(24)))]

    /// A cold run, then each edit followed by a cached run: every cached run
    /// reports exactly what a `--no-cache` run of the same tree reports.
    #[test]
    fn warm_runs_match_uncached_runs(
        first in state_strategy(),
        edits in prop::collection::vec((0..FIELDS, state_strategy()), 1..5),
    ) {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        let mut state = first;
        let mut written = files(&state);
        apply(root, &BTreeMap::new(), &written);
        run(root, true);
        for (field, from) in &edits {
            state = edited(&state, *field, from);
            let next = files(&state);
            apply(root, &written, &next);
            written = next;
            let cached = run(root, true);
            let uncached = run(root, false);
            prop_assert_eq!(cached, uncached, "{:?}", state);
        }
    }
}
