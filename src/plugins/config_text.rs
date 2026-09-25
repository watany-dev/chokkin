//! Line-aware readers for TOML / INI / YAML / Makefile-style config text.
//!
//! Values are read statically without a YAML or Makefile parser; only the
//! shapes the config scanners need (keys, block scalars, `\` continuations)
//! are recognised, and every hit keeps its source line for `--explain`.

use std::path::Path;

use toml::Value;

use super::types::ReferenceOrigin;
use super::util::relative_path;

/// `pyproject.toml` read once and shared by the scanners that need both the
/// parsed table and the raw text (for line numbers).
pub(super) struct PyprojectDoc {
    pub(super) rel: String,
    text: String,
    table: toml::Table,
}

impl PyprojectDoc {
    pub(super) fn load(root: &Path) -> Option<Self> {
        let (rel, text) = read_root_file(root, "pyproject.toml")?;
        let table = toml::from_str::<toml::Table>(&text).ok()?;
        Some(Self { rel, text, table })
    }

    /// Table at a dotted path such as `tool.pdm.scripts`.
    pub(super) fn table(&self, path: &str) -> Option<&toml::Table> {
        path.split('.')
            .try_fold(&self.table, |table, key| table.get(key)?.as_table())
    }

    pub(super) fn value(&self, table: &str, key: &str) -> Option<&Value> {
        self.table(table)?.get(key)
    }

    pub(super) fn origin(&self, table: &str, key: &str) -> ReferenceOrigin {
        ReferenceOrigin {
            file: self.rel.clone(),
            line: toml_key_line(&self.text, table, key),
            label: format!("{table}.{key}"),
        }
    }
}

/// A string, or an array of strings joined with spaces (`cmd = ["pytest", "-q"]`).
pub(super) fn toml_words(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    let words: Vec<&str> = value.as_array()?.iter().filter_map(Value::as_str).collect();
    Some(words.join(" "))
}

pub(super) fn read_root_file(root: &Path, name: &str) -> Option<(String, String)> {
    let path = root.join(name);
    let contents = std::fs::read_to_string(&path).ok()?;
    Some((relative_path(root, &path), contents))
}

/// 1-based line of `key = …` inside `[table]`, or of a `[table.key]` header.
pub(super) fn toml_key_line(text: &str, table: &str, key: &str) -> Option<u32> {
    let header = format!("[{table}]");
    let sub_header = format!("[{table}.{key}]");
    let mut in_table = false;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.split('#').next().unwrap_or(line).trim();
        if trimmed.starts_with('[') {
            if trimmed == sub_header {
                return line_number(index);
            }
            in_table = trimmed == header;
            continue;
        }
        let line_key = trimmed.split_once('=').map(|(name, _)| name.trim());
        if in_table && line_key.is_some_and(|name| name.trim_matches(['"', '\'']) == key) {
            return line_number(index);
        }
    }
    None
}

/// Value of `key` in INI `[section]` joined with its indented continuation
/// lines, plus the key's 0-based line index.
pub(super) fn ini_value(contents: &str, section: &str, key: &str) -> Option<(usize, String)> {
    let mut in_section = false;
    let mut found: Option<(usize, String)> = None;
    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if let Some((_, value)) = found.as_mut() {
            if trimmed.is_empty() {
                continue;
            }
            if !line.starts_with([' ', '\t']) {
                break;
            }
            value.push('\n');
            value.push_str(trimmed);
            continue;
        }
        if trimmed.starts_with('[') {
            in_section = ini_section_name(trimmed) == Some(section);
            continue;
        }
        if in_section
            && let Some((name, value)) = trimmed.split_once(['=', ':'])
            && name.trim() == key
        {
            found = Some((index, value.trim().to_owned()));
        }
    }
    found
}

/// 0-based line index of the INI `[section]` header.
pub(super) fn ini_section_line(contents: &str, section: &str) -> Option<usize> {
    contents
        .lines()
        .position(|line| ini_section_name(line.trim()) == Some(section))
}

fn ini_section_name(trimmed: &str) -> Option<&str> {
    trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .map(str::trim)
}

/// Lines with `\` continuations joined, each paired with its 0-based start index.
pub(super) fn logical_lines(contents: &str) -> Vec<(usize, String)> {
    let mut lines = Vec::new();
    let mut pending: Option<(usize, String)> = None;
    for (index, line) in contents.lines().enumerate() {
        let (start, mut text) = pending.take().unwrap_or((index, String::new()));
        if let Some(body) = line.strip_suffix('\\') {
            text.push_str(body);
            text.push(' ');
            pending = Some((start, text));
        } else {
            text.push_str(line);
            lines.push((start, text));
        }
    }
    lines.extend(pending);
    lines
}

/// Body of a YAML block scalar (`|` / `>`) starting at `start`: the lines
/// indented deeper than `parent_indent`, joined with `\n`, and the index of
/// the first line after the block.
pub(super) fn yaml_block_body(
    lines: &[&str],
    start: usize,
    parent_indent: usize,
) -> (String, usize) {
    let mut block = String::new();
    let mut cursor = start;
    while let Some(line) = lines.get(cursor) {
        if line.trim().is_empty() {
            block.push('\n');
            cursor += 1;
            continue;
        }
        if leading_spaces(line) <= parent_indent {
            break;
        }
        if !block.is_empty() {
            block.push('\n');
        }
        block.push_str(line.trim_start());
        cursor += 1;
    }
    (block, cursor)
}

pub(super) fn is_yaml_block_scalar(value: &str) -> bool {
    value.trim_start().starts_with(['|', '>'])
}

pub(super) fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

/// `origin` moved to the 0-based line `index`.
pub(super) fn origin_at(file: &str, index: usize, label: &str) -> ReferenceOrigin {
    ReferenceOrigin {
        file: file.to_owned(),
        line: line_number(index),
        label: label.to_owned(),
    }
}

fn line_number(index: usize) -> Option<u32> {
    u32::try_from(index + 1).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_key_line_finds_key_and_sub_table_header() {
        let text = "[project]\nname = \"demo\"\n\n[tool.pdm.scripts]\ntest = \"pytest\"\n\"lint-all\" = \"ruff check\"\n\n[tool.pdm.scripts.serve]\ncmd = \"gunicorn app:app\"\n";
        assert_eq!(toml_key_line(text, "tool.pdm.scripts", "test"), Some(5));
        assert_eq!(toml_key_line(text, "tool.pdm.scripts", "lint-all"), Some(6));
        assert_eq!(toml_key_line(text, "tool.pdm.scripts", "serve"), Some(8));
        assert_eq!(toml_key_line(text, "tool.pdm.scripts", "name"), None);
    }

    #[test]
    fn ini_value_joins_continuation_lines() {
        let contents = "[tool:pytest]\naddopts =\n    --cov=src\n    -n auto\ntestpaths = tests\n";
        assert_eq!(
            ini_value(contents, "tool:pytest", "addopts"),
            Some((1, "\n--cov=src\n-n auto".to_owned()))
        );
        assert_eq!(ini_value(contents, "pytest", "addopts"), None);
        assert_eq!(ini_section_line(contents, "tool:pytest"), Some(0));
    }

    #[test]
    fn logical_lines_join_backslash_continuations_on_crlf() {
        let contents = "a \\\r\n  b\r\nc\r\n";
        assert_eq!(
            logical_lines(contents),
            vec![(0, "a    b".to_owned()), (2, "c".to_owned())]
        );
    }

    #[test]
    fn yaml_block_body_stops_at_parent_indent() {
        let lines = ["  run: |", "    pytest", "    ruff check", "  next: 1"];
        assert_eq!(
            yaml_block_body(&lines, 1, 2),
            ("pytest\nruff check".to_owned(), 3)
        );
    }
}
