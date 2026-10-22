//! Property-based tests for dependency reconciliation (step 10) end to end.
//!
//! The reference model mirrors `docs/dev/formal/deps_rules_z3.py` (S1/S2,
//! W1–W3) extended with the post-v0.6.0 decisions in
//! `src/rules/deps/missing.rs`: a lock edge from a declared distribution makes
//! even an optional import CHK004 (#504, as a warning per #582), a locked-only distribution is a
//! Likely CHK004, and an unguarded import nothing resolves caps CHK002 at Likely (#911).

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

/// No name is an affixed form of another (`pyfoo`, `foo-py`), so the loose
/// resolver step never links two of them.
const POOL: [&str; 5] = ["zz-alpha", "zz-bravo", "zz-charlie", "zz-delta", "zz-echo"];

fn module(name: &str) -> String {
    name.replace('-', "_")
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent axis the property test varies"
)]
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
    // An import nothing resolves, unguarded, may come from any declared
    // distribution, so none is certainly unused (#911).
    let unresolved = spec.named().any(|(_, dep)| {
        let declared = dep.runtime.is_some() || dep.dev || dep.extra;
        let locked = spec.has_lock && (dep.locked || dep.hub_edge);
        !declared && !locked && (dep.plain || dep.test)
    });
    let mut out: Vec<Key> = spec
        .named()
        .flat_map(|(name, dep)| expected_for(name, *dep, spec.has_lock, strict))
        .map(|(code, target, severity, confidence)| {
            if unresolved && code == "CHK002" {
                (code, target, severity, "likely".to_owned())
            } else {
                (code, target, severity, confidence)
            }
        })
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
    #![proptest_config(ProptestConfig::with_cases(chokkin::internals::proptest_cases(40)))]

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
    #![proptest_config(ProptestConfig::with_cases(chokkin::internals::proptest_cases(24)))]

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

/// Distributions of the CHK005 strength properties, all declared only in the
/// dev group. `pytest` is the one a `pytest11` plugin module is host-loaded by.
const LATTICE: [&str; 4] = ["zz-alpha", "zz-bravo", "zz-charlie", "pytest"];

/// Where an import site sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// The entry module, `src/pkg/main.py`.
    Main,
    /// `src/pkg/<module>/__init__.py`, a subpackage named after the import,
    /// loaded from the entry module.
    OptIn,
    /// `src/pkg/testing.py`, test support loaded from the entry module.
    Testing,
    /// `src/pkg/plugin.py`, the `pytest11` entry point.
    Plugin,
    /// `src/pkg/conftest.py`, which only pytest loads.
    Conftest,
}

/// How the import statement is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Top,
    Function,
    TryExcept,
    Platform,
    TypeChecking,
}

/// Mirrors `ImportStrength` in `src/rules/deps/misplaced.rs`, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Strength {
    HostLoaded,
    OptIn,
    TestSupport,
    Optional,
    DeferredOptional,
    Deferred,
    TopLevel,
}

type Site = (Place, Form);

fn site_strategy() -> impl Strategy<Value = Site> {
    (
        prop::sample::select(vec![
            Place::Main,
            Place::OptIn,
            Place::Testing,
            Place::Plugin,
            Place::Conftest,
        ]),
        prop::sample::select(vec![
            Form::Top,
            Form::Function,
            Form::TryExcept,
            Form::Platform,
            Form::TypeChecking,
        ]),
    )
}

/// Import sites per `LATTICE` distribution, in file order.
fn lattice_strategy() -> impl Strategy<Value = Vec<Vec<Site>>> {
    prop::collection::vec(prop::collection::vec(site_strategy(), 0..5), LATTICE.len())
}

/// The strength one site gives `name`, or `None` when it is no runtime
/// import: a `TYPE_CHECKING` block or a conftest.
fn site_strength(name: &str, (place, form): Site) -> Option<Strength> {
    if form == Form::TypeChecking || place == Place::Conftest {
        return None;
    }
    if place == Place::Plugin && name == "pytest" {
        return Some(Strength::HostLoaded);
    }
    let mut strength = match form {
        Form::Top => Strength::TopLevel,
        Form::Function => Strength::Deferred,
        _ => Strength::Optional,
    };
    if place == Place::OptIn {
        strength = strength.min(Strength::OptIn);
    }
    if place == Place::Testing && strength < Strength::TopLevel {
        strength = strength.min(Strength::TestSupport);
    }
    Some(strength)
}

/// The reference lattice: the strongest site wins whatever the order, and a
/// `try`/`except ImportError` anywhere demotes a function-local winner (#808).
/// A platform guard says the package needs it there, so it does not (#808).
fn lattice_expected(name: &str, sites: &[Site]) -> Option<(String, String)> {
    let strongest = sites
        .iter()
        .filter_map(|site| site_strength(name, *site))
        .max()?;
    let optional = sites
        .iter()
        .any(|&(place, form)| form == Form::TryExcept && place != Place::Conftest);
    let strength = if strongest == Strength::Deferred && optional {
        Strength::DeferredOptional
    } else {
        strongest
    };
    let severity = match strength {
        Strength::TopLevel | Strength::Deferred => "warning",
        _ => "info",
    };
    let confidence = if strength == Strength::TopLevel {
        "certain"
    } else {
        "likely"
    };
    Some((severity.to_owned(), confidence.to_owned()))
}

fn lattice_project(sites: &[Vec<Site>]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    let dev: Vec<String> = LATTICE.iter().map(|name| (*name).to_owned()).collect();
    write(
        root,
        "pyproject.toml",
        &format!(
            "[project]\nname = \"pkg\"\nversion = \"0.1.0\"\ndependencies = []\n\n[dependency-groups]\ndev = [{}]\n\n[project.scripts]\npkg = \"pkg.main:main\"\n\n[project.entry-points.pytest11]\npkg = \"pkg.plugin\"\n",
            quoted(&dev)
        ),
    );
    let header = "import sys\nfrom typing import TYPE_CHECKING\n";
    let mut files: Vec<(String, String)> = Vec::new();
    let mut loads: BTreeSet<String> = BTreeSet::new();
    let mut functions = 0;
    for (name, sites) in LATTICE.iter().zip(sites) {
        let module = module(name);
        for &(place, form) in sites {
            let file = match place {
                Place::Main => "src/pkg/main.py".to_owned(),
                Place::OptIn => {
                    loads.insert(format!("pkg.{module}"));
                    format!("src/pkg/{module}/__init__.py")
                },
                Place::Testing => {
                    loads.insert("pkg.testing".to_owned());
                    "src/pkg/testing.py".to_owned()
                },
                Place::Plugin => "src/pkg/plugin.py".to_owned(),
                Place::Conftest => "src/pkg/conftest.py".to_owned(),
            };
            let statement = match form {
                Form::Top => format!("import {module}\n"),
                Form::Function => {
                    functions += 1;
                    format!("\n\ndef f_{functions}():\n    import {module}\n\n\n")
                },
                Form::TryExcept => {
                    format!("try:\n    import {module}\nexcept ImportError:\n    pass\n")
                },
                Form::Platform => format!("if sys.platform == \"win32\":\n    import {module}\n"),
                Form::TypeChecking => format!("if TYPE_CHECKING:\n    import {module}\n"),
            };
            match files.iter_mut().find(|(path, _)| *path == file) {
                Some((_, text)) => text.push_str(&statement),
                None => files.push((file, format!("{header}{statement}"))),
            }
        }
    }
    let mut main = header.to_owned();
    for load in &loads {
        writeln!(main, "import {load}").expect("fmt");
    }
    if let Some((_, text)) = files.iter().find(|(path, _)| path == "src/pkg/main.py") {
        main.push_str(text.trim_start_matches(header));
    }
    main.push_str("\n\ndef main():\n    pass\n");
    write(root, "src/pkg/__init__.py", "");
    write(root, "src/pkg/plugin.py", header);
    for (path, text) in &files {
        write(root, path, text);
    }
    write(root, "src/pkg/main.py", &main);
    dir
}

/// `(distribution, severity, confidence)` of each CHK005, sorted.
fn misplaced(root: &Path) -> Vec<(String, String, String)> {
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
    let mut found: Vec<(String, String, String)> = parsed["issues"]
        .as_array()
        .expect("issues array")
        .iter()
        .filter(|issue| issue["code"] == "CHK005")
        .map(|issue| {
            let field = |name: &str| issue[name].as_str().unwrap_or_default().to_owned();
            (
                field("distribution"),
                field("severity"),
                field("confidence"),
            )
        })
        .collect();
    found.sort();
    found
}

fn lattice_reference(sites: &[Vec<Site>]) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = LATTICE
        .iter()
        .zip(sites)
        .filter_map(|(name, sites)| {
            lattice_expected(name, sites)
                .map(|(severity, confidence)| ((*name).to_owned(), severity, confidence))
        })
        .collect();
    out.sort();
    out
}

fn confidence_rank(found: &[(String, String, String)], name: &str) -> u8 {
    found
        .iter()
        .find(|(dist, ..)| dist == name)
        .map_or(
            0,
            |(.., confidence)| if confidence == "certain" { 2 } else { 1 },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(chokkin::internals::proptest_cases(32)))]

    /// CHK005's severity and confidence follow the strength lattice: the
    /// strongest runtime site of a dev-only distribution decides, so one
    /// top-level import in a loaded module is certain whatever else is mixed
    /// in, and `TYPE_CHECKING` and conftest imports never count.
    #[test]
    fn misplaced_strength_is_the_strongest_site(sites in lattice_strategy()) {
        let dir = lattice_project(&sites);
        prop_assert_eq!(misplaced(dir.path()), lattice_reference(&sites), "{:#?}", sites);
    }

    /// Reordering the import sites, within and across files, changes nothing.
    #[test]
    fn misplaced_strength_ignores_site_order(
        sites in lattice_strategy(),
        turn in any::<prop::sample::Index>(),
    ) {
        let reordered: Vec<Vec<Site>> = sites
            .iter()
            .map(|sites| {
                let mut sites = sites.clone();
                let len = sites.len();
                sites.rotate_left(turn.index(len.max(1)).min(len));
                sites.reverse();
                sites
            })
            .collect();
        let first = lattice_project(&sites);
        let second = lattice_project(&reordered);
        prop_assert_eq!(misplaced(first.path()), misplaced(second.path()));
    }

    /// One more import site never lowers a CHK005's confidence.
    #[test]
    fn misplaced_confidence_is_monotone_in_sites(
        sites in lattice_strategy(),
        pick in any::<prop::sample::Index>(),
        extra in site_strategy(),
        at in any::<prop::sample::Index>(),
    ) {
        let target = pick.index(LATTICE.len());
        let mut grown = sites.clone();
        let position = at.index(grown[target].len() + 1);
        grown[target].insert(position, extra);
        let before = misplaced(lattice_project(&sites).path());
        let after = misplaced(lattice_project(&grown).path());
        prop_assert!(
            confidence_rank(&after, LATTICE[target]) >= confidence_rank(&before, LATTICE[target]),
            "{before:?} -> {after:?}"
        );
    }

    /// `--fix` moves exactly the certain CHK005 distributions to runtime and
    /// leaves every likely one in the dev group (#590, #808).
    #[test]
    fn fix_moves_only_certain_misplaced_dependencies(sites in lattice_strategy()) {
        let dir = lattice_project(&sites);
        let root = dir.path();
        let certain: BTreeSet<String> = lattice_reference(&sites)
            .into_iter()
            .filter(|(.., confidence)| confidence == "certain")
            .map(|(name, ..)| name)
            .collect();
        let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
            .args(["--fix", "--no-cache"])
            .current_dir(root)
            .output()
            .expect("run chokkin");
        prop_assert!(output.status.code().is_some_and(|code| code <= 1));
        let text = fs::read_to_string(root.join("pyproject.toml")).expect("read");
        let doc: toml::Table = text.parse().expect("pyproject stays valid TOML");
        let names = |value: &toml::Value| -> BTreeSet<String> {
            value
                .as_array()
                .expect("array")
                .iter()
                .map(|v| v.as_str().expect("string").to_owned())
                .collect()
        };
        let runtime = names(&doc["project"]["dependencies"]);
        let dev = names(&doc["dependency-groups"]["dev"]);
        prop_assert_eq!(&runtime, &certain);
        let rest: BTreeSet<String> = LATTICE
            .iter()
            .map(|name| (*name).to_owned())
            .filter(|name| !certain.contains(name))
            .collect();
        prop_assert_eq!(dev, rest);
    }
}

// ---------------------------------------------------------------------------
// CHK008: binaries a Makefile runs, against the declared distributions.

/// Binaries the bundled map resolves to a distribution of the same name,
/// with spellings that normalize to it.
const BINARIES: [(&str, &[&str]); 6] = [
    ("ruff", &["ruff", "Ruff"]),
    ("black", &["black", "BLACK"]),
    ("mypy", &["mypy", "MyPy"]),
    ("pytest", &["pytest", "PyTest"]),
    ("tox", &["tox", "TOX"]),
    ("pre-commit", &["pre-commit", "Pre_Commit", "pre.commit"]),
];

#[derive(Debug, Clone)]
struct BinarySpec {
    /// `(used in the Makefile, declared, spelling, in the dev group)` per
    /// `BINARIES` entry.
    binaries: Vec<(bool, bool, usize, bool)>,
    /// Recipe and declaration order: a permutation of `BINARIES` indices.
    order: Vec<usize>,
}

fn binary_spec_strategy() -> impl Strategy<Value = BinarySpec> {
    (
        prop::collection::vec(
            (any::<bool>(), any::<bool>(), 0usize..3, any::<bool>()),
            BINARIES.len(),
        ),
        Just((0..BINARIES.len()).collect::<Vec<usize>>()).prop_shuffle(),
    )
        .prop_map(|(binaries, order)| BinarySpec { binaries, order })
}

fn binary_project(spec: &BinarySpec) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    let mut runtime = Vec::new();
    let mut dev = Vec::new();
    let mut makefile = String::new();
    for &index in &spec.order {
        let (used, declared, spelling, in_dev) = spec.binaries[index];
        let (binary, spellings) = BINARIES[index];
        if declared {
            let name = spellings[spelling % spellings.len()].to_owned();
            if in_dev { dev.push(name) } else { runtime.push(name) }
        }
        if used {
            write!(makefile, "{index}:\n\t{binary} --version\n\n").expect("fmt");
        }
    }
    write(
        root,
        "pyproject.toml",
        &format!(
            "[project]\nname = \"pkg\"\nversion = \"0.1.0\"\ndependencies = [{}]\n\n[dependency-groups]\ndev = [{}]\n\n[tool.chokkin]\nmode = \"app\"\n",
            quoted(&runtime),
            quoted(&dev),
        ),
    );
    write(root, "Makefile", &makefile);
    write(root, "src/pkg/__init__.py", "");
    dir
}

/// The binaries CHK008 reports.
fn unlisted_binaries(root: &Path) -> BTreeSet<String> {
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
        .filter(|issue| issue["code"] == "CHK008")
        .map(|issue| issue["target"].as_str().unwrap_or_default().to_owned())
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(chokkin::internals::proptest_cases(24)))]

    /// CHK008 reports exactly the binaries run but not declared, whatever
    /// the declaration order, group or PEP 503 spelling.
    #[test]
    fn unlisted_binaries_match_reference_model(spec in binary_spec_strategy()) {
        let expected: BTreeSet<String> = spec
            .binaries
            .iter()
            .zip(BINARIES)
            .filter(|((used, declared, ..), _)| *used && !*declared)
            .map(|(_, (binary, _))| binary.to_owned())
            .collect();
        let dir = binary_project(&spec);
        prop_assert_eq!(unlisted_binaries(dir.path()), expected, "{:?}", spec);
    }

    /// Declaring one more distribution never adds a CHK008.
    #[test]
    fn declaring_a_distribution_never_adds_chk008(
        spec in binary_spec_strategy(),
        pick in any::<prop::sample::Index>(),
    ) {
        let before = unlisted_binaries(binary_project(&spec).path());
        let mut declared = spec;
        declared.binaries[pick.index(BINARIES.len())].1 = true;
        let after = unlisted_binaries(binary_project(&declared).path());
        prop_assert!(after.is_subset(&before), "{after:?} vs {before:?}");
    }
}
