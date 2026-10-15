//! Property-based tests for `--fix` across every manifest table it edits
//! (#861 §8): each table loses exactly its unused entries and nothing else,
//! a shared name leaves one table per run (ADR 0003), and a fresh analysis
//! after the fix finds exactly the applied issues gone.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command;

use proptest::prelude::*;
use tempfile::TempDir;

/// A dependency table `--fix` removes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Table {
    Project,
    OptionalExtra,
    DependencyGroup,
    UvDev,
    Poetry,
    PoetryGroup,
    Requirements,
    SetupCfgInstall,
    SetupCfgExtra,
}

/// Tables that live in the same project layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Pep621,
    Poetry,
    Requirements,
    SetupCfg,
}

impl Table {
    const ALL: [Self; 9] = [
        Self::Project,
        Self::OptionalExtra,
        Self::DependencyGroup,
        Self::UvDev,
        Self::Poetry,
        Self::PoetryGroup,
        Self::Requirements,
        Self::SetupCfgInstall,
        Self::SetupCfgExtra,
    ];

    fn family(self) -> Family {
        match self {
            Self::Project | Self::OptionalExtra | Self::DependencyGroup | Self::UvDev => {
                Family::Pep621
            },
            Self::Poetry | Self::PoetryGroup => Family::Poetry,
            Self::Requirements => Family::Requirements,
            Self::SetupCfgInstall | Self::SetupCfgExtra => Family::SetupCfg,
        }
    }

    /// Runtime code importing a dev table's entry is CHK005, not a use.
    fn is_dev(self) -> bool {
        matches!(
            self,
            Self::DependencyGroup | Self::UvDev | Self::PoetryGroup
        )
    }
}

impl Family {
    fn tables(self) -> Vec<Table> {
        Table::ALL
            .into_iter()
            .filter(|table| table.family() == self)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    name: String,
    /// `>=1` when set.
    pinned: bool,
    used: bool,
    /// setup.cfg: written on the option's own line (`key = name`).
    inline: bool,
}

/// The entries of each table of one family, in table order.
#[derive(Debug, Clone)]
struct Manifest {
    family: Family,
    tables: Vec<(Table, Vec<Entry>)>,
    crlf: bool,
    /// Every name declared before any fix: the unrelated sections mention
    /// them all, so a fix must not change them.
    mentions: Vec<String>,
}

impl Manifest {
    fn new(family: Family, tables: Vec<(Table, Vec<Entry>)>, crlf: bool) -> Self {
        let mentions = tables
            .iter()
            .flat_map(|(_, entries)| entries.iter().map(|entry| entry.name.clone()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Self {
            family,
            tables,
            crlf,
            mentions,
        }
    }

    fn entries(&self, table: Table) -> &[Entry] {
        self.tables
            .iter()
            .find(|(at, _)| *at == table)
            .map_or(&[], |(_, entries)| entries.as_slice())
    }

    /// The same manifest with `keep` applied to each table's entries.
    fn filtered(&self, keep: impl Fn(Table, &Entry) -> bool) -> Self {
        Self {
            tables: self
                .tables
                .iter()
                .map(|(table, entries)| {
                    let kept = entries
                        .iter()
                        .filter(|entry| keep(*table, entry))
                        .cloned()
                        .collect();
                    (*table, kept)
                })
                .collect(),
            ..self.clone()
        }
    }

    /// Every manifest file with its contents.
    fn render(&self) -> Vec<(&'static str, String)> {
        // Unrelated bytes that name every dependency: `--fix` must leave them.
        let names: Vec<&str> = self.mentions.iter().map(String::as_str).collect();
        let files = match self.family {
            Family::Pep621 => vec![("pyproject.toml", self.render_pep621(&names))],
            Family::Poetry => vec![("pyproject.toml", self.render_poetry(&names))],
            Family::Requirements => vec![
                (
                    "pyproject.toml",
                    "[project]\nname = \"pkg\"\nversion = \"0.1\"\n".to_owned(),
                ),
                ("requirements.txt", self.render_requirements(&names)),
            ],
            Family::SetupCfg => vec![("setup.cfg", self.render_setup_cfg(&names))],
        };
        files
            .into_iter()
            .map(|(file, text)| {
                let text = if self.crlf {
                    text.replace('\n', "\r\n")
                } else {
                    text
                };
                (file, text)
            })
            .collect()
    }

    fn render_pep621(&self, names: &[&str]) -> String {
        let mut text = String::from(
            "# top comment\n[project]\nname = \"pkg\"  # keep me\nversion = \"0.1\"\n",
        );
        text.push_str(&toml_array("dependencies", self.entries(Table::Project)));
        for (table, header, key) in [
            (Table::OptionalExtra, "project.optional-dependencies", "x"),
            // Not `dev`: uv merges `tool.uv.dev-dependencies` into that
            // group, which makes a shared name a CHK009 duplicate.
            (Table::DependencyGroup, "dependency-groups", "test"),
            (Table::UvDev, "tool.uv", "dev-dependencies"),
        ] {
            if self.tables.iter().any(|(at, _)| *at == table) {
                writeln!(text, "\n[{header}]").expect("write to string");
                text.push_str(&toml_array(key, self.entries(table)));
            }
        }
        text.push_str(&unrelated_toml(names));
        text
    }

    fn render_poetry(&self, names: &[&str]) -> String {
        let mut text = String::from(
            "# top comment\n[tool.poetry]\nname = \"pkg\"\nversion = \"0.1\"\n\n\
             [tool.poetry.dependencies]\npython = \"^3.9\"\n",
        );
        text.push_str(&poetry_table(self.entries(Table::Poetry)));
        if self.tables.iter().any(|(at, _)| *at == Table::PoetryGroup) {
            text.push_str("\n[tool.poetry.group.dev.dependencies]\n");
            text.push_str(&poetry_table(self.entries(Table::PoetryGroup)));
        }
        text.push_str(&unrelated_toml(names));
        text
    }

    fn render_requirements(&self, names: &[&str]) -> String {
        let mut text = String::from("# pinned for the app\n");
        for entry in self.entries(Table::Requirements) {
            text.push_str(&requirement(entry));
            text.push('\n');
        }
        for name in names {
            writeln!(text, "# {name} is mentioned here").expect("write to string");
        }
        text
    }

    fn render_setup_cfg(&self, names: &[&str]) -> String {
        let mut text =
            String::from("[metadata]\nname = pkg\nversion = 0.1\n\n[options]\npackages = find:\n");
        text.push_str(&cfg_option(
            "install_requires",
            self.entries(Table::SetupCfgInstall),
        ));
        if self
            .tables
            .iter()
            .any(|(at, _)| *at == Table::SetupCfgExtra)
        {
            text.push_str("\n[options.extras_require]\n");
            text.push_str(&cfg_option("x", self.entries(Table::SetupCfgExtra)));
        }
        text.push_str("\n[flake8]\nexclude =\n");
        for name in names {
            writeln!(text, "    {name}").expect("write to string");
        }
        text
    }
}

fn requirement(entry: &Entry) -> String {
    let pin = if entry.pinned { ">=1" } else { "" };
    format!("{}{pin}", entry.name)
}

fn toml_array(key: &str, entries: &[Entry]) -> String {
    let mut text = format!("{key} = [\n");
    for entry in entries {
        writeln!(text, "    \"{}\",  # c-{}", requirement(entry), entry.name)
            .expect("write to string");
    }
    text.push_str("]\n");
    text
}

fn poetry_table(entries: &[Entry]) -> String {
    entries.iter().fold(String::new(), |mut text, entry| {
        let version = if entry.pinned { ">=1" } else { "*" };
        writeln!(text, "{} = \"{version}\"", entry.name).expect("write to string");
        text
    })
}

fn cfg_option(key: &str, entries: &[Entry]) -> String {
    let mut text = format!("{key} =");
    let rest = match entries.split_first() {
        Some((first, after)) if first.inline => {
            write!(text, " {}", requirement(first)).expect("write to string");
            after
        },
        _ => entries,
    };
    text.push('\n');
    for entry in rest {
        writeln!(text, "    {}", requirement(entry)).expect("write to string");
    }
    text
}

fn unrelated_toml(names: &[&str]) -> String {
    let mut text = String::from("\n[tool.other]\nkeys = [\n");
    for name in names {
        writeln!(text, "    \"{name}\",").expect("write to string");
    }
    text.push_str("]  # untouched\n");
    text
}

/// Writes the project: `pkg/__init__.py` imports the used entries and
/// `extra_source`, except that a used dev entry is imported from
/// `tests/test_pkg.py` unless `runtime_imports_dev` (which makes it CHK005).
fn write_project(manifest: &Manifest, extra_source: &str, runtime_imports_dev: bool) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("pkg")).expect("mkdir");
    fs::create_dir_all(root.join("tests")).expect("mkdir");
    let mut source = String::new();
    let mut test_source = String::from("import pkg\n");
    for (table, entries) in &manifest.tables {
        let target = if table.is_dev() && !runtime_imports_dev {
            &mut test_source
        } else {
            &mut source
        };
        for entry in entries.iter().filter(|entry| entry.used) {
            writeln!(target, "import {}", entry.name).expect("write to string");
        }
    }
    source.push_str(extra_source);
    fs::write(root.join("pkg/__init__.py"), source).expect("source");
    fs::write(root.join("tests/test_pkg.py"), test_source).expect("test source");
    for (file, text) in manifest.render() {
        fs::write(root.join(file), text).expect("manifest");
    }
    dir
}

fn read_files(root: &Path, manifest: &Manifest) -> Vec<(&'static str, String)> {
    manifest
        .render()
        .into_iter()
        .map(|(file, _)| (file, fs::read_to_string(root.join(file)).expect("read")))
        .collect()
}

/// Runs chokkin with `args` (`--strict --no-cache` added) in `root`; returns
/// stdout and the `(code, name)` of each applied fix.
fn chokkin(root: &Path, args: &[&str]) -> (String, Vec<(String, String)>) {
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--strict", "--no-cache"])
        .args(args)
        .current_dir(root)
        .output()
        .expect("run chokkin");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.code().is_some_and(|code| code <= 1),
        "chokkin failed: {stderr}"
    );
    let applied = stderr
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("applied "))
        .filter_map(|rest| {
            let mut words = rest.split_whitespace();
            Some((words.next()?.to_owned(), words.next()?.to_owned()))
        })
        .collect();
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        applied,
    )
}

fn fix(root: &Path) -> Vec<(String, String)> {
    chokkin(root, &["--fix"]).1
}

/// Distinct `zz…` names, shuffled so declaration order is not sorted.
fn names_strategy(len: std::ops::Range<usize>) -> impl Strategy<Value = Vec<String>> {
    prop::collection::btree_set("zz[a-z]{3,6}", len)
        .prop_flat_map(|names| Just(names.into_iter().collect::<Vec<_>>()).prop_shuffle())
}

fn entries_for(names: Vec<String>, shapes: Vec<(bool, bool)>, inline: bool) -> Vec<Entry> {
    names
        .into_iter()
        .zip(shapes)
        .enumerate()
        .map(|(index, (name, (pinned, used)))| Entry {
            name,
            pinned,
            used,
            inline: inline && index == 0,
        })
        .collect()
}

fn single_table_strategy() -> impl Strategy<Value = Manifest> {
    (
        prop::sample::select(Table::ALL.to_vec()),
        names_strategy(1..6),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_flat_map(|(table, names, inline, crlf)| {
            let len = names.len();
            (
                Just(table),
                Just(names),
                prop::collection::vec((any::<bool>(), any::<bool>()), len),
                Just(inline),
                Just(crlf),
            )
        })
        .prop_map(|(table, names, shapes, inline, crlf)| {
            Manifest::new(
                table.family(),
                vec![(table, entries_for(names, shapes, inline))],
                crlf,
            )
        })
}

/// Two tables of one family that both declare the unused `shared`.
fn shared_name_strategy() -> impl Strategy<Value = (Manifest, String)> {
    let pairs: Vec<(Table, Table)> = [Family::Pep621, Family::Poetry, Family::SetupCfg]
        .into_iter()
        .flat_map(|family| {
            let tables = family.tables();
            let mut pairs = Vec::new();
            for (index, first) in tables.iter().enumerate() {
                for second in &tables[index + 1..] {
                    pairs.push((*first, *second));
                }
            }
            pairs
        })
        .collect();
    (
        prop::sample::select(pairs),
        names_strategy(3..8),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_flat_map(|((first, second), names, inline, crlf)| {
            let len = names.len();
            (
                Just((first, second)),
                Just(names),
                prop::collection::vec((any::<bool>(), any::<bool>()), len),
                1..len - 1,
                any::<prop::sample::Index>(),
                any::<prop::sample::Index>(),
                Just((inline, crlf)),
            )
        })
        .prop_map(
            |((first, second), names, shapes, split, at_first, at_second, (inline, crlf))| {
                let shared = names[0].clone();
                let mut entries = entries_for(names, shapes, false);
                let mut shared_entry = entries.remove(0);
                shared_entry.used = false;
                let mut rest = entries.split_off(split - 1);
                let place = |entries: &mut Vec<Entry>, at: &prop::sample::Index| {
                    entries.insert(at.index(entries.len() + 1), shared_entry.clone());
                    if let Some(entry) = entries.first_mut() {
                        entry.inline = inline;
                    }
                };
                place(&mut entries, &at_first);
                place(&mut rest, &at_second);
                let manifest =
                    Manifest::new(first.family(), vec![(first, entries), (second, rest)], crlf);
                (manifest, shared)
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// In every table `--fix` edits, it removes exactly the unused entries,
    /// leaves every other byte (comments, line endings, unrelated sections
    /// naming the same distributions) as written, and a second run changes
    /// nothing.
    #[test]
    fn fix_removes_only_unused_entries_in_every_table(manifest in single_table_strategy()) {
        let dir = write_project(&manifest, "", false);
        let root = dir.path();
        let applied = fix(root);
        let once = read_files(root, &manifest);

        let unused: BTreeSet<(String, String)> = manifest
            .tables
            .iter()
            .flat_map(|(_, entries)| entries.iter().filter(|entry| !entry.used))
            .map(|entry| ("CHK002".to_owned(), entry.name.clone()))
            .collect();
        prop_assert_eq!(applied.iter().cloned().collect::<BTreeSet<_>>(), unused);
        prop_assert_eq!(&once, &manifest.filtered(|_, entry| entry.used).render());

        prop_assert_eq!(fix(root), Vec::new());
        prop_assert_eq!(read_files(root, &manifest), once);
    }

    /// A name two tables declare leaves only the reported table per run;
    /// the other table keeps it until a fresh analysis reports it there
    /// (ADR 0003), so the fixed point takes one run per declaration.
    #[test]
    fn fix_removes_a_shared_name_from_one_table_per_run(
        (manifest, shared) in shared_name_strategy(),
    ) {
        let dir = write_project(&manifest, "", false);
        let root = dir.path();
        let tables: Vec<Table> = manifest.tables.iter().map(|(table, _)| *table).collect();

        let applied = fix(root);
        prop_assert_eq!(
            applied.iter().filter(|(_, name)| *name == shared).count(),
            1
        );
        let once = read_files(root, &manifest);
        let from_one_table = tables.iter().any(|from| {
            manifest
                .filtered(|table, entry| entry.used || (entry.name == shared && table != *from))
                .render()
                == once
        });
        prop_assert!(from_one_table, "{once:#?}");

        prop_assert_eq!(fix(root), vec![("CHK002".to_owned(), shared)]);
        let twice = read_files(root, &manifest);
        prop_assert_eq!(&twice, &manifest.filtered(|_, entry| entry.used).render());

        prop_assert_eq!(fix(root), Vec::new());
        prop_assert_eq!(read_files(root, &manifest), twice);
    }
}

/// Imports whose distribution chokkin knows by name, for CHK003.
const MISSING: [(&str, &str); 5] = [
    ("yaml", "pyyaml"),
    ("dateutil", "python-dateutil"),
    ("nacl", "pynacl"),
    ("zmq", "pyzmq"),
    ("attr", "attrs"),
];

#[derive(Debug, Clone)]
struct Mixed {
    manifest: Manifest,
    /// Indices into [`MISSING`] imported without a declaration.
    missing: Vec<usize>,
    /// An unresolvable import (CHK010), which keeps CHK002 at likely.
    unresolved: bool,
}

/// PEP 621 tables with unused entries, dev entries the runtime imports
/// (CHK005), and undeclared imports (CHK003); each name in one table.
fn mixed_strategy() -> impl Strategy<Value = Mixed> {
    (
        names_strategy(1..8),
        prop::sample::subsequence((0..MISSING.len()).collect::<Vec<_>>(), 0..3),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_flat_map(|(names, missing, unresolved, crlf)| {
            let len = names.len();
            let tables = Family::Pep621.tables();
            (
                Just(names),
                prop::collection::vec(
                    (prop::sample::select(tables), any::<bool>(), any::<bool>()),
                    len,
                ),
                Just(missing),
                Just(unresolved),
                Just(crlf),
            )
        })
        .prop_map(|(names, placed, missing, unresolved, crlf)| {
            let mut tables: Vec<(Table, Vec<Entry>)> = Family::Pep621
                .tables()
                .into_iter()
                .map(|table| (table, Vec::new()))
                .collect();
            for (name, (table, pinned, used)) in names.into_iter().zip(placed) {
                let entries = &mut tables
                    .iter_mut()
                    .find(|(at, _)| *at == table)
                    .expect("pep621 table")
                    .1;
                entries.push(Entry {
                    name,
                    pinned,
                    used,
                    inline: false,
                });
            }
            Mixed {
                manifest: Manifest::new(Family::Pep621, tables, crlf),
                missing,
                unresolved,
            }
        })
}

/// `(code, distribution, fingerprint, confidence)` of each issue.
fn issues(json: &str) -> Vec<(String, Option<String>, String, String)> {
    let parsed: serde_json::Value = serde_json::from_str(json).expect("json reporter output");
    parsed["issues"]
        .as_array()
        .expect("issues array")
        .iter()
        .map(|issue| {
            (
                issue["code"].as_str().expect("code").to_owned(),
                issue["distribution"].as_str().map(str::to_owned),
                issue["fingerprint"]
                    .as_str()
                    .expect("fingerprint")
                    .to_owned(),
                issue["confidence"].as_str().expect("confidence").to_owned(),
            )
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// `--fix --add-missing` applies every certain CHK002, CHK003 and
    /// CHK005 and nothing else, and a fresh analysis afterwards finds
    /// exactly those issues gone and every other issue unchanged.
    #[test]
    fn fix_resolves_exactly_the_applied_issues(mixed in mixed_strategy()) {
        let mut extra = String::new();
        for &index in &mixed.missing {
            writeln!(extra, "import {}", MISSING[index].0).expect("write to string");
        }
        if mixed.unresolved {
            extra.push_str("import zz_unresolved\n");
        }
        let dir = write_project(&mixed.manifest, &extra, true);
        let root = dir.path();

        let (before, applied) =
            chokkin(root, &["--fix", "--add-missing", "--reporter", "json"]);
        let before = issues(&before);
        let applied: BTreeSet<(String, String)> = applied.into_iter().collect();
        let fixable: BTreeSet<(String, String)> = before
            .iter()
            .filter(|(code, _, _, confidence)| {
                ["CHK002", "CHK003", "CHK005"].contains(&code.as_str()) && confidence == "certain"
            })
            .map(|(code, distribution, _, _)| {
                (code.clone(), distribution.clone().expect("distribution"))
            })
            .collect();
        prop_assert_eq!(&applied, &fixable);
        if mixed.unresolved {
            prop_assert!(applied.iter().all(|(code, _)| code != "CHK002"), "{applied:?}");
        }

        // Moving and adding entries leaves every other entry's line, and
        // each `# c-name` stays on the line of the entry it was written for.
        let (_, original) = &mixed.manifest.render()[0];
        let (_, fixed) = &read_files(root, &mixed.manifest)[0];
        let fixed_lines: BTreeSet<&str> = fixed.lines().collect();
        let touched = |name: &str| {
            ["CHK002", "CHK005"]
                .iter()
                .any(|code| applied.contains(&((*code).to_owned(), name.to_owned())))
        };
        for (_, entries) in &mixed.manifest.tables {
            for entry in entries.iter().filter(|entry| !touched(&entry.name)) {
                let line = original
                    .lines()
                    .find(|line| line.ends_with(&format!("# c-{}", entry.name)) || line.ends_with(&format!("# c-{}\r", entry.name)))
                    .expect("entry line");
                prop_assert!(fixed_lines.contains(line), "{line:?} lost:\n{fixed}");
            }
        }
        for line in fixed.lines() {
            if let Some((_, name)) = line.trim_end_matches('\r').rsplit_once("# c-") {
                prop_assert!(line.contains(&format!("\"{name}")), "{line:?} in:\n{fixed}");
            }
        }

        let after: BTreeSet<String> = issues(&chokkin(root, &["--reporter", "json"]).0)
            .into_iter()
            .map(|(_, _, fingerprint, _)| fingerprint)
            .collect();
        let expected: BTreeSet<String> = before
            .into_iter()
            .filter(|(code, distribution, _, _)| {
                !distribution
                    .as_ref()
                    .is_some_and(|name| applied.contains(&(code.clone(), name.clone())))
            })
            .map(|(_, _, fingerprint, _)| fingerprint)
            .collect();
        prop_assert_eq!(after, expected);
    }
}
