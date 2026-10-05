//! `uv.lock` graph extraction.

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
}

#[derive(Deserialize)]
#[serde(untagged)]
enum UvDependency {
    Table { name: String },
    Name(String),
    Other(IgnoredAny),
}

/// Parse `uv.lock` into a dependency name graph.
pub fn extract_uv_lock(path: &Path) -> Result<LockfileGraph, ManifestError> {
    let contents = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let lock: UvLock = toml::from_str(&contents).map_err(|error| ManifestError::InvalidUvLock {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;

    let edges = lock
        .package
        .into_iter()
        .filter_map(|package| {
            let deps = package
                .dependencies
                .iter()
                .filter_map(|dep| match dep {
                    UvDependency::Table { name } | UvDependency::Name(name) => {
                        Some(normalize_distribution_name(name))
                    },
                    UvDependency::Other(_) => None,
                })
                .collect();
            Some((normalize_distribution_name(&package.name?), deps))
        })
        .collect();

    Ok(LockfileGraph { edges })
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
    fn rejects_invalid_toml() {
        let error = parse("[[package\n").expect_err("invalid TOML");
        assert!(matches!(error, ManifestError::InvalidUvLock { .. }));
    }

    mod props {
        use std::fmt::Write as _;

        use super::*;
        use proptest::prelude::*;

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
