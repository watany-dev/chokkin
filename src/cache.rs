//! Conservative cache policy types for Phase 2 warm-run support.

use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Default cache directory name below the project root.
pub const DEFAULT_CACHE_DIR: &str = ".chokkin/cache";

/// Cache configuration for analysis runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheOptions {
    /// Whether cache reads/writes are allowed for this run.
    pub enabled: bool,
    /// Project-root-relative cache directory.
    pub directory: PathBuf,
}

impl Default for CacheOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            directory: PathBuf::from(DEFAULT_CACHE_DIR),
        }
    }
}

impl CacheOptions {
    /// Disable cache reads and writes for this run.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

/// Stable inputs shared by cache units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKeyContext {
    /// chokkin version string.
    pub chokkin_version: String,
    /// Stable hash of effective config inputs.
    pub config_hash: String,
    /// Stable hash of manifest inputs.
    pub manifest_hash: String,
    /// Python target version label.
    pub target_version: String,
    /// Cache unit version, bumped when serialized shape changes.
    pub unit_version: String,
}

/// Fingerprint for one root-relative source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFingerprint {
    /// Root-relative path using `/` separators.
    pub path: String,
    /// File size in bytes.
    pub size: u64,
    /// Modified time in nanoseconds since the Unix epoch, when available.
    pub modified_ns: Option<u128>,
    /// Stable hash of file bytes.
    pub content_hash: String,
}

impl SourceFingerprint {
    /// Build a conservative file fingerprint.
    ///
    /// # Errors
    ///
    /// Returns an IO error when metadata or file contents cannot be read.
    pub fn from_root_relative(root: &Path, path: &str) -> io::Result<Self> {
        let absolute = root.join(path);
        let metadata = std::fs::metadata(&absolute)?;
        let bytes = std::fs::read(&absolute)?;
        Ok(Self {
            path: normalize_cache_path(path),
            size: metadata.len(),
            modified_ns: metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos()),
            content_hash: stable_hex_hash(&bytes),
        })
    }
}

/// Key for a cacheable parse result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseCacheKey {
    /// Shared key context.
    pub context: CacheKeyContext,
    /// Source file fingerprint.
    pub source: SourceFingerprint,
}

/// Normalize a path for cache keys.
#[must_use]
pub fn normalize_cache_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_owned()
}

/// Stable 64-bit FNV-1a hash rendered as lowercase hex.
#[must_use]
pub fn stable_hex_hash(bytes: &[u8]) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cache_test_dir(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "chokkin-cache-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("create temp cache dir");
        root
    }

    #[test]
    fn default_cache_is_enabled_under_project_root() {
        let options = CacheOptions::default();
        assert!(options.enabled);
        assert_eq!(options.directory, PathBuf::from(DEFAULT_CACHE_DIR));
    }

    #[test]
    fn disabled_cache_keeps_directory_policy() {
        let options = CacheOptions::disabled();
        assert!(!options.enabled);
        assert_eq!(options.directory, PathBuf::from(DEFAULT_CACHE_DIR));
    }

    #[test]
    fn normalizes_cache_paths() {
        assert_eq!(
            normalize_cache_path(".\\src\\acme\\main.py"),
            "src/acme/main.py"
        );
    }

    #[test]
    fn stable_hash_changes_with_content() {
        assert_eq!(stable_hex_hash(b""), "cbf29ce484222325");
        assert_ne!(stable_hex_hash(b"alpha"), stable_hex_hash(b"beta"));
    }

    #[test]
    fn source_fingerprint_tracks_content_changes() {
        let root = temp_cache_test_dir("content");
        let path = root.join("src/app.py");
        std::fs::write(&path, "import requests\n").expect("write first source");
        let first =
            SourceFingerprint::from_root_relative(&root, "src/app.py").expect("first fingerprint");

        std::fs::write(&path, "import yaml\n").expect("write second source");
        let second =
            SourceFingerprint::from_root_relative(&root, "src/app.py").expect("second fingerprint");

        assert_eq!(first.path, "src/app.py");
        assert_eq!(second.path, "src/app.py");
        assert_ne!(first.content_hash, second.content_hash);
        let _ = std::fs::remove_dir_all(root);
    }
}
