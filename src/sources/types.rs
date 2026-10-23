//! Source file discovery types.

use std::collections::BTreeSet;

use crate::cache::CacheKeyHasher;
use crate::discovery::ProjectRoot;

use super::warnings::SourcesWarning;

/// Detected project layout (§2, §8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProjectLayout {
    /// `src/<package>/` tree.
    Src,
    /// `<package>/` under a declared directory other than `src`
    /// (`lib/sqlalchemy/`).
    PackageRoot,
    /// `<package>/` at repository root.
    Flat,
    /// Could not infer src/flat; broad globs used.
    #[default]
    Unknown,
}

impl ProjectLayout {
    /// Stable identifier for reporters and `--explain` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Src => "src",
            Self::PackageRoot => "package-root",
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
    /// A Jupyter notebook (`.ipynb`) with Python code cells.
    Notebook,
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayoutInfo {
    /// Detected layout kind.
    pub layout: ProjectLayout,
    /// Root-relative directory holding `packages`: `""` for flat, `src` for
    /// src layout, or a declared directory such as `lib`.
    pub package_root: String,
    /// Package directory names (e.g. `acme` for `src/acme` or `acme/`).
    pub packages: Vec<String>,
    /// Root-level non-distribution packages (`tests/` with `__init__.py`)
    /// that import as `tests.*` but never count as the distributed package.
    pub local_packages: Vec<String>,
    /// Globs used when `config.project` was empty.
    pub inferred_globs: Vec<String>,
    /// Workspace members' own layouts. The root layout cannot name a module
    /// inside a member, so without them no import within a member resolves.
    pub members: Vec<MemberLayout>,
    /// Root-relative directories holding a discovered `__init__.py`. A file
    /// the layout names no module for takes its relative imports' package
    /// context from this chain, as the directory above it would on
    /// `sys.path` (#896).
    pub package_dirs: BTreeSet<String>,
}

/// Layout of one workspace member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberLayout {
    /// Root-relative member directory using `/` separators.
    pub path: String,
    /// Layout inferred from the member's own manifest and sources.
    pub layout: LayoutInfo,
}

impl LayoutInfo {
    /// The innermost member containing root-relative `path`, with the path
    /// relative to that member.
    #[must_use]
    pub fn member_for<'a>(&self, path: &'a str) -> Option<(&MemberLayout, &'a str)> {
        self.members
            .iter()
            .filter_map(|member| {
                let rest = path.strip_prefix(member.path.as_str())?.strip_prefix('/')?;
                Some((member, rest))
            })
            .max_by_key(|(member, _)| member.path.len())
    }

    /// Root-relative directory of `package` (`lib/sqlalchemy`, or `acme` flat).
    #[must_use]
    pub fn package_dir(&self, package: &str) -> String {
        if self.package_root.is_empty() {
            package.to_owned()
        } else {
            format!("{}/{package}", self.package_root)
        }
    }

    #[must_use]
    pub fn in_local_package(&self, path: &str) -> bool {
        self.local_packages.iter().any(|package| {
            path.strip_prefix(package.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        })
    }

    /// Stable hash of the layout inputs that cache keys depend on.
    ///
    /// Fields are hashed explicitly rather than through `Debug`, whose output
    /// carries no stability guarantee and allocates a string proportional to
    /// the package count. Sequence order is preserved because callers treat
    /// the first matching candidate as the winner, so two permutations must
    /// not share a cache entry.
    #[must_use]
    pub fn cache_key_hash(&self) -> String {
        let mut hasher = CacheKeyHasher::new();
        hasher.field_str(self.layout.as_str());
        hasher.field_str(&self.package_root);
        hash_str_list(&mut hasher, &self.packages);
        hash_str_list(&mut hasher, &self.local_packages);
        hash_str_list(&mut hasher, &self.inferred_globs);
        for member in &self.members {
            hasher.field_str(&member.path);
            hasher.field_str(&member.layout.cache_key_hash());
        }
        hash_str_list(&mut hasher, &self.package_dirs);
        hasher.finish()
    }
}

fn hash_str_list<'a>(
    hasher: &mut CacheKeyHasher,
    values: impl IntoIterator<Item = &'a String, IntoIter: ExactSizeIterator>,
) {
    let values = values.into_iter();
    hasher.field_u64(u64::try_from(values.len()).unwrap_or(u64::MAX));
    for value in values {
        hasher.field_str(value);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_local_package_needs_a_path_inside_the_package() {
        let layout = LayoutInfo {
            local_packages: vec!["tests".to_owned()],
            ..LayoutInfo::default()
        };
        assert!(layout.in_local_package("tests/test_a.py"));
        assert!(!layout.in_local_package("tests_extra/a.py"));
        assert!(!layout.in_local_package("tests"));
        assert!(!layout.in_local_package("src/tests/a.py"));
    }

    #[test]
    fn cache_key_hash_tells_package_dirs_apart() {
        let base = LayoutInfo::default();
        let with_dirs = LayoutInfo {
            package_dirs: BTreeSet::from(["tests/app".to_owned()]),
            ..LayoutInfo::default()
        };
        assert_ne!(base.cache_key_hash(), with_dirs.cache_key_hash());
    }
}
