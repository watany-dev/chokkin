//! Project root discovery by walking upward for §4 markers.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use super::error::DiscoveryError;

/// Marker that determined the project root (§4 priority order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootMarker {
    /// `pyproject.toml` exists in the root directory.
    PyProjectToml,
    /// `uv.lock` exists in the root directory.
    UvLock,
    /// `setup.cfg` exists in the root directory.
    SetupCfg,
    /// `setup.py` exists in the root directory.
    SetupPy,
    /// `requirements.txt` exists in the root directory.
    RequirementsTxt,
    /// `.git` directory exists in the root directory.
    Git,
}

impl RootMarker {
    /// Stable marker identifier for reporters and `--explain` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PyProjectToml => "pyproject.toml",
            Self::UvLock => "uv.lock",
            Self::SetupCfg => "setup.cfg",
            Self::SetupPy => "setup.py",
            Self::RequirementsTxt => "requirements.txt",
            Self::Git => ".git",
        }
    }
}

impl fmt::Display for RootMarker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A discovered Python project root directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRoot {
    /// Canonical or normalized absolute path to the project root.
    pub path: PathBuf,
    /// Which marker caused this directory to be selected.
    pub marker: RootMarker,
    /// Original `start` argument as passed by the caller (not canonicalized).
    pub start: PathBuf,
}

const FILE_MARKERS: &[(&str, RootMarker)] = &[
    ("pyproject.toml", RootMarker::PyProjectToml),
    ("uv.lock", RootMarker::UvLock),
    ("setup.cfg", RootMarker::SetupCfg),
    ("setup.py", RootMarker::SetupPy),
    ("requirements.txt", RootMarker::RequirementsTxt),
];

/// Discover the project root by walking upward from `start`.
///
/// Checks markers in §4 priority order at each ancestor directory.
/// Returns [`DiscoveryError::NotFound`] if the filesystem root is reached
/// without a match.
pub fn discover_project_root(start: &Path) -> Result<ProjectRoot, DiscoveryError> {
    let original_start = start.to_path_buf();

    if !start.is_dir() {
        return Err(DiscoveryError::InvalidStart {
            path: original_start,
        });
    }

    let mut current = normalize_start(start)?;

    loop {
        if let Some(marker) = probe_markers(&current)? {
            let path = fs::canonicalize(&current).unwrap_or(current);
            return Ok(ProjectRoot {
                path,
                marker,
                start: original_start,
            });
        }

        let Some(parent) = current.parent() else {
            return Err(DiscoveryError::NotFound {
                start: original_start,
            });
        };

        if parent == current {
            return Err(DiscoveryError::NotFound {
                start: original_start,
            });
        }

        current = parent.to_path_buf();
    }
}

fn normalize_start(start: &Path) -> Result<PathBuf, DiscoveryError> {
    fs::canonicalize(start).or_else(|_| {
        if start.is_absolute() {
            Ok(start.to_path_buf())
        } else {
            let cwd = std::env::current_dir().map_err(|source| DiscoveryError::Io {
                path: start.to_path_buf(),
                source,
            })?;
            Ok(cwd.join(start))
        }
    })
}

fn probe_markers(dir: &Path) -> Result<Option<RootMarker>, DiscoveryError> {
    for (name, marker) in FILE_MARKERS {
        let path = dir.join(name);
        if is_file(&path)? {
            return Ok(Some(*marker));
        }
    }

    let git_path = dir.join(".git");
    if is_directory(&git_path)? {
        return Ok(Some(RootMarker::Git));
    }

    Ok(None)
}

fn metadata_for(path: &Path) -> Result<Option<fs::Metadata>, DiscoveryError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(DiscoveryError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn is_file(path: &Path) -> Result<bool, DiscoveryError> {
    Ok(metadata_for(path)?.is_some_and(|metadata| metadata.is_file()))
}

fn is_directory(path: &Path) -> Result<bool, DiscoveryError> {
    Ok(metadata_for(path)?.is_some_and(|metadata| metadata.is_dir()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directories");
        }
        let mut file = fs::File::create(path).expect("create file");
        file.write_all(contents.as_bytes())
            .expect("write file contents");
    }

    #[test]
    fn root_marker_display_matches_as_str() {
        assert_eq!(RootMarker::PyProjectToml.to_string(), "pyproject.toml");
        assert_eq!(RootMarker::Git.as_str(), ".git");
    }

    #[test]
    fn probe_markers_prefers_pyproject_over_requirements() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_file(
            &temp.path().join("pyproject.toml"),
            "[project]\nname = \"a\"\n",
        );
        write_file(&temp.path().join("requirements.txt"), "requests\n");

        let marker = probe_markers(temp.path()).expect("probe").expect("marker");
        assert_eq!(marker, RootMarker::PyProjectToml);
    }

    #[test]
    fn probe_markers_detects_git_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(temp.path().join(".git")).expect("create .git");
        write_file(&temp.path().join(".git/HEAD"), "ref: refs/heads/main\n");

        let marker = probe_markers(temp.path()).expect("probe").expect("marker");
        assert_eq!(marker, RootMarker::Git);
    }
}
