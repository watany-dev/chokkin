//! Property-based tests for the CHK002 use of workspace sources (#1109 §2).
//!
//! `mark_workspace_source_distributions` finds the members a root uses round
//! by round: the root's evidence marks a member, and every import under a
//! used member's tree marks the members it names in the next round. The
//! reference model is that least fixed point, written as a plain closure.
//!
//! The root and a member read their imports differently, on purpose: the
//! root counts only the imports of files a root reaches, while a used member
//! counts every file under its tree, tests included. A member's own tests
//! are not the root's, so nothing reaches them, yet the member's manifest is
//! what declares what they import; reading them only through reachability
//! would report a member the used one's test suite needs as unused.

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

/// Member distributions. No name is an affixed form of another, so the
/// loose resolver step never links two of them.
const MEMBERS: [&str; 4] = ["zzm-alpha", "zzm-bravo", "zzm-charlie", "zzm-delta"];

fn module(name: &str) -> String {
    name.replace('-', "_")
}

fn dir(name: &str) -> &str {
    name.trim_start_matches("zzm-")
}

/// How the root shows it uses a member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Evidence {
    None,
    /// `import` in `main.py`, the app's root.
    Main,
    /// `import` in `orphan.py`, which no root reaches.
    Orphan,
    /// A `[project.scripts]` target in the member's package.
    Script,
    /// `-p <member>.core` in the root's pytest `addopts`.
    PytestPlugin,
    /// `import` in `tests/test_root.py`.
    Test,
}

impl Evidence {
    const ALL: [Self; 6] = [
        Self::None,
        Self::Main,
        Self::Orphan,
        Self::Script,
        Self::PytestPlugin,
        Self::Test,
    ];

    /// Whether the evidence alone makes the member used.
    fn uses(self) -> bool {
        !matches!(self, Self::None | Self::Orphan)
    }
}

#[derive(Debug, Clone)]
struct Member {
    evidence: Evidence,
    /// Members `core.py` of this member imports.
    core: Vec<usize>,
    /// Members `tests/test_member.py` of this member imports.
    tests: Vec<usize>,
}

#[derive(Debug, Clone)]
struct Spec {
    members: Vec<Member>,
    /// Order of `members` and `dependencies` in the root manifest.
    order: Vec<usize>,
}

fn member_strategy(count: usize) -> impl Strategy<Value = Member> {
    (
        prop::sample::select(Evidence::ALL.to_vec()),
        prop::collection::btree_set(0..count, 0..=2),
        prop::collection::btree_set(0..count, 0..=1),
    )
        .prop_map(|(evidence, core, tests)| Member {
            evidence,
            core: core.into_iter().collect(),
            tests: tests.into_iter().collect(),
        })
}

fn spec_strategy() -> impl Strategy<Value = Spec> {
    (2..=MEMBERS.len())
        .prop_flat_map(|count| {
            (
                prop::collection::vec(member_strategy(count), count),
                Just((0..count).collect::<Vec<_>>()).prop_shuffle(),
            )
        })
        .prop_map(|(members, order)| Spec { members, order })
}

fn write(root: &Path, file: &str, text: &str) {
    let path = root.join(file);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, text).expect("write");
}

fn imports(targets: &[usize]) -> String {
    targets.iter().fold(String::new(), |mut text, &target| {
        writeln!(text, "import {}", module(MEMBERS[target])).expect("fmt");
        text
    })
}

fn project(spec: &Spec) -> TempDir {
    let dir_handle = TempDir::new().expect("tempdir");
    let root = dir_handle.path();
    let names: Vec<&str> = spec.order.iter().map(|&index| MEMBERS[index]).collect();
    let quoted = |items: Vec<String>| items.join(", ");
    let scripts: Vec<String> = spec
        .members
        .iter()
        .enumerate()
        .filter(|(_, member)| member.evidence == Evidence::Script)
        .map(|(index, _)| {
            let name = MEMBERS[index];
            format!("{} = \"{}.core:main\"", dir(name), module(name))
        })
        .collect();
    let plugins: Vec<String> = spec
        .members
        .iter()
        .enumerate()
        .filter(|(_, member)| member.evidence == Evidence::PytestPlugin)
        .map(|(index, _)| format!("-p {}.core", module(MEMBERS[index])))
        .collect();
    let mut manifest = format!(
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [{}]\n\n[project.scripts]\n{}\n\n[tool.uv.sources]\n{}\n\n[tool.uv.workspace]\nmembers = [{}]\n\n[tool.chokkin]\nmode = \"app\"\n",
        quoted(names.iter().map(|name| format!("\"{name}\"")).collect()),
        scripts.join("\n"),
        names
            .iter()
            .map(|name| format!("{name} = {{ workspace = true }}"))
            .collect::<Vec<_>>()
            .join("\n"),
        quoted(
            names
                .iter()
                .map(|name| format!("\"{}\"", dir(name)))
                .collect()
        ),
    );
    if !plugins.is_empty() {
        write!(
            manifest,
            "\n[tool.pytest.ini_options]\naddopts = \"{}\"\n",
            plugins.join(" ")
        )
        .expect("fmt");
    }
    write(root, "pyproject.toml", &manifest);

    let with = |evidence| -> Vec<usize> {
        (0..spec.members.len())
            .filter(|&index| spec.members[index].evidence == evidence)
            .collect()
    };
    write(root, "main.py", &imports(&with(Evidence::Main)));
    write(root, "orphan.py", &imports(&with(Evidence::Orphan)));
    write(root, "tests/test_root.py", &imports(&with(Evidence::Test)));

    for &index in &spec.order {
        let (name, member) = (MEMBERS[index], &spec.members[index]);
        let (dir, package) = (dir(name), module(name));
        write(
            root,
            &format!("{dir}/pyproject.toml"),
            &format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
        );
        write(root, &format!("{dir}/src/{package}/__init__.py"), "");
        write(
            root,
            &format!("{dir}/src/{package}/core.py"),
            &format!("{}\n\ndef main():\n    pass\n", imports(&member.core)),
        );
        write(
            root,
            &format!("{dir}/tests/test_member.py"),
            &imports(&member.tests),
        );
    }
    dir_handle
}

/// Members CHK002 reports, by distribution name.
fn unused(root: &Path) -> BTreeSet<String> {
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
        .filter(|issue| issue["code"] == "CHK002")
        .filter_map(|issue| issue["distribution"].as_str())
        .map(str::to_owned)
        .collect()
}

/// The members the reference closure does not reach.
fn expected(spec: &Spec) -> BTreeSet<String> {
    // A member's tests are test roots, so what they import is the root's
    // reachable evidence whether or not anything uses that member.
    let tested: BTreeSet<usize> = spec
        .members
        .iter()
        .flat_map(|member| member.tests.iter().copied())
        .collect();
    let mut used: Vec<bool> = spec
        .members
        .iter()
        .enumerate()
        .map(|(index, member)| member.evidence.uses() || tested.contains(&index))
        .collect();
    loop {
        let next: Vec<bool> = (0..used.len())
            .map(|target| {
                used[target]
                    || spec
                        .members
                        .iter()
                        .enumerate()
                        .any(|(index, member)| used[index] && member.core.contains(&target))
            })
            .collect();
        if next == used {
            break;
        }
        used = next;
    }
    (0..used.len())
        .filter(|&index| !used[index])
        .map(|index| MEMBERS[index].to_owned())
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(chokkin::internals::proptest_cases(32)))]

    /// CHK002 reports exactly the members the reference closure leaves out.
    #[test]
    fn workspace_members_used_match_the_closure(spec in spec_strategy()) {
        let dir = project(&spec);
        prop_assert_eq!(unused(dir.path()), expected(&spec), "{:#?}", spec);
    }

    /// The order the root declares its members in never changes the result.
    #[test]
    fn workspace_members_used_ignore_declaration_order(
        spec in spec_strategy(),
        shuffled in Just((0..MEMBERS.len()).collect::<Vec<usize>>()).prop_shuffle(),
    ) {
        let reordered = Spec {
            order: shuffled.into_iter().filter(|&index| index < spec.members.len()).collect(),
            ..spec.clone()
        };
        let (first, second) = (project(&spec), project(&reordered));
        prop_assert_eq!(unused(first.path()), unused(second.path()));
    }

    /// More evidence, from the root or a member's tree, never adds a CHK002.
    #[test]
    fn workspace_members_used_are_monotone(
        spec in spec_strategy(),
        pick in any::<prop::sample::Index>(),
        evidence in prop::sample::select(Evidence::ALL.to_vec()),
        edge in any::<prop::sample::Index>(),
    ) {
        let base = unused(project(&spec).path());
        let target = pick.index(spec.members.len());

        let mut stronger = spec.clone();
        if !spec.members[target].evidence.uses() {
            stronger.members[target].evidence = evidence;
        }
        let after = unused(project(&stronger).path());
        prop_assert!(after.is_subset(&base), "{after:?} vs {base:?}");

        let mut wider = spec.clone();
        let to = edge.index(spec.members.len());
        if !wider.members[target].core.contains(&to) {
            wider.members[target].core.push(to);
        }
        let after = unused(project(&wider).path());
        prop_assert!(after.is_subset(&base), "{after:?} vs {base:?}");
    }
}

/// A `pytest-*` member is used whenever pytest is, without any import or
/// entry point, and the members its tree imports are used through it
/// (#1109).
#[test]
fn pytest_plugin_member_passes_its_imports_on() {
    let dir_handle = TempDir::new().expect("tempdir");
    let root = dir_handle.path();
    write(
        root,
        "pyproject.toml",
        "[project]\nname = \"acme\"\nversion = \"0.1.0\"\ndependencies = [\"zzm-alpha\"]\n\n[dependency-groups]\ndev = [\"pytest\", \"pytest-zzm\"]\n\n[tool.uv.sources]\nzzm-alpha = { workspace = true }\npytest-zzm = { workspace = true }\n\n[tool.uv.workspace]\nmembers = [\"alpha\", \"plugin\"]\n\n[tool.chokkin]\nmode = \"app\"\n",
    );
    write(root, "main.py", "");
    write(root, "tests/test_root.py", "import pytest\n");
    write(
        root,
        "alpha/pyproject.toml",
        "[project]\nname = \"zzm-alpha\"\nversion = \"0.1.0\"\n",
    );
    write(root, "alpha/src/zzm_alpha/__init__.py", "");
    write(
        root,
        "plugin/pyproject.toml",
        "[project]\nname = \"pytest-zzm\"\nversion = \"0.1.0\"\n",
    );
    write(root, "plugin/src/pytest_zzm/__init__.py", "");
    write(
        root,
        "plugin/src/pytest_zzm/plugin.py",
        "import zzm_alpha\n",
    );
    assert_eq!(unused(root), BTreeSet::new());
}
