//! Conservative cache policy types for Phase 2 warm-run support.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::config::ConfigSources;
use crate::manifest::ManifestSources;
use crate::parser::ParsedModule;

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

    /// Resolve the cache directory below `project_root`.
    #[must_use]
    pub fn directory_path(&self, project_root: &Path) -> PathBuf {
        project_root.join(&self.directory)
    }

    /// Absolute path for a persisted parse cache entry.
    #[must_use]
    pub fn parse_entry_path(&self, project_root: &Path, key: &ParseCacheKey) -> PathBuf {
        self.directory_path(project_root).join(key.relative_path())
    }

    /// Read a persisted parse cache entry.
    ///
    /// Corrupt JSON entries are treated as misses; callers then reparse source.
    ///
    /// # Errors
    ///
    /// Returns an IO error when the cache file exists but cannot be read.
    pub fn read_parse_entry(
        &self,
        project_root: &Path,
        key: &ParseCacheKey,
    ) -> io::Result<Option<ParsedModule>> {
        if !self.enabled {
            return Ok(None);
        }
        let path = self.parse_entry_path(project_root, key);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(serde_json::from_slice(&bytes).ok())
    }

    /// Write a persisted parse cache entry.
    ///
    /// # Errors
    ///
    /// Returns an IO error when the cache directory or file cannot be written.
    pub fn write_parse_entry(
        &self,
        project_root: &Path,
        key: &ParseCacheKey,
        parsed: &ParsedModule,
    ) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let path = self.parse_entry_path(project_root, key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec(parsed).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes)?;
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        std::fs::rename(tmp, path)
    }
}

/// Stable inputs shared by cache units.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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
        Self::from_absolute(root, &absolute)
    }

    /// Build a conservative file fingerprint from an absolute or root-relative path.
    ///
    /// # Errors
    ///
    /// Returns an IO error when metadata or file contents cannot be read.
    pub fn from_absolute(root: &Path, path: &Path) -> io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        let bytes = std::fs::read(path)?;
        let key_path = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        Ok(Self {
            path: normalize_cache_path(&key_path),
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

fn config_input_fingerprints(
    root: &Path,
    sources: &ConfigSources,
) -> io::Result<Vec<SourceFingerprint>> {
    let mut paths = Vec::new();
    if let Some(path) = &sources.dot_chokkin_toml {
        paths.push(path.clone());
    }
    if let Some(path) = &sources.chokkin_toml {
        paths.push(path.clone());
    }
    let pyproject = root.join("pyproject.toml");
    if pyproject.is_file() {
        paths.push(pyproject);
    }
    fingerprint_paths(root, paths)
}

fn manifest_input_fingerprints(
    root: &Path,
    sources: &ManifestSources,
) -> io::Result<Vec<SourceFingerprint>> {
    let mut paths = Vec::new();
    if sources.pyproject_toml {
        paths.push(root.join("pyproject.toml"));
    }
    if sources.setup_cfg {
        paths.push(root.join("setup.cfg"));
    }
    if sources.setup_py {
        paths.push(root.join("setup.py"));
    }
    if sources.uv_lock {
        paths.push(root.join("uv.lock"));
    }
    for path in &sources.requirements_files {
        paths.push(root.join(path));
    }
    fingerprint_paths(root, paths)
}

fn fingerprint_paths(root: &Path, paths: Vec<PathBuf>) -> io::Result<Vec<SourceFingerprint>> {
    let mut fingerprints = Vec::new();
    for path in paths {
        fingerprints.push(SourceFingerprint::from_absolute(root, &path)?);
    }
    fingerprints.sort_by(|left, right| left.path.cmp(&right.path));
    fingerprints.dedup_by(|left, right| left.path == right.path);
    Ok(fingerprints)
}

/// Fingerprints of files that affect manifest/config scan results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanInputFingerprints {
    /// Config-layer files.
    pub config: Vec<SourceFingerprint>,
    /// Manifest-layer files.
    pub manifest: Vec<SourceFingerprint>,
}

impl ScanInputFingerprints {
    /// Collect fingerprints for currently observed config and manifest inputs.
    ///
    /// # Errors
    ///
    /// Returns an IO error when a recorded input exists but cannot be read.
    pub fn collect(
        root: &Path,
        config: &ConfigSources,
        manifest: &ManifestSources,
    ) -> io::Result<Self> {
        Ok(Self {
            config: config_input_fingerprints(root, config)?,
            manifest: manifest_input_fingerprints(root, manifest)?,
        })
    }
}

/// Key for a cacheable parse result.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ParseCacheKey {
    /// Shared key context.
    pub context: CacheKeyContext,
    /// Source file fingerprint.
    pub source: SourceFingerprint,
}

impl ParseCacheKey {
    /// Stable filename for the cached parse result.
    #[must_use]
    pub fn file_name(&self) -> String {
        let input = format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.context.chokkin_version,
            self.context.config_hash,
            self.context.manifest_hash,
            self.context.target_version,
            self.context.unit_version,
            self.source.path,
            self.source.size,
            self.source
                .modified_ns
                .map_or_else(String::new, |value| value.to_string()),
            self.source.content_hash
        );
        format!("{}.json", stable_hex_hash(input.as_bytes()))
    }

    /// Project-root-relative path for a persisted parse cache entry.
    #[must_use]
    pub fn relative_path(&self) -> PathBuf {
        PathBuf::from("parse").join(self.file_name())
    }
}

/// Parse cache hit/miss counters for observability and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParseCacheStats {
    /// Number of cache hits.
    pub hits: u32,
    /// Number of cache misses.
    pub misses: u32,
    /// Number of values inserted into the cache.
    pub stores: u32,
}

/// In-memory parse cache used as the first conservative cache backend.
#[derive(Debug, Default)]
pub struct ParseCacheStore {
    entries: BTreeMap<ParseCacheKey, ParsedModule>,
    stats: ParseCacheStats,
}

impl ParseCacheStore {
    /// Create an empty parse cache store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return cached parse output for `key` when available.
    pub fn get(&mut self, key: &ParseCacheKey) -> Option<ParsedModule> {
        if let Some(parsed) = self.entries.get(key) {
            self.stats.hits = self.stats.hits.saturating_add(1);
            return Some(parsed.clone());
        }
        self.stats.misses = self.stats.misses.saturating_add(1);
        None
    }

    /// Store parse output for `key`.
    pub fn insert(&mut self, key: ParseCacheKey, parsed: ParsedModule) {
        self.entries.insert(key, parsed);
        self.stats.stores = self.stats.stores.saturating_add(1);
    }

    /// Current cache counters.
    #[must_use]
    pub const fn stats(&self) -> ParseCacheStats {
        self.stats
    }

    /// Number of entries currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
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

    #[test]
    fn scan_fingerprints_include_uv_only_pyproject_config_input() {
        let root = temp_cache_test_dir("scan-config");
        std::fs::write(
            root.join("pyproject.toml"),
            "[tool.uv.workspace]\nmembers = []\n",
        )
        .expect("write pyproject");
        let config = ConfigSources {
            used_defaults: true,
            dot_chokkin_toml: None,
            chokkin_toml: None,
            pyproject_tool_chokkin: false,
        };
        let manifest = ManifestSources::default();

        let fingerprints =
            ScanInputFingerprints::collect(&root, &config, &manifest).expect("fingerprints");

        assert_eq!(fingerprints.config.len(), 1);
        assert_eq!(fingerprints.config[0].path, "pyproject.toml");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn scan_fingerprints_include_manifest_requirement_files() {
        let root = temp_cache_test_dir("scan-manifest");
        std::fs::write(root.join("requirements.txt"), "requests\n").expect("write requirements");
        let config = ConfigSources {
            used_defaults: true,
            dot_chokkin_toml: None,
            chokkin_toml: None,
            pyproject_tool_chokkin: false,
        };
        let manifest = ManifestSources {
            requirements_files: vec!["requirements.txt".to_owned()],
            ..ManifestSources::default()
        };

        let fingerprints =
            ScanInputFingerprints::collect(&root, &config, &manifest).expect("fingerprints");

        assert!(fingerprints.config.is_empty());
        assert_eq!(fingerprints.manifest.len(), 1);
        assert_eq!(fingerprints.manifest[0].path, "requirements.txt");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parse_cache_store_tracks_hits_and_misses() {
        let key = ParseCacheKey {
            context: CacheKeyContext {
                chokkin_version: "test".to_owned(),
                config_hash: "config".to_owned(),
                manifest_hash: "manifest".to_owned(),
                target_version: "py311".to_owned(),
                unit_version: "parse-v1".to_owned(),
            },
            source: SourceFingerprint {
                path: "src/app.py".to_owned(),
                size: 1,
                modified_ns: Some(1),
                content_hash: "hash".to_owned(),
            },
        };
        let parsed = ParsedModule::empty("src/app.py".to_owned());
        let mut cache = ParseCacheStore::new();

        assert!(cache.get(&key).is_none());
        cache.insert(key.clone(), parsed.clone());
        assert_eq!(cache.get(&key), Some(parsed));

        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.stores, 1);
        assert_eq!(stats.hits, 1);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn parse_entry_path_uses_stable_hashed_filename() {
        let context = CacheKeyContext {
            chokkin_version: "test".to_owned(),
            config_hash: "config".to_owned(),
            manifest_hash: "manifest".to_owned(),
            target_version: "py311".to_owned(),
            unit_version: "parse-v1".to_owned(),
        };
        let key = ParseCacheKey {
            context,
            source: SourceFingerprint {
                path: "src/app.py".to_owned(),
                size: 1,
                modified_ns: Some(1),
                content_hash: "hash".to_owned(),
            },
        };

        let path = CacheOptions::default().parse_entry_path(Path::new("/repo"), &key);

        assert!(path.starts_with("/repo/.chokkin/cache/parse"));
        assert_eq!(path.extension().and_then(std::ffi::OsStr::to_str), Some("json"));
    }

    #[test]
    fn parse_entry_round_trips_to_disk() {
        let root = temp_cache_test_dir("disk");
        let key = ParseCacheKey {
            context: CacheKeyContext {
                chokkin_version: "test".to_owned(),
                config_hash: "config".to_owned(),
                manifest_hash: "manifest".to_owned(),
                target_version: "py311".to_owned(),
                unit_version: "parse-v1".to_owned(),
            },
            source: SourceFingerprint {
                path: "src/app.py".to_owned(),
                size: 1,
                modified_ns: Some(1),
                content_hash: "hash".to_owned(),
            },
        };
        let parsed = ParsedModule::empty("src/app.py".to_owned());
        let options = CacheOptions::default();

        options
            .write_parse_entry(&root, &key, &parsed)
            .expect("write parse cache");
        let restored = options
            .read_parse_entry(&root, &key)
            .expect("read parse cache")
            .expect("cache hit");

        assert_eq!(restored, parsed);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_parse_entry_is_cache_miss() {
        let root = temp_cache_test_dir("corrupt");
        let key = ParseCacheKey {
            context: CacheKeyContext {
                chokkin_version: "test".to_owned(),
                config_hash: "config".to_owned(),
                manifest_hash: "manifest".to_owned(),
                target_version: "py311".to_owned(),
                unit_version: "parse-v1".to_owned(),
            },
            source: SourceFingerprint {
                path: "src/app.py".to_owned(),
                size: 1,
                modified_ns: Some(1),
                content_hash: "hash".to_owned(),
            },
        };
        let options = CacheOptions::default();
        let path = options.parse_entry_path(&root, &key);
        std::fs::create_dir_all(path.parent().expect("cache parent")).expect("create cache parent");
        std::fs::write(&path, b"not json").expect("write corrupt cache");

        assert_eq!(
            options
                .read_parse_entry(&root, &key)
                .expect("read corrupt cache"),
            None
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
