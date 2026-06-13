//! `.venv` dist-info metadata reading.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::discovery::ProjectRoot;
use crate::manifest::normalize_distribution_name;

use super::metadata::parse_metadata;
use super::types::ResolveWarning;

/// Import root → distribution names discovered from a project virtualenv.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VenvIndex {
    /// Import root to normalized distribution names.
    pub imports: BTreeMap<String, Vec<String>>,
}

/// Load import mappings from a project virtualenv when present.
#[must_use]
pub fn load_venv_index(root: &ProjectRoot, warnings: &mut Vec<ResolveWarning>) -> VenvIndex {
    let Some(venv_path) = discover_venv(&root.path) else {
        return VenvIndex::default();
    };

    match read_venv_index(&venv_path) {
        Ok(index) => index,
        Err(reason) => {
            warnings.push(ResolveWarning::VenvUnreadable {
                path: venv_path.display().to_string(),
                reason,
            });
            VenvIndex::default()
        },
    }
}

fn discover_venv(root: &Path) -> Option<PathBuf> {
    for name in [".venv", "venv"] {
        let candidate = root.join(name);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

fn read_venv_index(venv: &Path) -> Result<VenvIndex, String> {
    let site_packages =
        find_site_packages(venv).ok_or_else(|| "site-packages not found".to_owned())?;
    let mut index = VenvIndex::default();

    let entries = fs::read_dir(&site_packages).map_err(|err| err.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.ends_with(".dist-info") {
            continue;
        }
        merge_dist_info(&path, &mut index);
    }

    for imports in index.imports.values_mut() {
        imports.sort();
        imports.dedup();
    }
    Ok(index)
}

fn find_site_packages(venv: &Path) -> Option<PathBuf> {
    // Windows venvs use Lib/site-packages (no pythonX.Y segment).
    let windows_style = venv.join("Lib").join("site-packages");
    if windows_style.is_dir() {
        return Some(windows_style);
    }

    let lib = venv.join("lib");
    let entries = fs::read_dir(&lib).ok()?;
    for entry in entries.flatten() {
        let python_dir = entry.path();
        if !python_dir.is_dir() {
            continue;
        }
        let site_packages = python_dir.join("site-packages");
        if site_packages.is_dir() {
            return Some(site_packages);
        }
    }
    None
}

fn merge_dist_info(dist_info: &Path, index: &mut VenvIndex) {
    let metadata_path = dist_info.join("METADATA");
    let Ok(contents) = fs::read_to_string(&metadata_path) else {
        return;
    };
    let metadata = parse_metadata(&contents);
    let distribution = metadata
        .name
        .as_deref()
        .map(normalize_distribution_name)
        .or_else(|| {
            dist_info
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(parse_dist_info_name)
        });

    let Some(distribution) = distribution else {
        return;
    };

    let top_level_path = dist_info.join("top_level.txt");
    if let Ok(top_level) = fs::read_to_string(&top_level_path) {
        for line in top_level.lines() {
            let import = line.trim();
            if !import.is_empty() && !import.starts_with('_') {
                push_import(index, import, &distribution);
            }
        }
    }

    for import in metadata.import_names {
        push_import(index, &import, &distribution);
    }
    for import in metadata.import_namespaces {
        push_import(index, &import, &distribution);
    }
}

fn push_import(index: &mut VenvIndex, import: &str, distribution: &str) {
    index
        .imports
        .entry(import.to_owned())
        .or_default()
        .push(distribution.to_owned());
}

fn parse_dist_info_name(file_name: &str) -> Option<String> {
    let stem = file_name.strip_suffix(".dist-info")?;
    let (name, _version) = stem.rsplit_once('-')?;
    Some(normalize_distribution_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dist_info_directory_name() {
        assert_eq!(
            parse_dist_info_name("PyYAML-6.0.1.dist-info").as_deref(),
            Some("pyyaml")
        );
    }
}
