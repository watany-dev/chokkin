//! Property-based tests for how the analysis mode and the run flags move
//! findings, end to end on generated projects (#861 §9).
//!
//! - Analyzing one uv workspace member alone never raises the severity of its
//!   CHK006 findings over a run of the whole workspace, and finds every CHK006
//!   the whole run found there: the other members only add uses
//!   (`docs/dev/precision-gap-analysis.ja.md` appendix B 4(b)). The member's
//!   mode comes from its own manifest, so dropping its neighbours must not
//!   flip it.
//! - `--strict` only widens what the default run reports.
//! - `--production` drops the non-runtime contexts. That can only remove the
//!   findings an import raises (CHK003/CHK004/CHK010), and can only add the
//!   ones reachability raises (spec §2: "productionで未使用" is stricter):
//!   a file unreachable by default stays CHK001, and a symbol unused by
//!   default stays CHK006 unless its whole file turned CHK001. CHK002,
//!   CHK005 and CHK009 move both ways by design (a test-only dependency stops
//!   being used, a dev group stops being read), so they are not compared.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command;

use proptest::prelude::*;
use tempfile::TempDir;

/// `fingerprint -> severity` of the run's issues.
fn issues(root: &Path, flags: &[&str]) -> BTreeMap<String, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--reporter", "json", "--no-cache"])
        .args(flags)
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
            (
                issue["fingerprint"]
                    .as_str()
                    .expect("fingerprint")
                    .to_owned(),
                issue["severity"].as_str().expect("severity").to_owned(),
            )
        })
        .collect()
}

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, contents).expect("write");
}

fn severity_rank(severity: &str) -> u8 {
    match severity {
        "info" => 0,
        "warning" => 1,
        "error" => 2,
        other => panic!("unknown severity {other}"),
    }
}

// ---------------------------------------------------------------------------
// Workspace: one member alone vs. the whole workspace.

#[derive(Debug, Clone)]
struct Member {
    /// Without `[project].name` the member is no library (app mode).
    named: bool,
    /// `Some(true)` declares a console script on the member's own `main`,
    /// `Some(false)` a gui script (an app signal); `None` neither.
    script: Option<bool>,
    functions: usize,
    /// `(member, function)` raw picks, wrapped onto the generated members.
    imports: Vec<(usize, usize)>,
}

#[derive(Debug, Clone)]
struct Workspace {
    members: Vec<Member>,
    root_imports: Vec<(usize, usize)>,
}

fn pick() -> impl Strategy<Value = (usize, usize)> {
    (0usize..4, 0usize..3)
}

fn workspace_strategy() -> impl Strategy<Value = Workspace> {
    let member = (
        prop::bool::weighted(0.8),
        prop::option::of(any::<bool>()),
        1usize..4,
        prop::collection::vec(pick(), 0..4),
    )
        .prop_map(|(named, script, functions, imports)| Member {
            named,
            script,
            functions,
            imports,
        });
    (
        prop::collection::vec(member, 1..5),
        prop::collection::vec(pick(), 0..4),
    )
        .prop_map(|(members, root_imports)| Workspace {
            members,
            root_imports,
        })
}

impl Workspace {
    /// The `f{member}_{function}` a raw pick names.
    fn target(&self, (member, function): (usize, usize)) -> (usize, usize) {
        let member = member % self.members.len();
        (member, function % self.members[member].functions)
    }

    fn render(&self, root: &Path, members: &[usize]) {
        let listed = members
            .iter()
            .map(|index| format!("\"packages/m{index}\""))
            .collect::<Vec<_>>()
            .join(", ");
        write(
            root,
            "pyproject.toml",
            &format!(
                "[project]\nname = \"root\"\nversion = \"0\"\ndependencies = []\n\n\
                 [tool.uv.workspace]\nmembers = [{listed}]\n"
            ),
        );
        let mut main = String::new();
        let mut calls = String::new();
        for &raw in &self.root_imports {
            let (member, function) = self.target(raw);
            writeln!(main, "from m{member}.api import f{member}_{function}").unwrap();
            writeln!(calls, "    f{member}_{function}()").unwrap();
        }
        write(
            root,
            "main.py",
            &format!("{main}\n\ndef run():\n{calls}    pass\n"),
        );

        for (index, member) in self.members.iter().enumerate() {
            let base = format!("packages/m{index}");
            let mut manifest = String::from("[project]\n");
            if member.named {
                writeln!(manifest, "name = \"m{index}\"").unwrap();
            }
            manifest.push_str("version = \"0\"\ndependencies = []\n");
            match member.script {
                Some(true) => {
                    writeln!(
                        manifest,
                        "\n[project.scripts]\nm{index} = \"m{index}.api:main\""
                    )
                    .unwrap();
                },
                Some(false) => {
                    manifest.push_str("\n[project.gui-scripts]\nwin = \"zz_outside.gui:main\"\n");
                },
                None => {},
            }
            write(root, &format!("{base}/pyproject.toml"), &manifest);
            write(root, &format!("{base}/src/m{index}/__init__.py"), "");

            let mut api = String::new();
            let mut uses = String::new();
            for &raw in &member.imports {
                let (other, function) = self.target(raw);
                if other == index {
                    continue;
                }
                writeln!(api, "from m{other}.api import f{other}_{function}").unwrap();
                writeln!(uses, "    f{other}_{function}()").unwrap();
            }
            writeln!(api, "\n\ndef main():\n{uses}    pass").unwrap();
            for function in 0..member.functions {
                writeln!(api, "\n\ndef f{index}_{function}():\n    pass").unwrap();
            }
            write(root, &format!("{base}/src/m{index}/api.py"), &api);
        }
    }

    /// CHK001 and CHK006 findings in member `index`, analyzed with
    /// `members` listed.
    fn unused(&self, index: usize, members: &[usize]) -> BTreeMap<String, String> {
        let dir = TempDir::new().expect("tempdir");
        self.render(dir.path(), members);
        let within = format!("packages/m{index}/");
        issues(dir.path(), &["--confidence", "maybe"])
            .into_iter()
            .filter(|(fingerprint, _)| {
                matches!(fingerprint.split(':').next(), Some("CHK001" | "CHK006"))
                    && path_of(fingerprint).starts_with(&within)
            })
            .collect()
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(chokkin::internals::proptest_cases(24)))]

    /// Analyzing a member alone never raises its CHK006 severity, and keeps
    /// every CHK006 the whole workspace reported in it. A file the other
    /// members alone reached turns CHK001 instead, which subsumes its
    /// symbols.
    #[test]
    fn single_member_unused_export_severity_is_bounded(
        workspace in workspace_strategy(),
        chosen in any::<prop::sample::Index>(),
    ) {
        let index = chosen.index(workspace.members.len());
        let all: Vec<usize> = (0..workspace.members.len()).collect();
        let whole = workspace.unused(index, &all);
        let alone = workspace.unused(index, &[index]);
        for (fingerprint, severity) in &whole {
            let single = alone.get(fingerprint);
            let unreachable = alone.contains_key(&format!("CHK001:{}", path_of(fingerprint)));
            prop_assert!(
                single.is_some_and(|single| severity_rank(single) <= severity_rank(severity))
                    || (fingerprint.starts_with("CHK006:") && unreachable),
                "{fingerprint}: {severity} in the workspace, {single:?} alone"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// One project: default vs. `--strict` and `--production`.

const DISTRIBUTIONS: [&str; 4] = ["requests", "click", "attrs", "pytest"];
/// Imported but never declared, or resolving to nothing at all.
const UNDECLARED: [&str; 2] = ["toml", "zz_missing"];
const MODULES: usize = 4;

#[derive(Debug, Clone)]
struct Module {
    /// Imported from `__init__.py`, so reachable at runtime.
    exported: bool,
    externals: Vec<usize>,
    /// `(module, function)` raw picks of first-party calls.
    calls: Vec<(usize, usize)>,
}

#[derive(Debug, Clone)]
struct Project {
    library: bool,
    /// Per distribution: 0 undeclared, 1 runtime, 2 test group.
    declared: Vec<u8>,
    modules: Vec<Module>,
    test_externals: Vec<usize>,
    test_calls: Vec<(usize, usize)>,
}

fn external() -> impl Strategy<Value = usize> {
    0..DISTRIBUTIONS.len() + UNDECLARED.len()
}

fn call() -> impl Strategy<Value = (usize, usize)> {
    (0..MODULES, 0usize..2)
}

fn project_strategy() -> impl Strategy<Value = Project> {
    let module = (
        any::<bool>(),
        prop::collection::vec(external(), 0..3),
        prop::collection::vec(call(), 0..3),
    )
        .prop_map(|(exported, externals, calls)| Module {
            exported,
            externals,
            calls,
        });
    (
        any::<bool>(),
        prop::collection::vec(0u8..3, DISTRIBUTIONS.len()),
        prop::collection::vec(module, 1..=MODULES),
        prop::collection::vec(external(), 0..3),
        prop::collection::vec(call(), 0..4),
    )
        .prop_map(
            |(library, declared, modules, test_externals, test_calls)| Project {
                library,
                declared,
                modules,
                test_externals,
                test_calls,
            },
        )
}

fn external_name(index: usize) -> &'static str {
    DISTRIBUTIONS
        .get(index)
        .copied()
        .unwrap_or_else(|| UNDECLARED[index - DISTRIBUTIONS.len()])
}

impl Project {
    fn declared_in(&self, bucket: u8) -> String {
        DISTRIBUTIONS
            .iter()
            .zip(&self.declared)
            .filter(|(_, at)| **at == bucket)
            .map(|(name, _)| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `import`s and a body calling what they name.
    fn imports(
        &self,
        externals: &[usize],
        calls: &[(usize, usize)],
        skip: Option<usize>,
    ) -> (String, String) {
        let mut head = String::new();
        let mut body = String::new();
        for &index in externals {
            let name = external_name(index);
            writeln!(head, "import {name}").unwrap();
            writeln!(body, "    {name}.x").unwrap();
        }
        for &(module, function) in calls {
            let module = module % self.modules.len();
            if Some(module) == skip {
                continue;
            }
            writeln!(head, "from acme.m{module} import f{module}_{function}").unwrap();
            writeln!(body, "    f{module}_{function}()").unwrap();
        }
        (head, body)
    }

    fn render(&self, root: &Path) {
        let mut manifest = String::from("[project]\n");
        if self.library {
            manifest.push_str("name = \"acme\"\n");
        }
        writeln!(
            manifest,
            "version = \"0\"\ndependencies = [{}]\n\n[dependency-groups]\ntest = [{}]",
            self.declared_in(1),
            self.declared_in(2)
        )
        .unwrap();
        write(root, "pyproject.toml", &manifest);

        let mut init = String::new();
        for (index, module) in self.modules.iter().enumerate() {
            if module.exported {
                writeln!(init, "from acme import m{index} as m{index}").unwrap();
            }
            let (head, body) = self.imports(&module.externals, &module.calls, Some(index));
            let mut source = format!("{head}\n\ndef run_{index}():\n{body}    pass\n");
            for function in 0..2 {
                writeln!(source, "\n\ndef f{index}_{function}():\n    pass").unwrap();
            }
            write(root, &format!("src/acme/m{index}.py"), &source);
        }
        write(root, "src/acme/__init__.py", &init);
        // An app needs an entry to reach its package.
        if !self.library {
            write(root, "main.py", "import acme\n\nacme.x\n");
        }
        let (head, body) = self.imports(&self.test_externals, &self.test_calls, None);
        write(
            root,
            "tests/test_acme.py",
            &format!("{head}\n\ndef test_acme():\n{body}    pass\n"),
        );
    }
}

/// `fingerprint -> severity` with `code`.
fn with_code(issues: &BTreeMap<String, String>, code: &str) -> BTreeSet<String> {
    issues
        .keys()
        .filter(|fingerprint| fingerprint.split(':').next() == Some(code))
        .cloned()
        .collect()
}

fn path_of(fingerprint: &str) -> &str {
    fingerprint.split(':').nth(1).expect("path in fingerprint")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(chokkin::internals::proptest_cases(24)))]

    /// `--strict` keeps every finding of the default run.
    #[test]
    fn strict_only_widens_the_default(project in project_strategy()) {
        let dir = TempDir::new().expect("tempdir");
        project.render(dir.path());
        let default = issues(dir.path(), &[]);
        let strict = issues(dir.path(), &["--strict"]);
        for fingerprint in default.keys() {
            prop_assert!(strict.contains_key(fingerprint), "{fingerprint} lost under --strict");
        }
    }

    /// `--production` only drops import findings and only adds reachability
    /// findings (the module doc says why CHK002/CHK005/CHK009 are left out).
    #[test]
    fn production_moves_findings_one_way_per_rule(project in project_strategy()) {
        let dir = TempDir::new().expect("tempdir");
        project.render(dir.path());
        let default = issues(dir.path(), &["--confidence", "maybe"]);
        let production = issues(dir.path(), &["--confidence", "maybe", "--production"]);

        for code in ["CHK003", "CHK004", "CHK010"] {
            let extra: Vec<_> = with_code(&production, code)
                .difference(&with_code(&default, code))
                .cloned()
                .collect();
            prop_assert!(extra.is_empty(), "--production added {extra:?}");
        }

        let unreachable = with_code(&production, "CHK001");
        let runtime = |fingerprint: &&String| path_of(fingerprint).starts_with("src/");
        for fingerprint in with_code(&default, "CHK001").iter().filter(runtime) {
            prop_assert!(unreachable.contains(fingerprint), "{fingerprint} reachable under --production");
        }
        // `--production` counts an unreachable public module's calls as a
        // library's outside callers (#588), even when the default run found
        // that module orphaned too, so such a call may clear a CHK006 the
        // default run reported (#950).
        let orphans: Vec<String> = with_code(&default, "CHK001")
            .iter()
            .filter(runtime)
            .map(|fingerprint| fs::read_to_string(dir.path().join(path_of(fingerprint))).expect("read"))
            .collect();
        let unused = with_code(&production, "CHK006");
        for fingerprint in with_code(&default, "CHK006").iter().filter(runtime) {
            let file = format!("CHK001:{}", path_of(fingerprint));
            let symbol = fingerprint.rsplit(':').next().expect("symbol");
            let orphan_call = format!("{symbol}()");
            prop_assert!(
                unused.contains(fingerprint)
                    || unreachable.contains(&file)
                    || (project.library && orphans.iter().any(|source| source.contains(&orphan_call))),
                "{fingerprint} used under --production"
            );
        }
    }
}
