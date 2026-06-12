//! Config file discovery at the project root.

use std::path::{Path, PathBuf};

/// Paths to optional config files at the project root.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_field_names)]
pub(super) struct ConfigFileSet {
    pub dot_yokei_toml: Option<PathBuf>,
    pub yokei_toml: Option<PathBuf>,
    pub pyproject_toml: Option<PathBuf>,
}

/// Discover config file paths under `root` without reading their contents.
#[must_use]
pub fn discover_config_files(root: &Path) -> ConfigFileSet {
    let dot_yokei = root.join(".yokei.toml");
    let yokei = root.join("yokei.toml");
    let pyproject = root.join("pyproject.toml");

    ConfigFileSet {
        dot_yokei_toml: file_if_exists(&dot_yokei),
        yokei_toml: file_if_exists(&yokei),
        pyproject_toml: file_if_exists(&pyproject),
    }
}

fn file_if_exists(path: &Path) -> Option<PathBuf> {
    path.is_file().then(|| path.to_path_buf())
}
