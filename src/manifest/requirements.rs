//! `requirements*.txt` parsing.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::path_util::{is_file_named, rel_to_root};

use super::error::ManifestError;
use super::pep508::{is_python_whitespace, is_valid_marker, strip_python};
use super::pep508_util::{
    extract_egg_name, has_invalid_egg, invalid, is_url_like, normalize_distribution_name,
    parse_pep508_requirement, parse_requirement, requirement_name,
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
    if !is_file_named(root, filename) {
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

    for line in join_continued_lines(split_lines_inclusive(&contents).map(strip_line_break)) {
        parse_requirements_line(&mut ctx, &rel, &line.text, line.first)?;
    }

    ctx.include_stack.pop();
    Ok(())
}

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
    let origin = || DependencyOrigin {
        file: rel.to_owned(),
        line: Some(line_no),
        label: rel.to_owned(),
    };

    // pip reads every line as `break_args_options`, then `shlex.split` and
    // `optparse` over the options (#927). A `-r` / `-c` / `-e` line is an
    // option line whose flag takes one value, so `-r base.txt --pre` names
    // `base.txt` (#964). A line pip rejects is an invalid requirement, as a
    // requirement line with bad options is, and the rest of the file is read.
    let (requirement, options) = break_args_options(trimmed);
    let Some(options) = pip_line_options(options) else {
        ctx.result
            .warnings
            .push(invalid(trimmed, ctx.default_context.clone(), origin()));
        return Ok(());
    };

    if !requirement.is_empty() {
        let (parse, declared): (fn(&str, DependencyContext, DependencyOrigin) -> _, _) =
            match ctx.mode {
                RequirementsParseMode::Dependencies => {
                    (parse_requirement, &mut ctx.result.dependencies)
                },
                RequirementsParseMode::Constraints => {
                    (parse_pep508_requirement, &mut ctx.result.constraints)
                },
            };
        match parse_pip_line(requirement, ctx.default_context.clone(), origin(), parse) {
            Ok(dep) => declared.push(dep),
            Err(warning) => ctx.result.warnings.push(warning),
        }
        return Ok(());
    }

    // pip's `ParsedLine`: `-e` makes the line a requirement, so a `-r` or
    // `-c` beside it is ignored; otherwise `-r` wins over `-c`.
    if let Some(editable) = options.editable {
        if matches!(ctx.mode, RequirementsParseMode::Dependencies) {
            // pip builds a `Link` from a non-path `-e` value, which rejects
            // an `#egg=` that is not a project name (#929).
            if !is_local_path(&editable) && has_invalid_egg(&editable) {
                ctx.result
                    .warnings
                    .push(invalid(trimmed, ctx.default_context.clone(), origin()));
            } else {
                push_editable_dependency(ctx.result, &editable, ctx.default_context, rel, line_no);
            }
            return Ok(());
        }
    } else if let Some(include_path) = options.requirement {
        return follow_requirements_include(ctx, &include_path);
    } else if let Some(constraint_path) = options.constraint {
        return follow_constraints_include(ctx, rel, constraint_path);
    }

    ctx.result
        .warnings
        .push(ManifestWarning::RequirementsOptionIgnored {
            file: rel.to_owned(),
            line: line_no,
            raw: trimmed.to_owned(),
        });
    Ok(())
}

/// Read the file a `-r` names, with the context its path implies; its
/// absence stops the file, as it does pip.
fn follow_requirements_include(
    ctx: &mut RequirementsParseContext<'_>,
    include_path: &str,
) -> Result<(), ManifestError> {
    let resolved = resolve_requirements_include(
        ctx.root,
        ctx.path,
        include_path,
        &mut ctx.result.files_missing,
    );
    let resolved_path = match resolved {
        Include::Found(path) => path,
        Include::Missing => {
            return Err(ManifestError::RequirementsIncludeMissing {
                path: include_path.to_owned(),
            });
        },
        Include::OutsideRoot => {
            return Err(ManifestError::RequirementsIncludeOutsideRoot {
                path: include_path.to_owned(),
            });
        },
    };
    let include_context = requirements_path_context(&origin_rel(ctx.root, &resolved_path))
        .unwrap_or_else(|| ctx.default_context.clone());
    parse_requirements_file_path(RequirementsParseContext {
        root: ctx.root,
        path: &resolved_path,
        default_context: &include_context,
        include_stack: ctx.include_stack,
        result: ctx.result,
        mode: RequirementsParseMode::Dependencies,
    })
}

/// Read the file a `-c` names as constraints; its absence is a warning.
fn follow_constraints_include(
    ctx: &mut RequirementsParseContext<'_>,
    rel: &str,
    constraint_path: String,
) -> Result<(), ManifestError> {
    let resolved = match resolve_requirements_include(
        ctx.root,
        ctx.path,
        &constraint_path,
        &mut ctx.result.files_missing,
    ) {
        Include::Found(resolved) => resolved,
        Include::Missing => {
            ctx.result
                .warnings
                .push(ManifestWarning::RequirementsConstraintMissing {
                    path: constraint_path,
                });
            return Ok(());
        },
        Include::OutsideRoot => {
            ctx.result.warnings.push(ManifestWarning::PathOutsideRoot {
                file: rel.to_owned(),
                field: "-c".to_owned(),
                path: constraint_path,
            });
            return Ok(());
        },
    };
    parse_requirements_file_path(RequirementsParseContext {
        root: ctx.root,
        path: &resolved,
        default_context: ctx.default_context,
        include_stack: ctx.include_stack,
        result: ctx.result,
        mode: RequirementsParseMode::Constraints,
    })
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
        return Err(invalid(line, context, origin));
    };
    // Rewrite the warning's `raw` rather than clone `origin` up front: that
    // clone cost every valid line two allocations.
    let mut dep = parse(requirement, context, origin).map_err(|mut warning| {
        if let ManifestWarning::InvalidRequirementLine { raw, .. } = &mut warning {
            line.clone_into(raw);
        }
        warning
    })?;
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
    // A `char` for `;`: a `&str` pattern builds a two-way searcher per line.
    let split = if is_pip_url(line) {
        line.split_once("; ")
    } else {
        line.split_once(';')
    };
    let Some((requirement, marker)) = split else {
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
        unportable_key: None,
    });
}

/// Distribution a requirements-file line declares, read the way [`extract_requirements_file`]
/// reads it; `None` for blank, option, and opaque (nameless) lines.
pub(crate) fn requirements_line_distribution(line: &str) -> Option<String> {
    let trimmed = strip_python(strip_comment(line));
    let (requirement, options) = break_args_options(trimmed);
    let options = pip_line_options(options)?;
    if requirement.is_empty() {
        return extract_egg_name(&options.editable?);
    }
    let (requirement, _) = split_pip_marker(requirement)?;
    requirement_name(requirement)
}

/// pip splits a requirements file with `str.splitlines()` (#1154): every
/// Python line boundary ends a line, a lone `\r` among them. Each line keeps
/// its terminator, so the pieces concatenate back to `contents`.
pub(crate) fn split_lines_inclusive(contents: &str) -> impl Iterator<Item = &str> {
    let mut rest = contents;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = rest
            .char_indices()
            .find(|&(_, c)| is_line_boundary(c))
            .map_or(rest.len(), |(index, c)| {
                let after = index + c.len_utf8();
                if c == '\r' && rest[after..].starts_with('\n') {
                    after + 1
                } else {
                    after
                }
            });
        let (line, tail) = rest.split_at(end);
        rest = tail;
        Some(line)
    })
}

/// A line from [`split_lines_inclusive`] without its terminator.
pub(crate) fn strip_line_break(line: &str) -> &str {
    line.strip_suffix("\r\n")
        .or_else(|| line.strip_suffix(is_line_boundary))
        .unwrap_or(line)
}

/// The characters Python's `str.splitlines()` splits on.
const fn is_line_boundary(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}'..='\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// One line of a requirements file after pip's `join_lines`.
pub(crate) struct LogicalLine<'a> {
    /// 0-based index of its first physical line, which names it in warnings.
    pub first: usize,
    pub span: usize,
    pub text: Cow<'a, str>,
}

/// pip's `join_lines`: a line ending in `\` is joined to the next with the
/// `\` stripped (`str.strip("\\")`, both ends), unless it is a comment line
/// (`#` after only whitespace). A comment line ends a continuation and is
/// joined with a space before it, so the comment is cut later. Comments are
/// stripped from the joined line, as pip does, so a `#` before a `\`
/// swallows the line it continues into.
pub(crate) fn join_continued_lines<'a>(
    lines: impl IntoIterator<Item = &'a str>,
) -> Vec<LogicalLine<'a>> {
    let mut joined = Vec::new();
    let mut pending: Option<(usize, String)> = None;
    let mut count = 0;
    for (index, line) in lines.into_iter().enumerate() {
        count = index + 1;
        let comment = line
            .trim_start_matches(is_python_whitespace)
            .starts_with('#');
        if line.ends_with('\\') && !comment {
            let part = line.trim_matches('\\');
            match &mut pending {
                Some((_, text)) => text.push_str(part),
                None => pending = Some((index, part.to_owned())),
            }
            continue;
        }
        joined.push(match pending.take() {
            Some((first, mut text)) => {
                if comment {
                    text.push(' ');
                }
                text.push_str(line);
                LogicalLine {
                    first,
                    span: count - first,
                    text: Cow::Owned(text),
                }
            },
            None => LogicalLine {
                first: index,
                span: 1,
                text: Cow::Borrowed(line),
            },
        });
    }
    if let Some((first, text)) = pending {
        joined.push(LogicalLine {
            first,
            span: count - first,
            text: Cow::Owned(text),
        });
    }
    joined
}

/// Whether a requirements line (comment included) carries `--hash`. A line
/// pip rejects counts when it names `--hash` anywhere: the `--fix` guard errs
/// toward leaving a hash-pinned file alone.
pub(crate) fn requirements_line_hashed(line: &str) -> bool {
    let trimmed = strip_python(strip_comment(line));
    let (_, options) = break_args_options(trimmed);
    pip_line_options(options).map_or_else(|| trimmed.contains("--hash"), |options| options.hashed)
}

/// pip's `break_args_options`: the requirement is the `' '`-separated tokens
/// before the first that starts with `-`, and the options are the rest. Only
/// a space separates, so `foo\t--hash=...` is all requirement and invalid.
fn break_args_options(line: &str) -> (&str, &str) {
    let mut start = 0;
    for token in line.split(' ') {
        if token.starts_with('-') {
            let requirement = &line[..start];
            return (
                requirement.strip_suffix(' ').unwrap_or(requirement),
                &line[start..],
            );
        }
        start += token.len() + 1;
    }
    (line, "")
}

/// pip's requirements-file options (`SUPPORTED_OPTIONS` and
/// `SUPPORTED_OPTIONS_REQ`): long name, short name, and whether it takes a
/// value. A requirement line may carry any of them; pip uses only `--hash`,
/// `--config-settings` and `--global-option` there.
const PIP_LINE_OPTIONS: &[(&str, Option<char>, bool)] = &[
    ("index-url", Some('i'), true),
    ("pypi-url", None, true),
    ("extra-index-url", None, true),
    ("no-index", None, false),
    ("constraint", Some('c'), true),
    ("requirement", Some('r'), true),
    ("editable", Some('e'), true),
    ("find-links", Some('f'), true),
    ("no-binary", None, true),
    ("only-binary", None, true),
    ("prefer-binary", None, false),
    ("require-hashes", None, false),
    ("pre", None, false),
    ("trusted-host", None, true),
    ("use-feature", None, true),
    ("global-option", None, true),
    ("hash", None, true),
    ("config-settings", Some('C'), true),
];

/// What the options of a requirements line say, read as pip reads them
/// (`shlex.split`, then `optparse`). pip uses only the first value of a
/// repeated `-r` / `-c` / `-e` (`opts.requirements[0]`).
#[derive(Debug, Default, PartialEq, Eq)]
struct PipLineOptions {
    hashed: bool,
    requirement: Option<String>,
    constraint: Option<String>,
    editable: Option<String>,
}

/// pip's reading of a requirements line's options: `--name=value` or
/// `--name value`, a unique prefix of a long name, `-xvalue` or `-x value`;
/// positional tokens are ignored. `None` where pip fails the line: an
/// unclosed quote, an unknown or ambiguous option, a missing or unexpected
/// value, and a `--hash` or `--config-settings` value its callback rejects.
fn pip_line_options(options: &str) -> Option<PipLineOptions> {
    let tokens = shlex_split(options)?;
    let mut tokens = tokens.iter();
    let mut parsed = PipLineOptions::default();
    while let Some(token) = tokens.next() {
        if token == "--" {
            break;
        }
        let (option, attached) = if let Some(long) = token.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            let option = PIP_LINE_OPTIONS
                .iter()
                .find(|(known, ..)| *known == name)
                .or_else(|| {
                    let mut prefixed = PIP_LINE_OPTIONS
                        .iter()
                        .filter(|(known, ..)| known.starts_with(name));
                    prefixed.next().filter(|_| prefixed.next().is_none())
                })?;
            (option, value.map(str::to_owned))
        } else if let Some(short) = token.strip_prefix('-').filter(|short| !short.is_empty()) {
            let mut chars = short.chars();
            let flag = chars.next();
            let option = PIP_LINE_OPTIONS
                .iter()
                .find(|(_, known, _)| *known == flag)?;
            let rest = chars.as_str();
            (option, (!rest.is_empty()).then(|| rest.to_owned()))
        } else {
            continue;
        };
        let &(name, _, takes_value) = option;
        let value = match (takes_value, attached) {
            (false, None) => None,
            (false, Some(_)) => return None,
            (true, Some(value)) => Some(value),
            (true, None) => Some(tokens.next()?.clone()),
        };
        let valid = match (name, value.as_deref()) {
            ("hash", Some(value)) => value
                .split_once(':')
                .is_some_and(|(algorithm, _)| matches!(algorithm, "sha256" | "sha384" | "sha512")),
            ("config-settings", Some(value)) => value.contains('='),
            _ => true,
        };
        if !valid {
            return None;
        }
        let first = match name {
            "hash" => {
                parsed.hashed = true;
                continue;
            },
            "requirement" => &mut parsed.requirement,
            "constraint" => &mut parsed.constraint,
            "editable" => &mut parsed.editable,
            _ => continue,
        };
        if first.is_none() {
            *first = value;
        }
    }
    Some(parsed)
}

/// Python's `shlex.split` (POSIX mode, no comments); `None` on an unclosed
/// quote or a trailing `\\`, where it raises.
fn shlex_split(text: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut in_token = false;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match ch {
            ' ' | '\t' | '\r' | '\n' => {
                if in_token {
                    tokens.push(std::mem::take(&mut token));
                    in_token = false;
                }
                continue;
            },
            '\\' => token.push(chars.next()?),
            '\'' => loop {
                match chars.next()? {
                    '\'' => break,
                    other => token.push(other),
                }
            },
            '"' => loop {
                match chars.next()? {
                    '"' => break,
                    '\\' => match chars.next()? {
                        escaped @ ('"' | '\\') => token.push(escaped),
                        other => {
                            token.push('\\');
                            token.push(other);
                        },
                    },
                    other => token.push(other),
                }
            },
            other => token.push(other),
        }
        in_token = true;
    }
    if in_token {
        tokens.push(token);
    }
    Some(tokens)
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

#[must_use]
fn is_local_path(spec: &str) -> bool {
    spec.starts_with('.')
}

/// Where a `-r` / `-c` reference led.
enum Include {
    Found(PathBuf),
    /// No candidate exists.
    Missing,
    /// A candidate exists but resolves outside the root (#981), so it is not
    /// read; "not found" would send the user hunting for a typo.
    OutsideRoot,
}

/// Absent candidates are appended to `missing` so the manifest cache can
/// notice when one appears later (a new `-c` file, or a nearer include that
/// shadows the root one).
fn resolve_requirements_include(
    root: &Path,
    base: &Path,
    include: &str,
    missing: &mut Vec<String>,
) -> Include {
    let candidates = [
        base.parent().map(|parent| parent.join(include)),
        Some(root.join(include)),
    ];

    let mut outside = false;
    for candidate in candidates.into_iter().flatten() {
        if !candidate.is_file() {
            missing.push(rel_to_root(root, &candidate));
            continue;
        }
        if path_is_within_root(root, &candidate) {
            return Include::Found(candidate);
        }
        outside = true;
    }
    if outside {
        Include::OutsideRoot
    } else {
        Include::Missing
    }
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
    fn split_lines_inclusive_matches_python_splitlines() {
        let contents = "a\rb\r\nc\nd\u{b}e\u{2028}f\r\rg";
        let lines: Vec<&str> = split_lines_inclusive(contents).collect();
        assert_eq!(
            lines,
            [
                "a\r",
                "b\r\n",
                "c\n",
                "d\u{b}",
                "e\u{2028}",
                "f\r",
                "\r",
                "g"
            ]
        );
        assert_eq!(lines.concat(), contents);
        let stripped: Vec<&str> = lines.into_iter().map(strip_line_break).collect();
        assert_eq!(stripped, ["a", "b", "c", "d", "e", "f", "", "g"]);
        assert_eq!(split_lines_inclusive("").count(), 0);
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
    fn pip_line_options_reads_flag_values_like_optparse() {
        let read = |options: &str| pip_line_options(options).expect(options);
        for options in [
            "--requirement=dev.txt",
            "--requirement dev.txt",
            "--requirem dev.txt",
            "-r dev.txt",
            "-rdev.txt",
            "-r 'dev.txt'",
        ] {
            assert_eq!(
                read(options).requirement.as_deref(),
                Some("dev.txt"),
                "{options:?}"
            );
        }
        assert_eq!(read("-cdev.txt").constraint.as_deref(), Some("dev.txt"));
        assert_eq!(read("--editable ./pkg").editable.as_deref(), Some("./pkg"));
        // pip uses only the first value of a repeated flag.
        assert_eq!(
            read("-r a.txt -r b.txt").requirement.as_deref(),
            Some("a.txt")
        );
        assert_eq!(
            read("-r a.txt --pre -c pins.txt"),
            PipLineOptions {
                hashed: false,
                requirement: Some("a.txt".to_owned()),
                constraint: Some("pins.txt".to_owned()),
                editable: None,
            }
        );
        // optparse fails the line: a missing value, an unknown option, an
        // ambiguous prefix (`--req` is `--requirement` and `--require-hashes`).
        for options in [
            "-r",
            "--requirement",
            "--requirementx dev.txt",
            "--no- dev.txt",
        ] {
            assert_eq!(pip_line_options(options), None, "{options:?}");
        }
    }

    #[test]
    fn editable_short_form_is_read_like_pip_without_a_path_heuristic() {
        // pip reads `-example-pkg` as `-e xample-pkg` (#964): an opaque
        // editable, not a package named `example-pkg` and not an error.
        let result = parse_requirements_text("-example-pkg\n");
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        assert_eq!(declared(&result), vec![("xample-pkg", Some(1))]);
        assert!(result.dependencies[0].opaque);
        assert_eq!(
            result.dependencies[0].specifier.as_deref(),
            Some("xample-pkg")
        );
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

    fn parse_requirements_text(text: &str) -> RequirementsExtraction {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("requirements.txt");
        std::fs::write(&path, text).expect("write requirements");
        let (result, outcome) =
            extract_requirements_path(temp.path(), &path, &DependencyContext::Runtime);
        outcome.expect("parse requirements");
        result
    }

    fn declared(result: &RequirementsExtraction) -> Vec<(&str, Option<u32>)> {
        result
            .dependencies
            .iter()
            .map(|dep| (dep.name.as_str(), dep.origin.line))
            .collect()
    }

    #[test]
    fn continued_and_hashed_lines_declare_their_requirement() {
        // #927: `pip-compile --generate-hashes` output.
        let result = parse_requirements_text(
            "click==8.1.7 \\\n    --hash=sha256:ae74fb96 \\\n    --hash=sha256:aaaa\n\
             rich==13.7.0 --hash=sha256:6da14c10\n\
             bar ; os_name == \"nt\" --hash=sha256:abc\n",
        );
        assert_eq!(result.warnings, []);
        assert_eq!(
            declared(&result),
            [("click", Some(1)), ("rich", Some(4)), ("bar", Some(5))]
        );
        assert_eq!(
            result.dependencies[2].marker.as_deref(),
            Some("os_name == \"nt\"")
        );
    }

    #[test]
    fn comment_lines_end_a_continuation_like_pip() {
        // A comment line never continues, even when it ends in `\`, and one
        // after a continued line ends it.
        let result = parse_requirements_text("# header \\\nfoo \\\n    # note\nbar\nbaz \\");
        assert_eq!(result.warnings, []);
        assert_eq!(
            declared(&result),
            [("foo", Some(2)), ("bar", Some(4)), ("baz", Some(5))]
        );
    }

    #[test]
    fn requirement_options_pip_accepts_are_dropped() {
        let result = parse_requirements_text(
            "a --hash sha256:abc\n\
             b --ha=sha512:abc\n\
             c -C key=value --config-settings=k=v --global-option=x\n\
             d --index-url https://host/simple --pre\n\
             e --hash=sha256:abc stray\n\
             f --hash='sha384:abc'\n\
             g --pypi-url https://host/simple\n",
        );
        assert_eq!(result.warnings, []);
        assert_eq!(
            result
                .dependencies
                .iter()
                .map(|dep| dep.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c", "d", "e", "f", "g"]
        );
    }

    #[test]
    fn options_pip_rejects_keep_the_line_invalid() {
        for line in [
            "foo --unknown",
            "foo --hash",
            "foo --hash=md5:abc",
            "foo --hash=sha256",
            "foo --hash='sha256:abc",
            "foo --no-index=1",
            "foo -C key",
            "foo --pr",
            "foo\t--hash=sha256:abc",
        ] {
            let result = parse_requirements_text(line);
            assert_eq!(result.dependencies.len(), 0, "{line:?}");
            assert_eq!(
                result.warnings,
                [ManifestWarning::InvalidRequirementLine {
                    file: "requirements.txt".to_owned(),
                    line: Some(1),
                    label: "requirements.txt".to_owned(),
                    raw: line.to_owned(),
                    context: DependencyContext::Runtime,
                }],
                "{line:?}"
            );
        }
    }

    #[test]
    fn invalid_egg_keeps_the_line_invalid() {
        // #929: pip 25 rejects an egg that is not a project name, on `-e`
        // lines too; a valid egg still names the dependency.
        for line in [
            "git+https://h/r.git#egg=acme;python_version<\"3\"",
            "-e \"git+https://h/r.git#egg=with space\"",
            "-e git+https://h/r.git#egg=acme[tests]",
        ] {
            let result = parse_requirements_text(line);
            assert_eq!(result.dependencies.len(), 0, "{line:?}");
            assert_eq!(
                result.warnings,
                [ManifestWarning::InvalidRequirementLine {
                    file: "requirements.txt".to_owned(),
                    line: Some(1),
                    label: "requirements.txt".to_owned(),
                    raw: line.to_owned(),
                    context: DependencyContext::Runtime,
                }],
                "{line:?}"
            );
            assert_eq!(requirements_line_distribution(line), None, "{line:?}");
        }
        let result = parse_requirements_text("-e git+https://h/r.git#egg=good-name");
        assert!(result.warnings.is_empty());
        assert_eq!(result.dependencies[0].name, "good-name");
    }

    #[test]
    fn flag_lines_take_only_the_flag_value() {
        // #964: pip reads `-r` / `-c` / `-e` lines with `optparse` too, so an
        // option after the value is not part of it, and quotes group a path.
        let temp = tempfile::tempdir().expect("temp dir");
        let write = |name: &str, text: &str| {
            std::fs::write(temp.path().join(name), text).expect("write");
        };
        write(
            "requirements.txt",
            "-r base.txt --pre\n\
             --requirement=more.txt --no-index\n\
             -r \"my reqs.txt\"\n\
             -c pins.txt --pre\n\
             -e git+https://h/foo.git#egg=foo --config-settings=k=v\n\
             -e ./pkg -r pins.txt\n",
        );
        write("base.txt", "a\n");
        write("more.txt", "b\n");
        write("my reqs.txt", "c\n");
        write("pins.txt", "d==1\n");

        let (result, outcome) = extract_requirements_path(
            temp.path(),
            &temp.path().join("requirements.txt"),
            &DependencyContext::Runtime,
        );
        outcome.expect("every include resolves");
        assert_eq!(result.warnings, []);
        assert_eq!(
            declared(&result),
            [
                ("a", Some(1)),
                ("b", Some(1)),
                ("c", Some(1)),
                ("foo", Some(5)),
                ("", Some(6)),
            ]
        );
        assert_eq!(
            result
                .constraints
                .iter()
                .map(|dep| dep.name.as_str())
                .collect::<Vec<_>>(),
            ["d"]
        );
        assert_eq!(result.dependencies[4].specifier.as_deref(), Some("./pkg"));
    }

    #[test]
    fn flag_lines_pip_rejects_are_invalid_and_do_not_stop_the_file() {
        // #964: a `-r` line pip fails on is one invalid line, not exit 2.
        for line in [
            "-r base.txt --unknown",
            "-r",
            "-r 'base.txt",
            "-c base.txt --hash=md5:x",
            "-e ./pkg --no-index=1",
        ] {
            let result = parse_requirements_text(&format!("{line}\nrequests\n"));
            assert_eq!(declared(&result), [("requests", Some(2))], "{line:?}");
            assert_eq!(
                result.warnings,
                [ManifestWarning::InvalidRequirementLine {
                    file: "requirements.txt".to_owned(),
                    line: Some(1),
                    label: "requirements.txt".to_owned(),
                    raw: line.to_owned(),
                    context: DependencyContext::Runtime,
                }],
                "{line:?}"
            );
            assert!(result.files_missing.is_empty(), "{line:?}");
        }
    }

    #[test]
    fn path_and_archive_lines_are_opaque_like_editable_paths() {
        // #963: pip installs these from a directory or an archive, and spec §4
        // records a local path as an opaque dependency, as it does `-e ./path`.
        let result = parse_requirements_text(
            "../../../llama-index-core\n\
             ./pkg ; os_name == \"nt\"\n\
             .\n\
             pkg/sub\n\
             wheels/foo-1.0-py3-none-any.whl\n\
             foo-1.0-py3-none-any.whl\n\
             foo.whl[tests]\n\
             C:\\pkg\n\
             requests>=2\n",
        );
        assert_eq!(result.warnings, []);
        assert_eq!(
            declared(&result),
            [
                ("", Some(1)),
                ("", Some(2)),
                ("", Some(3)),
                ("", Some(4)),
                ("", Some(5)),
                ("", Some(6)),
                ("", Some(7)),
                ("", Some(8)),
                ("requests", Some(9)),
            ]
        );
        let (paths, rest) = result.dependencies.split_at(8);
        for dep in paths {
            assert!(dep.opaque, "{dep:?}");
        }
        assert!(!rest[0].opaque);
        assert_eq!(
            paths[0].specifier.as_deref(),
            Some("../../../llama-index-core")
        );
        assert_eq!(paths[1].specifier.as_deref(), Some("./pkg"));
        assert_eq!(paths[1].marker.as_deref(), Some("os_name == \"nt\""));
        assert_eq!(paths[7].specifier.as_deref(), Some("C:\\pkg"));
        assert_eq!(requirements_line_distribution("./pkg"), None);
    }

    #[test]
    fn constraints_keep_path_lines_invalid() {
        // pip rejects a path in a constraints file ("Links are not allowed as
        // constraints"), so the opaque reading applies to requirements only.
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            temp.path().join("requirements.txt"),
            "-c pins.txt\nrequests\n",
        )
        .expect("write");
        std::fs::write(temp.path().join("pins.txt"), "./pkg\nd==1\n").expect("write");
        let (result, outcome) = extract_requirements_path(
            temp.path(),
            &temp.path().join("requirements.txt"),
            &DependencyContext::Runtime,
        );
        outcome.expect("constraints resolve");
        assert_eq!(declared(&result), [("requests", Some(2))]);
        assert_eq!(
            result
                .constraints
                .iter()
                .map(|dep| dep.name.as_str())
                .collect::<Vec<_>>(),
            ["d"]
        );
        assert_eq!(
            result.warnings,
            [ManifestWarning::InvalidRequirementLine {
                file: "pins.txt".to_owned(),
                line: Some(1),
                label: "pins.txt".to_owned(),
                raw: "./pkg".to_owned(),
                context: DependencyContext::Runtime,
            }]
        );
    }

    #[test]
    fn line_distribution_takes_only_the_editable_value() {
        assert_eq!(
            requirements_line_distribution(
                "-e git+https://h/foo.git#egg=foo --config-settings=k=v"
            )
            .as_deref(),
            Some("foo")
        );
        assert_eq!(
            requirements_line_distribution("--editable=git+https://h/foo.git#egg=foo --pre")
                .as_deref(),
            Some("foo")
        );
        assert_eq!(requirements_line_distribution("-r base.txt --pre"), None);
        assert_eq!(
            requirements_line_distribution("-e git+https://h/foo.git#egg=foo --unknown"),
            None
        );
    }

    #[test]
    fn line_distribution_drops_requirement_options() {
        assert_eq!(
            requirements_line_distribution("rich==13.7.0 --hash=sha256:abc").as_deref(),
            Some("rich")
        );
        assert_eq!(
            requirements_line_distribution("rich==13.7.0 --unknown"),
            None
        );
    }

    #[test]
    fn shlex_split_follows_posix_quoting() {
        assert_eq!(
            shlex_split(r#"--a 'b c' "d\"e" f\ g "h\i" ''"#),
            Some(vec![
                "--a".to_owned(),
                "b c".to_owned(),
                "d\"e".to_owned(),
                "f g".to_owned(),
                "h\\i".to_owned(),
                String::new(),
            ])
        );
        assert_eq!(shlex_split("'open"), None);
        assert_eq!(shlex_split("\"open"), None);
        assert_eq!(shlex_split("trailing\\"), None);
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
            fn pip_line_options_reads_every_flag_form(
                value in "[A-Za-z0-9_./][A-Za-z0-9_./-]{0,40}",
            ) {
                for options in [
                    format!("-r{value}"),
                    format!("-r {value}"),
                    format!("--requirement={value}"),
                    format!("--requirement {value}"),
                ] {
                    let parsed = pip_line_options(&options);
                    prop_assert_eq!(
                        parsed.as_ref().and_then(|parsed| parsed.requirement.as_deref()),
                        Some(value.as_str()),
                        "{}",
                        options
                    );
                }
                let short_form = format!("-e ./{value}");
                let long_form = format!("--editable=./{value}");
                let expected = format!("./{value}");
                for options in [short_form, long_form] {
                    let parsed = pip_line_options(&options);
                    prop_assert_eq!(
                        parsed.as_ref().and_then(|parsed| parsed.editable.as_deref()),
                        Some(expected.as_str()),
                        "{}",
                        options
                    );
                }
            }

            #[test]
            fn pip_line_options_rejects_long_name_followed_by_ident(
                ch in proptest::char::ranges(vec!['a'..='z', 'A'..='Z', '0'..='9'].into()),
                rest in "[^\\r\\n]{0,30}",
            ) {
                let options = format!("--requirement{ch}{rest}");
                prop_assert_eq!(pip_line_options(&options), None);
            }

            #[test]
            fn line_readers_never_panic(line in "\\PC{0,120}") {
                let _ = pip_line_options(&line);
                let _ = requirements_line_distribution(&line);
                let _ = requirements_line_hashed(&line);
            }
        }
    }
}
