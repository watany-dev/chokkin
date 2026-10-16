//! Line numbers for `pyproject.toml` declarations.
//!
//! Extraction reads the file with `toml`, which keeps no positions. The
//! declarations it yields carry a key-path label (`project.dependencies[0]`,
//! `tool.poetry.dependencies.zope.interface`); this re-parses the same text
//! with `toml_edit`, which keeps spans, and walks each label to its item
//! (#983). A label the walk cannot follow keeps `line: None`, the value it had
//! before, so reports lose nothing they showed.

use ruff_text_size::TextSize;
use toml_edit::{Document, Item};

use crate::parser::LineIndex;

use super::types::DeclaredDependency;

/// Set `origin.line` on every declaration from `file` that has none.
pub(super) fn fill_declaration_lines(
    contents: &str,
    file: &str,
    declarations: &mut [DeclaredDependency],
) {
    let Ok(document) = Document::parse(contents) else {
        return;
    };
    let lines = LineIndex::new(contents);
    for declaration in declarations
        .iter_mut()
        .filter(|declaration| declaration.origin.file == file && declaration.origin.line.is_none())
    {
        declaration.origin.line = label_offset(document.as_item(), &declaration.origin.label)
            .map(|offset| line_of(&lines, offset));
    }
}

/// Byte offset of the item `label` names under `item`. Keys may hold dots
/// (`"zope.interface" = "*"`), so each step tries the keys of the current
/// table that the rest of the label starts with, longest first.
fn label_offset(item: &Item, label: &str) -> Option<usize> {
    if let Some(rest) = label.strip_prefix('[') {
        let (index, rest) = rest.split_once(']')?;
        let value = item.as_array()?.get(index.parse().ok()?)?;
        // Labels end at an array element; none name a key inside one.
        return value
            .span()
            .filter(|_| rest.is_empty())
            .map(|span| span.start);
    }
    let table = item.as_table_like()?;
    let mut keys: Vec<_> = table
        .iter()
        .map(|(key, _)| key)
        .filter(|key| {
            label
                .strip_prefix(key)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '[']))
        })
        .collect();
    keys.sort_by_key(|key| std::cmp::Reverse(key.len()));
    keys.into_iter().find_map(|key| {
        let rest = &label[key.len()..];
        let (entry_key, child) = table.get_key_value(key)?;
        if rest.is_empty() {
            // A key's own span starts its line even when the value is a
            // multi-line inline table or array.
            return entry_key
                .span()
                .or_else(|| child.span())
                .map(|span| span.start);
        }
        label_offset(child, rest.strip_prefix('.').unwrap_or(rest))
    })
}

fn line_of(lines: &LineIndex, offset: usize) -> u32 {
    lines.line(TextSize::new(u32::try_from(offset).unwrap_or(u32::MAX)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offset_line(contents: &str, label: &str) -> Option<u32> {
        let document = Document::parse(contents).expect("valid toml");
        label_offset(document.as_item(), label)
            .map(|offset| line_of(&LineIndex::new(contents), offset))
    }

    #[test]
    fn labels_walk_tables_arrays_and_dotted_keys() {
        let contents = "[project]\ndependencies = [\n  \"a\",\n  \"b\",\n]\n\n\
                        [tool.poetry.dependencies]\n\"zope.interface\" = \"*\"\nrich = { version = \"*\" }\n\n\
                        [tool.poetry.group.dev.dependencies]\npytest = \"*\"\n";
        assert_eq!(offset_line(contents, "project.dependencies[1]"), Some(4));
        assert_eq!(
            offset_line(contents, "tool.poetry.dependencies.zope.interface"),
            Some(8)
        );
        assert_eq!(
            offset_line(contents, "tool.poetry.dependencies.rich"),
            Some(9)
        );
        assert_eq!(
            offset_line(contents, "tool.poetry.group.dev.dependencies.pytest"),
            Some(12)
        );
    }

    #[test]
    fn unknown_labels_have_no_line() {
        let contents = "[project]\ndependencies = [\"a\"]\n";
        assert_eq!(offset_line(contents, "project.dependencies[3]"), None);
        assert_eq!(offset_line(contents, "project.missing[0]"), None);
        assert_eq!(offset_line(contents, "project.dependencies.x"), None);
    }

    #[test]
    fn dotted_key_tables_resolve() {
        let contents = "project.name = \"acme\"\nproject.dependencies = [\"a\", \"b\"]\n";
        assert_eq!(offset_line(contents, "project.dependencies[1]"), Some(2));
    }
}
