//! Parsed ignore files and glob sets shared across source walks.
//!
//! A workspace probe walks the root and then every member, and the member
//! trees sit inside the root tree. The `ignore` walker parses every
//! `.gitignore` and `.git/info/exclude` it meets, ancestors included, so each
//! file used to be parsed once per walk that reached it (#757). This cache
//! reads each directory's ignore files once, compiles each distinct file
//! content once (monorepos repeat one `.gitignore` per package), and answers
//! the walker's gitignore question itself, mirroring `ignore` 0.4's
//! `Ignore::matched` for the options [`super::walk`] sets (git ignore +
//! exclude, parents, no global or `.ignore` files, `require_git`).

use std::borrow::Cow;
use std::ffi::OsString;
use std::fs::{self, File, FileType};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use globset::GlobSet;
use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder, Glob};
use ignore::overrides::{Override, OverrideBuilder};
use rustc_hash::FxHashMap as HashMap;

use super::error::SourcesError;
use super::glob::{build_glob_set, check_braces};

/// Parsed ignore state shared by every [`super::discover_sources_cached`] call
/// that gets a clone of it.
#[derive(Clone, Default)]
pub(crate) struct SourcesCache(Arc<Inner>);

#[derive(Default)]
struct Inner {
    dirs: Mutex<HashMap<OsString, Arc<DirIgnore>>>,
    /// [`WalkIgnore`]'s directory chains, by walk root and the directory
    /// they end at.
    chains: Mutex<HashMap<PathBuf, HashMap<PathBuf, Arc<DirChain>>>>,
    /// Compiled rules by ignore file content, rooted at `.`.
    rules: Mutex<HashMap<Vec<u8>, Arc<Gitignore>>>,
    globs: Mutex<HashMap<Vec<String>, Arc<GlobSet>>>,
    overrides: Mutex<HashMap<Vec<String>, Arc<Override>>>,
}

/// One directory's ignore files, as `ignore` builds them for a walked
/// directory or an ancestor of the walk root.
pub(super) struct DirIgnore {
    /// `.gitignore` rules, rooted at the directory.
    gitignore: Rules,
    /// `.git/info/exclude` rules, rooted at the directory.
    exclude: Rules,
    /// `.git` or `.jj` exists here.
    has_git: bool,
    /// Errors `ignore` attaches to the directory entry.
    errors: Vec<ignore::Error>,
    /// `.gitignore` as [`super::walk::root_gitignore_warning`] sees it, and
    /// as the walk sees each directory below the root (#915).
    root_load: RootLoad,
}

/// Whether [`super::walk::root_gitignore_warning`] finds the `.gitignore`,
/// finds none, finds one with lines that do not parse, or warns that it is
/// unreadable.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RootLoad {
    Missing,
    /// Some line does not parse: the walk reports it with the directory's
    /// errors (#903).
    Unparsable,
    /// Reading failed, which `ignore` drops without an error. A symlink
    /// whose target is missing counts: `ignore` skips it as if absent.
    Unreadable,
    Loaded,
}

/// The ignore files of a directory and of each directory above it up to a
/// walk root, root first, with the directory spelled as the root joined with
/// each component, as [`DirIgnore`] keys it.
struct DirChain {
    path: PathBuf,
    dirs: Vec<Arc<DirIgnore>>,
}

/// The walk root's ancestors, closest first, and the canonical root they
/// match against.
pub(super) struct Ancestors {
    base: PathBuf,
    dirs: Vec<Arc<DirIgnore>>,
}

impl SourcesCache {
    /// Glob set for `patterns`, built once per distinct list.
    pub(super) fn glob_set(&self, patterns: &[String]) -> Result<Arc<GlobSet>, SourcesError> {
        if let Some(set) = lock(&self.0.globs).get(patterns) {
            return Ok(Arc::clone(set));
        }
        let set = Arc::new(build_glob_set(patterns)?);
        lock(&self.0.globs).insert(patterns.to_vec(), Arc::clone(&set));
        Ok(set)
    }

    /// Exclude overrides rooted at `.`, matched against root-relative paths.
    pub(super) fn exclude_overrides(
        &self,
        patterns: &[String],
    ) -> Result<Arc<Override>, SourcesError> {
        if let Some(overrides) = lock(&self.0.overrides).get(patterns) {
            return Ok(Arc::clone(overrides));
        }
        let mut builder = OverrideBuilder::new(".");
        for pattern in patterns {
            let invalid = |reason| SourcesError::InvalidGlob {
                pattern: pattern.clone(),
                reason,
            };
            check_braces(pattern).map_err(invalid)?;
            builder
                .add(&format!("!{pattern}"))
                .map_err(|error| invalid(error.to_string()))?;
        }
        let overrides = Arc::new(builder.build().map_err(|error| SourcesError::InvalidGlob {
            pattern: String::new(),
            reason: error.to_string(),
        })?);
        lock(&self.0.overrides).insert(patterns.to_vec(), Arc::clone(&overrides));
        Ok(overrides)
    }

    /// Ignore files of `dir`, keyed by its exact spelling: `Gitignore`
    /// strips its root from candidates byte by byte.
    pub(super) fn dir(&self, dir: &Path) -> Arc<DirIgnore> {
        if let Some(found) = lock(&self.0.dirs).get(dir.as_os_str()) {
            return Arc::clone(found);
        }
        // Parse outside the lock; a racing walk computes the same value.
        let parsed = Arc::new(DirIgnore::read(dir, &self.0.rules));
        Arc::clone(
            lock(&self.0.dirs)
                .entry(dir.as_os_str().to_owned())
                .or_insert(parsed),
        )
    }

    /// Ancestors of `root`, as `ignore`'s `add_parents` reads them. `None`
    /// when it reads none: `root` is not a directory or does not
    /// canonicalize.
    pub(super) fn ancestors(&self, root: &Path) -> Option<Ancestors> {
        if !root.is_dir() {
            return None;
        }
        let base = root.canonicalize().ok()?;
        let dirs = base.ancestors().skip(1).map(|dir| self.dir(dir)).collect();
        Some(Ancestors { base, dirs })
    }

    /// The ignore files from `root` down to `dir`, `None` when `dir` is not
    /// under `root`. Memoized per directory: the member scan asks for every
    /// entry of a parallel walk, and rebuilding the chain from the root each
    /// time cost a locked lookup per path component (#1087).
    fn chain(&self, root: &Path, dir: &Path) -> Option<Arc<DirChain>> {
        if let Some(found) = lock(&self.0.chains)
            .get(root)
            .and_then(|chains| chains.get(dir))
        {
            return Some(Arc::clone(found));
        }
        let below = dir.strip_prefix(root).ok()?;
        let chain = match below.components().next_back() {
            None => DirChain {
                path: root.to_path_buf(),
                dirs: vec![self.dir(root)],
            },
            Some(last) => {
                let parent = self.chain(root, dir.parent()?)?;
                let path = parent.path.join(last);
                let mut dirs = parent.dirs.clone();
                dirs.push(self.dir(&path));
                DirChain { path, dirs }
            },
        };
        Some(Arc::clone(
            lock(&self.0.chains)
                .entry(root.to_path_buf())
                .or_default()
                .entry(dir.to_path_buf())
                .or_insert_with(|| Arc::new(chain)),
        ))
    }

    /// Directories whose ignore files have been read so far.
    #[cfg(test)]
    pub(super) fn parsed_dirs(&self) -> usize {
        lock(&self.0.dirs).len()
    }

    /// Distinct ignore file contents compiled so far.
    #[cfg(test)]
    pub(super) fn compiled_rules(&self) -> usize {
        lock(&self.0.rules).len()
    }
}

/// The walk's gitignore decision ([`gitignore_skips`]) for any path below
/// one walk root, for a walk that does not track its directory chain itself:
/// the workspace member scan, and the symlinks the source walk names.
pub(crate) struct WalkIgnore {
    cache: SourcesCache,
    root: PathBuf,
    ancestors: Option<Ancestors>,
}

impl WalkIgnore {
    pub(crate) fn new(cache: &SourcesCache, root: &Path) -> Self {
        Self {
            cache: cache.clone(),
            root: root.to_path_buf(),
            ancestors: cache.ancestors(root),
        }
    }

    /// Whether the walk drops `path`, a path below the root as the walker
    /// spells it.
    pub(crate) fn skips(&self, path: &Path, is_dir: bool) -> bool {
        let Some(chain) = path
            .parent()
            .and_then(|parent| self.cache.chain(&self.root, parent))
        else {
            return false;
        };
        gitignore_skips(
            &chain.dirs,
            self.ancestors.as_ref(),
            &self.root,
            path,
            is_dir,
        )
    }
}

impl Ancestors {
    /// Errors `ignore` reports before the walk root, outermost first.
    pub(super) fn errors(&self) -> impl Iterator<Item = &ignore::Error> {
        self.dirs.iter().rev().flat_map(|dir| dir.errors.iter())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl DirIgnore {
    fn read(dir: &Path, shared: &Mutex<HashMap<Vec<u8>, Arc<Gitignore>>>) -> Self {
        let git_type = dir
            .join(".git")
            .metadata()
            .ok()
            .map(|meta| meta.file_type());
        let has_git = git_type.is_some() || dir.join(".jj").exists();
        let mut errors = Vec::new();

        let gitignore_path = dir.join(".gitignore");
        let (gitignore, clean) = Rules::load(dir, &gitignore_path, shared, &mut errors);
        let root_load = if dangling(&gitignore_path) {
            RootLoad::Unreadable
        } else if !gitignore_path.is_file() {
            RootLoad::Missing
        } else if clean {
            RootLoad::Loaded
        } else if errors.is_empty() {
            RootLoad::Unreadable
        } else {
            RootLoad::Unparsable
        };

        let exclude = match resolve_git_commondir(dir, git_type) {
            Ok(git_dir) => Rules::load(dir, &git_dir.join("info/exclude"), shared, &mut errors).0,
            Err(error) => {
                errors.extend(error);
                Rules::None
            },
        };

        Self {
            gitignore,
            exclude,
            has_git,
            errors,
            root_load,
        }
    }

    /// Errors `ignore` attaches to the directory entry.
    pub(super) fn errors(&self) -> &[ignore::Error] {
        &self.errors
    }

    pub(super) const fn root_load(&self) -> RootLoad {
        self.root_load
    }
}

/// One ignore file's rules, as `ignore` would build them for a directory.
enum Rules {
    /// No rules: the file is missing, or `ignore` would build an empty set.
    None,
    /// A cleanly parsed file, compiled once per content with root `.`.
    /// `root` is the directory as `GitignoreBuilder::new` stores it, applied
    /// by [`strip`] the way `Gitignore::strip` would.
    Shared {
        root: PathBuf,
        gitignore: Arc<Gitignore>,
    },
    /// A file that failed to read or parse, built per directory so its
    /// errors and partial rules stay exactly `ignore`'s.
    Own(Gitignore),
}

impl Rules {
    /// `ignore`'s `create_gitignore` for the single file `path` of `dir`,
    /// pushing its errors onto `errors`; `false` as [`create_gitignore`].
    fn load(
        dir: &Path,
        path: &Path,
        shared: &Mutex<HashMap<Vec<u8>, Arc<Gitignore>>>,
        errors: &mut Vec<ignore::Error>,
    ) -> (Self, bool) {
        if !(cfg!(windows) || path.exists()) {
            return (Self::None, true);
        }
        let Ok(content) = fs::read(path) else {
            let (gitignore, clean) = create_gitignore(dir, path, errors);
            return (Self::Own(gitignore), clean);
        };
        let root = strip_prefix(Path::new("./"), dir)
            .unwrap_or(dir)
            .to_path_buf();
        if let Some(gitignore) = lock(shared).get(&content) {
            let gitignore = Arc::clone(gitignore);
            return (Self::Shared { root, gitignore }, true);
        }
        let Some(gitignore) = compile(&content) else {
            let (gitignore, clean) = create_gitignore(dir, path, errors);
            return (Self::Own(gitignore), clean);
        };
        let gitignore = Arc::clone(
            lock(shared)
                .entry(content)
                .or_insert_with(|| Arc::new(gitignore)),
        );
        (Self::Shared { root, gitignore }, true)
    }

    fn matched<'a>(&'a self, path: &Path, is_dir: bool) -> Match<&'a Glob> {
        match self {
            Self::None => Match::None,
            Self::Shared { root, gitignore } => gitignore.matched(strip(root, path), is_dir),
            Self::Own(gitignore) => gitignore.matched(path, is_dir),
        }
    }
}

/// `GitignoreBuilder::add` over `content` with root `.`, `None` when any
/// line fails: such files take the per-directory path for exact errors.
fn compile(content: &[u8]) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(".");
    for (index, line) in content.lines().enumerate() {
        let line = line.ok()?;
        let line = if index == 0 {
            line.trim_start_matches('\u{feff}')
        } else {
            &line
        };
        builder.add_line(None, &literal_braces(line)).ok()?;
    }
    builder.build().ok()
}

/// `line` with each `{` and `}` that `globset` would read as alternation
/// escaped: git's wildmatch has no brace expansion, so `*.{py,pyc}` names a
/// file literally called that and `{}` matches nothing (#1149). Escapes and
/// character classes are copied as `globset` tokenizes them, so every other
/// character keeps the meaning `ignore` gives it.
fn literal_braces(line: &str) -> Cow<'_, str> {
    if !line.contains(['{', '}']) {
        return Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len() + 2);
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push(c);
                out.extend(chars.next());
            },
            '{' | '}' => {
                out.push('\\');
                out.push(c);
            },
            '[' => {
                out.push(c);
                // `ignore` allows unclosed classes: that `[` is a literal,
                // and so is every later one, as no `]` follows them either.
                if let Some(close) = class_close(chars.as_str()) {
                    out.push_str(&chars.as_str()[..close]);
                    chars = chars.as_str()[close..].chars();
                }
            },
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Byte offset in `rest`, the text after a class's `[`, of the `]` that
/// closes it, as `globset`'s `parse_class` reads it: an optional `!` or `^`,
/// a leading `]` taken literally, no escapes. `None` when it never closes.
fn class_close(rest: &str) -> Option<usize> {
    let skipped = usize::from(rest.starts_with(['!', '^']));
    let body = &rest[skipped..];
    let first = body.strip_prefix(']').map_or(0, |_| 1);
    let close = body[first..].find(']')?;
    Some(skipped + first + close)
}

/// `GitignoreBuilder::add` for `path`, with each line through
/// [`literal_braces`] and parse errors naming the line as written.
fn add_file(builder: &mut GitignoreBuilder, path: &Path) -> Option<ignore::Error> {
    let tagged = |err: ignore::Error, line: u64| ignore::Error::WithPath {
        path: path.to_path_buf(),
        err: Box::new(ignore::Error::WithLineNumber {
            line,
            err: Box::new(err),
        }),
    };
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) => {
            return Some(ignore::Error::WithPath {
                path: path.to_path_buf(),
                err: Box::new(ignore::Error::Io(err)),
            });
        },
    };
    let mut errors = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let lineno = index as u64 + 1;
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                errors.push(tagged(ignore::Error::Io(err), lineno));
                break;
            },
        };
        let line = if index == 0 {
            line.trim_start_matches('\u{feff}')
        } else {
            &line
        };
        if let Err(err) = builder.add_line(Some(path.to_path_buf()), &literal_braces(line)) {
            let err = match err {
                ignore::Error::Glob { glob, err } => ignore::Error::Glob {
                    glob: glob.map(|_| {
                        if line.ends_with("\\ ") {
                            line
                        } else {
                            line.trim_end()
                        }
                        .to_owned()
                    }),
                    err,
                },
                err => err,
            };
            errors.push(tagged(err, lineno));
        }
    }
    match errors.len() {
        0 => None,
        1 => errors.pop(),
        _ => Some(ignore::Error::Partial(errors)),
    }
}

/// `Gitignore::strip` for a matcher rooted at `root`. The shared matcher,
/// rooted at `.`, then only strips a leading `./`, which no walked path
/// keeps once the directory prefix is gone.
fn strip<'a>(root: &Path, path: &'a Path) -> &'a Path {
    let mut path = strip_prefix(Path::new("./"), path).unwrap_or(path);
    if root != Path::new(".")
        && !is_file_name(path)
        && let Some(rest) = strip_prefix(root, path)
    {
        path = strip_prefix(Path::new("/"), rest).unwrap_or(rest);
    }
    path
}

/// `ignore`'s `pathutil::strip_prefix`: byte-wise on Unix. One function
/// for both platforms, so mutation tests on Unix reach its signature.
fn strip_prefix<'a>(prefix: &Path, path: &'a Path) -> Option<&'a Path> {
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let rest = path
            .as_os_str()
            .as_bytes()
            .strip_prefix(prefix.as_os_str().as_bytes())?;
        Some(Path::new(OsStr::from_bytes(rest)))
    }
    #[cfg(not(unix))]
    {
        path.strip_prefix(prefix).ok()
    }
}

/// `ignore`'s `pathutil::is_file_name`.
fn is_file_name(path: &Path) -> bool {
    if cfg!(unix) {
        !path.as_os_str().as_encoded_bytes().contains(&b'/')
    } else {
        path.parent()
            .is_some_and(|parent| parent.as_os_str().is_empty())
    }
}

/// A symlink at `path` that does not lead to anything readable.
fn dangling(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink())
        && path.metadata().is_err()
}

/// `ignore`'s `create_gitignore` for a single file, pushing its errors onto
/// `errors`; `false` when the file failed to add or the set to build.
fn create_gitignore(dir: &Path, path: &Path, errors: &mut Vec<ignore::Error>) -> (Gitignore, bool) {
    let mut builder = GitignoreBuilder::new(dir);
    let mut added_cleanly = true;
    if (cfg!(windows) || path.exists())
        && let Some(error) = add_file(&mut builder, path)
    {
        added_cleanly = false;
        if !error.is_io() {
            errors.push(error);
        }
    }
    match builder.build() {
        Ok(gitignore) => (gitignore, added_cleanly),
        Err(error) => {
            errors.push(error);
            let empty = GitignoreBuilder::new(dir).build();
            (empty.unwrap_or_else(|_| Gitignore::empty()), false)
        },
    }
}

/// `ignore`'s `resolve_git_commondir`: the directory holding `info/exclude`.
fn resolve_git_commondir(
    dir: &Path,
    git_type: Option<FileType>,
) -> Result<PathBuf, Option<ignore::Error>> {
    let git_dir = dir.join(".git");
    if !git_type.is_some_and(|file_type| file_type.is_file()) {
        return Ok(git_dir);
    }
    let io_error = |error: std::io::Error, path: &Path| {
        Some(ignore::Error::WithPath {
            path: path.to_path_buf(),
            err: Box::new(ignore::Error::Io(error)),
        })
    };
    let dot_git_line = match File::open(&git_dir).map(|file| BufReader::new(file).lines().next()) {
        Err(error) | Ok(Some(Err(error))) => return Err(io_error(error, &git_dir)),
        Ok(Some(Ok(line))) => line,
        Ok(None) => return Err(None),
    };
    let Some(real_git_dir) = dot_git_line.strip_prefix("gitdir: ").map(PathBuf::from) else {
        return Err(None);
    };
    let commondir_file = real_git_dir.join("commondir");
    let commondir_line =
        match File::open(&commondir_file).map(|file| BufReader::new(file).lines().next()) {
            Err(_) | Ok(None) => return Err(None),
            Ok(Some(Ok(line))) => line,
            Ok(Some(Err(error))) => return Err(io_error(error, &commondir_file)),
        };
    if commondir_line.starts_with('.') {
        Ok(real_git_dir.join(commondir_line))
    } else {
        Ok(PathBuf::from(commondir_line))
    }
}

/// Whether the walk's gitignore rules drop `path`, following `ignore`'s
/// `matched_ignore`. `chain` holds the walked directories from the walk root
/// down to the entry's parent; `root` is the walk root as given.
pub(super) fn gitignore_skips(
    chain: &[Arc<DirIgnore>],
    ancestors: Option<&Ancestors>,
    root: &Path,
    path: &Path,
    is_dir: bool,
) -> bool {
    let path = strip_prefix(Path::new("./"), path).unwrap_or(path);
    let ancestor_dirs = ancestors.map_or(&[][..], |ancestors| &ancestors.dirs[..]);
    let any_git = chain.iter().chain(ancestor_dirs).any(|dir| dir.has_git);
    if !any_git {
        // No repository, so git reads nothing; a plain directory's
        // `.gitignore` files still apply from the walk root down, the
        // closest match winning as in git (#1220).
        return chain
            .iter()
            .rev()
            .map(|dir| dir.gitignore.matched(path, is_dir))
            .find(|matched| !matched.is_none())
            .is_some_and(|matched| matched.is_ignore());
    }
    let mut state = MatchState {
        gitignore: Match::None,
        exclude: Match::None,
        saw_git: false,
    };
    for dir in chain.iter().rev() {
        state.visit(dir, path, is_dir);
    }
    if let Some(ancestors) = ancestors {
        let relative = if root == Path::new(".") {
            path
        } else {
            let root = strip_prefix(Path::new("./"), root).unwrap_or(root);
            let rest = strip_prefix(root, path).unwrap_or(path);
            strip_prefix(Path::new("/"), rest).unwrap_or(rest)
        };
        let absolute = ancestors.base.join(relative);
        for dir in &ancestors.dirs {
            state.visit(dir, &absolute, is_dir);
        }
    }
    state.gitignore.or(state.exclude).is_ignore()
}

struct MatchState<'a> {
    gitignore: Match<&'a Glob>,
    exclude: Match<&'a Glob>,
    saw_git: bool,
}

impl<'a> MatchState<'a> {
    fn visit(&mut self, dir: &'a DirIgnore, path: &Path, is_dir: bool) {
        if !self.saw_git {
            if self.gitignore.is_none() {
                self.gitignore = dir.gitignore.matched(path, is_dir);
            }
            if self.exclude.is_none() {
                self.exclude = dir.exclude.matched(path, is_dir);
            }
        }
        self.saw_git |= dir.has_git;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chains are memoized per walk root: the same directory under
    /// another root reads only the `.gitignore` files below that root.
    #[test]
    fn walk_ignore_reads_from_each_walk_root_down() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outer = temp.path();
        let inner = outer.join("a");
        fs::create_dir_all(inner.join("b")).expect("mkdir");
        fs::write(outer.join(".gitignore"), "y\n").expect("write");
        fs::write(inner.join(".gitignore"), "x\n").expect("write");
        let cache = SourcesCache::default();
        for _ in 0..2 {
            let from_outer = WalkIgnore::new(&cache, outer);
            let from_inner = WalkIgnore::new(&cache, &inner);
            assert!(from_outer.skips(&inner.join("b/y"), false));
            assert!(from_outer.skips(&inner.join("b/x"), false));
            assert!(!from_inner.skips(&inner.join("b/y"), false));
            assert!(from_inner.skips(&inner.join("b/x"), false));
            assert!(!from_inner.skips(&outer.join("y"), false));
        }
    }

    #[test]
    fn literal_braces_escapes_only_alternation() {
        for (line, expected) in [
            ("*.py", "*.py"),
            ("{}", "\\{\\}"),
            ("*.{py,pyc}", "*.\\{py,pyc\\}"),
            ("\\{a}", "\\{a\\}"),
            ("a\\\\{b}", "a\\\\\\{b\\}"),
            ("[{]x}", "[{]x\\}"),
            ("[!{}]", "[!{}]"),
            ("[]{]}", "[]{]\\}"),
            ("[{x", "[\\{x"),
            ("[{x[}]", "[{x[}]"),
            ("[!]{]x}", "[!]{]x\\}"),
            ("[!{]}", "[!{]\\}"),
            ("[x{", "[x\\{"),
            ("{\\", "\\{\\"),
        ] {
            assert_eq!(literal_braces(line), expected, "line {line}");
        }
    }

    /// `add_file` is `GitignoreBuilder::add`: no error for a clean file, a
    /// single error unwrapped, the BOM stripped from the first line only.
    #[test]
    fn add_file_matches_gitignore_builder_add() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join(".gitignore");
        fs::write(&path, "\u{feff}a\n\u{feff}b\n{c}\n").expect("write");
        let mut builder = GitignoreBuilder::new(temp.path());
        assert!(add_file(&mut builder, &path).is_none());
        let gitignore = builder.build().expect("build");
        assert!(gitignore.matched("a", false).is_ignore());
        assert!(gitignore.matched("\u{feff}b", false).is_ignore());
        assert!(gitignore.matched("{c}", false).is_ignore());
        assert!(gitignore.matched("b", false).is_none());

        fs::write(&path, "x\\\n").expect("write");
        let error = add_file(&mut GitignoreBuilder::new(temp.path()), &path).expect("error");
        assert!(matches!(error, ignore::Error::WithPath { .. }), "{error:?}");
    }

    /// Escaped braces leave no alternation for `globset` to recurse into.
    #[test]
    fn deeply_nested_braces_compile() {
        let line = format!("{}{}", "{".repeat(10_000), "}".repeat(10_000));
        assert!(compile(line.as_bytes()).is_some());
    }

    /// The per-directory path names the line as written in its errors.
    #[test]
    fn add_file_reports_the_original_line() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join(".gitignore");
        fs::write(&path, "ok\n{[z-a]}\n").expect("write");
        let mut builder = GitignoreBuilder::new(temp.path());
        let error = add_file(&mut builder, &path).expect("error");
        let message = error.to_string();
        assert!(message.contains("line 2"), "{message}");
        // The escaped line would read `\{[z-a]\}`.
        assert!(message.contains("'{[z-a]}'"), "{message}");
        let gitignore = builder.build().expect("build");
        assert!(gitignore.matched("ok", false).is_ignore());
    }

    /// A shared matcher with [`strip`] answers as `ignore`'s matcher rooted
    /// at the directory, including for bare names that merely start with it.
    #[test]
    fn shared_rules_strip_like_a_directory_rooted_gitignore() {
        let content = b"bar\n/top.py\nsub/\n";
        for root in ["pkg", "a/pkg"] {
            let mut builder = GitignoreBuilder::new(root);
            for line in content.lines() {
                builder.add_line(None, &line.expect("line")).expect("rule");
            }
            let own = builder.build().expect("gitignore");
            let shared = Rules::Shared {
                root: PathBuf::from(root),
                gitignore: Arc::new(compile(content).expect("compile")),
            };
            for path in [
                format!("{root}/bar"),
                format!("{root}/x/bar"),
                format!("{root}/top.py"),
                format!("{root}/x/top.py"),
                format!("./{root}/top.py"),
                format!("{root}/sub"),
                format!("{root}bar"),
                "pkgbar".to_owned(),
                "bar".to_owned(),
            ] {
                for is_dir in [false, true] {
                    let expected = own.matched(&path, is_dir);
                    let actual = shared.matched(Path::new(&path), is_dir);
                    assert_eq!(
                        (actual.is_ignore(), actual.is_whitelist()),
                        (expected.is_ignore(), expected.is_whitelist()),
                        "root {root}, path {path}, is_dir {is_dir}"
                    );
                }
            }
        }
    }
}
