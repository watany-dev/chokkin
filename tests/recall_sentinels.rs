//! Recall sentinels from `scripts/oss-recall.manifest` (#324).
//!
//! `scripts/oss-metrics.sh --gate` needs OSS clones, so CI would otherwise never
//! notice a sentinel going silent. This replays its recall and CHK002 labelling
//! checks on the in-repo fixtures only.

#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

type Key = (String, String, String);

fn repo_file(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn data_lines(rel: &str) -> Vec<Vec<String>> {
    fs::read_to_string(repo_file(rel))
        .expect(rel)
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| line.split('\t').map(str::to_owned).collect())
        .collect()
}

fn sentinels() -> Vec<(String, PathBuf)> {
    data_lines("scripts/oss-recall.manifest")
        .into_iter()
        .map(|fields| {
            assert_eq!(fields.len(), 2, "slug<TAB>path: {fields:?}");
            (fields[0].clone(), repo_file(&fields[1]))
        })
        .collect()
}

/// `(slug, code, target)` → verdict, for the given slugs.
fn labels(slugs: &BTreeSet<&str>) -> Vec<(Key, String)> {
    data_lines("scripts/oss-fixtures.labels.tsv")
        .into_iter()
        .filter(|fields| fields.len() >= 5 && slugs.contains(fields[0].as_str()))
        .map(|fields| {
            (
                (fields[0].clone(), fields[1].clone(), fields[2].clone()),
                fields[3].clone(),
            )
        })
        .collect()
}

fn findings(slug: &str, root: &Path) -> BTreeSet<Key> {
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--reporter", "json", "--no-exit-code"])
        .arg(root)
        .output()
        .expect("run chokkin");
    assert!(output.status.success(), "{slug}: {output:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    parsed["issues"]
        .as_array()
        .expect("issues array")
        .iter()
        .map(|issue| {
            (
                slug.to_owned(),
                issue["code"].as_str().unwrap_or_default().to_owned(),
                issue["target"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

#[test]
fn recall_sentinels_keep_every_tp_label_and_label_every_chk002() {
    let sentinels = sentinels();
    let slugs: BTreeSet<&str> = sentinels.iter().map(|(slug, _)| slug.as_str()).collect();
    let labels = labels(&slugs);

    let mut found = BTreeSet::new();
    for (slug, root) in &sentinels {
        assert!(root.is_dir(), "missing sentinel fixture {}", root.display());
        found.extend(findings(slug, root));
    }

    for (slug, _) in &sentinels {
        assert!(
            labels
                .iter()
                .any(|((label_slug, _, _), verdict)| label_slug == slug && verdict == "tp"),
            "sentinel {slug} has no tp label"
        );
    }
    let missed: Vec<_> = labels
        .iter()
        .filter(|(key, verdict)| verdict == "tp" && !found.contains(key))
        .map(|(key, _)| key)
        .collect();
    assert!(missed.is_empty(), "missed tp: {missed:?}\nfound: {found:?}");

    // oss-metrics.sh fails the FP gate on unlabelled or deferred CHK002.
    let unclassified: Vec<_> = found
        .iter()
        .filter(|(_, code, _)| code == "CHK002")
        .filter(|key| {
            !labels
                .iter()
                .any(|(label, verdict)| label == *key && (verdict == "tp" || verdict == "fp"))
        })
        .collect();
    assert!(
        unclassified.is_empty(),
        "unlabelled CHK002 on sentinels: {unclassified:?}"
    );
}
