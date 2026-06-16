//! Conservative cache policy types for Phase 2 warm-run support.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
        project_root.join(root_relative_directory(&self.directory))
    }

    /// Absolute path for a persisted parse cache entry.
    #[must_use]
    pub fn parse_entry_path(&self, project_root: &Path, key: &ParseCacheKey) -> PathBuf {
        self.directory_path(project_root).join(key.relative_path())
    }

    /// Absolute path for a persisted scan cache entry.
    #[must_use]
    pub fn scan_entry_path(&self, project_root: &Path, key: &ScanCacheKey) -> PathBuf {
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
        write_cache_bytes(&path, &bytes)
    }

    /// Read a persisted scan cache record.
    ///
    /// Corrupt JSON or key-mismatched entries are treated as misses.
    ///
    /// # Errors
    ///
    /// Returns an IO error when the cache file exists but cannot be read.
    pub fn read_scan_record(
        &self,
        project_root: &Path,
        key: &ScanCacheKey,
    ) -> io::Result<Option<ScanCacheRecord>> {
        if !self.enabled {
            return Ok(None);
        }
        let path = self.scan_entry_path(project_root, key);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let Ok(record) = serde_json::from_slice::<ScanCacheRecord>(&bytes) else {
            return Ok(None);
        };
        if record.key == *key && record.schema_version == SCAN_CACHE_SCHEMA_VERSION {
            Ok(Some(record))
        } else {
            Ok(None)
        }
    }

    /// Write a persisted scan cache record.
    ///
    /// # Errors
    ///
    /// Returns an IO error when the cache directory or file cannot be written.
    pub fn write_scan_record(
        &self,
        project_root: &Path,
        record: &ScanCacheRecord,
    ) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let path = self.scan_entry_path(project_root, &record.key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
        write_cache_bytes(&path, &bytes)
    }

    /// Read and deserialize the payload from a persisted scan cache record.
    ///
    /// Corrupt JSON, key mismatch, missing payload, or incompatible payload shape
    /// are treated as cache misses.
    ///
    /// # Errors
    ///
    /// Returns an IO error when the cache file exists but cannot be read.
    pub fn read_scan_payload<T>(
        &self,
        project_root: &Path,
        key: &ScanCacheKey,
    ) -> io::Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        let Some(record) = self.read_scan_record(project_root, key)? else {
            return Ok(None);
        };
        let Some(payload) = record.payload else {
            return Ok(None);
        };
        Ok(serde_json::from_value(payload).ok())
    }

    /// Serialize and write a scan cache payload.
    ///
    /// # Errors
    ///
    /// Returns an IO error when serialization or cache writing fails.
    pub fn write_scan_payload<T>(
        &self,
        project_root: &Path,
        key: ScanCacheKey,
        payload: &T,
    ) -> io::Result<()>
    where
        T: Serialize,
    {
        let payload = serde_json::to_value(payload).map_err(io::Error::other)?;
        let record = ScanCacheRecord {
            key,
            schema_version: SCAN_CACHE_SCHEMA_VERSION.to_owned(),
            payload: Some(payload),
        };
        self.write_scan_record(project_root, &record)
    }
}

fn root_relative_directory(directory: &Path) -> PathBuf {
    let mut relative = PathBuf::new();
    for component in directory.components() {
        if let Component::Normal(part) = component {
            relative.push(part);
        }
    }
    relative
}

fn write_cache_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing cache entry parent"))?;
    let mut temp = tempfile::Builder::new()
        .prefix(".chokkin-cache-")
        .tempfile_in(parent)?;
    temp.write_all(bytes)?;
    // The atomic rename below keeps readers from ever seeing a torn entry. We
    // deliberately skip `sync_all()` here: the parse/scan cache is fully
    // regenerable, the read path treats any corrupt entry as a miss, and a
    // per-entry fsync dominates cold-cache runs (one fsync per parsed module
    // makes the first analysis of a large project an order of magnitude slower
    // than `--no-cache`). Durability across power loss is not worth that cost.
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Stable inputs shared by cache units.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
        // Open once and read metadata from the handle. A separate
        // `fs::metadata` call would add a redundant `stat` syscall per file,
        // which dominates warm-cache fingerprinting over many small sources.
        let mut file = std::fs::File::open(path)?;
        let metadata = file.metadata()?;
        let capacity = usize::try_from(metadata.len()).unwrap_or(0);
        let mut bytes = Vec::with_capacity(capacity);
        std::io::Read::read_to_end(&mut file, &mut bytes)?;
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

fn manifest_candidate_fingerprints(root: &Path) -> io::Result<Vec<SourceFingerprint>> {
    let mut paths = Vec::new();
    for filename in [
        "pyproject.toml",
        "setup.cfg",
        "setup.py",
        "requirements.txt",
        "requirements-dev.txt",
        "dev-requirements.txt",
        "requirements-docs.txt",
        "requirements-tests.txt",
        "requirements-test.txt",
        "uv.lock",
    ] {
        let path = root.join(filename);
        if path.is_file() {
            paths.push(path);
        }
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
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScanInputFingerprints {
    /// Config-layer files.
    pub config: Vec<SourceFingerprint>,
    /// Manifest-layer files.
    pub manifest: Vec<SourceFingerprint>,
}

/// Key for cacheable config/manifest scan results.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScanCacheKey {
    /// Shared key context.
    pub context: CacheKeyContext,
    /// Files that affect scan output.
    pub inputs: ScanInputFingerprints,
}

impl ScanCacheKey {
    /// Stable filename for the cached scan result.
    #[must_use]
    pub fn file_name(&self) -> String {
        let mut input = format!(
            "{}\n{}\n{}\n{}\n{}",
            self.context.chokkin_version,
            self.context.config_hash,
            self.context.manifest_hash,
            self.context.target_version,
            self.context.unit_version
        );
        append_fingerprints(&mut input, "config", &self.inputs.config);
        append_fingerprints(&mut input, "manifest", &self.inputs.manifest);
        format!("{}.json", stable_hex_hash(input.as_bytes()))
    }

    /// Project-root-relative path for a persisted scan cache entry.
    #[must_use]
    pub fn relative_path(&self) -> PathBuf {
        PathBuf::from("scan").join(self.file_name())
    }
}

fn append_fingerprints(out: &mut String, label: &str, fingerprints: &[SourceFingerprint]) {
    out.push('\n');
    out.push_str(label);
    for fingerprint in fingerprints {
        use std::fmt::Write as _;
        let _ = write!(
            out,
            "\n{}\t{}\t{}\t{}",
            fingerprint.path,
            fingerprint.size,
            fingerprint
                .modified_ns
                .map_or_else(String::new, |value| value.to_string()),
            fingerprint.content_hash
        );
    }
}

/// Schema version for scan cache records.
pub const SCAN_CACHE_SCHEMA_VERSION: &str = "scan-record-v1";

/// JSON-safe envelope for config/manifest scan cache records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanCacheRecord {
    /// Key used to validate this record.
    pub key: ScanCacheKey,
    /// Schema version for the scan cache payload.
    pub schema_version: String,
    /// Serialized scan result payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
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

    /// Collect fingerprints for manifest extraction before extraction has run.
    ///
    /// # Errors
    ///
    /// Returns an IO error when a candidate manifest input exists but cannot be read.
    pub fn collect_manifest_candidates(root: &Path, config: &ConfigSources) -> io::Result<Self> {
        Ok(Self {
            config: config_input_fingerprints(root, config)?,
            manifest: manifest_candidate_fingerprints(root)?,
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

// Ordering compares `source` before `context`. The `BTreeMap` parse cache holds
// many keys that share one identical `context` within a run, so comparing the
// context first would scan five equal strings at every tree level before
// reaching the discriminating `source.path`. Leading with `source` lets each
// comparison short-circuit on the path, which keeps warm-cache lookups close to
// linear instead of paying that fixed string-compare cost per tree level.
impl Ord for ParseCacheKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.source
            .cmp(&other.source)
            .then_with(|| self.context.cmp(&other.context))
    }
}

impl PartialOrd for ParseCacheKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
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
        let root =
            std::env::temp_dir().join(format!("chokkin-cache-test-{name}-{}", std::process::id()));
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
    fn cache_directory_path_stays_under_project_root() {
        let options = CacheOptions {
            enabled: true,
            directory: PathBuf::from("../outside/cache"),
        };

        let path = options.directory_path(Path::new("/repo/project"));

        assert_eq!(path, PathBuf::from("/repo/project/outside/cache"));
    }

    #[test]
    fn absolute_cache_directory_is_made_project_relative() {
        let options = CacheOptions {
            enabled: true,
            directory: PathBuf::from("/tmp/chokkin-cache"),
        };

        let path = options.directory_path(Path::new("/repo/project"));

        assert_eq!(path, PathBuf::from("/repo/project/tmp/chokkin-cache"));
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
        assert_eq!(
            path.extension().and_then(std::ffi::OsStr::to_str),
            Some("json")
        );
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
    fn parse_entry_replaces_existing_disk_value() {
        let root = temp_cache_test_dir("disk-replace");
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
        let first = ParsedModule::empty("src/first.py".to_owned());
        let second = ParsedModule::empty("src/second.py".to_owned());
        let options = CacheOptions::default();

        options
            .write_parse_entry(&root, &key, &first)
            .expect("write first parse cache");
        options
            .write_parse_entry(&root, &key, &second)
            .expect("replace parse cache");
        let restored = options
            .read_parse_entry(&root, &key)
            .expect("read parse cache")
            .expect("cache hit");

        assert_eq!(restored, second);
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

    #[test]
    fn scan_entry_path_uses_stable_hashed_filename() {
        let key = ScanCacheKey {
            context: CacheKeyContext {
                chokkin_version: "test".to_owned(),
                config_hash: "config".to_owned(),
                manifest_hash: "manifest".to_owned(),
                target_version: "py311".to_owned(),
                unit_version: "scan-v1".to_owned(),
            },
            inputs: ScanInputFingerprints {
                config: vec![SourceFingerprint {
                    path: "pyproject.toml".to_owned(),
                    size: 1,
                    modified_ns: Some(1),
                    content_hash: "hash".to_owned(),
                }],
                manifest: Vec::new(),
            },
        };

        let path = CacheOptions::default().scan_entry_path(Path::new("/repo"), &key);

        assert!(path.starts_with("/repo/.chokkin/cache/scan"));
        assert_eq!(
            path.extension().and_then(std::ffi::OsStr::to_str),
            Some("json")
        );
    }

    #[test]
    fn scan_cache_record_is_json_safe() {
        let record = ScanCacheRecord {
            key: ScanCacheKey {
                context: CacheKeyContext {
                    chokkin_version: "test".to_owned(),
                    config_hash: "config".to_owned(),
                    manifest_hash: "manifest".to_owned(),
                    target_version: "py311".to_owned(),
                    unit_version: "scan-v1".to_owned(),
                },
                inputs: ScanInputFingerprints::default(),
            },
            schema_version: SCAN_CACHE_SCHEMA_VERSION.to_owned(),
            payload: None,
        };

        let bytes = serde_json::to_vec(&record).expect("serialize scan record");
        let restored: ScanCacheRecord =
            serde_json::from_slice(&bytes).expect("deserialize scan record");

        assert_eq!(restored, record);
    }

    #[test]
    fn scan_cache_record_without_payload_deserializes() {
        let json = r#"{
            "key": {
                "context": {
                    "chokkin_version": "test",
                    "config_hash": "config",
                    "manifest_hash": "manifest",
                    "target_version": "py311",
                    "unit_version": "scan-v1"
                },
                "inputs": {
                    "config": [],
                    "manifest": []
                }
            },
            "schema_version": "scan-record-v1"
        }"#;

        let restored: ScanCacheRecord =
            serde_json::from_str(json).expect("deserialize legacy scan record");

        assert_eq!(restored.payload, None);
    }

    #[test]
    fn scan_record_round_trips_to_disk() {
        let root = temp_cache_test_dir("scan-disk");
        let record = ScanCacheRecord {
            key: ScanCacheKey {
                context: CacheKeyContext {
                    chokkin_version: "test".to_owned(),
                    config_hash: "config".to_owned(),
                    manifest_hash: "manifest".to_owned(),
                    target_version: "py311".to_owned(),
                    unit_version: "scan-v1".to_owned(),
                },
                inputs: ScanInputFingerprints::default(),
            },
            schema_version: SCAN_CACHE_SCHEMA_VERSION.to_owned(),
            payload: None,
        };
        let options = CacheOptions::default();

        options
            .write_scan_record(&root, &record)
            .expect("write scan cache");
        let restored = options
            .read_scan_record(&root, &record.key)
            .expect("read scan cache")
            .expect("cache hit");

        assert_eq!(restored, record);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_scan_record_is_cache_miss() {
        let root = temp_cache_test_dir("scan-corrupt");
        let key = ScanCacheKey {
            context: CacheKeyContext {
                chokkin_version: "test".to_owned(),
                config_hash: "config".to_owned(),
                manifest_hash: "manifest".to_owned(),
                target_version: "py311".to_owned(),
                unit_version: "scan-v1".to_owned(),
            },
            inputs: ScanInputFingerprints::default(),
        };
        let options = CacheOptions::default();
        let path = options.scan_entry_path(&root, &key);
        std::fs::create_dir_all(path.parent().expect("cache parent")).expect("create cache parent");
        std::fs::write(&path, b"not json").expect("write corrupt cache");

        assert_eq!(
            options
                .read_scan_record(&root, &key)
                .expect("read corrupt cache"),
            None
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn mismatched_scan_record_key_is_cache_miss() {
        let root = temp_cache_test_dir("scan-mismatch");
        let expected = ScanCacheKey {
            context: CacheKeyContext {
                chokkin_version: "test".to_owned(),
                config_hash: "config".to_owned(),
                manifest_hash: "manifest".to_owned(),
                target_version: "py311".to_owned(),
                unit_version: "scan-v1".to_owned(),
            },
            inputs: ScanInputFingerprints::default(),
        };
        let mut stored = expected.clone();
        stored.context.config_hash = "other-config".to_owned();
        let path = CacheOptions::default().scan_entry_path(&root, &expected);
        std::fs::create_dir_all(path.parent().expect("cache parent")).expect("create cache parent");
        let record = ScanCacheRecord {
            key: stored,
            schema_version: SCAN_CACHE_SCHEMA_VERSION.to_owned(),
            payload: None,
        };
        std::fs::write(
            &path,
            serde_json::to_vec(&record).expect("serialize record"),
        )
        .expect("write mismatched cache");

        assert_eq!(
            CacheOptions::default()
                .read_scan_record(&root, &expected)
                .expect("read mismatched cache"),
            None
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn mismatched_scan_record_schema_is_cache_miss() {
        let root = temp_cache_test_dir("scan-schema-mismatch");
        let key = ScanCacheKey {
            context: CacheKeyContext {
                chokkin_version: "test".to_owned(),
                config_hash: "config".to_owned(),
                manifest_hash: "manifest".to_owned(),
                target_version: "py311".to_owned(),
                unit_version: "scan-v1".to_owned(),
            },
            inputs: ScanInputFingerprints::default(),
        };
        let path = CacheOptions::default().scan_entry_path(&root, &key);
        std::fs::create_dir_all(path.parent().expect("cache parent")).expect("create cache parent");
        let record = ScanCacheRecord {
            key: key.clone(),
            schema_version: "scan-record-v0".to_owned(),
            payload: None,
        };
        std::fs::write(
            &path,
            serde_json::to_vec(&record).expect("serialize record"),
        )
        .expect("write schema-mismatched cache");

        assert_eq!(
            CacheOptions::default()
                .read_scan_record(&root, &key)
                .expect("read schema-mismatched cache"),
            None
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn scan_payload_round_trips_typed_value() {
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        struct Payload {
            config_files: Vec<String>,
            manifest_files: Vec<String>,
        }

        let root = temp_cache_test_dir("scan-payload");
        let key = ScanCacheKey {
            context: CacheKeyContext {
                chokkin_version: "test".to_owned(),
                config_hash: "config".to_owned(),
                manifest_hash: "manifest".to_owned(),
                target_version: "py311".to_owned(),
                unit_version: "scan-v1".to_owned(),
            },
            inputs: ScanInputFingerprints::default(),
        };
        let payload = Payload {
            config_files: vec!["pyproject.toml".to_owned()],
            manifest_files: vec!["requirements.txt".to_owned()],
        };
        let options = CacheOptions::default();

        options
            .write_scan_payload(&root, key.clone(), &payload)
            .expect("write scan payload");
        let restored: Payload = options
            .read_scan_payload(&root, &key)
            .expect("read scan payload")
            .expect("cache hit");

        assert_eq!(restored, payload);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn incompatible_scan_payload_is_cache_miss() {
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        struct Payload {
            required: String,
        }

        let root = temp_cache_test_dir("scan-payload-miss");
        let key = ScanCacheKey {
            context: CacheKeyContext {
                chokkin_version: "test".to_owned(),
                config_hash: "config".to_owned(),
                manifest_hash: "manifest".to_owned(),
                target_version: "py311".to_owned(),
                unit_version: "scan-v1".to_owned(),
            },
            inputs: ScanInputFingerprints::default(),
        };
        let record = ScanCacheRecord {
            key: key.clone(),
            schema_version: SCAN_CACHE_SCHEMA_VERSION.to_owned(),
            payload: Some(serde_json::json!({"other": "shape"})),
        };
        CacheOptions::default()
            .write_scan_record(&root, &record)
            .expect("write scan record");

        let restored: Option<Payload> = CacheOptions::default()
            .read_scan_payload(&root, &key)
            .expect("read incompatible payload");

        assert_eq!(restored, None);
        let _ = std::fs::remove_dir_all(root);
    }
}
