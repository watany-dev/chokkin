//! Property-based tests for CHK010's per-file aggregation (#584) and the
//! inline-ignore rule that needs every aggregated line (#585), end to end.
//!
//! The reference model is spec §7: one CHK010 per (file, module), anchored at
//! the first import that can fail (the first line when none can), the other
//! lines listed in the explain, a warning when any import can fail, and an
//! inline `# chokkin: ignore[CHK010]` that suppresses it only on every line.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::process::Command;

use proptest::prelude::*;
use tempfile::TempDir;

const MODULE: &str = "zz_missing";
const FILE: &str = "src/pkg/main.py";

/// How one `import zz_missing` is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Top,
    Function,
    /// `if TYPE_CHECKING:` never runs.
    TypeChecking,
    /// `try:` body with `except ImportError:` falls back when missing.
    Optional,
    /// The `except ImportError:` handler is no optional import (#654).
    Fallback,
}

impl Form {
    fn can_fail(self) -> bool {
        !matches!(self, Self::TypeChecking | Self::Optional)
    }
}

#[derive(Debug, Clone, Copy)]
struct Site {
    form: Form,
    ignored: bool,
    /// Comment or blank lines before the site.
    padding: usize,
}

fn site_strategy() -> impl Strategy<Value = Site> {
    (
        prop_oneof![
            Just(Form::Top),
            Just(Form::Function),
            Just(Form::TypeChecking),
            Just(Form::Optional),
            Just(Form::Fallback),
        ],
        any::<bool>(),
        0usize..3,
    )
        .prop_map(|(form, ignored, padding)| Site {
            form,
            ignored,
            padding,
        })
}

fn sites_strategy() -> impl Strategy<Value = Vec<Site>> {
    prop::collection::vec(site_strategy(), 1..6)
}

/// The source and the 1-based line of each site's import.
fn render(sites: &[Site]) -> (String, Vec<u32>) {
    let mut lines = vec!["from typing import TYPE_CHECKING".to_owned()];
    let mut at = Vec::new();
    for (index, site) in sites.iter().enumerate() {
        for pad in 0..site.padding {
            lines.push(if pad % 2 == 0 {
                String::new()
            } else {
                "# filler".to_owned()
            });
        }
        let import = format!(
            "import {MODULE}{}",
            if site.ignored {
                "  # chokkin: ignore[CHK010]"
            } else {
                ""
            }
        );
        let (head, tail): (Vec<String>, &[&str]) = match site.form {
            Form::Top => (Vec::new(), &[]),
            Form::Function => (vec![format!("def f_{index}():")], &[]),
            Form::TypeChecking => (vec!["if TYPE_CHECKING:".to_owned()], &[]),
            Form::Optional => (
                vec!["try:".to_owned()],
                &["except ImportError:", "    pass"],
            ),
            Form::Fallback => (
                vec![
                    "try:".to_owned(),
                    "    import zz_primary".to_owned(),
                    "except ImportError:".to_owned(),
                ],
                &[],
            ),
        };
        lines.extend(head);
        let indent = if site.form == Form::Top { "" } else { "    " };
        lines.push(format!("{indent}{import}"));
        at.push(u32::try_from(lines.len()).expect("line"));
        lines.extend(tail.iter().map(|line| (*line).to_owned()));
    }
    lines.push(String::new());
    (lines.join("\n"), at)
}

fn project(sites: &[Site]) -> (TempDir, Vec<u32>) {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path();
    fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"pkg\"\nversion = \"0\"\ndependencies = []\n",
    )
    .expect("write");
    let package = root.join("src").join("pkg");
    fs::create_dir_all(&package).expect("mkdir");
    fs::write(package.join("__init__.py"), "").expect("write");
    let (source, at) = render(sites);
    fs::write(package.join("main.py"), source).expect("write");
    (dir, at)
}

/// What the run says about `zz_missing` in `main.py`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Found {
    /// `(fingerprint, severity, line)` of each CHK010.
    issues: Vec<(String, String, u64)>,
    /// The explain's `also imported at lines …`, if any.
    others: Option<String>,
}

fn run(sites: &[Site]) -> (Found, Vec<u32>) {
    let (dir, at) = project(sites);
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--reporter", "json", "--no-cache", "--explain"])
        .arg(format!("CHK010:{FILE}:{MODULE}"))
        .arg(dir.path())
        .output()
        .expect("run chokkin");
    assert!(
        output.status.code().is_some_and(|code| code <= 1),
        "chokkin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("json reporter output");
    let issues = parsed["issues"]
        .as_array()
        .expect("issues array")
        .iter()
        .filter(|issue| {
            issue["code"] == "CHK010" && issue["path"] == FILE && issue["symbol"] == MODULE
        })
        .map(|issue| {
            (
                issue["fingerprint"]
                    .as_str()
                    .expect("fingerprint")
                    .to_owned(),
                issue["severity"].as_str().expect("severity").to_owned(),
                issue["line"].as_u64().expect("line"),
            )
        })
        .collect();
    let others = String::from_utf8_lossy(&output.stderr)
        .lines()
        .find_map(|line| line.strip_prefix("also imported at lines "))
        .map(str::to_owned);
    (Found { issues, others }, at)
}

fn reference(sites: &[Site], at: &[u32]) -> Found {
    if sites.iter().all(|site| site.ignored) {
        return Found {
            issues: Vec::new(),
            others: None,
        };
    }
    let failing = sites
        .iter()
        .zip(at)
        .filter(|(site, _)| site.form.can_fail())
        .map(|(_, &line)| line)
        .min();
    let anchor = failing.unwrap_or_else(|| *at.iter().min().expect("one site"));
    let mut others: Vec<u32> = at.iter().copied().filter(|&line| line != anchor).collect();
    others.sort_unstable();
    let severity = if failing.is_some() { "warning" } else { "info" };
    Found {
        issues: vec![(
            format!("CHK010:{FILE}:{MODULE}"),
            severity.to_owned(),
            u64::from(anchor),
        )],
        others: (!others.is_empty()).then(|| {
            others
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        }),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(chokkin::internals::proptest_cases(32)))]

    /// However many times and however guarded a file imports an unresolved
    /// module, CHK010 reports it at most once, anchored and graded per the
    /// spec, and an inline ignore suppresses it only when every line has one.
    #[test]
    fn unresolved_imports_aggregate_per_file(sites in sites_strategy()) {
        let (found, at) = run(&sites);
        prop_assert_eq!(found, reference(&sites, &at));
    }

    /// Reordering the imports changes only which lines the issue names:
    /// the fingerprint, the count and the severity stay.
    #[test]
    fn unresolved_import_issue_ignores_import_order(
        sites in sites_strategy(),
        shuffled in any::<prop::sample::Index>(),
    ) {
        let mut reordered = sites.clone();
        let len = reordered.len();
        reordered.rotate_left(shuffled.index(len));
        reordered.reverse();
        let strip = |found: Found| -> Vec<(String, String)> {
            found
                .issues
                .into_iter()
                .map(|(fingerprint, severity, _)| (fingerprint, severity))
                .collect()
        };
        prop_assert_eq!(strip(run(&sites).0), strip(run(&reordered).0));
    }

    /// Ignoring k of n lines leaves the issue for every k < n.
    #[test]
    fn inline_ignore_needs_every_aggregated_line(
        sites in sites_strategy(),
        keep in any::<prop::sample::Index>(),
    ) {
        let mut sites = sites;
        for site in &mut sites {
            site.ignored = true;
        }
        prop_assert!(run(&sites).0.issues.is_empty());
        let keep = keep.index(sites.len());
        sites[keep].ignored = false;
        prop_assert_eq!(run(&sites).0.issues.len(), 1);
    }
}
