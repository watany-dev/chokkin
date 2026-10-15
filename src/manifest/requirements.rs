//! `requirements*.txt` parsing.

use std::path::{Path, PathBuf};

use crate::path_util::rel_to_root;

use super::error::ManifestError;
use super::pep508::{is_python_whitespace, is_valid_marker, strip_python};
use super::pep508_util::{
    extract_egg_name, invalid, is_url_like, normalize_distribution_name, parse_pep508_requirement,
    parse_requirement, requirement_name,
};
use super::types::{DeclaredDependency, DependencyContext, DependencyOrigin};
use super::util::{path_is_within_root, read_text};
use super::warnings::ManifestWarning;

/// Result of parsing one or more requirements files.
#[derive(Debug, Default)]
pub(super) struct RequirementsExtraction {
    /// Parsed dependencies.
    pub dependencies: Vec<DeclaredDependency>,
    /// Version constraints from `-c` files (not dependency declarations).
    pub constraints: Vec<DeclaredDependency>,
    /// Non-fatal warnings.
    pub warnings: Vec<ManifestWarning>,
    /// Root-relative paths that were read.
    pub files_read: Vec<String>,
    /// Root-relative `-r`/`-c` candidate paths that were probed but absent.
    pub files_missing: Vec<String>,
    /// A file in the chain could not be decoded and was left out.
    pub skipped: bool,
}

#[derive(Clone, Copy)]
enum RequirementsParseMode {
    Dependencies,
    Constraints,
}

struct RequirementsParseContext<'a> {
    root: &'a Path,
    path: &'a Path,
    default_context: &'a DependencyContext,
    include_stack: &'a mut Vec<PathBuf>,
    result: &'a mut RequirementsExtraction,
    mode: RequirementsParseMode,
}

/// Parse a root-level requirements file by conventional name.
pub(super) fn extract_requirements_file(
    root: &Path,
    filename: &str,
    default_context: &DependencyContext,
) -> Result<RequirementsExtraction, ManifestError> {
    let path = root.join(filename);
    if !path.is_file() {
        return Ok(RequirementsExtraction::default());
    }

    let mut result = RequirementsExtraction::default();
    parse_requirements_root(root, &path, default_context, &mut result)?;
    Ok(result)
}

/// Root `*requirements*.txt` and `requirements*.in`, and `requirements/*.txt`
/// / `requirements/*.in` (werkzeug `requirements/tests.in`, urllib3
/// `emscripten-requirements.txt`) and `tests/requirements/` (django
/// `tests/requirements/py3.txt`, #749), and the same names under `docs/` and
/// `doc/` (pluggy `docs/requirements.txt`, #723). The fixed root names match
/// too. Every file adds its names; the rest declare dependencies only when
/// their path names a context (#679, #786).
pub(crate) fn extra_requirements_candidates(root: &Path) -> Vec<PathBuf> {
    let requirements_name = |stem: &str, ext: &str| {
        (ext == "txt" && stem.contains("requirements"))
            || (ext == "in" && stem.starts_with("requirements"))
    };
    let mut paths = files_matching(root, requirements_name);
    for dir in ["docs", "doc"] {
        paths.extend(files_matching(&root.join(dir), requirements_name));
    }
    for dir in [
        root.join("requirements"),
        root.join("tests").join("requirements"),
    ] {
        paths.extend(files_matching(&dir, |_, ext| ext == "txt" || ext == "in"));
    }
    paths
}

/// Files in `dir` whose `(stem, extension)` satisfy `matches`, sorted.
fn files_matching(dir: &Path, matches: impl Fn(&str, &str) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            let stem = path.file_stem().and_then(|stem| stem.to_str());
            let ext = path.extension().and_then(|ext| ext.to_str());
            stem.zip(ext).is_some_and(|(stem, ext)| matches(stem, ext)) && path.is_file()
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

/// Parse a requirements file that `setup.py` or `extra_requirements` reads,
/// keeping what was parsed before the error that stopped parsing.
pub(super) fn extract_requirements_path(
    root: &Path,
    path: &Path,
    default_context: &DependencyContext,
) -> (RequirementsExtraction, Result<(), ManifestError>) {
    let mut result = RequirementsExtraction::default();
    let outcome = parse_requirements_root(root, path, default_context, &mut result);
    (result, outcome)
}

fn parse_requirements_root(
    root: &Path,
    path: &Path,
    default_context: &DependencyContext,
    result: &mut RequirementsExtraction,
) -> Result<(), ManifestError> {
    let mut include_stack = Vec::new();
    parse_requirements_file_path(RequirementsParseContext {
        root,
        path,
        default_context,
        include_stack: &mut include_stack,
        result,
        mode: RequirementsParseMode::Dependencies,
    })
}

fn parse_requirements_file_path(
    mut ctx: RequirementsParseContext<'_>,
) -> Result<(), ManifestError> {
    let canonical = std::fs::canonicalize(ctx.path).unwrap_or_else(|_| ctx.path.to_path_buf());
    if ctx.include_stack.contains(&canonical) {
        let cycle = ctx
            .include_stack
            .iter()
            .chain(std::iter::once(&canonical))
            .map(|p| rel_to_root(ctx.root, p))
            .collect::<Vec<_>>()
            .join(" -> ");
        return Err(ManifestError::RequirementsCircularInclude { cycle });
    }

    let rel = origin_rel(ctx.root, ctx.path);
    ctx.result.files_read.push(rel.clone());
    let Some(contents) = read_text(ctx.path)? else {
        ctx.result.skipped = true;
        ctx.result
            .warnings
            .push(ManifestWarning::FileUndecodable { file: rel });
        return Ok(());
    };
    ctx.include_stack.push(canonical);

    for (line_number, line) in contents.lines().enumerate() {
        parse_requirements_line(&mut ctx, &rel, line, line_number)?;
    }

    ctx.include_stack.pop();
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn parse_requirements_line(
    ctx: &mut RequirementsParseContext<'_>,
    rel: &str,
    line: &str,
    line_number: usize,
) -> Result<(), ManifestError> {
    let line_no = u32::try_from(line_number + 1).unwrap_or(u32::MAX);
    let trimmed = strip_python(strip_comment(line));
    if trimmed.is_empty() {
        return Ok(());
    }

    if let Some(include_path) = flag_value(trimmed, "-r", "--requirement", ShortAttachPolicy::Allow)
    {
        let resolved = resolve_requirements_include(
            ctx.root,
            ctx.path,
            include_path,
            &mut ctx.result.files_missing,
        );
        let resolved_path = resolved.ok_or_else(|| ManifestError::RequirementsIncludeMissing {
            path: include_path.to_owned(),
        })?;
        let include_context = requirements_path_context(&origin_rel(ctx.root, &resolved_path))
            .unwrap_or_else(|| ctx.default_context.clone());
        return parse_requirements_file_path(RequirementsParseContext {
            root: ctx.root,
            path: &resolved_path,
            default_context: &include_context,
            include_stack: ctx.include_stack,
            result: ctx.result,
            mode: RequirementsParseMode::Dependencies,
        });
    }

    if let Some(constraint_path) =
        flag_value(trimmed, "-c", "--constraint", ShortAttachPolicy::Allow)
    {
        if let Some(resolved) = resolve_requirements_include(
            ctx.root,
            ctx.path,
            constraint_path,
            &mut ctx.result.files_missing,
        ) {
            parse_requirements_file_path(RequirementsParseContext {
                root: ctx.root,
                path: &resolved,
                default_context: ctx.default_context,
                include_stack: ctx.include_stack,
                result: ctx.result,
                mode: RequirementsParseMode::Constraints,
            })?;
        } else {
            ctx.result
                .warnings
                .push(ManifestWarning::RequirementsConstraintMissing {
                    path: constraint_path.to_owned(),
                });
        }
        return Ok(());
    }

    if matches!(ctx.mode, RequirementsParseMode::Dependencies)
        && let Some(editable) = editable_flag_value(trimmed)
    {
        push_editable_dependency(ctx.result, editable, ctx.default_context, rel, line_no);
        return Ok(());
    }

    if trimmed.starts_with('-') {
        ctx.result
            .warnings
            .push(ManifestWarning::RequirementsOptionIgnored {
                file: rel.to_owned(),
                line: line_no,
                raw: trimmed.to_owned(),
            });
        return Ok(());
    }

    let (parse, declared): (fn(&str, DependencyContext, DependencyOrigin) -> _, _) = match ctx.mode
    {
        RequirementsParseMode::Dependencies => (parse_requirement, &mut ctx.result.dependencies),
        RequirementsParseMode::Constraints => {
            (parse_pep508_requirement, &mut ctx.result.constraints)
        },
    };
    let origin = DependencyOrigin {
        file: rel.to_owned(),
        line: Some(line_no),
        label: rel.to_owned(),
    };
    match parse_pip_line(trimmed, ctx.default_context.clone(), origin, parse) {
        Ok(dep) => declared.push(dep),
        Err(warning) => ctx.result.warnings.push(warning),
    }

    Ok(())
}

/// Read a requirements line the way pip's `parse_req_from_line` does: the
/// marker is split off first (see [`split_pip_marker`]) and checked against the
/// marker grammar, and only the rest goes to `parse`. A line pip rejects,
/// whether for its marker or its requirement, is an invalid requirement.
fn parse_pip_line(
    line: &str,
    context: DependencyContext,
    origin: DependencyOrigin,
    parse: fn(
        &str,
        DependencyContext,
        DependencyOrigin,
    ) -> Result<DeclaredDependency, ManifestWarning>,
) -> Result<DeclaredDependency, ManifestWarning> {
    let Some((requirement, marker)) = split_pip_marker(line) else {
        return Err(invalid(line, origin));
    };
    let mut dep = parse(requirement, context, origin.clone()).map_err(|_| invalid(line, origin))?;
    if let Some(marker) = marker {
        dep.marker = Some(marker.to_owned());
    }
    Ok(dep)
}

/// pip splits a line at its first `;` (`"; "` when the line starts with a URL,
/// so a `;` inside the URL stays) and strips both sides with `str.strip`, so
/// any Unicode whitespace may surround the `;`. An empty marker is no marker
/// (`foo ;`), as in pip (#889). `None` when the marker is not PEP 508, which
/// pip rejects.
fn split_pip_marker(line: &str) -> Option<(&str, Option<&str>)> {
    let separator = if is_pip_url(line) { "; " } else { ";" };
    let Some((requirement, marker)) = line.split_once(separator) else {
        return Some((line, None));
    };
    let marker = strip_python(marker);
    if marker.is_empty() {
        return Some((strip_python(requirement), None));
    }
    is_valid_marker(marker).then_some((strip_python(requirement), Some(marker)))
}

/// pip's `is_url`: the text before the first `:` is a URL or VCS scheme pip
/// knows. `name @ url` is not a URL line.
fn is_pip_url(line: &str) -> bool {
    const SCHEMES: &[&str] = &[
        "http",
        "https",
        "file",
        "ftp",
        "bzr+http",
        "bzr+https",
        "bzr+ssh",
        "bzr+sftp",
        "bzr+ftp",
        "bzr+lp",
        "bzr+file",
        "git+http",
        "git+https",
        "git+ssh",
        "git+git",
        "git+file",
        "hg+file",
        "hg+http",
        "hg+https",
        "hg+ssh",
        "hg+static-http",
        "svn+ssh",
        "svn+http",
        "svn+https",
        "svn+svn",
        "svn+file",
    ];
    line.split_once(':').is_some_and(|(scheme, _)| {
        SCHEMES
            .iter()
            .any(|known| scheme.eq_ignore_ascii_case(known))
    })
}

fn push_editable_dependency(
    result: &mut RequirementsExtraction,
    path_spec: &str,
    default_context: &DependencyContext,
    rel: &str,
    line_no: u32,
) {
    let origin = DependencyOrigin {
        file: rel.to_owned(),
        line: Some(line_no),
        label: rel.to_owned(),
    };

    let (name, opaque) = if is_local_path(path_spec) {
        (String::new(), true)
    } else if let Some(egg) = extract_egg_name(path_spec) {
        (egg, false)
    } else if is_url_like(path_spec) {
        (String::new(), true)
    } else {
        (
            normalize_distribution_name(
                Path::new(path_spec)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(path_spec),
            ),
            true,
        )
    };

    result.dependencies.push(DeclaredDependency {
        name,
        extras: Vec::new(),
        marker: None,
        specifier: Some(path_spec.to_owned()),
        context: default_context.clone(),
        origin,
        opaque,
        included_via: Vec::new(),
    });
}

/// Distribution a requirements-file line declares, read the way [`extract_requirements_file`]
/// reads it; `None` for blank, option, and opaque (nameless) lines.
pub(crate) fn requirements_line_distribution(line: &str) -> Option<String> {
    let trimmed = strip_python(strip_comment(line));
    if let Some(editable) = editable_flag_value(trimmed) {
        return extract_egg_name(editable);
    }
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return None;
    }
    let (requirement, _) = split_pip_marker(trimmed)?;
    requirement_name(requirement)
}

/// pip-compatible: only `#` preceded by whitespace (or at line start) starts a comment.
#[must_use]
fn strip_comment(line: &str) -> &str {
    let mut prev_is_space = true;
    for (idx, ch) in line.char_indices() {
        if ch == '#' && prev_is_space {
            return &line[..idx];
        }
        prev_is_space = is_python_whitespace(ch);
    }
    line
}

/// pip-compatible flag parsing: long form before short; long form requires `=` or whitespace.
#[derive(Clone, Copy)]
enum ShortAttachPolicy {
    /// `-rfoo`, `-cfoo`, and spaced forms are accepted.
    Allow,
    /// `-efoo` is accepted only when the value looks like a path or URL.
    EditablePath,
}

#[must_use]
fn flag_value<'a>(
    line: &'a str,
    short: &str,
    long: &str,
    policy: ShortAttachPolicy,
) -> Option<&'a str> {
    if let Some(rest) = line.strip_prefix(long) {
        return match rest.as_bytes().first() {
            Some(b'=') => Some(rest[1..].trim()),
            Some(b' ' | b'\t') | None => Some(rest.trim()),
            _ => None,
        };
    }
    let rest = line.strip_prefix(short)?;
    let direct_attach =
        line.len() > short.len() && !matches!(line.as_bytes().get(short.len()), Some(b' ' | b'\t'));
    if direct_attach {
        let value = rest.trim();
        return match policy {
            ShortAttachPolicy::Allow => Some(value),
            ShortAttachPolicy::EditablePath if editable_short_attach_valid(value) => Some(value),
            ShortAttachPolicy::EditablePath => None,
        };
    }
    Some(rest.trim())
}

#[must_use]
fn editable_short_attach_valid(value: &str) -> bool {
    value.starts_with('.')
        || value.starts_with('/')
        || value.starts_with("git+")
        || value.contains("://")
}

#[must_use]
fn editable_flag_value(line: &str) -> Option<&str> {
    flag_value(line, "-e", "--editable", ShortAttachPolicy::EditablePath)
}

#[must_use]
fn is_local_path(spec: &str) -> bool {
    spec.starts_with('.')
}

/// Absent candidates are appended to `missing` so the manifest cache can
/// notice when one appears later (a new `-c` file, or a nearer include that
/// shadows the root one).
fn resolve_requirements_include(
    root: &Path,
    base: &Path,
    include: &str,
    missing: &mut Vec<String>,
) -> Option<PathBuf> {
    let candidates = [
        base.parent().map(|parent| parent.join(include)),
        Some(root.join(include)),
    ];

    for candidate in candidates.into_iter().flatten() {
        if !candidate.is_file() {
            missing.push(rel_to_root(root, &candidate));
            continue;
        }
        if path_is_within_root(root, &candidate) {
            return Some(candidate);
        }
    }
    None
}

/// The root-relative origin of a requirements file, with `..` folded so
/// `docs/../dev-requirements.txt` keeps the origin `dev-requirements.txt` it
/// has when read directly. Only the label folds: the file itself is opened
/// through the OS like pip does, symlinks included.
fn origin_rel(root: &Path, path: &Path) -> String {
    let mut folded = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {},
            std::path::Component::ParentDir
                if matches!(
                    folded.components().next_back(),
                    Some(std::path::Component::Normal(_))
                ) =>
            {
                folded.pop();
            },
            other => folded.push(other),
        }
    }
    rel_to_root(root, &folded)
}

/// The context a requirements path names (Phase 1.5 §4.B), from its whole
/// directory names and `-`/`_`/`.`-separated stem words, so `protests.txt` or
/// `documentsdb.txt` name none (#505). `optional` requirements are runtime
/// features installed on demand, like an extra (#786).
pub(super) fn requirements_path_context(path: &str) -> Option<DependencyContext> {
    let path = Path::new(path);
    let lower = |s: &std::ffi::OsStr| s.to_string_lossy().to_ascii_lowercase();
    let basename = path.file_name().map(lower).unwrap_or_default();
    if matches!(
        basename.as_str(),
        "requirements-packaging.txt" | "requirements-github-actions.txt"
    ) {
        return Some(DependencyContext::Group("dev".to_owned()));
    }

    let stem = path.file_stem().map(lower).unwrap_or_default();
    let mut words: Vec<String> = path
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(lower(name)),
            _ => None,
        })
        .collect();
    words.extend(stem.split(['-', '_', '.']).map(str::to_owned));
    let has_word = |candidates: &[&str]| words.iter().any(|w| candidates.contains(&w.as_str()));

    let group = if has_word(&["dev"]) {
        "dev"
    } else if has_word(&["docs", "doc", "documentation"]) {
        "docs"
    } else if has_word(&["tests", "test", "testing"]) {
        "tests"
    } else if has_word(&["optional", "optionals"]) {
        return Some(DependencyContext::OptionalExtra("optional".to_owned()));
    } else {
        return None;
    };
    Some(DependencyContext::Group(group.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_comment_preserves_url_fragment() {
        assert_eq!(
            strip_comment("pkg @ https://host/p.zip#sha256=abc"),
            "pkg @ https://host/p.zip#sha256=abc"
        );
    }

    #[test]
    fn strip_comment_removes_whitespace_prefixed_hash() {
        assert_eq!(strip_comment("requests  # runtime").trim(), "requests");
    }

    #[test]
    fn information_separators_are_whitespace_like_pip() {
        // pip's `str.strip` and its comment regex `\s+#` match U+001C..U+001F
        // too (#890).
        assert_eq!(strip_comment("requests\u{1f}# runtime"), "requests\u{1f}");
        assert_eq!(
            requirements_line_distribution("\u{1c}acme===1.0\u{1f}").as_deref(),
            Some("acme")
        );
        assert_eq!(
            requirements_line_distribution("git+https://h/r.git#egg=acme\u{1f}").as_deref(),
            Some("acme")
        );
    }

    #[test]
    fn origin_rel_folds_only_parents_of_named_directories() {
        // A leading `..` of a relative root has nothing to fold into.
        let root = Path::new("../proj");
        assert_eq!(
            origin_rel(root, Path::new("../proj/docs/../dev-requirements.txt")),
            "dev-requirements.txt"
        );
    }

    #[test]
    fn flag_value_parses_long_requirement_form() {
        assert_eq!(
            flag_value(
                "--requirement=dev.txt",
                "-r",
                "--requirement",
                ShortAttachPolicy::Allow
            ),
            Some("dev.txt")
        );
    }

    #[test]
    fn flag_value_parses_short_constraint_form() {
        assert_eq!(
            flag_value("-cdev.txt", "-c", "--constraint", ShortAttachPolicy::Allow),
            Some("dev.txt")
        );
    }

    #[test]
    fn editable_flag_rejects_example_pkg_false_positive() {
        assert_eq!(editable_flag_value("-example-pkg"), None);
    }

    #[test]
    fn include_context_infers_docs_from_nested_path() {
        assert!(matches!(
            requirements_path_context("requirements-docs.txt"),
            Some(DependencyContext::Group(group)) if group == "docs"
        ));
        assert!(matches!(
            requirements_path_context("requirements/requirements-documentation.txt"),
            Some(DependencyContext::Group(group)) if group == "docs"
        ));
    }

    #[test]
    fn include_context_matches_whole_words_only() {
        let group_of = |path: &str| match requirements_path_context(path) {
            Some(DependencyContext::Group(group)) => Some(group),
            _ => None,
        };
        assert_eq!(group_of("reqs/protests.txt"), None);
        assert_eq!(group_of("reqs/documentsdb.txt"), None);
        assert_eq!(group_of("reqs/developer.txt"), None);
        assert_eq!(group_of("tests/requirements.txt").as_deref(), Some("tests"));
        assert_eq!(group_of("requirements_test.txt").as_deref(), Some("tests"));
        assert_eq!(group_of("requirements-docs.txt").as_deref(), Some("docs"));
        assert_eq!(group_of("dev-requirements.txt").as_deref(), Some("dev"));
        assert_eq!(group_of("requirements-dev.txt").as_deref(), Some("dev"));
        assert_eq!(
            group_of("requirements-packaging.txt").as_deref(),
            Some("dev")
        );
        assert_eq!(requirements_path_context("requirements/typing.in"), None);
    }

    #[test]
    fn optional_requirements_are_an_extra() {
        for path in [
            "requirements-optional.txt",
            "requirements/requirements-optionals.txt",
        ] {
            assert!(
                matches!(
                    requirements_path_context(path),
                    Some(DependencyContext::OptionalExtra(extra)) if extra == "optional"
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn editable_short_attach_accepts_paths_and_urls_only() {
        for (value, expected) in [
            (".", true),
            ("./pkg", true),
            ("../pkg", true),
            ("/abs/pkg", true),
            ("git+https://host/r.git", true),
            ("https://host/p.zip", true),
            ("file:///abs/pkg", true),
            ("xample-pkg", false),
            ("pkg", false),
            ("hg+https", false),
        ] {
            assert_eq!(editable_short_attach_valid(value), expected, "{value:?}");
            let line = format!("-e{value}");
            assert_eq!(editable_flag_value(&line).is_some(), expected, "{line:?}");
        }
        // A bare flag is not an attached value, so the path check is skipped.
        assert_eq!(editable_flag_value("-e"), Some(""));
    }

    #[test]
    fn line_distribution_splits_the_marker_like_pip() {
        // #889: pip strips both sides of the first `;` with `str.strip`.
        for (line, expected) in [
            ("bar\u{a0}; os_name == \"nt\"", Some("bar")),
            ("foo;\u{a0}os_name == \"nt\"", Some("foo")),
            ("baz ;", Some("baz")),
            (
                "git+https://h/r.git#egg=acme ; python_version < \"3\"",
                Some("acme"),
            ),
            ("bad ; not a marker", None),
        ] {
            assert_eq!(
                requirements_line_distribution(line).as_deref(),
                expected,
                "{line:?}"
            );
        }
    }

    #[test]
    fn url_lines_split_only_at_semicolon_space() {
        assert_eq!(
            split_pip_marker("https://h/a;os_name"),
            Some(("https://h/a;os_name", None))
        );
        assert_eq!(
            split_pip_marker("Git+HTTPS://h/r.git#egg=a; os_name == \"nt\""),
            Some(("Git+HTTPS://h/r.git#egg=a", Some("os_name == \"nt\"")))
        );
        // `name @ url` is not a URL line for pip: any `;` splits it, and the
        // rest must be a marker.
        assert_eq!(
            split_pip_marker("a @ https://h/a;os_name==\"nt\""),
            Some(("a @ https://h/a", Some("os_name==\"nt\"")))
        );
        assert_eq!(split_pip_marker("a @ https://h/a;b"), None);
        // Bare `git:` / `svn:` are not in pip's scheme list either.
        assert_eq!(
            split_pip_marker("git://h/r.git#egg=a;os_name==\"nt\""),
            Some(("git://h/r.git#egg=a", Some("os_name==\"nt\"")))
        );
    }

    #[test]
    fn option_lines_never_name_a_distribution() {
        assert_eq!(
            requirements_line_distribution("--find-links https://host/#egg=acme"),
            None
        );
        assert_eq!(
            requirements_line_distribution("-e git+https://host/r.git#egg=acme"),
            Some("acme".to_owned())
        );
        assert_eq!(
            requirements_line_distribution("acme>=1  # pinned"),
            Some("acme".to_owned())
        );
    }

    #[test]
    fn undecodable_include_is_skipped_and_marks_the_chain_incomplete() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            temp.path().join("requirements.txt"),
            "requests\n-r extra.txt\n",
        )
        .expect("write requirements");
        std::fs::write(temp.path().join("extra.txt"), b"# Ren\xe9\nurllib3\n")
            .expect("write extra");

        let (result, outcome) = extract_requirements_path(
            temp.path(),
            &temp.path().join("requirements.txt"),
            &DependencyContext::Runtime,
        );
        outcome.expect("an include that is read is not an error");
        assert!(result.skipped);
        assert_eq!(
            result
                .dependencies
                .iter()
                .map(|dep| dep.name.as_str())
                .collect::<Vec<_>>(),
            ["requests"]
        );
        assert_eq!(result.files_read, ["requirements.txt", "extra.txt"]);
        assert_eq!(
            result.warnings,
            [ManifestWarning::FileUndecodable {
                file: "extra.txt".to_owned()
            }]
        );
    }

    #[test]
    fn local_path_is_any_dot_prefix() {
        for (spec, expected) in [
            (".", true),
            ("./pkg", true),
            ("../pkg", true),
            (".venv/pkg", true),
            ("pkg", false),
            ("/abs/pkg", false),
            ("", false),
        ] {
            assert_eq!(is_local_path(spec), expected, "{spec:?}");
        }
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// Words that never start a comment: `#` may appear only after a
        /// non-space character (`pkg#egg=x`), so `strip_comment` must keep them.
        fn comment_free_body() -> impl Strategy<Value = String> {
            prop::collection::vec("[^#\\s\\p{C}][^\\s\\p{C}]{0,10}", 0..6)
                .prop_map(|words| words.join(" "))
        }

        proptest! {
            #[test]
            fn strip_comment_returns_prefix(line in "\\PC{0,120}") {
                let stripped = strip_comment(&line);
                prop_assert!(line.starts_with(stripped));
            }

            #[test]
            fn strip_comment_is_idempotent(line in "\\PC{0,120}") {
                let once = strip_comment(&line);
                prop_assert_eq!(strip_comment(once), once);
            }

            #[test]
            fn strip_comment_keeps_comment_free_line(body in comment_free_body()) {
                prop_assert_eq!(strip_comment(&body), body.as_str());
            }

            #[test]
            fn strip_comment_cuts_at_first_comment_start(
                body in comment_free_body(),
                sep in "[ \t]{1,3}",
                comment in "\\PC{0,40}",
            ) {
                let line = format!("{body}{sep}#{comment}");
                prop_assert_eq!(strip_comment(&line), format!("{body}{sep}"));
                let bare = format!("#{comment}");
                prop_assert_eq!(strip_comment(&bare), "");
            }

            #[test]
            fn flag_value_parses_long_forms(value in "[^\\r\\n=][^\\r\\n]{0,60}") {
                let equals_form = format!("--requirement={value}");
                prop_assert_eq!(
                    flag_value(&equals_form, "-r", "--requirement", ShortAttachPolicy::Allow),
                    Some(value.trim())
                );
                let space_form = format!("--requirement {value}");
                prop_assert_eq!(
                    flag_value(&space_form, "-r", "--requirement", ShortAttachPolicy::Allow),
                    Some(value.trim())
                );
            }

            #[test]
            fn flag_value_parses_short_forms(value in "[^\\r\\n]{0,60}") {
                let attached_form = format!("-r{value}");
                prop_assert_eq!(
                    flag_value(&attached_form, "-r", "--requirement", ShortAttachPolicy::Allow),
                    Some(value.trim())
                );
                let spaced_form = format!("-r {value}");
                prop_assert_eq!(
                    flag_value(&spaced_form, "-r", "--requirement", ShortAttachPolicy::Allow),
                    Some(value.trim())
                );
            }

            #[test]
            fn flag_value_rejects_long_prefix_followed_by_ident(
                ch in proptest::char::ranges(vec!['a'..='z', 'A'..='Z', '0'..='9'].into()),
                rest in "[^\\r\\n]{0,30}",
            ) {
                let line = format!("--requirement{ch}{rest}");
                prop_assert_eq!(
                    flag_value(&line, "-r", "--requirement", ShortAttachPolicy::Allow),
                    None
                );
            }

            #[test]
            fn editable_flag_value_never_panics(line in "\\PC{0,120}") {
                let _ = editable_flag_value(&line);
            }

            #[test]
            fn editable_flag_accepts_explicit_forms(path in "[a-z][a-z0-9/_.-]{0,30}") {
                let expected = format!("./{path}");
                let short_form = format!("-e ./{path}");
                prop_assert_eq!(editable_flag_value(&short_form), Some(expected.as_str()));
                let long_form = format!("--editable=./{path}");
                prop_assert_eq!(editable_flag_value(&long_form), Some(expected.as_str()));
            }
        }
    }
}
