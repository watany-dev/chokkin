//! `uv.lock` graph extraction.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde::de::IgnoredAny;

use super::error::ManifestError;
use super::pep508_util::normalize_distribution_name;
use super::types::LockfileGraph;

/// Only the fields the graph needs: skipping `sdist`/`wheels` without building
/// values keeps a monorepo's hundreds of member lockfiles cheap to read (#488).
#[derive(Deserialize)]
struct UvLock {
    #[serde(default)]
    package: Vec<UvPackage>,
}

#[derive(Deserialize)]
struct UvPackage {
    name: Option<String>,
    #[serde(default)]
    dependencies: Vec<UvDependency>,
    #[serde(default, rename = "optional-dependencies")]
    optional_dependencies: BTreeMap<String, Vec<UvDependency>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum UvDependency {
    Table { name: String },
    Name(String),
    Other(IgnoredAny),
}

/// Parse `uv.lock` into a dependency name graph.
pub(super) fn extract_uv_lock(path: &Path) -> Result<LockfileGraph, ManifestError> {
    let contents = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    // A filtered lock that no longer parses means the strip guessed wrong, so
    // the full text decides whether the file is really invalid.
    let stripped = strip_artifacts(&contents);
    let lock: UvLock = stripped
        .as_deref()
        .map_or_else(|| toml::from_str(&contents), toml::from_str)
        .or_else(|_| toml::from_str(&contents))
        .map_err(|error| ManifestError::InvalidUvLock {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;

    let mut graph = LockfileGraph::default();
    for package in lock.package {
        let Some(name) = package.name.as_deref().map(normalize_distribution_name) else {
            continue;
        };
        if !package.optional_dependencies.is_empty() {
            let extras = package
                .optional_dependencies
                .iter()
                .map(|(extra, deps)| (normalize_distribution_name(extra), dependency_names(deps)))
                .collect();
            graph.extras.insert(name.clone(), extras);
        }
        graph
            .edges
            .insert(name, dependency_names(&package.dependencies));
    }

    Ok(graph)
}

fn dependency_names(deps: &[UvDependency]) -> Vec<String> {
    deps.iter()
        .filter_map(|dep| match dep {
            UvDependency::Table { name } | UvDependency::Name(name) => {
                Some(normalize_distribution_name(name))
            },
            UvDependency::Other(_) => None,
        })
        .collect()
}

/// Drop the `sdist` and `wheels` lines uv writes for every package: they are
/// ~95% of a lock's bytes and TOML parsing them dominated the probe of a
/// monorepo with hundreds of member lockfiles (#513). `None` when a `wheels`
/// block is not uv's one-table-per-line layout, since skipping to the next
/// `]` line could then drop graph lines and still parse.
fn strip_artifacts(contents: &str) -> Option<String> {
    let mut kept = String::with_capacity(contents.len() / 16);
    let mut in_wheels = false;
    for line in contents.lines() {
        if in_wheels {
            in_wheels = line != "]";
            let item = line.trim();
            if in_wheels && !(item.starts_with('{') && item.ends_with("},")) {
                return None;
            }
        } else if line == "wheels = [" {
            in_wheels = true;
        } else if !line.starts_with("sdist = ") && !line.starts_with("wheels = [{") {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    (!in_wheels).then_some(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(contents: &str) -> Result<LockfileGraph, ManifestError> {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("uv.lock");
        std::fs::write(&path, contents).expect("write uv.lock");
        extract_uv_lock(&path)
    }

    #[test]
    fn extracts_package_edges() {
        let graph = parse(
            "requires-python = \">=3.11\"\n\n\
             [[package]]\nname = \"Acme_Lib\"\n\
             dependencies = [{ name = \"requests\" }, \"PyYAML\"]\n",
        )
        .expect("valid uv.lock");

        assert_eq!(
            graph.edges.get("acme-lib"),
            Some(&vec!["requests".to_owned(), "pyyaml".to_owned()])
        );
    }

    #[test]
    fn extracts_optional_dependencies_per_extra() {
        let graph = parse(
            "[[package]]\nname = \"psycopg\"\n\
             dependencies = [{ name = \"typing-extensions\" }]\n\n\
             [package.optional-dependencies]\n\
             pool = [{ name = \"Psycopg_Pool\" }]\n",
        )
        .expect("valid uv.lock");

        assert_eq!(
            graph
                .extras
                .get("psycopg")
                .and_then(|extras| extras.get("pool")),
            Some(&vec!["psycopg-pool".to_owned()])
        );
        assert_eq!(
            graph.edges.get("psycopg"),
            Some(&vec!["typing-extensions".to_owned()])
        );
    }

    #[test]
    fn skips_sdist_and_wheels() {
        let graph = parse(
            "[[package]]\nname = \"acme\"\n\
             sdist = { url = \"https://x/acme.tar.gz\", hash = \"sha256:00\" }\n\
             wheels = [\n    { url = \"https://x/acme.whl\", hash = \"sha256:00\" },\n]\n\
             dependencies = [{ name = \"idna\" }]\n\n\
             [[package]]\nname = \"idna\"\n\
             wheels = [{ url = \"https://x/idna.whl\" }]\n",
        )
        .expect("valid uv.lock");

        assert_eq!(graph.edges.get("acme"), Some(&vec!["idna".to_owned()]));
        assert_eq!(graph.edges.get("idna"), Some(&Vec::new()));
    }

    // The full-text fallback hides a wrong strip from `parse`, so check the
    // filtered text itself.
    #[test]
    fn strip_artifacts_keeps_only_graph_lines() {
        let stripped = strip_artifacts(
            "[[package]]\nname = \"acme\"\n\
             sdist = { url = \"https://x/acme.tar.gz\" }\n\
             wheels = [\n    { url = \"https://x/acme.whl\" },\n]\n\
             dependencies = [{ name = \"idna\" }]\n\
             wheels = [{ url = \"https://x/idna.whl\" }]\n",
        );

        assert_eq!(
            stripped.as_deref(),
            Some("[[package]]\nname = \"acme\"\ndependencies = [{ name = \"idna\" }]\n")
        );
        assert_eq!(
            strip_artifacts("wheels = [\n    { url = \"a\" }]\nx = 1\n]\n"),
            None
        );
        assert_eq!(strip_artifacts("wheels = [\n    { url = \"a\" },\n"), None);
        assert_eq!(strip_artifacts("wheels = [\n    { url = \"a\",\n]\n"), None);
        assert_eq!(strip_artifacts("wheels = [\n    url = \"a\" },\n]\n"), None);
    }

    #[test]
    fn wheels_array_closed_on_its_last_item_does_not_hide_later_packages() {
        let graph = parse(
            "[[package]]\nname = \"acme\"\n\
             dependencies = [{ name = \"idna\" }]\n\
             wheels = [\n    { url = \"https://x/acme.whl\" }]\n\n\
             [[package]]\nname = \"idna\"\n\
             dependencies = [{ name = \"six\" }]\n",
        )
        .expect("valid uv.lock");

        assert_eq!(graph.edges.get("acme"), Some(&vec!["idna".to_owned()]));
        assert_eq!(graph.edges.get("idna"), Some(&vec!["six".to_owned()]));
    }

    #[test]
    fn rejects_invalid_toml() {
        let error = parse("[[package\n").expect_err("invalid TOML");
        assert!(matches!(error, ManifestError::InvalidUvLock { .. }));
    }

    mod props {
        use std::fmt::Write as _;

        use super::*;
        use proptest::prelude::*;

        /// `wheels` arrays as uv writes them and as hand edits may leave them.
        const WHEELS: [&str; 7] = [
            "",
            "wheels = [\n    { url = \"https://x/a.whl\", hash = \"sha256:00\" },\n]\n",
            "wheels = [{ url = \"https://x/a.whl\" }]\n",
            "wheels = [\n    { url = \"https://x/a.whl\" }]\n",
            "wheels = [\n    { url = \"https://x/a.whl\" },\n    { url = \"https://x/b.whl\" }\n]\n",
            "wheels = [\n    { url = \"https://x/a.whl\",\n      hash = \"sha256:00\" },\n]\n",
            "wheels = [\n  # comment\n  { url = \"https://x/a.whl\" },\n]\n",
        ];

        fn package_name() -> impl Strategy<Value = String> {
            "[A-Za-z0-9]([A-Za-z0-9._-]{0,12}[A-Za-z0-9])?"
        }

        proptest! {
            #[test]
            fn extract_uv_lock_never_panics(contents in "\\PC{0,400}") {
                let _ = parse(&contents);
            }

            #[test]
            fn extract_uv_lock_roundtrips_generated_graph(
                packages in prop::collection::btree_map(
                    package_name(),
                    prop::collection::vec(package_name(), 0..4),
                    0..5,
                ),
            ) {
                let mut contents = String::new();
                for (name, deps) in &packages {
                    writeln!(contents, "\n[[package]]\nname = \"{name}\"").expect("write");
                    let rendered = deps
                        .iter()
                        .map(|dep| format!("{{ name = \"{dep}\" }}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    writeln!(contents, "dependencies = [{rendered}]").expect("write");
                }

                let graph = parse(&contents).expect("generated uv.lock is valid TOML");

                // Distinct raw names may normalize to the same key, so compare
                // against a reference map built with the same normalization.
                let mut expected = std::collections::BTreeMap::new();
                for (name, deps) in &packages {
                    expected.insert(
                        normalize_distribution_name(name),
                        deps.iter()
                            .map(|dep| normalize_distribution_name(dep))
                            .collect::<Vec<_>>(),
                    );
                }
                prop_assert_eq!(graph.edges.len(), expected.len());
                for (name, deps) in &expected {
                    prop_assert_eq!(graph.edges.get(name), Some(deps));
                }
            }

            #[test]
            fn artifact_layout_does_not_change_the_graph(
                packages in prop::collection::btree_map(
                    package_name(),
                    (prop::collection::vec(package_name(), 0..3), 0usize..WHEELS.len(), any::<bool>()),
                    1..5,
                ),
            ) {
                let mut contents = String::from("version = 1\n");
                let mut expected = std::collections::BTreeMap::new();
                for (name, (deps, wheels, sdist)) in &packages {
                    writeln!(contents, "\n[[package]]\nname = \"{name}\"").expect("write");
                    if *sdist {
                        contents.push_str("sdist = { url = \"https://x/a.tar.gz\", hash = \"sha256:00\" }\n");
                    }
                    contents.push_str(WHEELS[*wheels]);
                    let rendered = deps
                        .iter()
                        .map(|dep| format!("{{ name = \"{dep}\" }}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    writeln!(contents, "dependencies = [{rendered}]").expect("write");
                    expected.insert(
                        normalize_distribution_name(name),
                        deps.iter().map(|dep| normalize_distribution_name(dep)).collect::<Vec<_>>(),
                    );
                }

                let graph = parse(&contents).expect("generated uv.lock is valid TOML");
                prop_assert_eq!(graph.edges, expected, "{}", contents);
            }

            #[test]
            fn all_edge_names_are_normalized(contents in "\\PC{0,400}") {
                if let Ok(graph) = parse(&contents) {
                    for (name, deps) in &graph.edges {
                        let renormalized = normalize_distribution_name(name);
                        prop_assert_eq!(&renormalized, name);
                        for dep in deps {
                            let dep_renormalized = normalize_distribution_name(dep);
                            prop_assert_eq!(&dep_renormalized, dep);
                        }
                    }
                }
            }
        }
    }
}
