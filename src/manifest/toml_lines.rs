//! Line numbers for `pyproject.toml` declarations and plugin settings.
//!
//! Extraction reads the file into a `toml::Table`, which keeps no positions.
//! The declarations and entry points it yields carry a key-path label
//! (`project.dependencies[0]`, `tool.poetry.dependencies.zope.interface`,
//! `project.scripts.serve`); this walks each label through the spanned
//! `DeTable` the table was deserialized from (#983, #882). A label the walk
//! cannot follow keeps `line: None`, the value it had before, so reports lose
//! nothing they showed.

use ruff_text_size::TextSize;
use toml::de::{DeTable, DeValue};

use crate::parser::LineIndex;

use super::types::DependencyOrigin;

/// Set `line` on every origin from `file` that has none; `document` is
/// `contents` parsed.
pub(super) fn fill_origin_lines<'a>(
    document: &DeTable<'_>,
    contents: &str,
    file: &str,
    origins: impl IntoIterator<Item = &'a mut DependencyOrigin>,
) {
    let lines = LineIndex::new(contents);
    for origin in origins
        .into_iter()
        .filter(|origin| origin.file == file && origin.line.is_none())
    {
        origin.line = table_offset(document, &origin.label).map(|offset| line_of(&lines, offset));
    }
}

/// 1-based line of the item `label` names in the TOML `contents`
/// (`tool.pytest.ini_options.pytest_plugins[1]`), for a reader that keeps
/// only the parsed table; `None` when `contents` does not parse or the
/// label leads nowhere.
pub(crate) fn toml_label_line(contents: &str, label: &str) -> Option<u32> {
    let document = DeTable::parse(contents).ok()?;
    table_offset(document.get_ref(), label).map(|offset| line_of(&LineIndex::new(contents), offset))
}

/// Byte offset of the item `label` names under `value`.
fn label_offset(value: &DeValue<'_>, label: &str) -> Option<usize> {
    if let Some(rest) = label.strip_prefix('[') {
        let (index, rest) = rest.split_once(']')?;
        let element = value.as_array()?.get(index.parse::<usize>().ok()?)?;
        // Labels end at an array element; none name a key inside one.
        return rest.is_empty().then(|| element.span().start);
    }
    table_offset(value.as_table()?, label)
}

/// Keys may hold dots (`"zope.interface" = "*"`), so a key can end at any `.`
/// or `[` of `label`, or at its end; longer keys are tried first.
fn table_offset(table: &DeTable<'_>, label: &str) -> Option<usize> {
    std::iter::once(label.len())
        .chain(label.rmatch_indices(['.', '[']).map(|(end, _)| end))
        .find_map(|end| {
            let (key, child) = table.get_key_value(&label[..end])?;
            let rest = &label[end..];
            if rest.is_empty() {
                // A key's own span starts its line even when the value is a
                // multi-line inline table or array.
                return Some(key.span().start);
            }
            label_offset(child.get_ref(), rest.strip_prefix('.').unwrap_or(rest))
        })
}

fn line_of(lines: &LineIndex, offset: usize) -> u32 {
    lines.line(TextSize::new(u32::try_from(offset).unwrap_or(u32::MAX)))
}

#[cfg(test)]
mod tests {
    use super::super::types::{DeclaredDependency, DependencyContext};
    use super::*;

    fn offset_line(contents: &str, label: &str) -> Option<u32> {
        let document = DeTable::parse(contents).expect("valid toml");
        table_offset(document.get_ref(), label)
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
    fn only_lineless_declarations_of_the_file_are_filled() {
        let contents = "[project]\ndependencies = [\"a\"]\n";
        let document = DeTable::parse(contents).expect("valid toml");
        let declaration = |file: &str, line: Option<u32>| DeclaredDependency {
            name: "a".to_owned(),
            extras: Vec::new(),
            marker: None,
            specifier: None,
            context: DependencyContext::Runtime,
            origin: DependencyOrigin {
                file: file.to_owned(),
                line,
                label: "project.dependencies[0]".to_owned(),
            },
            opaque: false,
            included_via: Vec::new(),
            unportable_key: None,
        };
        let mut declarations = [
            declaration("pyproject.toml", None),
            declaration("pyproject.toml", Some(7)),
            declaration("requirements.txt", None),
        ];
        fill_origin_lines(
            document.get_ref(),
            contents,
            "pyproject.toml",
            declarations
                .iter_mut()
                .map(|declaration| &mut declaration.origin),
        );
        let lines: Vec<_> = declarations.iter().map(|d| d.origin.line).collect();
        assert_eq!(lines, [Some(2), Some(7), None]);
    }

    #[test]
    fn dotted_key_tables_resolve() {
        let contents = "project.name = \"acme\"\nproject.dependencies = [\"a\", \"b\"]\n";
        assert_eq!(offset_line(contents, "project.dependencies[1]"), Some(2));
    }
}
