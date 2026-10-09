//! Property-based tests for dependency reconciliation (step 10) end to end.
//!
//! The reference model mirrors `docs/dev/formal/deps_rules_z3.py` (S1/S2,
//! W1–W3) extended with the post-v0.6.0 decisions in
//! `src/rules/deps/missing.rs`: a lock edge from a declared distribution makes
//! even an optional import CHK004 (#504, as a warning per #582), and a locked-only distribution is a
//! Likely CHK004.

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

/// No name is an affixed form of another (`pyfoo`, `foo-py`), so the loose
/// resolver step never links two of them.
const POOL: [&str; 5] = ["zz-alpha", "zz-bravo", "zz-charlie", "zz-delta", "zz-echo"];

fn module(name: &str) -> String {
    name.replace('-', "_")
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Dep {
    /// `Some(marker)` when declared in `project.dependencies`.
    runtime: Option<bool>,
    dev: bool,
    extra: bool,
    /// `import name` in the entry module.
    plain: bool,
    /// `try: import name` in the entry module.
    optional: bool,
    /// `import name` in `tests/test_x.py`.
    test: bool,
    /// A `[[package]]` in `uv.lock`.
    locked: bool,
    /// `zzhub` (declared, imported) depends on it in `uv.lock`.
    hub_edge: bool,
}

#[derive(Debug, Clone)]
struct Spec {
    deps: Vec<Dep>,
    has_lock: bool,
    /// Declaration and import order: a permutation of `0..deps.len()`.
    order: Vec<usize>,
}

impl Spec {
    fn named(&self) -> impl Iterator<Item = (&'static str, &Dep)> + '_ {
        self.order.iter().map(|&i| (POOL[i], &self.deps[i]))
    }
}

fn dep_strategy() -> impl Strategy<Value = Dep> {
    (
        prop::option::of(any::<bool>()),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(runtime, dev, extra, plain, optional, test, locked, hub_edge)| Dep {
                runtime,
                dev,
                extra,
                plain,
                optional,
                test,
                locked,
                hub_edge,
            },
        )
}

fn spec_strategy() -> impl Strategy<Value = Spec> {
    (
        prop::collection::vec(dep_strategy(), 1..=POOL.len()),
        any::<bool>(),
    )
        .prop_flat_map(|(deps, has_lock)| {
            let order: Vec<usize> = (0..deps.len()).collect();
            Just(order).prop_shuffle().prop_map(move |order| Spec {
                deps: deps.clone(),
                has_lock,
                order,
            })
        })
}

fn quoted(entries: &[String]) -> String {
    entries
        .iter()
        .map(|entry| format!("\"{entry}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

fn write(root: &Path, file: &str, text: &str) {
    let path = root.join(file);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, text).expect("write");
}

/// `spell` maps a canonical name to the spelling written in the manifest.
fn project(spec: &Spec, spell: &dyn Fn(&str, usize) -> String) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    let mut runtime = vec!["zzhub".to_owned()];
    let mut dev = Vec::new();
    let mut extra = Vec::new();
    for (name, dep) in spec.named() {
        match dep.runtime {
            Some(false) => runtime.push(spell(name, 0)),
            Some(true) => runtime.push(format!("{}; sys_platform == 'win32'", spell(name, 0))),
            None => {},
        }
        if dep.dev {
            dev.push(spell(name, 1));
        }
        if dep.extra {
            extra.push(spell(name, 2));
        }
    }
    write(
        root,
        "pyproject.toml",
        &format!(
            "[project]\nname = \"pkg\"\nversion = \"0.1.0\"\ndependencies = [{}]\n\n[project.optional-dependencies]\nex = [{}]\n\n[dependency-groups]\ndev = [{}]\n\n[project.scripts]\npkg = \"pkg.main:main\"\n",
            quoted(&runtime),
            quoted(&extra),
            quoted(&dev),
        ),
    );

    let mut main = "import zzhub\n".to_owned();
    let mut tests = "import pkg.main\n".to_owned();
    for (name, dep) in spec.named() {
        if dep.plain {
            writeln!(main, "import {}", module(name)).expect("fmt");
        }
        if dep.optional {
            writeln!(
                main,
                "try:\n    import {}\nexcept ImportError:\n    pass",
                module(name)
            )
            .expect("fmt");
        }
        if dep.test {
            writeln!(tests, "import {}", module(name)).expect("fmt");
        }
    }
    main.push_str("\n\ndef main():\n    pass\n");
    write(root, "src/pkg/__init__.py", "");
    write(root, "src/pkg/main.py", &main);
    write(root, "tests/test_x.py", &tests);

    if spec.has_lock {
        let edges: Vec<String> = spec
            .named()
            .filter(|(_, dep)| dep.hub_edge)
            .map(|(name, _)| format!("{{ name = \"{name}\" }}"))
            .collect();
        let mut lock = format!(
            "version = 1\n\n[[package]]\nname = \"pkg\"\nversion = \"0.1.0\"\nsource = {{ editable = \".\" }}\ndependencies = [{{ name = \"zzhub\" }}]\n\n[[package]]\nname = \"zzhub\"\nversion = \"1.0\"\ndependencies = [{}]\n",
            edges.join(", ")
        );
        for (name, dep) in spec.named() {
            if dep.locked || dep.hub_edge {
                write!(
                    lock,
                    "\n[[package]]\nname = \"{name}\"\nversion = \"1.0\"\n"
                )
                .expect("fmt");
            }
        }
        write(root, "uv.lock", &lock);
    }
    dir
}

type Key = (String, String, String, String);

/// `(code, target, severity, confidence)` for issues about pool names.
fn issues(root: &Path, extra: &[&str]) -> Vec<Key> {
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--reporter", "json", "--no-cache"])
        .args(extra)
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
    let mut keys: Vec<Key> = parsed["issues"]
        .as_array()
        .expect("issues array")
        .iter()
        .filter(|issue| {
            issue["distribution"]
                .as_str()
                .is_some_and(|dist| POOL.contains(&dist))
        })
        .map(|issue| {
            let field = |name: &str| issue[name].as_str().unwrap_or_default().to_owned();
            (
                field("code"),
                field("target"),
                field("severity"),
                field("confidence"),
            )
        })
        .collect();
    keys.sort();
    keys
}

fn key(code: &str, target: &str, severity: &str, confidence: &str) -> Key {
    (
        code.to_owned(),
        target.to_owned(),
        severity.to_owned(),
        confidence.to_owned(),
    )
}

/// Reference decision for one distribution.
fn expected_for(name: &str, dep: Dep, has_lock: bool, strict: bool) -> Vec<Key> {
    let mut out = Vec::new();
    let declared = dep.runtime.is_some() || dep.dev || dep.extra;
    let locked = has_lock && (dep.locked || dep.hub_edge);
    let used = (declared || locked) && (dep.plain || dep.optional || dep.test);

    // S1 / W3: an unconditional runtime declaration nothing uses. Markers,
    // extras, and dev groups are suppressed outside `--strict`. One report
    // per name, taken from the runtime declaration first (#502), so its
    // marker decides the confidence even when an unmarked extra exists.
    if !used && (dep.runtime == Some(false) || (strict && declared)) {
        let confidence = if dep.runtime == Some(true) {
            "likely"
        } else {
            "certain"
        };
        out.push(key("CHK002", name, "error", confidence));
    }
    // A dev group or extra repeats only a runtime declaration with the same
    // (here: no) marker; groups and extras never duplicate each other (#555,
    // #629). Repeating the runtime declaration is only info (#696).
    if dep.runtime == Some(false) && (dep.dev || dep.extra) {
        out.push(key("CHK009", name, "info", "likely"));
    }

    let site = format!("src/pkg/main.py:{}", module(name));
    if (dep.plain || dep.optional) && dep.runtime.is_none() && !dep.extra {
        if dep.dev {
            // W1 counterpart: a dev-only declaration used at runtime is
            // misplaced, never missing. Only a top-level import makes it
            // certain (#583).
            out.push(if dep.plain {
                key("CHK005", name, "warning", "certain")
            } else {
                key("CHK005", name, "info", "likely")
            });
        } else if locked {
            if dep.plain {
                let confidence = if dep.hub_edge { "certain" } else { "likely" };
                out.push(key("CHK004", &site, "error", confidence));
            }
            if dep.optional {
                out.push(if dep.hub_edge {
                    key("CHK004", &site, "warning", "certain")
                } else if strict {
                    key("CHK003", &site, "warning", "likely")
                } else {
                    key("CHK003", &site, "info", "likely")
                });
            }
        }
    }
    // Strict mode also checks test-site imports of undeclared packages, one
    // issue per import site.
    if strict && dep.test && !declared && locked {
        let confidence = if dep.hub_edge { "certain" } else { "likely" };
        let site = format!("tests/test_x.py:{}", module(name));
        out.push(key("CHK004", &site, "error", confidence));
    }
    out
}

fn expected(spec: &Spec, strict: bool) -> Vec<Key> {
    let mut out: Vec<Key> = spec
        .named()
        .flat_map(|(name, dep)| expected_for(name, *dep, spec.has_lock, strict))
        .collect();
    out.sort();
    out
}

fn canonical(name: &str, _slot: usize) -> String {
    name.to_owned()
}

fn rule_count(keys: &[Key], code: &str, name: &str) -> usize {
    keys.iter()
        .filter(|(c, target, ..)| {
            c == code && (target == name || target.ends_with(&format!(":{}", module(name))))
        })
        .count()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(40))]

    /// CHK002/CHK003/CHK004/CHK005/CHK009 agree with the reference decision
    /// table for every generated distribution.
    #[test]
    fn dependency_issues_match_reference_model(spec in spec_strategy(), strict in any::<bool>()) {
        let dir = project(&spec, &canonical);
        let extra: &[&str] = if strict { &["--strict"] } else { &[] };
        prop_assert_eq!(issues(dir.path(), extra), expected(&spec, strict), "{:#?}", spec);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Declaration and import order never changes which issues are reported.
    #[test]
    fn dependency_issues_ignore_declaration_order(
        spec in spec_strategy(),
        shuffled in Just((0..POOL.len()).collect::<Vec<usize>>()).prop_shuffle(),
    ) {
        let reordered = Spec {
            order: shuffled.into_iter().filter(|i| *i < spec.deps.len()).collect(),
            ..spec.clone()
        };
        let first = project(&spec, &canonical);
        let second = project(&reordered, &canonical);
        prop_assert_eq!(issues(first.path(), &[]), issues(second.path(), &[]));
    }

    /// PEP 503 spellings (`ZZ_Alpha`, `zz.alpha`, `Zz--ALPHA`) of a declared
    /// name report exactly like the canonical spelling, strict or not.
    #[test]
    fn dependency_issues_ignore_name_spelling(
        spec in spec_strategy(),
        variants in prop::collection::vec(0usize..4, 3),
        strict in any::<bool>(),
    ) {
        let spell = |name: &str, slot: usize| {
            let tail = name.trim_start_matches("zz-");
            match variants[slot] {
                0 => format!("ZZ_{tail}"),
                1 => format!("zz.{tail}"),
                2 => format!("Zz--{}", tail.to_uppercase()),
                _ => name.to_owned(),
            }
        };
        let extra: &[&str] = if strict { &["--strict"] } else { &[] };
        let canonical_dir = project(&spec, &canonical);
        let variant_dir = project(&spec, &spell);
        prop_assert_eq!(
            issues(variant_dir.path(), extra),
            issues(canonical_dir.path(), extra)
        );
    }

    /// Adding an import never creates a CHK002 for that distribution, and
    /// adding a runtime declaration never creates a CHK003/CHK004 for it.
    #[test]
    fn dependency_issues_are_monotone(spec in spec_strategy(), pick in any::<prop::sample::Index>()) {
        let target = pick.index(spec.deps.len());
        let name = POOL[target];
        let base = issues(project(&spec, &canonical).path(), &[]);

        let mut imported = spec.clone();
        imported.deps[target].plain = true;
        let after_import = issues(project(&imported, &canonical).path(), &[]);
        prop_assert!(
            rule_count(&after_import, "CHK002", name) <= rule_count(&base, "CHK002", name),
            "{after_import:?} vs {base:?}"
        );

        let mut declared = spec;
        declared.deps[target].runtime = Some(false);
        let after_declare = issues(project(&declared, &canonical).path(), &[]);
        for code in ["CHK003", "CHK004"] {
            prop_assert_eq!(rule_count(&after_declare, code, name), 0, "{:?}", after_declare);
        }
    }
}

/// A distribution reaches `issues` only through the pool, so the helper's
/// filter cannot hide an unexpected rule firing for a pool name.
#[test]
fn pool_names_are_not_affixed_forms_of_each_other() {
    let names: BTreeSet<&str> = POOL.into_iter().collect();
    for name in POOL {
        for affix in ["py", "python-", "py-"] {
            assert!(!names.contains(format!("{affix}{name}").as_str()));
        }
        for affix in ["py", "-python", "-py"] {
            assert!(!names.contains(format!("{name}{affix}").as_str()));
        }
    }
}
