//! Conservative cache policy types for Phase 2 warm-run support.

use std::path::PathBuf;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
