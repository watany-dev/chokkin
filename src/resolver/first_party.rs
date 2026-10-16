//! First-party and workspace import classification.

use crate::manifest::{ProjectMetadata, normalize_distribution_name};
use crate::sources::LayoutInfo;

/// Returns `true` when `import_root` matches a first-party package.
#[must_use]
pub(crate) fn is_first_party_import(
    import_root: &str,
    layout: &LayoutInfo,
    metadata: &ProjectMetadata,
) -> bool {
    let import_norm = normalize_distribution_name(import_root);
    // A workspace member's own packages are first-party to every file, the
    // member's own included (`llama_dev` inside `llama-dev/`).
    if layout
        .packages
        .iter()
        .chain(&layout.local_packages)
        .chain(
            layout
                .members
                .iter()
                .flat_map(|member| &member.layout.packages),
        )
        .any(|package| normalize_distribution_name(package) == import_norm)
    {
        return true;
    }
    if metadata
        .version_files
        .iter()
        .any(|path| version_file_root(path, layout) == Some(import_root))
    {
        return true;
    }
    if let Some(name) = &metadata.name {
        return normalize_distribution_name(name) == import_norm;
    }
    false
}

/// Import root of a generated version file, taken below the layout's package
/// root or `src/` (`src/_black_version.py` → `_black_version`).
fn version_file_root<'a>(path: &'a str, layout: &LayoutInfo) -> Option<&'a str> {
    let module = path.strip_suffix(".py")?;
    let module = [layout.package_root.as_str(), "src"]
        .into_iter()
        .find_map(|dir| module.strip_prefix(dir)?.strip_prefix('/'))
        .unwrap_or(module);
    module.split('/').next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_package_is_first_party() {
        let layout = LayoutInfo {
            local_packages: vec!["tests".to_owned()],
            ..LayoutInfo::new("src", vec!["acme".to_owned()])
        };
        for root in ["acme", "tests"] {
            assert!(is_first_party_import(
                root,
                &layout,
                &ProjectMetadata::default()
            ));
        }
    }

    #[test]
    fn metadata_name_matches_normalized_import_root() {
        let layout = LayoutInfo::new("", Vec::new());
        let metadata = ProjectMetadata {
            name: Some("my-package".to_owned()),
            ..ProjectMetadata::default()
        };
        assert!(is_first_party_import("my_package", &layout, &metadata));
    }

    #[test]
    fn generated_version_file_is_first_party_below_the_package_root() {
        let metadata = ProjectMetadata {
            version_files: vec![
                "src/_black_version.py".to_owned(),
                "lib/acme/_version.py".to_owned(),
                "_flat_version.py".to_owned(),
            ],
            ..ProjectMetadata::default()
        };
        let layout = LayoutInfo::new("lib", Vec::new());
        for root in ["_black_version", "acme", "_flat_version"] {
            assert!(is_first_party_import(root, &layout, &metadata), "{root}");
        }
        for root in ["src", "lib", "_version"] {
            assert!(!is_first_party_import(root, &layout, &metadata), "{root}");
        }
    }
}
