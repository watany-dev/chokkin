//! Library-mode public surface from wheel target configuration (R-05).

use std::collections::BTreeSet;

use globset::{Glob, GlobMatcher};

use crate::manifest::{PackageFind, WheelTargets};
use crate::path_util::join_rel;

use super::types::DiscoveredFile;

/// Discovered files a wheel ships, resolved from [`WheelTargets`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublicSurface {
    /// Root-relative paths of distributed files.
    pub files: BTreeSet<String>,
}

impl PublicSurface {
    /// Resolve `targets` against discovered files.
    ///
    /// Returns `None` without targets or when they match no file (e.g. a typo
    /// in `packages`); callers then treat every file as public, as before R-05.
    #[must_use]
    pub fn resolve(targets: Option<&WheelTargets>, files: &[DiscoveredFile]) -> Option<Self> {
        let targets = targets?;
        let paths: Vec<PathTarget> = targets
            .paths
            .iter()
            .map(String::as_str)
            .filter_map(PathTarget::new)
            .collect();
        let finds: Vec<FindTarget> = targets
            .find
            .iter()
            .map(|find| FindTarget::new(find, files))
            .collect();
        let files: BTreeSet<String> = files
            .iter()
            .map(|file| file.path.as_str())
            .filter(|path| {
                paths.iter().any(|target| target.matches(path))
                    || finds.iter().any(|target| target.matches(path))
            })
            .map(str::to_owned)
            .collect();
        (!files.is_empty()).then_some(Self { files })
    }

    /// Whether `path` (root-relative) is shipped in the wheel.
    #[must_use]
    pub fn contains(&self, path: &str) -> bool {
        self.files.contains(path)
    }
}

enum PathTarget {
    /// A package directory or module file; matches itself and everything below.
    Prefix(String),
    Glob(GlobMatcher),
}

impl PathTarget {
    fn new(path: &str) -> Option<Self> {
        if path.contains(['*', '?', '[', '{']) {
            // An invalid pattern ships nothing we can resolve, so it is ignored.
            return Glob::new(path)
                .ok()
                .map(|glob| Self::Glob(glob.compile_matcher()));
        }
        Some(Self::Prefix(path.to_owned()))
    }

    fn matches(&self, file: &str) -> bool {
        match self {
            Self::Prefix(prefix) => is_under(file, prefix),
            Self::Glob(matcher) => {
                matcher.is_match(file) || parent_dirs(file).any(|dir| matcher.is_match(dir))
            },
        }
    }
}

/// setuptools `find_packages` / `find_namespace_packages` over discovered files.
struct FindTarget {
    where_dirs: Vec<String>,
    include: Vec<GlobMatcher>,
    exclude: Vec<GlobMatcher>,
    /// Package directories holding an `__init__.py`; `None` for namespace search.
    regular_packages: Option<BTreeSet<String>>,
}

impl FindTarget {
    fn new(find: &PackageFind, files: &[DiscoveredFile]) -> Self {
        let compile = |patterns: &[String]| -> Vec<GlobMatcher> {
            patterns
                .iter()
                .filter_map(|pattern| Glob::new(pattern).ok())
                .map(|glob| glob.compile_matcher())
                .collect()
        };
        let regular_packages = (!find.namespaces).then(|| {
            files
                .iter()
                .filter_map(|file| file.path.strip_suffix("/__init__.py"))
                .map(str::to_owned)
                .collect()
        });
        Self {
            where_dirs: find.where_dirs.clone(),
            include: compile(&find.include),
            exclude: compile(&find.exclude),
            regular_packages,
        }
    }

    fn matches(&self, file: &str) -> bool {
        self.where_dirs
            .iter()
            .any(|base| self.matches_in(base, file))
    }

    /// `find` only returns packages, so a module directly in `where` is not shipped.
    fn matches_in(&self, base: &str, file: &str) -> bool {
        let Some((package_dir, _)) = relative_to(base, file).and_then(|rel| rel.rsplit_once('/'))
        else {
            return false;
        };
        let regular = self.regular_packages.as_ref().is_none_or(|regular| {
            std::iter::once(package_dir)
                .chain(parent_dirs(package_dir))
                .all(|dir| regular.contains(&join_rel(base, dir)))
        });
        let package = package_dir.replace('/', ".");
        regular
            && self.include.iter().any(|glob| glob.is_match(&package))
            && !self.exclude.iter().any(|glob| glob.is_match(&package))
    }
}

fn relative_to<'a>(base: &str, file: &'a str) -> Option<&'a str> {
    if base.is_empty() {
        return Some(file);
    }
    file.strip_prefix(base)
        .and_then(|rest| rest.strip_prefix('/'))
}

fn is_under(file: &str, prefix: &str) -> bool {
    prefix.is_empty()
        || file == prefix
        || file
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Proper ancestors of a `/`-separated path, nearest first.
fn parent_dirs(path: &str) -> impl Iterator<Item = &str> {
    path.char_indices()
        .rev()
        .filter(|(_, ch)| *ch == '/')
        .map(move |(index, _)| &path[..index])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::{FileContext, FileKind};

    fn files(paths: &[&str]) -> Vec<DiscoveredFile> {
        paths
            .iter()
            .map(|path| DiscoveredFile {
                path: (*path).to_owned(),
                kind: FileKind::Python,
                context: FileContext::Runtime,
            })
            .collect()
    }

    fn surface(targets: &WheelTargets, paths: &[&str]) -> Option<PublicSurface> {
        PublicSurface::resolve(Some(targets), &files(paths))
    }

    fn path_targets(paths: &[&str]) -> WheelTargets {
        WheelTargets {
            source: "test".to_owned(),
            paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            find: Vec::new(),
        }
    }

    #[test]
    fn prefix_respects_directory_boundaries() {
        let surface = surface(
            &path_targets(&["src/acme"]),
            &["src/acme/a.py", "src/acme_tools/b.py", "src/internal/c.py"],
        )
        .expect("surface");
        assert!(surface.contains("src/acme/a.py"));
        assert!(!surface.contains("src/acme_tools/b.py"));
        assert!(!surface.contains("src/internal/c.py"));
    }

    #[test]
    fn glob_matches_files_and_directories() {
        let surface = surface(
            &path_targets(&["tools/*.py", "src/pkg*"]),
            &["tools/x.py", "src/pkg_a/m.py", "other/y.py"],
        )
        .expect("surface");
        assert!(surface.contains("tools/x.py"));
        assert!(surface.contains("src/pkg_a/m.py"));
        assert!(!surface.contains("other/y.py"));
    }

    #[test]
    fn unmatched_targets_fall_back() {
        assert!(surface(&path_targets(&["src/missing"]), &["src/acme/a.py"]).is_none());
        assert!(PublicSurface::resolve(None, &files(&["src/acme/a.py"])).is_none());
    }

    #[test]
    fn find_applies_where_include_exclude() {
        let targets = WheelTargets {
            source: "test".to_owned(),
            paths: Vec::new(),
            find: vec![PackageFind {
                where_dirs: vec!["src".to_owned()],
                include: vec!["acme*".to_owned()],
                exclude: vec!["acme.tests*".to_owned()],
                namespaces: true,
            }],
        };
        let surface = surface(
            &targets,
            &[
                "src/acme/a.py",
                "src/acme/sub/b.py",
                "src/acme/tests/t.py",
                "src/other/c.py",
                "src/top.py",
            ],
        )
        .expect("surface");
        assert!(surface.contains("src/acme/a.py"));
        assert!(surface.contains("src/acme/sub/b.py"));
        assert!(!surface.contains("src/acme/tests/t.py"));
        assert!(!surface.contains("src/other/c.py"));
        assert!(!surface.contains("src/top.py"));
    }

    #[test]
    fn find_without_namespaces_requires_init_files() {
        let targets = WheelTargets {
            source: "test".to_owned(),
            paths: Vec::new(),
            find: vec![PackageFind {
                where_dirs: vec![String::new()],
                include: vec!["*".to_owned()],
                exclude: Vec::new(),
                namespaces: false,
            }],
        };
        let surface = surface(
            &targets,
            &[
                "acme/__init__.py",
                "acme/a.py",
                "acme/nsdir/b.py",
                "scripts/run.py",
            ],
        )
        .expect("surface");
        assert!(surface.contains("acme/a.py"));
        assert!(!surface.contains("acme/nsdir/b.py"));
        assert!(!surface.contains("scripts/run.py"));
    }
}
