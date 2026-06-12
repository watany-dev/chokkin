//! Errors raised while extracting project manifests.

use std::path::PathBuf;

/// Failure while reading or parsing manifest files.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// Filesystem I/O failure while reading a manifest file.
    #[error("failed to read manifest file {path}")]
    Io {
        /// Path that triggered the I/O error.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// TOML syntax or type error in a manifest file.
    #[error("invalid TOML in {path}")]
    InvalidToml {
        /// Manifest file path.
        path: PathBuf,
        /// Underlying parse error.
        #[source]
        source: toml::de::Error,
    },

    /// A `-r` include referenced a requirements file that does not exist.
    #[error("requirements file not found: {path}")]
    RequirementsIncludeMissing {
        /// Missing include path.
        path: String,
    },

    /// Circular `-r` include chain in requirements files.
    #[error("circular requirements include: {cycle}")]
    RequirementsCircularInclude {
        /// Human-readable cycle description.
        cycle: String,
    },

    /// `uv.lock` could not be parsed.
    #[error("invalid uv.lock in {path}")]
    InvalidUvLock {
        /// Lockfile path.
        path: PathBuf,
        /// Human-readable parse error.
        message: String,
    },
}
