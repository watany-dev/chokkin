//! Source file discovery types.

use crate::discovery::ProjectRoot;

use super::warnings::SourcesWarning;

/// Detected project layout (§2, §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectLayout {
    /// `src/<package>/` tree.
    Src,
    /// `<package>/` at repository root.
    Flat,
    /// Could not infer src/flat; broad globs used.
    Unknown,
}

impl ProjectLayout {
    /// Stable identifier for reporters and `--explain` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Src => "src",
            Self::Flat => "flat",
            Self::Unknown => "unknown",
        }
    }
}

/// File-side dependency context (§10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileContext {
    /// Runtime application or library code.
    Runtime,
    /// Test files and directories.
    Test,
    /// Documentation tree.
    Docs,
    /// Scripts and developer tooling files.
    Dev,
}

impl FileContext {
    /// Whether this context is analyzed when `production = true`.
    #[must_use]
    pub const fn is_included_in_production(self) -> bool {
        matches!(self, Self::Runtime)
    }
}

/// Kind of Python-related file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// A `.py` source file.
    Python,
    /// A `.pyi` stub file.
    Stub,
}

/// One discovered file under the project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredFile {
    /// Root-relative path using `/` separators.
    pub path: String,
    /// File kind.
    pub kind: FileKind,
    /// Assigned file context.
    pub context: FileContext,
}

/// Layout inference result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutInfo {
    /// Detected layout kind.
    pub layout: ProjectLayout,
    /// Package directory names (e.g. `acme` for `src/acme` or `acme/`).
    pub packages: Vec<String>,
    /// Globs used when `config.project` was empty.
    pub inferred_globs: Vec<String>,
}

/// Outcome of source file discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSources {
    /// Project root from discovery step 1.
    pub root: ProjectRoot,
    /// Layout inference (even when user supplied explicit `project` globs).
    pub layout: LayoutInfo,
    /// Globs that were effectively used (explicit or inferred).
    pub effective_globs: Vec<String>,
    /// Files selected for analysis, sorted by path.
    pub files: Vec<DiscoveredFile>,
    /// Non-fatal conditions.
    pub warnings: Vec<SourcesWarning>,
}

impl DiscoveredSources {
    /// Iterate `.py` files (excludes `.pyi` stubs).
    pub fn python_files(&self) -> impl Iterator<Item = &DiscoveredFile> {
        self.files
            .iter()
            .filter(|file| file.kind == FileKind::Python)
    }
}
