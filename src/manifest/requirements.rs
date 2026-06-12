//! `requirements*.txt` parsing.

use std::path::{Path, PathBuf};

use super::error::ManifestError;
use super::pep508_util::{extract_egg_name, normalize_distribution_name, parse_requirement};
use super::types::{DeclaredDependency, DependencyContext, DependencyOrigin};
use super::util::{DependencyPush, push_dependency, relative_path};
use super::warnings::ManifestWarning;

/// Result of parsing one or more requirements files.
#[derive(Debug, Default)]
pub struct RequirementsExtraction {
    /// Parsed dependencies.
    pub dependencies: Vec<DeclaredDependency>,
    /// Version constraints from `-c` files (not dependency declarations).
    pub constraints: Vec<DeclaredDependency>,
    /// Non-fatal warnings.
    pub warnings: Vec<ManifestWarning>,
    /// Root-relative paths that were read.
    pub files_read: Vec<String>,
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
pub fn extract_requirements_file(
    root: &Path,
    filename: &str,
    default_context: &DependencyContext,
) -> Result<RequirementsExtraction, ManifestError> {
    let path = root.join(filename);
    if !path.is_file() {
        return Ok(RequirementsExtraction::default());
    }

    let mut include_stack = Vec::new();
    let mut result = RequirementsExtraction::default();
    parse_requirements_file_path(RequirementsParseContext {
        root,
        path: &path,
        default_context,
        include_stack: &mut include_stack,
        result: &mut result,
        mode: RequirementsParseMode::Dependencies,
    })?;
    Ok(result)
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
            .map(|p| relative_path(ctx.root, p))
            .collect::<Vec<_>>()
            .join(" -> ");
        return Err(ManifestError::RequirementsCircularInclude { cycle });
    }

    let rel = relative_path(ctx.root, ctx.path);
    ctx.result.files_read.push(rel.clone());
    ctx.include_stack.push(canonical);

    let contents = std::fs::read_to_string(ctx.path).map_err(|source| ManifestError::Io {
        path: ctx.path.to_path_buf(),
        source,
    })?;

    for (line_number, line) in contents.lines().enumerate() {
        parse_requirements_line(&mut ctx, &rel, line, line_number)?;
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
    let trimmed = strip_comment(line).trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    if let Some(include_path) = flag_value(trimmed, "-r", "--requirement") {
        let resolved = resolve_requirements_include(ctx.root, ctx.path, include_path);
        let resolved_path = resolved.ok_or_else(|| ManifestError::RequirementsIncludeMissing {
            path: include_path.to_owned(),
        })?;
        return parse_requirements_file_path(RequirementsParseContext {
            root: ctx.root,
            path: &resolved_path,
            default_context: ctx.default_context,
            include_stack: ctx.include_stack,
            result: ctx.result,
            mode: RequirementsParseMode::Dependencies,
        });
    }

    if let Some(constraint_path) = flag_value(trimmed, "-c", "--constraint") {
        if let Some(resolved) = resolve_requirements_include(ctx.root, ctx.path, constraint_path) {
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

    match ctx.mode {
        RequirementsParseMode::Dependencies => {
            let origin = DependencyOrigin {
                file: rel.to_owned(),
                line: Some(line_no),
                label: rel.to_owned(),
            };
            match parse_requirement(trimmed, ctx.default_context.clone(), origin) {
                Ok(dep) => ctx.result.dependencies.push(dep),
                Err(warning) => ctx.result.warnings.push(warning),
            }
        },
        RequirementsParseMode::Constraints => {
            push_dependency(DependencyPush {
                dependencies: &mut ctx.result.constraints,
                warnings: &mut ctx.result.warnings,
                raw: trimmed,
                context: ctx.default_context.clone(),
                file: rel,
                label: rel,
                line: Some(line_no),
            });
        },
    }

    Ok(())
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
    });
}

/// pip-compatible: only `#` preceded by whitespace (or at line start) starts a comment.
#[must_use]
fn strip_comment(line: &str) -> &str {
    let mut prev_is_space = true;
    for (idx, ch) in line.char_indices() {
        if ch == '#' && prev_is_space {
            return &line[..idx];
        }
        prev_is_space = ch.is_whitespace();
    }
    line
}

/// pip-compatible flag parsing: long form before short; long form requires `=` or whitespace.
#[must_use]
fn flag_value<'a>(line: &'a str, short: &str, long: &str) -> Option<&'a str> {
    if let Some(rest) = line.strip_prefix(long) {
        return match rest.as_bytes().first() {
            Some(b'=') => Some(rest[1..].trim()),
            Some(b' ' | b'\t') | None => Some(rest.trim()),
            _ => None,
        };
    }
    line.strip_prefix(short).map(str::trim)
}

#[must_use]
fn editable_flag_value(line: &str) -> Option<&str> {
    let value = flag_value(line, "-e", "--editable")?;
    if line.starts_with("-e")
        && !line.starts_with("-e ")
        && !line.starts_with("-e.")
        && !line.starts_with("-e/")
        && !line.starts_with("-e..")
        && !value.starts_with("git+")
        && !value.contains("://")
    {
        return None;
    }
    Some(value)
}

#[must_use]
fn is_local_path(spec: &str) -> bool {
    spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('.')
}

#[must_use]
fn is_url_like(spec: &str) -> bool {
    spec.contains("://")
        || spec.starts_with("git+")
        || spec.starts_with("hg+")
        || spec.starts_with("bzr+")
        || spec.starts_with("svn+")
}

fn resolve_requirements_include(root: &Path, base: &Path, include: &str) -> Option<PathBuf> {
    if let Some(parent) = base.parent() {
        let candidate = parent.join(include);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    let root_candidate = root.join(include);
    if root_candidate.is_file() {
        return Some(root_candidate);
    }

    None
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
    fn flag_value_parses_long_requirement_form() {
        assert_eq!(
            flag_value("--requirement=dev.txt", "-r", "--requirement"),
            Some("dev.txt")
        );
    }

    #[test]
    fn editable_flag_rejects_example_pkg_false_positive() {
        assert_eq!(editable_flag_value("-example-pkg"), None);
    }
}
