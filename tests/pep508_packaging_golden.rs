//! Requirement acceptance pinned against `packaging`
//! (`scripts/generate-pep508-golden.py` writes the golden file).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::PathBuf;

use chokkin::{ManifestWarning, ProjectRoot, RootMarker, extract_manifest, load_config};
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
}

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
        if case.valid {
            let dep = dep.unwrap_or_else(|| panic!("{:?} must be accepted", case.input));
            let mut extras = dep.extras.clone();
            extras.sort();
            assert_eq!(
                (dep.name.as_str(), extras),
                (case.name.as_str(), case.extras.clone()),
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
