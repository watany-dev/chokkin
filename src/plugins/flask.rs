//! Flask plugin extractor.

use std::path::Path;

use crate::config::{EntrySpec, PluginId};
use crate::path_util::rel_to_root;
use crate::sources::{DiscoveredSources, FileContext, path_to_module};

use super::commands::is_dotted_identifier;
use super::context::PluginContext;
use super::types::{PluginContribution, PluginEntry, ReferenceOrigin};
use super::util::{decorator_suffix, push_app_ref, push_binary, push_decorated_modules};

/// Attributes and factories Flask looks up in the module `--app` names.
const APP_NAMES: [&str; 4] = ["app", "application", "create_app", "make_app"];

/// Extract Flask app references from static configuration.
pub(super) fn extract(ctx: &PluginContext<'_>) -> PluginContribution {
    let mut contrib = PluginContribution::empty(PluginId::Flask);
    let root = ctx.root.path.as_path();

    extract_flaskenv(root, ctx.sources, &mut contrib);
    extract_scripts(root, ctx.sources, &mut contrib);
    push_decorated_modules(
        ctx,
        &mut contrib,
        is_route_decorator,
        "flask route decorator",
    );

    contrib
}

fn extract_flaskenv(root: &Path, sources: &DiscoveredSources, contrib: &mut PluginContribution) {
    for file_name in [".flaskenv", ".env"] {
        let path = root.join(file_name);
        if !path.is_file() {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = rel_to_root(root, &path);
        for (line_index, line) in contents.lines().enumerate() {
            let Some(value) = env_assignment(line, "FLASK_APP") else {
                continue;
            };
            let origin = ReferenceOrigin {
                file: rel.clone(),
                line: u32::try_from(line_index + 1).ok(),
                label: "FLASK_APP".to_owned(),
            };
            push_flask_app(sources, contrib, value, &origin);
            push_binary(contrib, "flask", origin);
        }
    }
}

fn extract_scripts(root: &Path, sources: &DiscoveredSources, contrib: &mut PluginContribution) {
    for dir_name in ["scripts", "bin"] {
        let dir = root.join(dir_name);
        if !dir.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            let rel = rel_to_root(root, &path);
            for (line_index, line) in contents.lines().enumerate() {
                let Some(target) = flask_app_arg(line) else {
                    continue;
                };
                let origin = ReferenceOrigin {
                    file: rel.clone(),
                    line: u32::try_from(line_index + 1).ok(),
                    label: "flask --app".to_owned(),
                };
                push_flask_app(sources, contrib, target, &origin);
                push_binary(contrib, "flask", origin);
            }
        }
    }
}

/// Push the app a `FLASK_APP` / `--app` value names. A module name goes to
/// [`push_app_ref`]; a file path (`hello.py`, `src/hello`, `src/hello.py:app`)
/// names the file Flask's `prepare_import` imports, read from the project root
/// (#866). That file under the module name the layout gives it is the same
/// reference as the module form, so the app names in it are used; a file the
/// layout names no module for (a script at the root of a project without a
/// package) is an entry root, as Flask imports it from its own directory.
/// A module name of such a file (`--app hello`) is read as its path.
///
/// A path naming no discovered file, an absolute one, one leaving the root,
/// or one built from a variable adds nothing: like a command target naming
/// nothing discovered, it hides no project file (ADR 0005).
fn push_flask_app(
    sources: &DiscoveredSources,
    contrib: &mut PluginContribution,
    value: &str,
    origin: &ReferenceOrigin,
) {
    let value = value.trim().trim_matches(['"', '\'']);
    let (path, attr) = split_app_attr(value);
    let file = if is_file_path(path) {
        let Some(file) = app_file(sources, path) else {
            return;
        };
        file
    } else {
        // `prepare_import` puts the working directory on `sys.path` for a
        // module name too, so `hello` is the root's `hello.py` even where
        // the layout names no module for it.
        let unnamed = is_dotted_identifier(path)
            .then(|| app_file(sources, &path.replace('.', "/")))
            .flatten()
            .filter(|file| path_to_module(file, &sources.layout).is_none());
        let Some(file) = unnamed else {
            push_app_ref(contrib, value, origin, &APP_NAMES);
            return;
        };
        file
    };
    let symbol = attr.map(|attr| attr.split_once('(').map_or(attr, |(name, _)| name).trim());
    if let Some(module) = path_to_module(file, &sources.layout) {
        let target = symbol.map_or_else(|| module.clone(), |symbol| format!("{module}:{symbol}"));
        push_app_ref(contrib, &target, origin, &APP_NAMES);
        return;
    }
    contrib.entries.push(PluginEntry {
        spec: EntrySpec {
            path: file.to_owned(),
            symbol: symbol
                .filter(|symbol| !symbol.is_empty())
                .map(str::to_owned),
        },
        context: FileContext::Runtime,
        origin: origin.clone(),
    });
}

/// Split `path:attr` where Flask's `ScriptInfo` does: at the first `:` not
/// followed by a path separator, so a drive (`C:\app.py`) stays in the path.
fn split_app_attr(value: &str) -> (&str, Option<&str>) {
    value
        .match_indices(':')
        .find(|(index, _)| !value[index + 1..].starts_with(['/', '\\']))
        .map_or((value, None), |(index, _)| {
            (&value[..index], Some(&value[index + 1..]))
        })
}

/// Flask reads a value with the extension `.py` (case-sensitive, as
/// `os.path.splitext` is) or holding a separator as a path.
fn is_file_path(value: &str) -> bool {
    Path::new(value).extension().is_some_and(|ext| ext == "py") || value.contains(['/', '\\'])
}

/// The discovered `.py` file a root-relative path names: the package
/// `__init__.py` first, as Python's import prefers it, then the module file.
/// Either separator is read, as a Windows script may use `\`.
fn app_file<'s>(sources: &'s DiscoveredSources, path: &str) -> Option<&'s str> {
    if path.starts_with(['/', '\\']) || path.contains(['$', '%', '`', '{', '}', '*', ':']) {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {},
            ".." => {
                parts.pop()?;
            },
            _ => parts.push(part),
        }
    }
    let joined = parts.join("/");
    let stem = joined.strip_suffix(".py").unwrap_or(&joined);
    let stem = stem.strip_suffix("/__init__").unwrap_or(stem);
    if stem.is_empty() || stem == "__init__" {
        return None;
    }
    let candidates = [format!("{stem}/__init__.py"), format!("{stem}.py")];
    candidates.iter().find_map(|candidate| {
        sources
            .python_files()
            .find(|file| file.path == *candidate)
            .map(|file| file.path.as_str())
    })
}

/// Route decorator test shared by the parse path and its syntax-error text
/// fallback: a method call on a receiver (`@app.route("/")`, `@bp.post(...)`).
/// Bare `@app.route` and receiverless `@get(...)` are not Flask routes.
fn is_route_decorator(name: &str, is_call: bool) -> bool {
    let (receiver, suffix) = decorator_suffix(name);
    is_call
        && receiver.is_some()
        && matches!(
            suffix,
            "route" | "get" | "post" | "put" | "patch" | "delete"
        )
}

fn env_assignment<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return None;
    }
    let (lhs, rhs) = trimmed.split_once('=')?;
    if lhs.trim() != key {
        return None;
    }
    Some(rhs.trim().trim_matches(['"', '\'']))
}

fn flask_app_arg(line: &str) -> Option<&str> {
    let tokens: Vec<_> = line.split_whitespace().collect();
    for (index, token) in tokens.iter().enumerate() {
        if *token == "--app" {
            return tokens.get(index + 1).copied();
        }
        if let Some(value) = token.strip_prefix("--app=") {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::sources::{DiscoveredFile, FileKind, LayoutInfo};

    fn sources(paths: &[&str]) -> DiscoveredSources {
        DiscoveredSources {
            root: ProjectRoot {
                path: std::env::temp_dir(),
                marker: RootMarker::PyProjectToml,
            },
            layout: LayoutInfo::new("src", vec!["web".to_owned()]),
            effective_globs: Vec::new(),
            files: paths
                .iter()
                .map(|path| DiscoveredFile {
                    path: (*path).to_owned(),
                    kind: FileKind::Python,
                    context: FileContext::Runtime,
                })
                .collect(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn app_paths_split_attr_after_a_drive() {
        assert_eq!(
            split_app_attr("src/app.py:app"),
            ("src/app.py", Some("app"))
        );
        assert_eq!(split_app_attr(r"C:\src\app.py"), (r"C:\src\app.py", None));
        assert_eq!(
            split_app_attr("web.app:create_app()"),
            ("web.app", Some("create_app()"))
        );
    }

    /// A Windows script names the file with `\` (#866).
    #[test]
    fn app_paths_read_either_separator() {
        let sources = sources(&[
            "src/web/__init__.py",
            "src/web/app.py",
            "src/web/pkg/__init__.py",
        ]);
        assert_eq!(
            app_file(&sources, r"src\web\app.py"),
            Some("src/web/app.py")
        );
        assert_eq!(
            app_file(&sources, r".\src\web\pkg"),
            Some("src/web/pkg/__init__.py")
        );
        assert_eq!(
            app_file(&sources, "src/web/x/../app"),
            Some("src/web/app.py")
        );
        assert_eq!(
            app_file(&sources, "src/web/__init__.py"),
            Some("src/web/__init__.py")
        );
        for unread in [
            r"C:\src\web\app.py",
            "/src/web/app.py",
            "../src/web/app.py",
            "$DIR/app.py",
            "src/web/missing.py",
        ] {
            assert_eq!(app_file(&sources, unread), None, "{unread}");
        }
    }
}
