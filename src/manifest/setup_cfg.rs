//! `setup.cfg` manifest extraction.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use crate::path_util::rel_to_root;

use super::entry_points::EntryPoints;
use super::error::ManifestError;
use super::extract::unreadable_reason;
use super::pep508::is_python_whitespace;
use super::requirements::extract_requirements_path;
use super::setuptools_reqs::{SetuptoolsItem, setuptools_requirement_items};
use super::types::{
    DeclaredDependency, DependencyContext, EntryPointDecl, ExtrasState, ProjectMetadata,
};
use super::util::{DependencyPush, path_is_within_root, push_dependency, read_text};
use super::warnings::ManifestWarning;

/// Partial extraction result from `setup.cfg`.
#[derive(Debug, Default)]
pub(super) struct SetupCfgExtraction {
    /// Project metadata.
    pub metadata: ProjectMetadata,
    /// Declared dependencies.
    pub dependencies: Vec<DeclaredDependency>,
    /// Non-fatal warnings.
    pub warnings: Vec<ManifestWarning>,
    /// The file could not be decoded and was left out.
    pub skipped: bool,
    /// `install_requires` names a `file:` that could not be read.
    pub runtime_unknown: bool,
    /// How much of `[options.extras_require]` could be read; `None` when
    /// the file has no such section.
    pub extras: Option<ExtrasState>,
    /// Root-relative requirements files read through `file:`.
    pub files_read: Vec<String>,
    /// Root-relative requirements files `file:` names that do not exist.
    pub files_missing: Vec<String>,
    pub entry_points: Vec<EntryPointDecl>,
}

/// Extract manifest data from `setup.cfg`.
pub(super) fn extract_setup_cfg(
    root: &Path,
    path: &Path,
) -> Result<SetupCfgExtraction, ManifestError> {
    let rel = rel_to_root(root, path);
    let mut result = SetupCfgExtraction::default();
    let Some(contents) = read_text(path)? else {
        result.skipped = true;
        result.extras = Some(ExtrasState::Unknown);
        result
            .warnings
            .push(ManifestWarning::FileUndecodable { file: rel });
        return Ok(result);
    };

    let sections = parse_ini_sections(&contents);
    let one_line = one_line_options(&contents);
    if let Some(metadata) = sections.get("metadata") {
        result.metadata.name = metadata.get("name").cloned();
        result.metadata.version = metadata.get("version").cloned();
    }

    let options = sections.get("options");
    result.metadata.requires_python = options.and_then(|o| o.get("python_requires")).cloned();
    if let Some(requires) = options.and_then(|o| o.get("install_requires")) {
        let option = Requirements {
            root,
            rel: &rel,
            option: "options.install_requires".to_owned(),
            context: DependencyContext::Runtime,
            one_line: one_line.contains(&("options", "install_requires".to_owned())),
        };
        result.runtime_unknown = !option.push(&mut result, requires);
    }

    if let Some(extras) = sections.get("options.extras_require") {
        let mut lost = BTreeSet::new();
        for (extra, requires) in extras {
            if extra.is_empty() {
                continue;
            }
            let option = Requirements {
                root,
                rel: &rel,
                option: format!("options.extras_require.{extra}"),
                context: DependencyContext::SetupExtra(extra.clone()),
                one_line: one_line.contains(&("options.extras_require", extra.clone())),
            };
            if !option.push(&mut result, requires) {
                lost.insert(extra.clone());
            }
        }
        result.extras = Some(if lost.is_empty() {
            ExtrasState::Known
        } else {
            ExtrasState::Partial(lost)
        });
    }

    let mut entry_points = EntryPoints {
        rel: &rel,
        entries: &mut result.entry_points,
        warnings: &mut result.warnings,
    };
    if options.is_some_and(|options| options.contains_key("entry_points")) {
        entry_points.skip_setup_cfg_option();
    }
    for (group, value) in sections.get("options.entry_points").into_iter().flatten() {
        entry_points.read_setup_cfg_group(group, value);
    }

    Ok(result)
}

/// One requirements option: `install_requires` or an extra.
struct Requirements<'a> {
    root: &'a Path,
    rel: &'a str,
    /// Dotted option name, e.g. `options.install_requires`.
    option: String,
    context: DependencyContext,
    /// The value is one `key = value` line, which setuptools splits at `;`.
    one_line: bool,
}

impl Requirements<'_> {
    /// Push the declarations in `value`; `false` when part of them could
    /// not be read.
    ///
    /// `file: a.txt, b.txt` reads each file as setuptools does, with the
    /// requirements parser (which also follows `-r`, which setuptools
    /// rejects). A file that does not exist, leaves the root, or stops
    /// parsing is reported and makes the option unread: setuptools skips a
    /// missing file, but it may be generated before the build.
    fn push(&self, result: &mut SetupCfgExtraction, value: &str) -> bool {
        let Some(spec) = value.strip_prefix("file:") else {
            for item in setup_cfg_requirement_items(value, self.one_line) {
                push_dependency(DependencyPush {
                    dependencies: &mut result.dependencies,
                    warnings: &mut result.warnings,
                    raw: &item.text,
                    context: self.context.clone(),
                    file: self.rel,
                    label: format!("{}[{}]", self.option, item.index),
                    line: None,
                });
            }
            return true;
        };
        let mut complete = true;
        for written in spec
            .split(',')
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            complete &= self.read_file(result, written);
        }
        complete
    }

    /// Read one `file:` path; `false` when it could not be read in full.
    fn read_file(&self, result: &mut SetupCfgExtraction, written: &str) -> bool {
        // setuptools normalizes the path before checking it stays inside.
        let mut relative = PathBuf::new();
        let mut stays_inside = true;
        for part in Path::new(written).components() {
            match part {
                Component::Normal(name) => relative.push(name),
                Component::CurDir => {},
                Component::ParentDir => stays_inside &= relative.pop(),
                Component::RootDir | Component::Prefix(_) => stays_inside = false,
            }
        }
        let path = self.root.join(&relative);
        let (file, reason) = if !stays_inside {
            (written.to_owned(), "outside the project root".to_owned())
        } else if !path.is_file() {
            let missing = rel_to_root(self.root, &path);
            result.files_missing.push(missing.clone());
            (missing, "not found".to_owned())
        } else if !path_is_within_root(self.root, &path) {
            (written.to_owned(), "outside the project root".to_owned())
        } else {
            let (extracted, outcome) = extract_requirements_path(self.root, &path, &self.context);
            let skipped = extracted.skipped;
            result.dependencies.extend(extracted.dependencies);
            result.warnings.extend(extracted.warnings);
            result.files_read.extend(extracted.files_read);
            result.files_missing.extend(extracted.files_missing);
            match outcome {
                // An undecodable file already carries its own warning.
                Ok(()) => return !skipped,
                Err(error) => (rel_to_root(self.root, &path), unreadable_reason(&error)),
            }
        };
        result
            .warnings
            .push(ManifestWarning::RequirementsUnreadable {
                file,
                reason: format!("{reason} (named by `{}` {})", self.rel, self.option),
            });
        false
    }
}

/// The items of a requirements option value as setuptools'
/// `_parse_list_semicolon` and then `parse_strings` read them (#989): a
/// one-line value splits at `;`, so `requests; rich` is two items and a
/// marker is an item of its own; a multi-line value splits at line ends,
/// where `;` starts a marker. The index counts the split items.
pub(crate) fn setup_cfg_requirement_items(value: &str, one_line: bool) -> Vec<SetuptoolsItem> {
    if one_line {
        setuptools_requirement_items(value.split(';'))
    } else {
        setuptools_requirement_items(value.lines())
    }
}

/// `(section, option)` pairs whose value `configparser` reads from one
/// `key = value` line: no continuation line and no repeat. A blank line
/// is no continuation: `configparser` keeps it only when a continuation
/// follows, which then counts.
pub(crate) fn one_line_options(contents: &str) -> BTreeSet<(&str, String)> {
    let mut lines: std::collections::BTreeMap<(&str, String), usize> =
        std::collections::BTreeMap::new();
    for (_, line) in ini_lines(contents) {
        if let IniLine::Value { section, key, .. } = line {
            *lines.entry((section, key)).or_default() += 1;
        }
    }
    lines
        .into_iter()
        .filter_map(|(option, count)| (count == 1).then_some(option))
        .collect()
}

/// One `setup.cfg` line as `configparser` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IniLine<'a> {
    /// Blank, a comment, or a line `configparser` would reject (no
    /// delimiter): setuptools cannot read such a file either, so no
    /// declaration hides in it.
    Skipped,
    /// `[name]`, trimmed.
    Section(&'a str),
    /// Part of an option's value: the `key = value` line itself
    /// (`continuation == false`) or a line indented deeper after it.
    Value {
        section: &'a str,
        /// Lowercased, except in `[options.entry_points]`: setuptools reads
        /// with `optionxform = str`, and a group name is case-sensitive.
        key: String,
        /// Trimmed.
        value: &'a str,
        continuation: bool,
    },
}

/// Classify each line of `setup.cfg`, paired with the line as written
/// (end-of-line bytes included): an option line splits at its first `=` or
/// `:`. As in `configparser`, a line continues the previous option only when
/// it is indented deeper than that option's line; otherwise it is read as a
/// header or an option whatever its indent, so `  install_requires = a`
/// right after `[options]` is an option. Lines before any header belong to
/// `default`.
pub(crate) fn ini_lines(contents: &str) -> impl Iterator<Item = (&str, IniLine<'_>)> {
    let mut current = "default";
    let mut last_key: Option<String> = None;
    // `configparser`'s `indent_level`: the indent of the last header or
    // option line. Blank and comment lines leave it alone.
    let mut indent_level = 0;
    contents.split_inclusive('\n').map(move |raw| {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            return (raw, IniLine::Skipped);
        }
        let indent = raw.chars().take_while(|&c| is_python_whitespace(c)).count();
        if let Some(key) = last_key.as_ref().filter(|_| indent > indent_level) {
            let line = IniLine::Value {
                section: current,
                key: key.clone(),
                value: trimmed,
                continuation: true,
            };
            return (raw, line);
        }
        indent_level = indent;
        let line = if let Some(header) = section_header(trimmed) {
            current = header.trim();
            last_key = None;
            IniLine::Section(current)
        } else if let Some((key, value)) = trimmed.split_once(['=', ':']) {
            let key = if current == "options.entry_points" {
                key.trim().to_owned()
            } else {
                key.trim().to_ascii_lowercase()
            };
            last_key = Some(key.clone());
            IniLine::Value {
                section: current,
                key,
                value: value.trim(),
                continuation: false,
            }
        } else {
            IniLine::Skipped
        };
        (raw, line)
    })
}

/// The name in a `[name]` header line, as `configparser`'s `SECTCRE`
/// (`\[(?P<header>.+)\]`) reads it: a prefix match whose greedy group runs to
/// the last `]`, so text after it (`[options] ; runtime`) is ignored.
fn section_header(trimmed: &str) -> Option<&str> {
    let rest = trimmed.strip_prefix('[')?;
    let end = rest.rfind(']')?;
    (end > 0).then(|| &rest[..end])
}

/// Split `setup.cfg` into sections the way `configparser` does (see
/// [`ini_lines`]); a repeated section or option appends to the first.
fn parse_ini_sections(
    contents: &str,
) -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>> {
    use std::collections::BTreeMap;

    let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (_, line) in ini_lines(contents) {
        match line {
            IniLine::Skipped => {},
            IniLine::Section(name) => {
                sections.entry(name.to_owned()).or_default();
            },
            IniLine::Value {
                section,
                key,
                value,
                continuation,
            } => append_value(
                sections.entry(section.to_owned()).or_default(),
                &key,
                value,
                continuation,
            ),
        }
    }
    sections
}

/// Append `value` to `key` in `section`, newline-joining duplicates.
/// Continuation lines (`continuation`) append even when empty-valued
/// entries exist, matching key=value duplicate handling otherwise.
fn append_value(
    section: &mut std::collections::BTreeMap<String, String>,
    key: &str,
    value: &str,
    continuation: bool,
) {
    if let Some(existing) = section.get_mut(key) {
        if continuation || !value.is_empty() {
            if !existing.is_empty() {
                existing.push('\n');
            }
            existing.push_str(value);
        }
    } else {
        section.insert(key.to_owned(), value.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn item_texts(value: &str) -> Vec<String> {
        setuptools_requirement_items(value.lines())
            .into_iter()
            .map(|item| item.text)
            .collect()
    }

    #[test]
    fn entry_point_groups_keep_their_case() {
        let sections = parse_ini_sections(
            "[Options]\nInstall_Requires = x\n\n[options.entry_points]\nMyPlugins =\n    a = b:c\n",
        );
        assert!(sections["Options"].contains_key("install_requires"));
        assert!(sections["options.entry_points"].contains_key("MyPlugins"));
    }

    #[test]
    fn parses_multiline_install_requires() {
        let contents = r"[options]
install_requires =
    requests
    flask>=1.0
";
        let sections = parse_ini_sections(contents);
        let options = sections.get("options").expect("options section");
        let requires = options.get("install_requires").expect("install_requires");
        assert_eq!(requires, "requests\nflask>=1.0");
        assert_eq!(item_texts(requires), ["requests", "flask>=1.0"]);
    }

    #[test]
    fn accepts_colon_delimiters_like_configparser() {
        let contents =
            "[options]\ninstall_requires:\n    requests\npython_requires : >=3.9\nurl = http://x\n";
        let sections = parse_ini_sections(contents);
        let options = sections.get("options").expect("options section");
        assert_eq!(
            options.get("install_requires").map(String::as_str),
            Some("requests")
        );
        assert_eq!(
            options.get("python_requires").map(String::as_str),
            Some(">=3.9")
        );
        // The first delimiter wins, so a colon inside the value stays put.
        assert_eq!(options.get("url").map(String::as_str), Some("http://x"));
    }

    #[test]
    fn undecodable_setup_cfg_is_skipped_with_a_warning() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("setup.cfg");
        std::fs::write(
            &path,
            b"[metadata]\nname = acme\nauthor = Ren\xe9\n[options]\ninstall_requires = requests\n",
        )
        .expect("write setup.cfg");

        let result = extract_setup_cfg(temp.path(), &path).expect("extract");
        assert!(result.skipped);
        assert_eq!(result.metadata.name, None);
        assert_eq!(result.dependencies, []);
        assert_eq!(
            result.warnings,
            [ManifestWarning::FileUndecodable {
                file: "setup.cfg".to_owned()
            }]
        );
    }

    /// setuptools reads the value with `parse_strings` (#965): ` #` starts
    /// a comment, a `\`-ending item takes the next line, and the label index
    /// is the item's first line so `--fix` finds the line it counts.
    #[test]
    fn values_are_read_like_setuptools_parse_strings() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("setup.cfg");
        std::fs::write(
            &path,
            "[options]\ninstall_requires =\n    requests \\\n      >=2\n    rich  # pretty output\n    rich#x\n\
             [options.extras_require]\ntest =\n    pytest \\\n",
        )
        .expect("write setup.cfg");

        let result = extract_setup_cfg(temp.path(), &path).expect("extract");
        let declared: Vec<(&str, &str, &str)> = result
            .dependencies
            .iter()
            .map(|dep| {
                (
                    dep.name.as_str(),
                    dep.specifier.as_deref().unwrap_or(""),
                    dep.origin.label.as_str(),
                )
            })
            .collect();
        assert_eq!(
            declared,
            [
                ("requests", ">=2", "options.install_requires[0]"),
                ("rich", "", "options.install_requires[2]"),
            ]
        );
        // A bare `#` is no comment, and a `\` with nothing to join stays as
        // written: both are invalid requirements, not silent drops.
        assert_eq!(
            result.warnings,
            [
                ManifestWarning::InvalidRequirementLine {
                    file: "setup.cfg".to_owned(),
                    line: None,
                    label: "options.install_requires[3]".to_owned(),
                    raw: "rich#x".to_owned(),
                    context: DependencyContext::Runtime,
                },
                ManifestWarning::InvalidRequirementLine {
                    file: "setup.cfg".to_owned(),
                    line: None,
                    label: "options.extras_require.test[0]".to_owned(),
                    raw: "pytest \\".to_owned(),
                    context: DependencyContext::SetupExtra("test".to_owned()),
                },
            ]
        );
    }

    /// setuptools' `_parse_list_semicolon` splits a one-line value at `;`
    /// and a multi-line value at line ends, where `;` starts a marker (#989).
    #[test]
    fn one_line_values_split_at_semicolons_like_setuptools() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("setup.cfg");
        std::fs::write(
            &path,
            "[options]\ninstall_requires = requests; rich ;; # c\n\
             [options.extras_require]\nwin = pywin32; os_name == \"nt\"\n\
             old =\n    six; python_version < \"3\"\nany:  a;b\n\n    c\n",
        )
        .expect("write setup.cfg");

        let result = extract_setup_cfg(temp.path(), &path).expect("extract");
        let declared: Vec<(&str, Option<&str>, &str)> = result
            .dependencies
            .iter()
            .map(|dep| {
                (
                    dep.name.as_str(),
                    dep.marker.as_deref(),
                    dep.origin.label.as_str(),
                )
            })
            .collect();
        assert_eq!(
            declared,
            [
                ("requests", None, "options.install_requires[0]"),
                ("rich", None, "options.install_requires[1]"),
                ("c", None, "options.extras_require.any[1]"),
                (
                    "six",
                    Some("python_version < \"3\""),
                    "options.extras_require.old[0]"
                ),
                ("pywin32", None, "options.extras_require.win[0]"),
            ]
        );
        // On one line a marker is a requirement of its own, which setuptools
        // also rejects; a blank line and a continuation make `a;b` a line of
        // a multi-line value, where `b` is a marker.
        let invalid =
            |label: &str, raw: &str, extra: &str| ManifestWarning::InvalidRequirementLine {
                file: "setup.cfg".to_owned(),
                line: None,
                label: label.to_owned(),
                raw: raw.to_owned(),
                context: DependencyContext::SetupExtra(extra.to_owned()),
            };
        assert_eq!(
            result.warnings,
            [
                invalid("options.extras_require.any[0]", "a;b", "any"),
                invalid("options.extras_require.win[1]", "os_name == \"nt\"", "win"),
            ]
        );
    }

    /// An indented comment is not part of the value, and an extra's `]` at
    /// the end of a continuation does not open a section.
    #[test]
    fn continuations_skip_comments_and_keep_extras() {
        let contents = "[options]\ninstall_requires =\n    requests[security]\n    # pinned: see #12\n    flask\n";
        let sections = parse_ini_sections(contents);
        let requires = sections
            .get("options")
            .and_then(|options| options.get("install_requires"))
            .expect("install_requires");
        assert_eq!(item_texts(requires), ["requests[security]", "flask"]);
    }

    /// `configparser` continues an option only with a line indented deeper
    /// than the option's own line: an indented key right after a header is
    /// an option, and a line no deeper than the option opens the next one.
    #[test]
    fn indented_keys_are_options_like_configparser() {
        let contents = "[options]\n  install_requires =\n    requests\n  # c\n\n      flask\n  python_requires = >=3.9\n\
                        zip_safe = no\n[options.extras_require]\n\ttest = pytest\n";
        let sections = parse_ini_sections(contents);
        let options = &sections["options"];
        assert_eq!(options["install_requires"], "requests\nflask");
        assert_eq!(options["python_requires"], ">=3.9");
        assert_eq!(options["zip_safe"], "no");
        assert_eq!(sections["options.extras_require"]["test"], "pytest");
        let one_line = one_line_options(contents);
        assert!(!one_line.contains(&("options", "install_requires".to_owned())));
        assert!(one_line.contains(&("options.extras_require", "test".to_owned())));
    }

    #[test]
    fn indented_install_requires_declares_its_dependencies() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("setup.cfg");
        std::fs::write(
            &path,
            "[options]\n  install_requires = requests; rich\n  packages = find:\n",
        )
        .expect("write setup.cfg");

        let result = extract_setup_cfg(temp.path(), &path).expect("extract");
        let names: Vec<&str> = result
            .dependencies
            .iter()
            .map(|dep| dep.name.as_str())
            .collect();
        assert_eq!(names, ["requests", "rich"]);
        assert_eq!(result.warnings, []);
    }

    #[test]
    fn headers_ignore_text_after_the_last_bracket_like_configparser() {
        let contents = "[options] ; runtime\ninstall_requires = a\n[x]y] #z\nk = v\n";
        let sections = parse_ini_sections(contents);
        assert_eq!(
            sections
                .get("options")
                .and_then(|o| o.get("install_requires"))
                .map(String::as_str),
            Some("a")
        );
        // The group is greedy, so it runs to the last `]` on the line.
        assert_eq!(
            sections
                .get("x]y")
                .and_then(|o| o.get("k"))
                .map(String::as_str),
            Some("v")
        );
    }

    #[test]
    fn section_header_needs_a_name_between_the_brackets() {
        assert_eq!(section_header("[a]"), Some("a"));
        // `.+` needs a character, so `[]` is no header (configparser
        // rejects the line).
        assert_eq!(section_header("[]"), None);
        assert_eq!(section_header("[a"), None);
        assert_eq!(section_header("a]"), None);
    }

    #[test]
    fn merges_repeated_sections_and_keeps_empty_ones() {
        let contents =
            "[options]\ninstall_requires = a\n[empty]\n[options]\ninstall_requires =\n    b\n";
        let sections = parse_ini_sections(contents);
        assert_eq!(
            sections
                .get("options")
                .and_then(|o| o.get("install_requires"))
                .map(String::as_str),
            Some("a\nb")
        );
        assert!(sections.get("empty").is_some_and(BTreeMap::is_empty));
        assert!(!sections.contains_key("default"));
    }

    mod props {
        use std::fmt::Write as _;

        use super::*;
        use proptest::prelude::*;

        /// INI values that survive `key = value` rendering unchanged: no
        /// newlines, no comment leaders, and no surrounding whitespace.
        fn ini_value() -> impl Strategy<Value = String> {
            "[A-Za-z0-9><=~. _-]{0,30}".prop_map(|value| value.trim().to_owned())
        }

        proptest! {
            #[test]
            fn parse_ini_sections_never_panics(contents in "\\PC{0,400}") {
                let _ = parse_ini_sections(&contents);
            }

            #[test]
            fn parse_ini_sections_roundtrips_flat_keys(
                section in "[a-z][a-z0-9.]{0,15}",
                entries in prop::collection::btree_map("[a-z][a-z0-9_]{0,12}", ini_value(), 0..6),
            ) {
                let mut contents = format!("[{section}]\n");
                for (key, value) in &entries {
                    writeln!(contents, "{key} = {value}").expect("write to string");
                }

                let sections = parse_ini_sections(&contents);
                let parsed = sections.get(&section).expect("section must exist");
                prop_assert_eq!(parsed, &entries);
            }

            #[test]
            fn parse_ini_sections_joins_continuation_lines(
                values in prop::collection::vec("[a-z][a-z0-9>=.-]{0,15}", 1..6),
            ) {
                let mut contents = String::from("[options]\ninstall_requires =\n");
                for value in &values {
                    writeln!(contents, "    {value}").expect("write to string");
                }

                let sections = parse_ini_sections(&contents);
                let requires = sections
                    .get("options")
                    .and_then(|options| options.get("install_requires"))
                    .expect("install_requires must exist");
                prop_assert_eq!(item_texts(requires), values);
            }
        }
    }
}
