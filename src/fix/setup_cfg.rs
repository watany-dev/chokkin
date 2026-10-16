//! `setup.cfg` dependency line edits (limited sections).

use crate::manifest::{
    IniLine, ini_lines, normalize_distribution_name, pep508_distribution_name,
    setuptools_requirement_items,
};

use super::error::FixError;
use super::write::{read_manifest, write_manifest};

/// Remove the declaration at `label` (`options.install_requires[i]` or
/// `options.extras_require.<extra>[i]`) from `setup.cfg`.
///
/// Only that item's bytes change: a continuation line goes with its line
/// ending, and an inline value leaves `key =` and the line ending behind, so
/// the option stays declared. Other sections, options and items, comments
/// and line endings stay as written. Refuses when the item no longer names
/// `distribution`, so a stale index never removes another dependency.
pub(super) fn remove_dependency(
    path: &std::path::Path,
    label: &str,
    distribution: &str,
) -> Result<String, FixError> {
    let (rel, contents) = read_manifest(path, "setup.cfg")?;
    let (section, key, index) = parse_label(label).ok_or_else(|| FixError::Unsupported {
        detail: format!("unsupported setup.cfg label `{label}`"),
    })?;
    let target = normalize_distribution_name(distribution);

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
            // One physical line is at most one item; a line continued with
            // `\` keeps its `\` here and names no distribution (#965).
            && let Some(item) =
                setuptools_requirement_items(std::iter::once(*value)).into_iter().next()
        {
            if seen == index {
                if pep508_distribution_name(&item.text).is_none_or(|name| name != target) {
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

    write_manifest(path, rel, output.as_bytes())?;
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
