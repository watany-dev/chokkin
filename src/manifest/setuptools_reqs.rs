//! The reader setuptools applies to a requirements option value
//! (`setuptools._reqs.parse_strings`).

use super::pep508::strip_python;

/// One requirement text of a setuptools option value.
pub(crate) struct SetuptoolsItem {
    /// Index of the item's first line among the value's non-blank,
    /// non-comment lines: the index `--fix` counts physical lines by, so a
    /// label points at the line the item starts on.
    pub(crate) index: usize,
    /// The text after comment removal and continuation joining.
    pub(crate) text: String,
}

/// Read the lines of a setuptools requirements value (`install_requires`
/// or an extra in `setup.cfg`, the string form and list items in
/// `setup.py`) as `parse_strings` does (#965): `yield_lines` strips each
/// line and drops blank and `#`-leading lines, `drop_comment` cuts the line
/// at ` #` (a `#` with no space before it stays, unlike pip), and
/// `join_continuation` appends the next line to a line ending in `\` with
/// the `\` and the character before it removed and the rest stripped
/// (`requests \` + `>=2` is `requests>=2`, `requests\` + `>=2` is
/// `request>=2`).
///
/// setuptools drops an item whose last line ends in `\` with nothing to
/// join. chokkin keeps it as written, so it is reported as an invalid
/// requirement line (#928) instead of vanishing (ADR 0005), unless a URL
/// takes the `\` in (`pkg @ https://h/x.zip\` stays declared).
pub(crate) fn setuptools_requirement_items<'a>(
    lines: impl IntoIterator<Item = &'a str>,
) -> Vec<SetuptoolsItem> {
    let mut items = Vec::new();
    let mut pending: Option<SetuptoolsItem> = None;
    let lines = lines
        .into_iter()
        .map(strip_python)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(drop_comment);
    for (index, line) in lines.enumerate() {
        let item = match pending.take() {
            Some(mut item) => {
                // `item[:-2].strip()`: the `\` and the character before it go.
                let mut head = item.text.chars();
                head.next_back();
                head.next_back();
                item.text = format!("{}{line}", strip_python(head.as_str()));
                item
            },
            None => SetuptoolsItem {
                index,
                text: line.to_owned(),
            },
        };
        if item.text.ends_with('\\') {
            pending = Some(item);
        } else {
            items.push(trimmed(item));
        }
    }
    items.extend(pending.map(trimmed));
    items
}

fn drop_comment(line: &str) -> &str {
    line.split_once(" #").map_or(line, |(head, _)| head)
}

/// The item with the space `drop_comment` leaves before ` #` removed.
fn trimmed(mut item: SetuptoolsItem) -> SetuptoolsItem {
    item.text.truncate(item.text.trim_end().len());
    item
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(value: &str) -> Vec<(usize, String)> {
        setuptools_requirement_items(value.lines())
            .into_iter()
            .map(|item| (item.index, item.text))
            .collect()
    }

    #[test]
    fn reads_comments_and_continuations_like_setuptools() {
        assert_eq!(
            texts(
                "\nrequests \\\n  >=2\nrich  # pretty output\nrich#x\n# note\n\n a \\\n b \\\n c\n"
            ),
            [
                (0, "requests>=2".to_owned()),
                (2, "rich".to_owned()),
                (3, "rich#x".to_owned()),
                (4, "abc".to_owned()),
            ]
        );
        // The comment goes before the join, so a commented `\` still joins.
        assert_eq!(texts("a \\ # c\nb"), [(0, "ab".to_owned())]);
        // `join_continuation` also drops the character before the `\`.
        assert_eq!(texts("requests\\\n>=2"), [(0, "request>=2".to_owned())]);
        assert_eq!(texts("ab\\\\\ncd"), [(0, "abcd".to_owned())]);
        assert_eq!(texts("a\\\n\\\nb"), [(0, "b".to_owned())]);
        // `str.strip()` also removes the information separators (#890).
        assert_eq!(
            texts("\x1f# note\n\x1f\nrich\x1f"),
            [(0, "rich".to_owned())]
        );
    }

    #[test]
    fn keeps_a_dangling_continuation_as_written() {
        assert_eq!(texts("requests \\"), [(0, "requests \\".to_owned())]);
        assert_eq!(texts("a \\\nb \\\n"), [(0, "ab \\".to_owned())]);
        // A space after the `\` is no continuation for setuptools either.
        assert_eq!(
            texts("a \\  # c\nb"),
            [(0, "a \\".to_owned()), (1, "b".to_owned())]
        );
        assert_eq!(texts(""), []);
        assert_eq!(texts("\\"), [(0, "\\".to_owned())]);
    }
}
