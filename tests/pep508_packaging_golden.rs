//! Requirement acceptance pinned against `packaging`
//! (`scripts/generate-pep508-golden.py` writes the golden file).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::PathBuf;

use chokkin::internals::{ManifestWarning, ProjectRoot, RootMarker, extract_manifest, load_config};
use serde::Deserialize;

#[derive(Deserialize)]
struct Golden {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    input: String,
    valid: bool,
    #[serde(default)]
    name: String,
    #[serde(default)]
    extras: Vec<String>,
    #[serde(default)]
    specifiers: Vec<String>,
}

/// Names `packaging` accepts but chokkin rejects on purpose (#572): its name
/// token ends at a regex word boundary, so a trailing `_` slips past the
/// PEP 508 rule that a name ends in a letter or digit. uv rejects these too.
const STRICTER_THAN_PACKAGING: [&str; 6] = [
    "foo_",
    "foo-_",
    "foo._",
    "foo[te_]",
    "foo_[a]>=1",
    "foo_ @ https://h/x.whl",
];

#[test]
fn pyproject_dependencies_match_packaging() {
    let golden: Golden = serde_json::from_str(
        &std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/pep508/packaging_golden.json"),
        )
        .expect("read golden"),
    )
    .expect("parse golden");

    let dependencies: Vec<toml::Value> = golden
        .cases
        .iter()
        .map(|case| toml::Value::String(case.input.clone()))
        .collect();
    let pyproject = format!(
        "[project]\nname = \"golden\"\ndependencies = {}\n",
        toml::Value::Array(dependencies)
    );
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("pyproject.toml"), pyproject).expect("write pyproject");
    let root = ProjectRoot {
        path: dir.path().canonicalize().expect("canonicalize"),
        marker: RootMarker::PyProjectToml,
    };
    let config = load_config(&root).expect("load config");
    let manifest = extract_manifest(&root, &config).expect("extract manifest");

    for (index, case) in golden.cases.iter().enumerate() {
        let label = format!("project.dependencies[{index}]");
        let dep = manifest
            .dependencies
            .iter()
            .find(|dep| dep.origin.label == label);
        let warned = manifest.warnings.iter().any(|warning| {
            matches!(warning, ManifestWarning::InvalidRequirementLine { label: l, .. } if *l == label)
        });
        let stricter = STRICTER_THAN_PACKAGING.contains(&case.input.as_str());
        assert!(
            !stricter || case.valid,
            "{:?} is no longer a deviation",
            case.input
        );
        if case.valid && !stricter {
            let dep = dep.unwrap_or_else(|| panic!("{:?} must be accepted", case.input));
            let mut extras = dep.extras.clone();
            extras.sort();
            // A direct reference stores its URL where a version set would go.
            let mut specifiers: Vec<_> = dep
                .specifier
                .iter()
                .filter(|spec| spec.starts_with(['<', '>', '=', '!', '~']))
                .flat_map(|spec| spec.split(',').map(|item| item.trim().to_owned()))
                .collect();
            specifiers.sort();
            assert_eq!(
                (dep.name.as_str(), extras, specifiers),
                (
                    case.name.as_str(),
                    case.extras.clone(),
                    case.specifiers.clone()
                ),
                "{:?}",
                case.input
            );
        } else {
            assert!(
                dep.is_none() && warned,
                "{:?} must be rejected with a warning",
                case.input
            );
        }
    }
}
