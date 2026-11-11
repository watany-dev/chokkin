//! `setup.cfg` dependency line edits (limited sections).

use crate::manifest::{
    IniLine, ini_lines, normalize_distribution_name, one_line_options, pep508_distribution_name,
    setup_cfg_requirement_items,
};

use super::error::FixError;
use super::write::{ManifestText, read_manifest, write_manifest};

/// Remove the declaration at `label` (`options.install_requires[i]` or
/// `options.extras_require.<extra>[i]`) from `setup.cfg`.
///
/// Only that item's bytes change: a continuation line goes with its line
/// ending, and an inline value leaves `key =` and the line ending behind, so
/// the option stays declared. Other sections, options and items, comments
/// and line endings stay as written. Refuses when the item no longer names
/// `distribution`, so a stale index never removes another dependency, and
/// when a one-line value holds several items (`requests; rich`, #989), as
/// the line cannot go without the others.
pub(super) fn remove_dependency(
    path: &std::path::Path,
    label: &str,
    distribution: &str,
) -> Result<String, FixError> {
    let ManifestText { rel, contents, bom } = read_manifest(path, "setup.cfg")?;
    let (section, key, index) = parse_label(label).ok_or_else(|| FixError::Unsupported {
        detail: format!("unsupported setup.cfg label `{label}`"),
    })?;
    let target = normalize_distribution_name(distribution);
    let one_line = one_line_options(&contents).contains(&(section, key.to_owned()));

    let mut output = String::with_capacity(contents.len());
    let mut seen = 0;
    let mut removed = false;
    for (raw, line) in ini_lines(&contents) {
        if let IniLine::Value {
            section: at,
            key: option,
            value,
            continuation,
        } = &line
            && !removed
            && *at == section
            && *option == key
            // One physical line is at most one item (#965), unless the value
            // is that one line.
            && let items = setup_cfg_requirement_items(value, one_line)
            && let Some(item) = items.first()
        {
            if items.len() > 1 {
                return Err(FixError::Unsupported {
                    detail: format!("`{label}` shares its line in {rel} with other dependencies"),
                });
            }
            if seen == index {
                // A line continued with `\` is only the start of its item:
                // removing it alone would leave the rest of the item behind.
                if item.text.ends_with('\\')
                    || pep508_distribution_name(&item.text).is_none_or(|name| name != target)
                {
                    break;
                }
                removed = true;
                // `ini_lines` splits an option line at this delimiter, so it
                // is always found.
                if !continuation && let Some(delimiter) = raw.find(['=', ':']) {
                    output.push_str(&raw[..=delimiter]);
                    output.push_str(line_ending(raw));
                }
                continue;
            }
            seen += 1;
        }
        output.push_str(raw);
    }
    if !removed {
        return Err(FixError::Unsupported {
            detail: format!("could not find `{label}` in {rel}"),
        });
    }

    write_manifest(path, rel, bom, &output)?;
    Ok(format!("removed `{distribution}` from {rel}"))
}

/// `(section, option, index)` named by a setup.cfg dependency label.
fn parse_label(label: &str) -> Option<(&str, &str, usize)> {
    let (option, index) = label.strip_suffix(']')?.rsplit_once('[')?;
    let index = index.parse().ok()?;
    if option == "options.install_requires" {
        return Some(("options", "install_requires", index));
    }
    let extra = option.strip_prefix("options.extras_require.")?;
    Some(("options.extras_require", extra, index))
}

fn line_ending(raw: &str) -> &str {
    if raw.ends_with("\r\n") {
        "\r\n"
    } else if raw.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn remove(contents: &str, label: &str, distribution: &str) -> Result<String, String> {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("setup.cfg");
        std::fs::write(&path, contents).expect("write");
        remove_dependency(&path, label, distribution).map_err(|error| error.to_string())?;
        Ok(std::fs::read_to_string(&path).expect("read"))
    }

    #[test]
    fn removes_the_labelled_item_only() {
        let contents = "[options]\ninstall_requires =\n    bar>=1.0\n    foobar>=2.0\n";
        assert_eq!(
            remove(contents, "options.install_requires[0]", "bar").as_deref(),
            Ok("[options]\ninstall_requires =\n    foobar>=2.0\n")
        );
    }

    /// An indented key right after the header is an option (configparser),
    /// so its items are found and the key keeps its indent.
    #[test]
    fn removes_from_an_indented_key() {
        let contents = "[options]\n  install_requires = bar\n  extras =\n    x\n";
        assert_eq!(
            remove(contents, "options.install_requires[0]", "bar").as_deref(),
            Ok("[options]\n  install_requires =\n  extras =\n    x\n")
        );
    }

    #[test]
    fn bom_manifest_is_edited_and_keeps_its_bom() {
        let contents = "\u{feff}[options]\ninstall_requires =\n    flask\n    requests\n";
        assert_eq!(
            remove(contents, "options.install_requires[0]", "flask").as_deref(),
            Ok("\u{feff}[options]\ninstall_requires =\n    requests\n")
        );
    }

    #[test]
    fn refuses_a_stale_index() {
        let contents = "[options]\ninstall_requires =\n    bar\n    foobar\n";
        assert_eq!(
            remove(contents, "options.install_requires[1]", "bar"),
            Err("could not find `options.install_requires[1]` in setup.cfg".to_owned())
        );
        assert_eq!(
            remove(contents, "options.install_requires[2]", "bar"),
            Err("could not find `options.install_requires[2]` in setup.cfg".to_owned())
        );
        assert_eq!(
            remove(contents, "metadata.name[0]", "bar"),
            Err("unsupported setup.cfg label `metadata.name[0]`".to_owned())
        );
    }

    #[test]
    fn counts_items_across_repeated_sections_and_skipped_lines() {
        // configparser merges a repeated section; comments and blank lines
        // inside a value are no items.
        let contents = "[options]\ninstall_requires = a\n# note\n[other]\nx = 1\n[options]\n\
                        install_requires =\n\n    ; c\n    b\n    c\n";
        assert_eq!(
            remove(contents, "options.install_requires[2]", "c").as_deref(),
            Ok(
                "[options]\ninstall_requires = a\n# note\n[other]\nx = 1\n[options]\n\
                install_requires =\n\n    ; c\n    b\n"
            )
        );
    }

    /// An inline comment is not part of the item (#965), and an item
    /// continued with `\` spans two lines, so it is left alone.
    #[test]
    fn reads_items_like_setuptools_parse_strings() {
        let contents = "[options]\ninstall_requires =\n    requests \\\n      >=2\n    rich  # pretty output\n";
        assert_eq!(
            remove(contents, "options.install_requires[2]", "rich").as_deref(),
            Ok("[options]\ninstall_requires =\n    requests \\\n      >=2\n")
        );
        assert_eq!(
            remove(contents, "options.install_requires[0]", "requests"),
            Err("could not find `options.install_requires[0]` in setup.cfg".to_owned())
        );
        // A `\` glued to a URL still parses as a requirement on its own, but
        // the line is only the start of an item.
        let url = "[options]\ninstall_requires =\n    requests @ https://h/x.zip\\\n    ; python_version > \"3\"\n";
        assert_eq!(
            remove(url, "options.install_requires[0]", "requests"),
            Err("could not find `options.install_requires[0]` in setup.cfg".to_owned())
        );
    }

    /// A one-line value holds every item setuptools splits at `;` (#989),
    /// so one of several cannot be removed by its line.
    #[test]
    fn leaves_a_line_shared_by_several_items() {
        let contents = "[options]\ninstall_requires = requests; rich\n";
        for (label, distribution) in [
            ("options.install_requires[0]", "requests"),
            ("options.install_requires[1]", "rich"),
        ] {
            assert_eq!(
                remove(contents, label, distribution),
                Err(format!(
                    "`{label}` shares its line in setup.cfg with other dependencies"
                ))
            );
        }
        assert_eq!(
            remove(
                "[options]\ninstall_requires = rich;\n",
                "options.install_requires[0]",
                "rich"
            )
            .as_deref(),
            Ok("[options]\ninstall_requires =\n")
        );
        // On several lines `;` starts a marker, and the line is one item.
        assert_eq!(
            remove(
                "[options]\ninstall_requires =\n    six; python_version < \"3\"\n    rich\n",
                "options.install_requires[0]",
                "six"
            )
            .as_deref(),
            Ok("[options]\ninstall_requires =\n    rich\n")
        );
    }

    #[test]
    fn leaves_other_options_keys_alone() {
        let contents = "[options]\npython_requires = >=3.9\ninstall_requires =\n    boto3\n    requests\n\
                        [options.extras_require]\nTest: pytest\n    boto3\n";
        assert_eq!(
            remove(contents, "options.install_requires[0]", "boto3").as_deref(),
            Ok(
                "[options]\npython_requires = >=3.9\ninstall_requires =\n    requests\n\
                [options.extras_require]\nTest: pytest\n    boto3\n"
            )
        );
        assert_eq!(
            remove(contents, "options.extras_require.test[0]", "pytest").as_deref(),
            Ok(
                "[options]\npython_requires = >=3.9\ninstall_requires =\n    boto3\n    requests\n\
                [options.extras_require]\nTest:\n    boto3\n"
            )
        );
    }
}
