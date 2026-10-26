//! `DJANGO_SETTINGS_MODULE` set in the environment Django starts in: dotenv
//! files, docker-compose `environment:` and Dockerfile `ENV` (#999, #1239).
//!
//! Only the lines that set `DJANGO_SETTINGS_MODULE` are read; a dotenv file
//! may hold secrets, so no other line's text leaves this module. Variables
//! are never expanded: a value that holds one is not a dotted module name, so
//! the caller counts it as set in a form not read.

use std::path::Path;

use crate::sources::is_dockerfile;

use super::super::config_text::{logical_lines, yaml_scalar};

const KEY: &str = "DJANGO_SETTINGS_MODULE";

/// Dotenv files `django-environ` and `python-dotenv` load, the committed
/// example after the real one.
const DOTENV_NAMES: &[&str] = &[".env", ".env.example"];

/// A value set for `DJANGO_SETTINGS_MODULE`: the root-relative file, its
/// 1-based line and the value as written, quotes dropped.
pub(super) struct EnvSetting {
    pub file: String,
    pub line: u32,
    pub value: String,
}

/// The kinds of file read, each with its own line form.
#[derive(Clone, Copy)]
enum Kind {
    Dotenv,
    Compose,
    Dockerfile,
}

/// Every value set in the dotenv files, compose files and Dockerfiles of
/// each directory in `dirs` (root-relative, `""` for the root), in that
/// order per directory.
pub(super) fn read<'a>(root: &Path, dirs: impl Iterator<Item = &'a str>) -> Vec<EnvSetting> {
    let mut found = Vec::new();
    for dir in dirs {
        let files = DOTENV_NAMES
            .iter()
            .map(|name| ((*name).to_owned(), Kind::Dotenv))
            .chain(
                listed(root, dir, is_compose_file)
                    .into_iter()
                    .map(|name| (name, Kind::Compose)),
            )
            .chain(
                listed(root, dir, is_dockerfile)
                    .into_iter()
                    .map(|name| (name, Kind::Dockerfile)),
            );
        for (name, kind) in files {
            let file = if dir.is_empty() {
                name
            } else {
                format!("{dir}/{name}")
            };
            let Ok(contents) = std::fs::read_to_string(root.join(&file)) else {
                continue;
            };
            let lines = match kind {
                Kind::Dockerfile => logical_lines(&contents),
                Kind::Dotenv | Kind::Compose => contents
                    .lines()
                    .enumerate()
                    .map(|(index, line)| (index, line.to_owned()))
                    .collect(),
            };
            for (index, line) in lines {
                let value = match kind {
                    Kind::Dotenv => dotenv_value(&line),
                    Kind::Compose => compose_value(&line),
                    Kind::Dockerfile => dockerfile_value(&line),
                };
                if let Some(value) = value {
                    found.push(EnvSetting {
                        file: file.clone(),
                        line: u32::try_from(index + 1).unwrap_or(u32::MAX),
                        value,
                    });
                }
            }
        }
    }
    found
}

/// The files directly in `dir` that `keep` accepts, sorted.
fn listed(root: &Path, dir: &str, keep: fn(&Path) -> bool) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && keep(path))
        .filter_map(|path| Some(path.file_name()?.to_str()?.to_owned()))
        .collect();
    names.sort();
    names
}

/// `docker-compose.yml`, `compose.yaml` and their override files
/// (`docker-compose.prod.yml`).
fn is_compose_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    (name.starts_with("docker-compose") || name.starts_with("compose"))
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("yml") || ext.eq_ignore_ascii_case("yaml"))
}

/// `KEY=VALUE`, with an optional `export ` prefix; a quoted value is taken
/// up to its closing quote, an unquoted one up to a ` #` comment.
fn dotenv_value(line: &str) -> Option<String> {
    let line = line.trim_start();
    let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
    let (key, value) = line.split_once('=')?;
    if key.trim_end() != KEY {
        return None;
    }
    let value = value.trim();
    for quote in ['"', '\''] {
        if let Some(rest) = value.strip_prefix(quote) {
            return rest.split_once(quote).map(|(inner, _)| inner.to_owned());
        }
    }
    let value = value.split_once(" #").map_or(value, |(value, _)| value);
    Some(value.trim_end().to_owned())
}

/// An `environment:` entry in either form: the list item
/// `- DJANGO_SETTINGS_MODULE=value` (quoted or not) or the mapping
/// `DJANGO_SETTINGS_MODULE: value`. A bare list item passes the host's value
/// through and sets nothing.
fn compose_value(line: &str) -> Option<String> {
    let line = line.trim_start();
    if let Some(item) = line.strip_prefix("- ") {
        let (key, value) = yaml_scalar(item).split_once('=')?;
        return (key.trim_end() == KEY).then(|| value.trim().to_owned());
    }
    let (key, value) = line.split_once(':')?;
    (yaml_scalar(key) == KEY).then(|| yaml_scalar(value).to_owned())
}

/// `ENV DJANGO_SETTINGS_MODULE=value` among other `KEY=VALUE` pairs, or the
/// legacy `ENV DJANGO_SETTINGS_MODULE value`.
fn dockerfile_value(line: &str) -> Option<String> {
    let (instruction, rest) = line.trim_start().split_once(char::is_whitespace)?;
    if !instruction.eq_ignore_ascii_case("ENV") {
        return None;
    }
    let rest = rest.trim();
    let first = rest.split_whitespace().next()?;
    if !first.contains('=') {
        let value = rest.strip_prefix(KEY)?;
        return value
            .starts_with(char::is_whitespace)
            .then(|| unquote(value.trim()).to_owned());
    }
    rest.split_whitespace()
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == KEY)
        .map(|(_, value)| unquote(value).to_owned())
}

fn unquote(value: &str) -> &str {
    ['"', '\'']
        .into_iter()
        .find_map(|quote| value.strip_prefix(quote)?.strip_suffix(quote))
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv_lines_read_only_the_settings_key() {
        let cases = [
            (
                "DJANGO_SETTINGS_MODULE=mysite.settings",
                Some("mysite.settings"),
            ),
            (
                "export DJANGO_SETTINGS_MODULE = mysite.prod",
                Some("mysite.prod"),
            ),
            (
                "DJANGO_SETTINGS_MODULE=\"mysite.prod\" # live",
                Some("mysite.prod"),
            ),
            ("DJANGO_SETTINGS_MODULE='mysite.prod'", Some("mysite.prod")),
            (
                "DJANGO_SETTINGS_MODULE=mysite.dev # local",
                Some("mysite.dev"),
            ),
            ("DJANGO_SETTINGS_MODULE=${SETTINGS}", Some("${SETTINGS}")),
            ("# DJANGO_SETTINGS_MODULE=mysite.old", None),
            ("SECRET_KEY=DJANGO_SETTINGS_MODULE=x", None),
            ("DJANGO_SETTINGS_MODULE_X=mysite.x", None),
        ];
        for (line, expected) in cases {
            assert_eq!(dotenv_value(line).as_deref(), expected, "{line}");
        }
    }

    #[test]
    fn compose_lines_read_list_and_mapping_entries() {
        let cases = [
            (
                "      - DJANGO_SETTINGS_MODULE=mysite.prod",
                Some("mysite.prod"),
            ),
            (
                "      - \"DJANGO_SETTINGS_MODULE=mysite.prod\"",
                Some("mysite.prod"),
            ),
            (
                "      DJANGO_SETTINGS_MODULE: mysite.prod",
                Some("mysite.prod"),
            ),
            (
                "      DJANGO_SETTINGS_MODULE: \"mysite.prod\" # live",
                Some("mysite.prod"),
            ),
            (
                "      \"DJANGO_SETTINGS_MODULE\": 'mysite.prod'",
                Some("mysite.prod"),
            ),
            ("      - DJANGO_SETTINGS_MODULE", None),
            ("      - OTHER=DJANGO_SETTINGS_MODULE", None),
            ("    command: python manage.py runserver", None),
        ];
        for (line, expected) in cases {
            assert_eq!(compose_value(line).as_deref(), expected, "{line}");
        }
    }

    #[test]
    fn dockerfile_lines_read_both_env_forms() {
        let cases = [
            (
                "ENV DJANGO_SETTINGS_MODULE=mysite.prod",
                Some("mysite.prod"),
            ),
            (
                "env PYTHONUNBUFFERED=1 DJANGO_SETTINGS_MODULE=\"mysite.prod\"",
                Some("mysite.prod"),
            ),
            (
                "ENV DJANGO_SETTINGS_MODULE mysite.prod",
                Some("mysite.prod"),
            ),
            ("ENV DJANGO_SETTINGS_MODULE_X mysite.prod", None),
            ("ARG DJANGO_SETTINGS_MODULE=mysite.prod", None),
            (
                "RUN DJANGO_SETTINGS_MODULE=mysite.prod python manage.py check",
                None,
            ),
        ];
        for (line, expected) in cases {
            assert_eq!(dockerfile_value(line).as_deref(), expected, "{line}");
        }
    }

    #[test]
    fn compose_files_include_override_files() {
        for (name, expected) in [
            ("docker-compose.yml", true),
            ("docker-compose.prod.yaml", true),
            ("compose.yaml", true),
            ("compose.override.yml", true),
            ("docker-compose.env", false),
            ("composer.json", false),
        ] {
            assert_eq!(is_compose_file(Path::new(name)), expected, "{name}");
        }
    }
}
