//! PEP 751 `pylock.toml` graph extraction.

use std::collections::BTreeMap;
use std::path::Path;

use toml::Value;

use super::error::ManifestError;
use super::lockfile::{array_tables, merge_package, read_lock_table};
use super::pep508_util::normalize_distribution_name;
use super::types::LockfileGraph;

/// Parse `pylock.toml` / `pylock.<name>.toml` into a dependency name graph.
///
/// `[[packages]].dependencies` is optional in PEP 751; packages without it
/// still become graph nodes so "listed in the lock" stays observable.
pub fn extract_pylock(path: &Path) -> Result<LockfileGraph, ManifestError> {
    let table = read_lock_table(path)?;
    let mut edges = BTreeMap::new();
    for package in array_tables(&table, "packages") {
        let Some(name) = package.get("name").and_then(Value::as_str) else {
            continue;
        };
        let deps = array_tables(package, "dependencies")
            .filter_map(|dep| dep.get("name").and_then(Value::as_str))
            .map(normalize_distribution_name);
        merge_package(&mut edges, name, deps);
    }
    Ok(LockfileGraph { edges })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(contents: &str) -> Result<LockfileGraph, ManifestError> {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("pylock.toml");
        std::fs::write(&path, contents).expect("write pylock.toml");
        extract_pylock(&path)
    }

    /// Shaped after the example in PEP 751.
    const PEP_751_EXAMPLE: &str = r#"
lock-version = "1.0"
environments = ["sys_platform == 'win32'", "sys_platform == 'linux'"]
requires-python = "==3.12"
created-by = "mousebender"

[[packages]]
name = "attrs"
version = "25.1.0"
requires-python = ">=3.8"
wheels = [
  {name = "attrs-25.1.0-py3-none-any.whl", upload-time = 2025-01-25T11:30:10.164985+00:00, url = "https://files.pythonhosted.org/packages/fc/30/d4986a882011f9df997a55e6becd864812ccfcd821d64aac8570ee39f719/attrs-25.1.0-py3-none-any.whl", size = 63152, hashes = {sha256 = "c75a69e28a550a7e93789579c22aa26b0f5b83b75dc4e08fe092980051e1090a"}},
]

[[packages]]
name = "Cattrs"
version = "24.1.2"
requires-python = ">=3.8"
dependencies = [
    {name = "attrs"},
]
wheels = [
  {name = "cattrs-24.1.2-py3-none-any.whl", upload-time = 2024-09-22T14:58:34.812643+00:00, url = "https://files.pythonhosted.org/packages/c8/d5/867e75361fc45f6de75fe277dd085627a9db5ebb511a87f27dc1396b5351/cattrs-24.1.2-py3-none-any.whl", size = 66446, hashes = {sha256 = "67c7495b760168d931a10233f979b28dc04daf853b30752246f4f8471c6d68d0"}},
]

[[packages]]
name = "numpy"
version = "2.2.3"
requires-python = ">=3.10"

[tool.mousebender]
command = ["."]
"#;

    #[test]
    fn extracts_pep_751_example() {
        let graph = parse(PEP_751_EXAMPLE).expect("valid pylock.toml");
        assert_eq!(graph.edges.get("cattrs"), Some(&vec!["attrs".to_owned()]));
        assert_eq!(graph.edges.get("attrs"), Some(&Vec::new()));
        assert_eq!(graph.edges.get("numpy"), Some(&Vec::new()));
        assert_eq!(graph.edges.len(), 3);
    }

    #[test]
    fn merges_repeated_package_entries() {
        let graph = parse(
            "[[packages]]\nname = \"a\"\ndependencies = [{name = \"b\"}]\n\
             [[packages]]\nname = \"A\"\ndependencies = [{name = \"c\"}, {name = \"b\"}]\n",
        )
        .expect("valid pylock.toml");
        assert_eq!(
            graph.edges.get("a"),
            Some(&vec!["b".to_owned(), "c".to_owned()])
        );
    }

    #[test]
    fn rejects_invalid_toml() {
        let error = parse("[[packages\n").expect_err("invalid TOML");
        assert!(matches!(error, ManifestError::InvalidLockfile { .. }));
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn extract_pylock_never_panics(contents in "\\PC{0,400}") {
                let _ = parse(&contents);
            }

            #[test]
            fn extract_pylock_never_panics_on_package_shapes(
                name in "\\PC{0,20}",
                dep in "\\PC{0,20}",
            ) {
                let contents = format!(
                    "[[packages]]\nname = {name:?}\ndependencies = [{{name = {dep:?}}}, 1, \"x\"]\n"
                );
                let _ = parse(&contents);
            }
        }
    }
}
