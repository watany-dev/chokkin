//! Fixture tests for `collect_files` over ignore files, root spellings and a
//! shared cache. They pin what the reference walker that `collect_files`
//! replaced in #757 agreed on, so the walk no longer keeps a second copy.

use std::fs;
use std::path::{Path, PathBuf};

use globset::GlobSet;
use proptest::prelude::*;
use tempfile::tempdir;

use super::cache::SourcesCache;
use super::glob::{build_glob_set, effective_exclude};
use super::types::DiscoveredFile;
use super::walk::{CollectOptions, collect_files, root_gitignore_warning};
use super::warnings::SourcesWarning;

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
fn walk(cache: &SourcesCache, root: &Path, custom: &[String], respect: bool) -> Outcome {
    let exclude_patterns = effective_exclude(custom);
    let exclude = cache.glob_set(&exclude_patterns).expect("exclude globs");
    let project = project_globs();
    let gitignore_warning = respect
        .then(|| root_gitignore_warning(root, cache))
        .flatten();
    let options = CollectOptions {
        root,
        project_matcher: &project,
        exclude_matcher: &exclude,
        test_matcher: &GlobSet::empty(),
        exclude_patterns: &exclude_patterns,
        respect_gitignore: respect,
        production: false,
        cache,
        dirs: None,
    };
    let collected = collect_files(&options).expect("collect");
    let mut warnings = collected.warnings;
    warnings.extend(gitignore_warning);
    (collected.files, warnings)
}

/// Walks `roots` in order through one shared cache, as a workspace probe
/// does, first with `respect_gitignore` and then without. Each walk must
/// match a walk through a fresh cache.
fn walk_roots(roots: &[PathBuf], custom: &[String]) -> Vec<Outcome> {
    let mut outcomes = Vec::new();
    for respect in [true, false] {
        let cache = SourcesCache::default();
        for root in roots {
            let shared = walk(&cache, root, custom, respect);
            let fresh = walk(&SourcesCache::default(), root, custom, respect);
            assert_eq!(shared, fresh, "root {}, respect {respect}", root.display());
            outcomes.push(shared);
        }
    }
    outcomes
}

fn paths(outcome: &Outcome) -> Vec<&str> {
    outcome.0.iter().map(|file| file.path.as_str()).collect()
}

fn all_paths(outcomes: &[Outcome]) -> Vec<Vec<&str>> {
    outcomes.iter().map(paths).collect()
}

fn no_warnings(outcomes: &[Outcome]) {
    for outcome in outcomes {
        assert_eq!(outcome.1, [], "{:?}", paths(outcome));
    }
}

#[test]
fn nested_whitelists_and_exclude() {
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

    let outcomes = walk_roots(&[root.clone(), root.join("pkg"), root.join("pkg/sub")], &[]);
    no_warnings(&outcomes);
    // `pkg/.gitignore` re-includes `pkg/gen` over the root's `gen`, as git
    // does (#1220).
    assert_eq!(
        all_paths(&outcomes),
        [
            vec![
                "app.py",
                "keep.pyi",
                "pkg/gen/model.py",
                "pkg/sub/only_here.py",
                "pkg/sub/secret.py",
                "pkg/top.py",
            ],
            vec![
                "gen/model.py",
                "sub/only_here.py",
                "sub/secret.py",
                "top.py",
            ],
            vec!["only_here.py", "secret.py"],
            vec![
                "app.py",
                "drop.pyi",
                "gen/model.py",
                "keep.pyi",
                "local_dev.py",
                "pkg/build/out.py",
                "pkg/gen/model.py",
                "pkg/only_here.py",
                "pkg/secret.py",
                "pkg/sub/data/x.py",
                "pkg/sub/only_here.py",
                "pkg/sub/secret.py",
                "pkg/top.py",
                "top.py",
            ],
            vec![
                "gen/model.py",
                "only_here.py",
                "secret.py",
                "sub/data/x.py",
                "sub/only_here.py",
                "sub/secret.py",
                "top.py",
            ],
            vec!["data/x.py", "only_here.py", "secret.py"],
        ]
    );
}

#[test]
fn ancestor_gitignore_and_nested_repos() {
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
    // A `.git` file that names no gitdir still bounds the outer repo.
    write_file(&root.join("broken/.git"), "garbage\n");
    write_file(&root.join("broken/ignored_e.py"), "");

    let outcomes = walk_roots(
        &[
            root.clone(),
            root.join("pkg"),
            root.join("sub"),
            root.join("wt"),
            root.join("broken"),
        ],
        &[],
    );
    no_warnings(&outcomes);
    assert_eq!(
        all_paths(&outcomes),
        [
            vec![
                "app.py",
                "broken/ignored_e.py",
                "sub/ignored_c.py",
                "sub/inner/ignored_d.py",
                "wt/kept.py",
            ],
            vec![],
            vec!["ignored_c.py", "inner/ignored_d.py"],
            vec!["kept.py"],
            vec!["ignored_e.py"],
            vec![
                "app.py",
                "broken/ignored_e.py",
                "ignored_a.py",
                "pkg/ignored_b.py",
                "sub/excluded.py",
                "sub/ignored_c.py",
                "sub/inner/ignored_d.py",
                "vendored/lib.py",
                "wt/kept.py",
                "wt/wt_excluded.py",
            ],
            vec!["ignored_b.py"],
            vec!["excluded.py", "ignored_c.py", "inner/ignored_d.py"],
            vec!["kept.py", "wt_excluded.py"],
            vec!["ignored_e.py"],
        ]
    );
}

#[test]
fn invalid_ignore_lines_warn_once_per_file() {
    let temp = tempdir().expect("tempdir");
    let outer = temp.path().join("outer");
    write_file(&outer.join(".gitignore"), "bad[z-a]glob\n");
    let root = outer.join("repo");
    write_file(&root.join(".git/info/exclude"), "ex[z-a]clude\n");
    write_file(&root.join(".gitignore"), "foo[z-a]bar\n*.pyi\n");
    write_file(&root.join("pkg/.gitignore"), "baz[z-a]qux\n");
    write_file(&root.join("pkg/app.py"), "");
    write_file(&root.join("pkg/types.pyi"), "");
    // A directory named `.gitignore` is no ignore file.
    fs::create_dir_all(root.join("dirs/.gitignore")).expect("dir");
    write_file(&root.join("dirs/x.py"), "");

    let outcomes = walk_roots(&[root.clone(), root.join("pkg"), root.join("dirs")], &[]);
    assert_eq!(
        all_paths(&outcomes),
        [
            vec!["dirs/x.py", "pkg/app.py"],
            vec!["app.py"],
            vec!["x.py"],
            vec!["dirs/x.py", "pkg/app.py", "pkg/types.pyi"],
            vec!["app.py", "types.pyi"],
            vec!["x.py"],
        ]
    );
    // Each warning names the ignore file whose line failed, once (#903): the
    // ancestor's, the root's, `info/exclude`, then those below the root.
    let failed = |outcome: &Outcome| -> Vec<(String, String)> {
        outcome
            .1
            .iter()
            .map(|warning| {
                let SourcesWarning::PathUnreadable { path, reason } = warning else {
                    panic!("{warning}");
                };
                let file = Path::new(path).file_name().expect("file name");
                let glob = reason
                    .strip_prefix("line 1: error parsing glob '")
                    .and_then(|rest| rest.split_once('\''))
                    .expect(reason)
                    .0;
                (file.to_string_lossy().into_owned(), glob.to_owned())
            })
            .collect()
    };
    let named = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(file, glob)| ((*file).to_owned(), (*glob).to_owned()))
            .collect()
    };
    let above = [
        (".gitignore", "bad[z-a]glob"),
        (".gitignore", "foo[z-a]bar"),
        ("exclude", "ex[z-a]clude"),
    ];
    let with_pkg = [above.as_slice(), &[(".gitignore", "baz[z-a]qux")]].concat();
    assert_eq!(failed(&outcomes[0]), named(&with_pkg));
    assert_eq!(failed(&outcomes[1]), named(&with_pkg));
    assert_eq!(failed(&outcomes[2]), named(&above));
    no_warnings(&outcomes[3..]);
}

/// A root `.gitignore` that cannot be read is named once; the lines before
/// the one that fails still apply, in a repository or not.
#[test]
fn unreadable_root_gitignore_warns() {
    for repository in [true, false] {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("repo");
        write_file(&root.join("app.py"), "");
        write_file(&root.join("main.py"), "");
        if repository {
            fs::create_dir_all(root.join(".git")).expect("git dir");
        }
        fs::write(root.join(".gitignore"), b"app.py\n\xff\n").expect("write");

        let outcomes = walk_roots(&[root], &[]);
        assert_eq!(
            all_paths(&outcomes),
            [vec!["main.py"], vec!["app.py", "main.py"]],
            "repository {repository}"
        );
        assert_eq!(
            outcomes[0].1,
            [SourcesWarning::GitignoreUnreadable {
                path: ".gitignore".to_owned(),
            }]
        );
        no_warnings(&outcomes[1..]);
    }
}

/// A `.gitignore` below the root that cannot be read is named once, by its
/// path from the walk root; a dangling symlink counts too (#915). A member
/// walk spells the same file from its own root.
#[test]
fn unreadable_nested_gitignore_warns_once_per_file() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("repo");
    write_file(&root.join("pkg/sub/gen.py"), "");
    fs::create_dir_all(root.join("pkg/sub")).expect("dir");
    fs::write(root.join("pkg/sub/.gitignore"), b"\xff\ngen.py\n").expect("write");
    write_file(&root.join("lib/x.py"), "");
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("missing"), root.join("lib/.gitignore")).expect("symlink");

    let outcomes = walk_roots(&[root.clone(), root.join("pkg")], &[]);
    assert_eq!(
        all_paths(&outcomes),
        [
            vec!["lib/x.py", "pkg/sub/gen.py"],
            vec!["sub/gen.py"],
            vec!["lib/x.py", "pkg/sub/gen.py"],
            vec!["sub/gen.py"],
        ]
    );
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
    // Without `respect_gitignore` no ignore file is read.
    no_warnings(&outcomes[2..]);
}

/// A dangling root `.gitignore` symlink is as unread as a broken file.
#[cfg(unix)]
#[test]
fn dangling_root_gitignore_warns_like_an_unreadable_one() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("repo");
    write_file(&root.join("app.py"), "");
    std::os::unix::fs::symlink(root.join("missing"), root.join(".gitignore")).expect("symlink");

    let outcomes = walk_roots(&[root], &[]);
    assert_eq!(all_paths(&outcomes), [["app.py"], ["app.py"]]);
    assert_eq!(
        outcomes[0].1,
        [SourcesWarning::GitignoreUnreadable {
            path: ".gitignore".to_owned(),
        }]
    );
    no_warnings(&outcomes[1..]);
}

#[test]
fn relative_root_spellings_walk_alike() {
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
    let outcomes = walk_roots(&roots, &[]);
    no_warnings(&outcomes);
    let repo = ["app.py", "pkg/top.py"];
    let pkg = ["top.py"];
    let repo_all = [
        "app.py",
        "pkg/gen/x.py",
        "pkg/local.py",
        "pkg/top.py",
        "top.py",
    ];
    let pkg_all = ["gen/x.py", "local.py", "top.py"];
    assert_eq!(
        all_paths(&outcomes),
        [
            &repo[..],
            &repo,
            &repo,
            &pkg,
            &pkg,
            &repo_all,
            &repo_all,
            &repo_all,
            &pkg_all,
            &pkg_all,
        ]
    );
}

#[cfg(unix)]
#[test]
fn symlinked_roots_read_the_target_ignore_files() {
    let temp = tempdir().expect("tempdir");
    let real = temp.path().join("real");
    write_file(&real.join(".git/HEAD"), "");
    write_file(&real.join(".gitignore"), "foo[z-a]bar\nskipped.py\n");
    write_file(&real.join("app.py"), "");
    write_file(&real.join("skipped.py"), "");
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    let outcomes = walk_roots(std::slice::from_ref(&link), &[]);
    assert_eq!(
        all_paths(&outcomes),
        [vec!["app.py"], vec!["app.py", "skipped.py"]]
    );
    assert_eq!(
        outcomes[0].1,
        [SourcesWarning::PathUnreadable {
            path: link.join(".gitignore").to_string_lossy().into_owned(),
            reason: "line 1: error parsing glob 'foo[z-a]bar': invalid range; 'z' > 'a'".to_owned(),
        }]
    );
    no_warnings(&outcomes[1..]);
}

#[cfg(unix)]
#[test]
fn nested_directory_symlinks_stay_unread() {
    let temp = tempdir().expect("tempdir");
    let other = temp.path().join("other");
    write_file(&other.join(".gitignore"), "foo[z-a]bar\n");
    write_file(&other.join("lib.py"), "");
    let root = temp.path().join("repo");
    write_file(&root.join(".git/HEAD"), "");
    write_file(&root.join("app.py"), "");
    std::os::unix::fs::symlink(&other, root.join("vendor")).expect("symlink");

    let outcomes = walk_roots(std::slice::from_ref(&root), &[]);
    let skipped = [SourcesWarning::SymlinkSkipped {
        path: "vendor".to_owned(),
        reason: "it points outside the project root".to_owned(),
    }];
    for outcome in &outcomes {
        assert_eq!(paths(outcome), ["app.py"]);
        assert_eq!(outcome.1, skipped);
    }
}

#[test]
fn concurrent_walks_sharing_a_cache_agree_with_fresh_ones() {
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
                let shared = walk(cache, root, &[], true);
                assert_eq!(shared, walk(&SourcesCache::default(), root, &[], true));
                assert_eq!(shared.1, []);
                let expected: Vec<String> = if root.ends_with("repo") {
                    (0..8).map(|index| format!("m{index}/app.py")).collect()
                } else {
                    vec!["app.py".to_owned()]
                };
                assert_eq!(paths(&shared), expected);
            });
        }
    });
    assert_eq!(cache.compiled_rules(), 2);
}

#[test]
fn plain_directories_and_custom_excludes() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("plain");
    // No `.git` anywhere: every `.gitignore` from the walk root down still
    // applies, in git's order (#1220).
    write_file(&root.join(".gitignore"), "skipped.py\nnested/\n");
    write_file(&root.join("pkg/.gitignore"), "inner.py\n");
    write_file(&root.join("skipped.py"), "");
    write_file(&root.join("nested/a.py"), "");
    write_file(&root.join("pkg/inner.py"), "");
    write_file(&root.join("pkg/generated/model.py"), "");
    write_file(&root.join(".venv/lib.py"), "");
    let outcomes = walk_roots(
        &[root.clone(), root.join("pkg")],
        &["pkg/generated/**".to_owned(), "*.pyi".to_owned()],
    );
    no_warnings(&outcomes);
    // The custom exclude is relative to each walk root.
    assert_eq!(
        all_paths(&outcomes),
        [
            vec![],
            vec!["generated/model.py"],
            vec!["nested/a.py", "pkg/inner.py", "skipped.py"],
            vec!["generated/model.py", "inner.py"],
        ]
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
    walk(&cache, &root, &[], true);
    let after_root = cache.parsed_dirs();
    walk(&cache, &root.join("packages/a"), &[], true);
    walk(&cache, &root.join("packages/b"), &[], true);
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
    let outcomes = walk_roots(&roots, &[]);
    no_warnings(&outcomes);
    // `/top.py` anchors at each `.gitignore`'s own directory, and `sub`
    // drops the directory before `!sub/keep.py` could re-include it.
    assert_eq!(
        all_paths(&outcomes[..3]),
        [
            vec!["app.py", "other/app.py", "pkg/app.py", "pkg/inner/app.py"],
            vec!["app.py", "inner/app.py"],
            vec![],
        ]
    );
    assert_eq!(paths(&outcomes[5]), ["sub/x.py", "top.py"]);

    let cache = SourcesCache::default();
    walk(&cache, &root, &[], true);
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

/// A tree with every file of [`FILES`] in the base `t` and each of [`DIRS`].
fn pattern_tree(base: &Path) {
    for dir in std::iter::once("").chain(DIRS) {
        for file in FILES {
            write_file(&base.join(dir).join(file), "");
        }
    }
}

/// The files a walk with `respect_gitignore` leaves out of the tree.
fn dropped(outcomes: &[Outcome]) -> Vec<&str> {
    let kept = paths(&outcomes[0]);
    paths(&outcomes[1])
        .into_iter()
        .filter(|path| !kept.contains(path))
        .collect()
}

/// `.gitignore` files to write, by directory under the tree, and the files
/// a walk that respects them leaves out.
type RuleCase = (Vec<(&'static str, &'static str)>, Vec<&'static str>);

const A_C: [&str; 4] = ["a/c/gen.py", "a/c/keep.py", "a/c/x.py", "a/c/y.pyi"];
const B_GEN: [&str; 4] = ["b/gen/gen.py", "b/gen/keep.py", "b/gen/x.py", "b/gen/y.pyi"];
const UNDER_A: [&str; 8] = [
    "a/c/gen.py",
    "a/c/keep.py",
    "a/c/x.py",
    "a/c/y.pyi",
    "a/gen.py",
    "a/keep.py",
    "a/x.py",
    "a/y.pyi",
];

/// Every file of [`pattern_tree`], sorted.
fn every_file() -> Vec<&'static str> {
    [
        &UNDER_A[..],
        &["b/gen.py"],
        &B_GEN,
        &[
            "b/keep.py",
            "b/x.py",
            "b/y.pyi",
            "gen.py",
            "keep.py",
            "x.py",
            "y.pyi",
        ],
    ]
    .concat()
}

/// Each case in a repository and in a plain directory: without `.git` the
/// `.gitignore` files drop the same files (#1220).
fn assert_rule_cases(cases: &[RuleCase]) {
    for (ignores, expected) in cases {
        for repository in [true, false] {
            let temp = tempdir().expect("tempdir");
            let base = temp.path().join("t");
            pattern_tree(&base);
            if repository {
                fs::create_dir_all(base.join(".git/info")).expect("git dir");
            }
            for (dir, rules) in ignores {
                write_file(&base.join(dir).join(".gitignore"), rules);
            }
            let outcomes = walk_roots(std::slice::from_ref(&base), &[]);
            no_warnings(&outcomes);
            assert_eq!(
                dropped(&outcomes),
                *expected,
                "{ignores:?}, repository {repository}"
            );
        }
    }
}

/// Each rule of [`PATTERNS`] from a `.gitignore` in the repository root and
/// in `a/`. `git status --ignored` drops the same files from this tree.
#[test]
fn single_gitignore_rules_drop_what_git_drops() {
    let in_root = |pattern| vec![("", pattern)];
    let in_a = |pattern| vec![("a", pattern)];
    assert_rule_cases(&[
        (
            in_root("*.pyi"),
            vec!["a/c/y.pyi", "a/y.pyi", "b/gen/y.pyi", "b/y.pyi", "y.pyi"],
        ),
        (in_a("*.pyi"), vec!["a/c/y.pyi", "a/y.pyi"]),
        (
            in_root("x.py"),
            vec!["a/c/x.py", "a/x.py", "b/gen/x.py", "b/x.py", "x.py"],
        ),
        (in_a("x.py"), vec!["a/c/x.py", "a/x.py"]),
        (in_root("/x.py"), vec!["x.py"]),
        (in_a("/x.py"), vec!["a/x.py"]),
        (in_root("gen"), B_GEN.to_vec()),
        (in_a("gen"), vec![]),
        (in_root("gen/"), B_GEN.to_vec()),
        (in_a("gen/"), vec![]),
        (in_root("c/"), A_C.to_vec()),
        (in_a("c/"), A_C.to_vec()),
        (in_root("!keep.py"), vec![]),
        (in_a("!keep.py"), vec![]),
        (in_root("**/c/*.py"), A_C[..3].to_vec()),
        (in_a("**/c/*.py"), A_C[..3].to_vec()),
        (in_root("!gen"), vec![]),
        (in_a("!gen"), vec![]),
        (in_root("*"), every_file()),
        (in_a("*"), UNDER_A.to_vec()),
    ]);
}

/// Rules that interact within one `.gitignore`, as `git status --ignored`
/// reads them.
#[test]
fn combined_gitignore_rules_drop_what_git_drops() {
    let all_but = |kept: &[&str]| -> Vec<&'static str> {
        every_file()
            .into_iter()
            .filter(|path| !kept.contains(path))
            .collect()
    };
    assert_rule_cases(&[
        // `*` drops the directories too, so only the root `keep.py` comes
        // back until `!*/` re-includes them.
        (vec![("", "*\n!keep.py")], all_but(&["keep.py"])),
        (
            vec![("", "*\n!*/\n!keep.py")],
            all_but(&[
                "a/c/keep.py",
                "a/keep.py",
                "b/gen/keep.py",
                "b/keep.py",
                "keep.py",
            ]),
        ),
        (vec![("a", "*.pyi\n!y.pyi")], vec![]),
        // A deeper `.gitignore` wins over the root's (#1220).
        (
            vec![("", "gen.py"), ("a", "!gen.py")],
            vec!["b/gen.py", "b/gen/gen.py", "gen.py"],
        ),
        (vec![("", "c/"), ("a", "!c/")], vec![]),
        (vec![("", "/a/c/")], A_C.to_vec()),
        (
            vec![("", "b/**/*.py")],
            vec![
                "b/gen.py",
                "b/gen/gen.py",
                "b/gen/keep.py",
                "b/gen/x.py",
                "b/keep.py",
                "b/x.py",
            ],
        ),
    ]);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(crate::internals::proptest_cases(48)))]

    /// Random ignore files, nested repositories and member roots: a walk
    /// through a cache shared across roots sees what a fresh one does, and
    /// `respect_gitignore` only ever drops files.
    #[test]
    fn random_trees_walk_alike_through_a_shared_cache(
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
        pattern_tree(&base);
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
        let outcomes = walk_roots(&roots, &[]);
        let (respected, unfiltered) = outcomes.split_at(roots.len());
        for (kept, all) in respected.iter().zip(unfiltered) {
            prop_assert_eq!(&all.1, &[]);
            let all = paths(all);
            prop_assert!(paths(kept).iter().all(|path| all.contains(path)));
        }
    }
}
