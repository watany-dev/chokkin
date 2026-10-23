//! The walker `collect_files` replaced in #757, kept as a test oracle: it lets
//! `ignore` read every ignore file itself, once per walk.

use std::path::Path;

use globset::GlobSet;
use ignore::WalkBuilder;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::overrides::OverrideBuilder;

use crate::path_util::normalize_rel_path;

use super::context::assign_file_context;
use super::error::SourcesError;
use super::types::{DiscoveredFile, FileContext, FileKind};
use super::walk::{
    SkippedSymlinks, matches_source, nested_gitignore_unreadable, push_walk_error, should_prune_dir,
};
use super::warnings::SourcesWarning;

const GITIGNORE_PATH: &str = ".gitignore";

/// Options for [`collect_files_reference`].
pub(super) struct ReferenceOptions<'a> {
    pub root: &'a Path,
    pub project_matcher: &'a GlobSet,
    pub exclude_matcher: &'a GlobSet,
    pub test_matcher: &'a GlobSet,
    pub exclude_patterns: &'a [String],
    pub respect_gitignore: bool,
    pub gitignore: Option<&'a Gitignore>,
    pub production: bool,
}

/// Load `.gitignore` from the project root when present.
pub(super) fn load_gitignore_reference(root: &Path) -> (Option<Gitignore>, Option<SourcesWarning>) {
    let gitignore_path = root.join(".gitignore");
    if !gitignore_path.is_file() {
        let warning =
            gitignore_unreadable(&gitignore_path).then(|| SourcesWarning::GitignoreUnreadable {
                path: GITIGNORE_PATH.to_owned(),
            });
        return (None, warning);
    }

    // The walk reports lines that do not parse; `ignore` drops a read error.
    let mut builder = GitignoreBuilder::new(root);
    if let Some(error) = builder.add(&gitignore_path) {
        let warning = error.is_io().then(|| SourcesWarning::GitignoreUnreadable {
            path: GITIGNORE_PATH.to_owned(),
        });
        return (None, warning);
    }
    (builder.build().ok(), None)
}

/// A `.gitignore` that exists but whose lines `ignore` cannot read, or a
/// symlink that leads nowhere.
fn gitignore_unreadable(path: &Path) -> bool {
    if path.is_file() {
        return std::fs::read_to_string(path).is_err();
    }
    path.symlink_metadata().is_ok() && path.metadata().is_err()
}

fn build_exclude_overrides(
    root: &Path,
    patterns: &[String],
) -> Result<ignore::overrides::Override, SourcesError> {
    let mut builder = OverrideBuilder::new(root);
    for pattern in patterns {
        let ignore_glob = format!("!{pattern}");
        builder
            .add(&ignore_glob)
            .map_err(|error| SourcesError::InvalidGlob {
                pattern: pattern.clone(),
                reason: error.to_string(),
            })?;
    }
    builder.build().map_err(|error| SourcesError::InvalidGlob {
        pattern: String::new(),
        reason: error.to_string(),
    })
}

fn configure_walker(
    root: &Path,
    exclude_patterns: &[String],
    exclude_matcher: &GlobSet,
    respect_gitignore: bool,
) -> Result<WalkBuilder, SourcesError> {
    let overrides = build_exclude_overrides(root, exclude_patterns)?;
    let exclude_for_prune = exclude_matcher.clone();
    let root_for_filter = root.to_path_buf();

    let mut builder = WalkBuilder::new(root);
    builder
        .git_ignore(respect_gitignore)
        .git_global(false)
        .git_exclude(respect_gitignore)
        .ignore(false)
        .follow_links(false)
        .hidden(false)
        .overrides(overrides)
        .filter_entry(move |entry| {
            let path = entry.path();
            let Ok(rel) = path.strip_prefix(&root_for_filter) else {
                return true;
            };
            if entry
                .file_type()
                .is_some_and(|file_type| file_type.is_dir())
            {
                let rel_str = normalize_rel_path(rel);
                return !should_prune_dir(&rel_str, &exclude_for_prune);
            }
            true
        });
    Ok(builder)
}

/// Collect Python-related files under `root` with `ignore`'s own walker.
pub(super) fn collect_files_reference(
    options: &ReferenceOptions<'_>,
) -> Result<(Vec<DiscoveredFile>, Vec<SourcesWarning>), SourcesError> {
    let root = options.root.to_path_buf();
    let project_matcher = options.project_matcher;
    let exclude_matcher = options.exclude_matcher;
    let test_matcher = options.test_matcher;
    let production = options.production;
    let gitignore = options.gitignore;

    let builder = configure_walker(
        &root,
        options.exclude_patterns,
        exclude_matcher,
        options.respect_gitignore,
    )?;

    let mut files = Vec::new();
    let mut warnings = Vec::new();
    let mut symlinks = SkippedSymlinks::default();
    let gitignored = |rel: &Path, is_dir| {
        gitignore.is_some_and(|ignore| ignore.matched_path_or_any_parents(rel, is_dir).is_ignore())
    };

    for result in builder.build() {
        let entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                push_walk_error(&root, &error, &mut warnings);
                continue;
            },
        };

        if let Some(error) = entry.error() {
            push_walk_error(entry.path(), error, &mut warnings);
        }
        if options.respect_gitignore
            && entry.depth() > 0
            && entry
                .file_type()
                .is_some_and(|file_type| file_type.is_dir())
            && gitignore_unreadable(&entry.path().join(GITIGNORE_PATH))
        {
            warnings.extend(nested_gitignore_unreadable(&root, entry.path()));
        }

        let path = entry.path();
        symlinks.check(&root, &entry, exclude_matcher, &gitignored);
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }

        let kind = match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("py") => FileKind::Python,
            Some(ext) if ext.eq_ignore_ascii_case("pyi") => FileKind::Stub,
            Some(ext) if ext.eq_ignore_ascii_case("ipynb") => FileKind::Notebook,
            _ => continue,
        };

        let Ok(rel) = path.strip_prefix(&root) else {
            continue;
        };
        let rel_str = normalize_rel_path(rel);

        if !matches_source(project_matcher, &rel_str) || matches_source(exclude_matcher, &rel_str) {
            continue;
        }

        if gitignored(rel, false) {
            continue;
        }

        let context = if matches_source(test_matcher, &rel_str) {
            FileContext::Test
        } else {
            assign_file_context(&rel_str)
        };
        if production && !context.is_included_in_production() {
            continue;
        }

        files.push(DiscoveredFile {
            path: rel_str,
            kind,
            context,
        });
    }

    files.sort_by(|left, right| left.path.cmp(&right.path));
    symlinks.finish(&mut warnings);
    Ok((files, warnings))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use proptest::prelude::*;
    use tempfile::tempdir;

    use super::*;
    use crate::sources::cache::SourcesCache;
    use crate::sources::glob::{build_glob_set, effective_exclude};
    use crate::sources::walk::{CollectOptions, collect_files, load_gitignore};

    type Outcome = (Vec<DiscoveredFile>, Vec<SourcesWarning>);

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write file");
    }

    fn project_globs() -> GlobSet {
        build_glob_set(&["**/*.{py,pyi,ipynb}".to_owned()]).expect("project globs")
    }

    /// `discover_sources`' warnings order: the walk's, then the root
    /// `.gitignore`'s.
    fn cached(cache: &SourcesCache, root: &Path, custom: &[String], respect: bool) -> Outcome {
        let exclude_patterns = effective_exclude(custom);
        let exclude = cache.glob_set(&exclude_patterns).expect("exclude globs");
        let project = project_globs();
        let (gitignore, gitignore_warning) = if respect {
            load_gitignore(root, cache)
        } else {
            (None, None)
        };
        let options = CollectOptions {
            root,
            project_matcher: &project,
            exclude_matcher: &exclude,
            test_matcher: &GlobSet::empty(),
            exclude_patterns: &exclude_patterns,
            respect_gitignore: respect,
            gitignore: gitignore.as_deref(),
            production: false,
            cache,
            dirs: None,
        };
        let collected = collect_files(&options).expect("collect");
        let mut warnings = collected.warnings;
        warnings.extend(gitignore_warning);
        (collected.files, warnings)
    }

    fn reference(root: &Path, custom: &[String], respect: bool) -> Outcome {
        let exclude_patterns = effective_exclude(custom);
        let exclude = build_glob_set(&exclude_patterns).expect("exclude globs");
        let project = project_globs();
        let (gitignore, gitignore_warning) = if respect {
            load_gitignore_reference(root)
        } else {
            (None, None)
        };
        let options = ReferenceOptions {
            root,
            project_matcher: &project,
            exclude_matcher: &exclude,
            test_matcher: &GlobSet::empty(),
            exclude_patterns: &exclude_patterns,
            respect_gitignore: respect,
            gitignore: gitignore.as_ref(),
            production: false,
        };
        let (files, mut warnings) = collect_files_reference(&options).expect("collect");
        warnings.extend(gitignore_warning);
        (files, warnings)
    }

    /// Walks `roots` in order through one shared cache, as a workspace probe
    /// does, and checks each against a fresh reference walk.
    fn assert_equivalent(roots: &[PathBuf], custom: &[String]) -> Vec<Outcome> {
        let mut outcomes = Vec::new();
        for respect in [true, false] {
            let cache = SourcesCache::default();
            for root in roots {
                let actual = cached(&cache, root, custom, respect);
                let expected = reference(root, custom, respect);
                assert_eq!(
                    actual,
                    expected,
                    "root {}, respect {respect}",
                    root.display()
                );
                outcomes.push(actual);
            }
        }
        outcomes
    }

    fn paths(outcome: &Outcome) -> Vec<&str> {
        outcome.0.iter().map(|file| file.path.as_str()).collect()
    }

    #[test]
    fn nested_whitelists_and_exclude_match_the_ignore_walker() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("repo");
        write_file(&root.join(".git/info/exclude"), "local_*.py\n");
        write_file(
            &root.join(".gitignore"),
            "build/\n*.pyi\n!keep.pyi\n/top.py\ngen\n",
        );
        write_file(
            &root.join("pkg/.gitignore"),
            "!gen\nsecret.py\n/only_here.py\n",
        );
        write_file(&root.join("pkg/sub/.gitignore"), "!secret.py\ndata/\n");
        for file in [
            "top.py",
            "app.py",
            "local_dev.py",
            "keep.pyi",
            "drop.pyi",
            "build/out.py",
            "gen/model.py",
            "pkg/gen/model.py",
            "pkg/secret.py",
            "pkg/only_here.py",
            "pkg/top.py",
            "pkg/build/out.py",
            "pkg/sub/secret.py",
            "pkg/sub/only_here.py",
            "pkg/sub/data/x.py",
        ] {
            write_file(&root.join(file), "");
        }
        // A file named like a directory pattern.
        write_file(&root.join("pkg/data"), "");

        let outcomes =
            assert_equivalent(&[root.clone(), root.join("pkg"), root.join("pkg/sub")], &[]);
        let respected = paths(&outcomes[0]);
        assert!(respected.contains(&"app.py"));
        assert!(respected.contains(&"keep.pyi"));
        // `pkg/.gitignore` re-includes it for the walk, but the root
        // `.gitignore` post-filter still drops it.
        assert!(!respected.contains(&"pkg/gen/model.py"));
        assert!(respected.contains(&"pkg/sub/secret.py"));
        assert!(!respected.contains(&"local_dev.py"));
        assert!(!respected.contains(&"pkg/secret.py"));
        assert!(!respected.contains(&"pkg/sub/data/x.py"));
    }

    #[test]
    fn ancestor_gitignore_and_nested_repos_match_the_ignore_walker() {
        let temp = tempdir().expect("tempdir");
        let outer = temp.path().join("outer");
        write_file(&outer.join(".git/HEAD"), "");
        write_file(&outer.join(".gitignore"), "ignored_*.py\nproj/vendored/\n");
        let root = outer.join("proj");
        write_file(&root.join("app.py"), "");
        write_file(&root.join("ignored_a.py"), "");
        write_file(&root.join("vendored/lib.py"), "");
        write_file(&root.join("pkg/ignored_b.py"), "");
        // A submodule stops the outer rules below it.
        write_file(&root.join("sub/.git/info/exclude"), "excluded.py\n");
        write_file(&root.join("sub/ignored_c.py"), "");
        write_file(&root.join("sub/excluded.py"), "");
        write_file(&root.join("sub/inner/ignored_d.py"), "");
        // A worktree `.git` file pointing at a shared common dir.
        let common = temp.path().join("common.git");
        write_file(&common.join("info/exclude"), "wt_excluded.py\n");
        let gitdir = common.join("worktrees/wt");
        write_file(&gitdir.join("commondir"), "../..\n");
        write_file(
            &root.join("wt/.git"),
            &format!("gitdir: {}\n", gitdir.display()),
        );
        write_file(&root.join("wt/wt_excluded.py"), "");
        write_file(&root.join("wt/kept.py"), "");
        // A `.git` file that names no gitdir.
        write_file(&root.join("broken/.git"), "garbage\n");
        write_file(&root.join("broken/ignored_e.py"), "");

        let outcomes = assert_equivalent(
            &[
                root.clone(),
                root.join("pkg"),
                root.join("sub"),
                root.join("wt"),
                root.join("broken"),
            ],
            &[],
        );
        let respected = paths(&outcomes[0]);
        assert!(respected.contains(&"app.py"));
        assert!(!respected.contains(&"ignored_a.py"));
        assert!(!respected.contains(&"pkg/ignored_b.py"));
        assert!(!respected.contains(&"sub/excluded.py"));
        assert!(!respected.contains(&"wt/wt_excluded.py"));
        assert!(respected.contains(&"wt/kept.py"));
    }

    #[test]
    fn invalid_ignore_lines_warn_like_the_ignore_walker() {
        let temp = tempdir().expect("tempdir");
        let outer = temp.path().join("outer");
        write_file(&outer.join(".gitignore"), "bad{glob\n");
        let root = outer.join("repo");
        write_file(&root.join(".git/info/exclude"), "ex{clude\n");
        write_file(&root.join(".gitignore"), "foo{bar\n*.pyi\n");
        write_file(&root.join("pkg/.gitignore"), "baz{qux\n");
        write_file(&root.join("pkg/app.py"), "");
        write_file(&root.join("pkg/types.pyi"), "");
        // A directory named `.gitignore` is no ignore file.
        fs::create_dir_all(root.join("dirs/.gitignore")).expect("dir");
        write_file(&root.join("dirs/x.py"), "");

        let outcomes = assert_equivalent(&[root.clone(), root.join("pkg"), root.join("dirs")], &[]);
        assert_ne!(outcomes[0].1, []);
        assert!(!paths(&outcomes[0]).contains(&"pkg/types.pyi"));
        // Each warning names the ignore file whose line failed, once (#903).
        for warning in &outcomes[0].1 {
            let SourcesWarning::PathUnreadable { path, reason } = warning else {
                panic!("{warning}");
            };
            let file = Path::new(path).file_name().and_then(|name| name.to_str());
            assert!(matches!(file, Some(".gitignore" | "exclude")), "{warning}");
            assert!(
                reason.starts_with("line 1: error parsing glob"),
                "{warning}"
            );
        }
        assert_eq!(outcomes[0].1.len(), 4, "{:?}", outcomes[0].1);
    }

    /// `ignore` drops a `.gitignore` it cannot read, so only the root's
    /// warning reports it.
    #[test]
    fn unreadable_root_gitignore_warns_like_the_ignore_walker() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("repo");
        write_file(&root.join("app.py"), "");
        fs::write(root.join(".gitignore"), b"app.py\n\xff\n").expect("write");

        let outcomes = assert_equivalent(&[root], &[]);
        assert_eq!(
            outcomes[0].1,
            [SourcesWarning::GitignoreUnreadable {
                path: GITIGNORE_PATH.to_owned(),
            }]
        );
    }

    /// A `.gitignore` below the root that `ignore` cannot read is named
    /// once, by its path from the walk root; a dangling symlink counts too
    /// (#915). A member walk spells the same file from its own root.
    #[test]
    fn unreadable_nested_gitignore_warns_once_per_file() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("repo");
        write_file(&root.join("pkg/sub/gen.py"), "");
        write_file(&root.join("pkg/sub/.gitignore"), "");
        fs::write(root.join("pkg/sub/.gitignore"), b"\xff\ngen.py\n").expect("write");
        write_file(&root.join("lib/x.py"), "");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("missing"), root.join("lib/.gitignore"))
            .expect("symlink");

        let outcomes = assert_equivalent(&[root.clone(), root.join("pkg")], &[]);
        let gitignore = |path: &str| SourcesWarning::GitignoreUnreadable {
            path: path.to_owned(),
        };
        let mut expected = Vec::new();
        if cfg!(unix) {
            expected.push(gitignore("lib/.gitignore"));
        }
        expected.push(gitignore("pkg/sub/.gitignore"));
        assert_eq!(outcomes[0].1, expected);
        assert_eq!(outcomes[1].1, [gitignore("sub/.gitignore")]);
        assert!(paths(&outcomes[0]).contains(&"pkg/sub/gen.py"));
        // Without `respect_gitignore` no ignore file is read.
        assert!(outcomes[2].1.is_empty(), "{:?}", outcomes[2].1);
    }

    /// A dangling root `.gitignore` symlink is as unread as a broken file.
    #[cfg(unix)]
    #[test]
    fn dangling_root_gitignore_warns_like_an_unreadable_one() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("repo");
        write_file(&root.join("app.py"), "");
        std::os::unix::fs::symlink(root.join("missing"), root.join(".gitignore")).expect("symlink");

        let outcomes = assert_equivalent(&[root], &[]);
        assert_eq!(
            outcomes[0].1,
            [SourcesWarning::GitignoreUnreadable {
                path: GITIGNORE_PATH.to_owned(),
            }]
        );
    }

    #[test]
    fn relative_root_spellings_match_the_ignore_walker() {
        // Under the package directory, which `cargo test` runs in.
        fs::create_dir_all("target").expect("target dir");
        let temp = tempfile::tempdir_in("target").expect("tempdir");
        let relative = temp
            .path()
            .strip_prefix(std::env::current_dir().expect("cwd"))
            .expect("tempdir under the package directory")
            .join("repo");
        write_file(&relative.join(".git/HEAD"), "");
        write_file(&relative.join(".gitignore"), "/top.py\npkg/gen/\n");
        write_file(&relative.join("pkg/.gitignore"), "/local.py\n");
        for file in [
            "top.py",
            "app.py",
            "pkg/top.py",
            "pkg/local.py",
            "pkg/gen/x.py",
        ] {
            write_file(&relative.join(file), "");
        }

        let spelled = relative.to_string_lossy().into_owned();
        let roots: Vec<PathBuf> = [
            spelled.clone(),
            format!("{spelled}/"),
            format!("./{spelled}"),
            format!("./{spelled}/pkg/"),
            format!("{spelled}/pkg/."),
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        let outcomes = assert_equivalent(&roots, &[]);
        assert_eq!(paths(&outcomes[0]), ["app.py", "pkg/top.py"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_roots_warn_like_the_ignore_walker() {
        let temp = tempdir().expect("tempdir");
        let real = temp.path().join("real");
        write_file(&real.join(".git/HEAD"), "");
        write_file(&real.join(".gitignore"), "foo{bar\nskipped.py\n");
        write_file(&real.join("app.py"), "");
        write_file(&real.join("skipped.py"), "");
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let outcomes = assert_equivalent(std::slice::from_ref(&link), &[]);
        assert_ne!(outcomes[0].1, []);
        assert_eq!(paths(&outcomes[0]), ["app.py"]);
    }

    #[cfg(unix)]
    #[test]
    fn nested_directory_symlinks_stay_unread_like_the_ignore_walker() {
        let temp = tempdir().expect("tempdir");
        let other = temp.path().join("other");
        write_file(&other.join(".gitignore"), "foo{bar\n");
        write_file(&other.join("lib.py"), "");
        let root = temp.path().join("repo");
        write_file(&root.join(".git/HEAD"), "");
        write_file(&root.join("app.py"), "");
        std::os::unix::fs::symlink(&other, root.join("vendor")).expect("symlink");

        let outcomes = assert_equivalent(std::slice::from_ref(&root), &[]);
        assert_eq!(
            outcomes[0].1,
            [SourcesWarning::SymlinkSkipped {
                path: "vendor".to_owned(),
                reason: "it points outside the project root".to_owned(),
            }]
        );
        assert_eq!(paths(&outcomes[0]), ["app.py"]);
    }

    #[test]
    fn concurrent_walks_sharing_a_cache_match_the_ignore_walker() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("repo");
        write_file(&root.join(".git/HEAD"), "");
        write_file(&root.join(".gitignore"), "build/\n");
        let members: Vec<PathBuf> = (0..8).map(|index| root.join(format!("m{index}"))).collect();
        for member in &members {
            write_file(&member.join(".gitignore"), "/local.py\n");
            write_file(&member.join("app.py"), "");
            write_file(&member.join("local.py"), "");
            write_file(&member.join("build/out.py"), "");
        }

        let cache = SourcesCache::default();
        let roots: Vec<&PathBuf> = std::iter::once(&root).chain(&members).collect();
        std::thread::scope(|scope| {
            for root in &roots {
                let cache = &cache;
                scope.spawn(move || {
                    assert_eq!(cached(cache, root, &[], true), reference(root, &[], true));
                });
            }
        });
        assert_eq!(cache.compiled_rules(), 2);
    }

    #[test]
    fn plain_directories_and_custom_excludes_match_the_ignore_walker() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("plain");
        // No `.git` anywhere: `.gitignore` applies only through the post-filter.
        write_file(&root.join(".gitignore"), "skipped.py\nnested/\n");
        write_file(&root.join("pkg/.gitignore"), "inner.py\n");
        write_file(&root.join("skipped.py"), "");
        write_file(&root.join("nested/a.py"), "");
        write_file(&root.join("pkg/inner.py"), "");
        write_file(&root.join("pkg/generated/model.py"), "");
        write_file(&root.join(".venv/lib.py"), "");
        assert_equivalent(
            &[root.clone(), root.join("pkg")],
            &["pkg/generated/**".to_owned(), "*.pyi".to_owned()],
        );
    }

    #[test]
    fn member_walks_reuse_the_root_walk_parse() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical").join("repo");
        write_file(&root.join(".git/HEAD"), "");
        write_file(&root.join(".gitignore"), "*.pyi\n");
        write_file(&root.join("packages/a/.gitignore"), "gen/\n");
        write_file(&root.join("packages/a/src/a.py"), "");
        write_file(&root.join("packages/b/src/b.py"), "");
        let cache = SourcesCache::default();
        cached(&cache, &root, &[], true);
        let after_root = cache.parsed_dirs();
        cached(&cache, &root.join("packages/a"), &[], true);
        cached(&cache, &root.join("packages/b"), &[], true);
        assert_eq!(cache.parsed_dirs(), after_root);
    }

    #[test]
    fn identical_ignore_files_compile_once_and_stay_directory_relative() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("repo");
        write_file(&root.join(".git/HEAD"), "");
        let rules = "/top.py\nsub/*.py\n!sub/keep.py\nsub\n";
        for dir in ["", "pkg", "pkg/inner", "other"] {
            write_file(&root.join(dir).join(".gitignore"), rules);
            for file in ["top.py", "app.py", "sub/x.py", "sub/keep.py"] {
                write_file(&root.join(dir).join(file), "");
            }
        }
        // The same rules behind a BOM and CRLF line ends.
        write_file(
            &root.join("bom/.gitignore"),
            "\u{feff}/top.py\r\nsub/*.py\r\n",
        );
        write_file(&root.join("bom/top.py"), "");
        write_file(&root.join("bom/sub/x.py"), "");

        let roots = [root.clone(), root.join("pkg"), root.join("bom")];
        let outcomes = assert_equivalent(&roots, &[]);
        let respected = paths(&outcomes[0]);
        // `/top.py` anchors at each `.gitignore`'s own directory.
        assert!(respected.contains(&"pkg/app.py"));
        assert!(!respected.contains(&"pkg/top.py"));
        assert!(!respected.contains(&"pkg/inner/top.py"));
        assert!(!respected.contains(&"bom/top.py"));
        assert!(!respected.contains(&"bom/sub/x.py"));

        let cache = SourcesCache::default();
        cached(&cache, &root, &[], true);
        // One rule set per distinct content, plus nothing for missing files.
        assert_eq!(cache.compiled_rules(), 2);
    }

    const DIRS: [&str; 4] = ["a", "b", "a/c", "b/gen"];
    const FILES: [&str; 4] = ["x.py", "y.pyi", "gen.py", "keep.py"];
    const PATTERNS: [&str; 10] = [
        "*.pyi",
        "x.py",
        "/x.py",
        "gen",
        "gen/",
        "c/",
        "!keep.py",
        "**/c/*.py",
        "!gen",
        "*",
    ];

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(crate::internals::proptest_cases(48)))]

        #[test]
        fn random_trees_match_the_ignore_walker(
            git_dirs in proptest::collection::vec(0..=DIRS.len(), 0..3),
            ignores in proptest::collection::vec(
                (0..=DIRS.len(), proptest::collection::vec(0..PATTERNS.len(), 1..3)),
                0..4,
            ),
            excludes in proptest::collection::vec(0..PATTERNS.len(), 0..2),
            roots in proptest::collection::vec(0..=DIRS.len(), 1..4),
        ) {
            let temp = tempdir().expect("tempdir");
            let base = temp.path().join("t");
            // Index `DIRS.len()` names the base itself.
            let dir = |index: usize| DIRS.get(index).map_or_else(|| base.clone(), |dir| base.join(dir));
            for index in 0..=DIRS.len() {
                for file in FILES {
                    write_file(&dir(index).join(file), "");
                }
            }
            for index in git_dirs {
                fs::create_dir_all(dir(index).join(".git/info")).expect("git dir");
            }
            if let Some(&pattern) = excludes.first() {
                write_file(&base.join(".git/info/exclude"), PATTERNS[pattern]);
            }
            for (index, lines) in ignores {
                let text: Vec<&str> = lines.iter().map(|line| PATTERNS[*line]).collect();
                write_file(&dir(index).join(".gitignore"), &text.join("\n"));
            }
            let roots: Vec<PathBuf> = roots.into_iter().map(dir).collect();
            assert_equivalent(&roots, &[]);
        }
    }
}
