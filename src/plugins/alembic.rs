//! Alembic plugin: the migration environment and revision files alembic loads
//! by path from `script_location` (#667).

use std::collections::BTreeSet;
use std::path::Path;

use crate::config::{EntrySpec, PluginId};
use crate::path_util::{join_rel, rel_to_root};
use crate::sources::{FileContext, path_to_module};

use super::config_text::{ini_value, toml_key_line};
use super::context::PluginContext;
use super::types::{PluginContribution, PluginEntry, ReferenceOrigin};
use super::util::{origin_for_file, push_binary};

/// Entry used when no config names a `script_location`.
const DEFAULT_ENV: &str = "alembic/env.py";

/// One `script_location` read from `alembic.ini` or `[tool.alembic]`.
struct AlembicConfig {
    /// Root-relative directory of the config file (`%(here)s`).
    dir: String,
    script_location: String,
    version_locations: Vec<String>,
    /// `recursive_version_locations`: revisions in subdirectories load too.
    recursive: bool,
    origin: ReferenceOrigin,
}

#[must_use]
pub(super) fn extract(ctx: &PluginContext<'_>) -> PluginContribution {
    let root = ctx.root.path.as_path();
    let mut contrib = PluginContribution::empty(PluginId::Alembic);
    let ini = root.join("alembic.ini");
    if ini.is_file() {
        push_binary(
            &mut contrib,
            "alembic",
            origin_for_file(root, &ini, "alembic.ini"),
        );
    }

    let mut seen = BTreeSet::new();
    for config in &configs(ctx) {
        let Some(script) = resolve_location(ctx, &config.dir, &config.script_location) else {
            continue;
        };
        let versions: Vec<String> = if config.version_locations.is_empty() {
            vec![join_rel(&script, "versions")]
        } else {
            config
                .version_locations
                .iter()
                .filter_map(|location| resolve_location(ctx, &config.dir, location))
                .collect()
        };
        let env = join_rel(&script, "env.py");
        for file in ctx.sources.python_files() {
            let in_versions = versions.iter().any(|dir| {
                file.path
                    .strip_prefix(dir.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
                    .is_some_and(|rest| config.recursive || !rest.contains('/'))
            });
            if (file.path == env || in_versions) && seen.insert(file.path.clone()) {
                push_entry(
                    &mut contrib,
                    file.path.clone(),
                    file.context,
                    config.origin.clone(),
                );
            }
        }
    }

    let env = root.join(DEFAULT_ENV);
    if contrib.entries.is_empty() && env.is_file() {
        push_entry(
            &mut contrib,
            DEFAULT_ENV.to_owned(),
            FileContext::Dev,
            origin_for_file(root, &env, DEFAULT_ENV),
        );
    }
    contrib
}

/// `alembic.ini` in the root, each workspace member, and each package
/// directory, then `[tool.alembic]` in the root and member `pyproject.toml`.
fn configs(ctx: &PluginContext<'_>) -> Vec<AlembicConfig> {
    let root = ctx.root.path.as_path();
    let members: Vec<&str> = std::iter::once("")
        .chain(
            ctx.sources
                .layout
                .members
                .iter()
                .map(|member| member.path.as_str()),
        )
        .collect();
    let packages = ctx
        .sources
        .python_files()
        .filter_map(|file| file.path.strip_suffix("__init__.py"))
        .map(|dir| dir.trim_end_matches('/'));
    let ini_dirs: BTreeSet<&str> = members.iter().copied().chain(packages).collect();

    let mut configs: Vec<AlembicConfig> = ini_dirs
        .into_iter()
        .filter_map(|dir| ini_config(root, dir))
        .collect();
    configs.extend(
        members
            .into_iter()
            .filter_map(|dir| pyproject_config(root, dir)),
    );
    configs
}

fn ini_config(root: &Path, dir: &str) -> Option<AlembicConfig> {
    let path = root.join(dir).join("alembic.ini");
    let contents = std::fs::read_to_string(&path).ok()?;
    let (index, script_location) = ini_value(&contents, "alembic", "script_location")?;
    let setting = |key: &str| {
        ini_value(&contents, "alembic", key).map(|(_, value)| strip_comment(&value).to_owned())
    };
    let separators = location_separators(
        setting("path_separator")
            .or_else(|| setting("version_path_separator"))
            .as_deref(),
    );
    let version_locations = setting("version_locations")
        .map(|value| {
            value
                .split(separators)
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let recursive = setting("recursive_version_locations")
        .is_some_and(|value| ["true", "yes", "on", "1"].contains(&value.to_lowercase().as_str()));
    Some(AlembicConfig {
        dir: dir.to_owned(),
        script_location: strip_comment(&script_location).to_owned(),
        version_locations,
        recursive,
        origin: ReferenceOrigin {
            file: rel_to_root(root, &path),
            line: u32::try_from(index + 1).ok(),
            label: "script_location".to_owned(),
        },
    })
}

fn pyproject_config(root: &Path, dir: &str) -> Option<AlembicConfig> {
    let path = root.join(dir).join("pyproject.toml");
    let text = std::fs::read_to_string(&path).ok()?;
    let table = toml::from_str::<toml::Table>(&text).ok()?;
    let alembic = table.get("tool")?.get("alembic")?;
    let script_location = alembic.get("script_location")?.as_str()?.to_owned();
    let version_locations = alembic
        .get("version_locations")
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let recursive = alembic
        .get("recursive_version_locations")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    Some(AlembicConfig {
        dir: dir.to_owned(),
        script_location,
        version_locations,
        recursive,
        origin: ReferenceOrigin {
            file: rel_to_root(root, &path),
            line: toml_key_line(&text, "tool.alembic", "script_location"),
            label: "tool.alembic.script_location".to_owned(),
        },
    })
}

/// How `version_locations` splits, per `path_separator` (alembic 1.16) or
/// the older `version_path_separator`; unset is the legacy space or comma.
fn location_separators(setting: Option<&str>) -> &'static [char] {
    match setting {
        // `os.pathsep`: `:` on POSIX, `;` on Windows.
        Some("os") => &[':', ';', '\n'],
        Some("colon") => &[':', '\n'],
        Some("semicolon") => &[';', '\n'],
        Some("newline") => &['\n'],
        Some("space") => &[' ', '\t', '\n'],
        _ => &[' ', '\t', '\n', ','],
    }
}

/// configparser keeps `value  # note`; alembic's own template writes those.
fn strip_comment(value: &str) -> &str {
    value.find(" #").map_or(value, |end| &value[..end]).trim()
}

/// Root-relative directory a location names: `pkg.sub:dir` under the
/// package, else a path from the config's directory (`%(here)s`), or, for a
/// bare relative path missing there, from the root, where alembic usually runs.
fn resolve_location(ctx: &PluginContext<'_>, config_dir: &str, location: &str) -> Option<String> {
    let location = location.trim();
    if let Some((package, dir)) = location.split_once(':') {
        // `C:\...` is an absolute Windows path, not a package resource.
        if dir.starts_with(['/', '\\']) || !package.split('.').all(is_identifier) {
            return None;
        }
        return join_lexical(&package_dir(ctx, package)?, dir);
    }
    let bases: &[&str] = if location.contains("%(here)s") {
        &[config_dir]
    } else {
        &[config_dir, ""]
    };
    let location = location.replace("%(here)s", ".");
    if location.starts_with(['/', '\\']) {
        return None;
    }
    bases
        .iter()
        .filter_map(|base| join_lexical(base, &location))
        .find(|dir| ctx.root.path.join(dir).is_dir())
}

fn package_dir(ctx: &PluginContext<'_>, package: &str) -> Option<String> {
    ctx.sources.python_files().find_map(|file| {
        let dir = file.path.strip_suffix("__init__.py")?;
        (path_to_module(&file.path, &ctx.sources.layout)? == package)
            .then(|| dir.trim_end_matches('/').to_owned())
    })
}

fn is_identifier(part: &str) -> bool {
    !part.is_empty()
        && !part.starts_with(|c: char| c.is_ascii_digit())
        && part.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// `rel` appended to root-relative `base`, with `.` and `..` folded; `None`
/// when it climbs out of the root.
fn join_lexical(base: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = base.split('/').filter(|part| !part.is_empty()).collect();
    for part in rel.split(['/', '\\']) {
        match part {
            "" | "." => {},
            ".." => {
                parts.pop()?;
            },
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

fn push_entry(
    contrib: &mut PluginContribution,
    path: String,
    context: FileContext,
    origin: ReferenceOrigin,
) {
    contrib.entries.push(PluginEntry {
        spec: EntrySpec { path, symbol: None },
        context,
        origin,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_lexical_folds_dots_and_stays_under_root() {
        assert_eq!(
            join_lexical("pkg/db", "./migrations").as_deref(),
            Some("pkg/db/migrations")
        );
        assert_eq!(
            join_lexical("pkg/db", "..\\alembic").as_deref(),
            Some("pkg/alembic")
        );
        assert_eq!(join_lexical("pkg", ".").as_deref(), Some("pkg"));
        assert_eq!(join_lexical("", "../x"), None);
    }

    #[test]
    fn strip_comment_drops_inline_note() {
        assert_eq!(strip_comment("os  # Use os.pathsep."), "os");
        assert_eq!(strip_comment("%(here)s/migrations"), "%(here)s/migrations");
    }
}
