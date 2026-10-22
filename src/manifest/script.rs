//! PEP 723 inline script metadata (`# /// script` blocks).
//!
//! Blocks are read from the file text, not the AST, so a script whose body
//! does not parse still gets its own dependency scope.

use std::path::Path;

use toml::Value;

use crate::config::TargetVersion;

use super::extract::infer_target_version_from_requires_python;
use super::pep508::parse_version_specifiers;
use super::pep508_util::parse_pep508_requirement;
use super::types::{DeclaredDependency, DependencyContext, DependencyOrigin};
use super::warnings::ManifestWarning;

/// Cheap prefilter so files without a block are never split into lines.
const SCRIPT_OPENING: &str = "# /// script";

/// PEP 723 metadata of one standalone script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineScript {
    /// Root-relative script path.
    pub path: String,
    /// `dependencies` entries of the script block.
    pub dependencies: Vec<DeclaredDependency>,
    /// `requires-python` as written.
    pub requires_python: Option<String>,
    /// Lower bound of `requires-python` as a target version.
    pub target_version: Option<TargetVersion>,
    /// `dependencies` entries that could not be parsed, as written, with
    /// their line: each may declare the distribution it names (#936).
    pub invalid_requirements: Vec<(String, Option<u32>)>,
}

impl InlineScript {
    /// Whether `requires-python` is written but could not be read, so the
    /// script's imports were resolved with the project's target (#936).
    #[must_use]
    pub fn requires_python_unreadable(&self) -> bool {
        self.requires_python
            .as_deref()
            .is_some_and(|value| parse_version_specifiers(value).is_none())
    }
}

struct MetadataBlock<'a> {
    kind: &'a str,
    opening: usize,
    closing: usize,
}

struct ScriptBlock {
    /// 0-based line index of the first content line.
    first_content: usize,
    content: Vec<String>,
    table: toml::Table,
}

/// Parse the PEP 723 `script` block of `text`.
///
/// Returns `None` when the file has no valid block. Multiple `script` blocks
/// or broken TOML push a warning, and the file stays an ordinary source.
pub(super) fn parse_inline_script(
    path: &str,
    text: &str,
    warnings: &mut Vec<ManifestWarning>,
) -> Option<InlineScript> {
    if !text.contains(SCRIPT_OPENING) {
        return None;
    }
    let result = read_script_block(text).and_then(|block| {
        block
            .map(|block| build_script(path, &block, warnings))
            .transpose()
    });
    match result {
        Ok(script) => script,
        Err(reason) => {
            warnings.push(ManifestWarning::InlineScriptInvalid {
                file: path.to_owned(),
                reason,
            });
            None
        },
    }
}

/// Target version from a valid `script` block's `requires-python`.
///
/// Uses the same validity rules as [`parse_inline_script`], so the parser and
/// the resolver agree on which files are scripts.
pub(crate) fn inline_script_target(text: &str) -> Option<TargetVersion> {
    parse_inline_script("", text, &mut Vec::new())?.target_version
}

/// Read every listed Python file below `root` and collect its script block.
///
/// Unreadable or undecodable files are skipped here; parse (step 5) reports them.
pub fn discover_inline_scripts<'a>(
    root: &Path,
    paths: impl IntoIterator<Item = &'a str>,
) -> (Vec<InlineScript>, Vec<ManifestWarning>) {
    let paths: Vec<&str> = paths.into_iter().collect();
    let found = crate::parallel::map_in_order(&paths, |path| file_inline_script(root, path));
    let mut scripts = Vec::new();
    let mut warnings = Vec::new();
    for (script, file_warnings) in found {
        scripts.extend(script);
        warnings.extend(file_warnings);
    }
    (scripts, warnings)
}

fn file_inline_script(root: &Path, path: &str) -> (Option<InlineScript>, Vec<ManifestWarning>) {
    let mut warnings = Vec::new();
    let Ok(bytes) = std::fs::read(root.join(path)) else {
        return (None, warnings);
    };
    // Every supported coding keeps ASCII bytes as the same characters, so a
    // file whose bytes lack the opening line cannot decode to text holding it;
    // skipping the decode spares nearly every file a full copy (#761).
    if memchr::memmem::find(&bytes, SCRIPT_OPENING.as_bytes()).is_none() {
        return (None, warnings);
    }
    let script = crate::parser::decode_python_source(bytes)
        .and_then(|text| parse_inline_script(path, &text, &mut warnings));
    (script, warnings)
}

fn read_script_block(text: &str) -> Result<Option<ScriptBlock>, String> {
    // `str::lines` drops a trailing `\r`, so CRLF files match the same rules.
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = metadata_blocks(&lines)
        .into_iter()
        .filter(|block| block.kind == "script");
    let Some(block) = blocks.next() else {
        return Ok(None);
    };
    if blocks.next().is_some() {
        return Err("multiple `script` blocks".to_owned());
    }
    let content: Vec<String> = lines
        .get(block.opening + 1..block.closing)
        .unwrap_or_default()
        .iter()
        .map(|line| line.get(2..).unwrap_or_default().to_owned())
        .collect();
    let table = toml::from_str::<toml::Table>(&content.join("\n")).map_err(|error| {
        let message = error.to_string();
        message.lines().next().unwrap_or_default().to_owned()
    })?;
    Ok(Some(ScriptBlock {
        first_content: block.opening + 1,
        content,
        table,
    }))
}

fn metadata_blocks<'a>(lines: &[&'a str]) -> Vec<MetadataBlock<'a>> {
    let mut blocks = Vec::new();
    let mut index = 0;
    while let Some(line) = lines.get(index) {
        if let Some(kind) = opening_kind(line)
            && let Some(closing) = closing_index(lines, index)
        {
            blocks.push(MetadataBlock {
                kind,
                opening: index,
                closing,
            });
            index = closing + 1;
        } else {
            index += 1;
        }
    }
    blocks
}

fn opening_kind(line: &str) -> Option<&str> {
    let kind = line.strip_prefix("# /// ")?;
    let valid = !kind.is_empty()
        && kind
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-');
    valid.then_some(kind)
}

fn is_content_line(line: &str) -> bool {
    line == "#" || line.starts_with("# ")
}

/// PEP 723 closes a block at the last `# ///` of the comment run after the
/// opening line, leaving at least one content line.
fn closing_index(lines: &[&str], opening: usize) -> Option<usize> {
    let body = lines.get(opening + 1..)?;
    let run = body.iter().take_while(|line| is_content_line(line)).count();
    (1..run)
        .rev()
        .find(|&offset| body.get(offset).is_some_and(|line| *line == "# ///"))
        .map(|offset| opening + 1 + offset)
}

fn build_script(
    path: &str,
    block: &ScriptBlock,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<InlineScript, String> {
    let requires_python = match block.table.get("requires-python") {
        None => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => return Err("`requires-python` must be a string".to_owned()),
    };
    let raw_dependencies = match block.table.get("dependencies") {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| "`dependencies` must be an array of strings".to_owned())?,
        Some(_) => return Err("`dependencies` must be an array of strings".to_owned()),
    };
    let mut invalid_requirements = Vec::new();
    let dependencies = declared_dependencies(
        path,
        block,
        &raw_dependencies,
        &mut invalid_requirements,
        warnings,
    );
    if let Some(value) = &requires_python
        && parse_version_specifiers(value).is_none()
    {
        warnings.push(ManifestWarning::InlineScriptRequiresPythonUnreadable {
            file: path.to_owned(),
            value: value.clone(),
        });
    }
    let target_version = requires_python
        .as_deref()
        .and_then(infer_target_version_from_requires_python);
    Ok(InlineScript {
        path: path.to_owned(),
        dependencies,
        requires_python,
        target_version,
        invalid_requirements,
    })
}

fn declared_dependencies(
    path: &str,
    block: &ScriptBlock,
    raw_dependencies: &[String],
    invalid: &mut Vec<(String, Option<u32>)>,
    warnings: &mut Vec<ManifestWarning>,
) -> Vec<DeclaredDependency> {
    let mut dependencies = Vec::new();
    let content = block.content.join("\n");
    // Byte offset after the previous entry: each entry is found past it, so
    // a repeated entry or a name inside an earlier line (`s` is in
    // `dependencies = [`) does not take an earlier line.
    let mut cursor = 0;
    for (index, raw) in raw_dependencies.iter().enumerate() {
        let rest = &content[cursor..];
        let found = [format!("\"{raw}\""), format!("'{raw}'")]
            .iter()
            .filter_map(|quoted| rest.find(quoted.as_str()).map(|at| (at, quoted.len())))
            .min()
            .or_else(|| rest.find(raw.as_str()).map(|at| (at, raw.len())));
        let start = match found {
            Some((at, len)) => {
                let start = cursor + at;
                cursor = start + len;
                start
            },
            None => cursor,
        };
        let line_offset = content[..start].matches('\n').count();
        let line = u32::try_from(block.first_content + line_offset + 1).ok();
        let origin = DependencyOrigin {
            file: path.to_owned(),
            line,
            label: format!("script.dependencies[{index}]"),
        };
        match parse_pep508_requirement(raw, DependencyContext::Runtime, origin) {
            Ok(dependency) => dependencies.push(dependency),
            Err(warning) => {
                invalid.push((raw.clone(), line));
                warnings.push(warning);
            },
        }
    }
    dependencies
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_EXAMPLE: &str = "# /// script\n# requires-python = \">=3.11\"\n# dependencies = [\n#   \"requests<3\",\n#   \"rich\",\n# ]\n# ///\n\nimport requests\nfrom rich.pretty import pprint\n";

    fn parse(text: &str) -> (Option<InlineScript>, Vec<ManifestWarning>) {
        let mut warnings = Vec::new();
        let script = parse_inline_script("tools/run.py", text, &mut warnings);
        (script, warnings)
    }

    #[test]
    fn parses_pep723_spec_example() {
        let (script, warnings) = parse(SPEC_EXAMPLE);
        let script = script.expect("script block");
        assert_eq!(warnings, []);
        assert_eq!(script.requires_python.as_deref(), Some(">=3.11"));
        assert_eq!(
            script.target_version.as_ref().map(TargetVersion::as_str),
            Some("py311")
        );
        let names: Vec<_> = script
            .dependencies
            .iter()
            .map(|dep| (dep.name.as_str(), dep.origin.line))
            .collect();
        assert_eq!(names, [("requests", Some(4)), ("rich", Some(5))]);
        assert_eq!(
            script.dependencies[0].origin.label,
            "script.dependencies[0]"
        );
    }

    #[test]
    fn dependency_lines_skip_names_inside_earlier_lines() {
        let (script, _) = parse(
            "# /// script\n# requires-python = \">=3.11\"\n# dependencies = [\n#   \"s\",\n#   \"rich\",\n#   \"rich\",\n# ]\n# ///\n",
        );
        let lines: Vec<_> = script
            .expect("script block")
            .dependencies
            .iter()
            .map(|dep| (dep.name.clone(), dep.origin.line))
            .collect();
        assert_eq!(
            lines,
            [
                ("s".to_owned(), Some(4)),
                ("rich".to_owned(), Some(5)),
                ("rich".to_owned(), Some(6))
            ]
        );
    }

    #[test]
    fn keeps_what_the_block_could_not_read() {
        let (script, _) = parse(
            "# /// script\n# requires-python = \"bogus\"\n# dependencies = [\n#   \"rich>=\",\n#   \"httpx\",\n# ]\n# ///\n",
        );
        let script = script.expect("script block");
        assert!(script.requires_python_unreadable());
        assert_eq!(
            script.invalid_requirements,
            [("rich>=".to_owned(), Some(4))]
        );
        let (readable, _) = parse(SPEC_EXAMPLE);
        assert!(!readable.expect("script block").requires_python_unreadable());
    }

    #[test]
    fn latin1_script_file_is_discovered() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut bytes = b"# -*- coding: latin-1 -*-\n".to_vec();
        bytes.extend_from_slice(SPEC_EXAMPLE.as_bytes());
        bytes.extend_from_slice(b"s = 'caf\xe9'\n");
        std::fs::write(temp.path().join("run.py"), bytes).expect("write");
        let (scripts, warnings) = discover_inline_scripts(temp.path(), ["run.py"]);
        assert_eq!(warnings, []);
        assert_eq!(scripts.len(), 1);
    }

    #[test]
    fn discovery_keeps_path_order_across_workers() {
        let temp = tempfile::tempdir().expect("tempdir");
        let broken = "# /// script\n# dependencies = [\n# ///\n";
        let paths: Vec<String> = (0..100).map(|index| format!("mod_{index:03}.py")).collect();
        for (index, path) in paths.iter().enumerate() {
            let text = match index {
                7 | 63 => SPEC_EXAMPLE.to_owned(),
                40 => format!("\u{feff}{SPEC_EXAMPLE}"),
                20 | 90 => broken.to_owned(),
                _ => "import os\n".to_owned(),
            };
            std::fs::write(temp.path().join(path), text).expect("write");
        }
        let (scripts, warnings) =
            discover_inline_scripts(temp.path(), paths.iter().map(String::as_str));
        let found: Vec<&str> = scripts.iter().map(|script| script.path.as_str()).collect();
        assert_eq!(found, ["mod_007.py", "mod_040.py", "mod_063.py"]);
        let warned: Vec<&str> = warnings
            .iter()
            .map(|warning| match warning {
                ManifestWarning::InlineScriptInvalid { file, .. } => file.as_str(),
                other => panic!("unexpected warning {other:?}"),
            })
            .collect();
        assert_eq!(warned, ["mod_020.py", "mod_090.py"]);
    }

    #[test]
    fn crlf_block_matches_lf_block() {
        let crlf = SPEC_EXAMPLE.replace('\n', "\r\n");
        let (lf_script, _) = parse(SPEC_EXAMPLE);
        let (crlf_script, warnings) = parse(&crlf);
        assert_eq!(warnings, []);
        assert_eq!(crlf_script, lf_script);
    }

    #[test]
    fn two_script_blocks_warn_and_are_ignored() {
        let text = format!("{SPEC_EXAMPLE}\n# /// script\n# dependencies = []\n# ///\n");
        let (script, warnings) = parse(&text);
        assert!(script.is_none());
        assert!(matches!(
            warnings.as_slice(),
            [ManifestWarning::InlineScriptInvalid { file, reason }]
                if file == "tools/run.py" && reason.contains("multiple")
        ));
    }

    #[test]
    fn broken_toml_warns_and_is_ignored() {
        let (script, warnings) = parse("# /// script\n# dependencies = [\n# ///\nimport os\n");
        assert!(script.is_none());
        assert!(matches!(
            warnings.as_slice(),
            [ManifestWarning::InlineScriptInvalid { .. }]
        ));
    }

    #[test]
    fn unclosed_block_is_not_a_script() {
        let (script, warnings) = parse("# /// script\n# dependencies = []\nimport os\n");
        assert!(script.is_none());
        assert_eq!(warnings, []);
    }

    #[test]
    fn other_block_kinds_do_not_count_as_script() {
        let text = format!("# /// pyproject\n# [tool.x]\n# ///\n\n{SPEC_EXAMPLE}");
        let (script, warnings) = parse(&text);
        assert!(script.is_some());
        assert_eq!(warnings, []);
    }

    #[test]
    fn inline_script_target_reads_requires_python() {
        assert_eq!(
            inline_script_target(SPEC_EXAMPLE)
                .as_ref()
                .map(TargetVersion::as_str),
            Some("py311")
        );
        assert!(inline_script_target("import os\n").is_none());
        let invalid =
            "# /// script\n# requires-python = \">=3.12\"\n# dependencies = \"rich\"\n# ///\n";
        assert!(inline_script_target(invalid).is_none());
    }

    mod props {
        use super::*;
        use proptest::prelude::*;
        use std::fmt::Write as _;

        /// The reference implementation's regex from PEP 723.
        const PEP_723_REGEX: &str =
            r"(?m)^# /// (?P<type>[a-zA-Z0-9-]+)$\s(?P<content>(^#(| .*)$\s)+)^# ///$";

        /// `(kind, opening line, closing line)` of each block the PEP's
        /// regex finds, scanning `text` the way `re.finditer` does.
        fn reference_blocks(text: &str) -> Vec<(String, usize, usize)> {
            let regex = regex::Regex::new(PEP_723_REGEX).expect("PEP 723 regex");
            let line_of = |offset: usize| text[..offset].matches('\n').count();
            regex
                .captures_iter(text)
                .map(|captures| {
                    let whole = captures.get(0).expect("match");
                    (
                        captures["type"].to_owned(),
                        line_of(whole.start()),
                        line_of(whole.end()),
                    )
                })
                .collect()
        }

        fn blocks(text: &str) -> Vec<(String, usize, usize)> {
            let lines: Vec<&str> = text.lines().collect();
            metadata_blocks(&lines)
                .into_iter()
                .map(|block| (block.kind.to_owned(), block.opening, block.closing))
                .collect()
        }

        /// Lines that open, fill or close a block, or end its comment run.
        const LINES: &[&str] = &[
            "# /// script",
            "# /// pyproject",
            "# /// my-tool2",
            "# /// ",
            "# /// bad_kind",
            "# ///",
            "# ///  ",
            "#",
            "# dependencies = []",
            "# x",
            "#x",
            " # ///",
            "import os",
            "",
        ];

        /// Normalized PEP 508 names: lowercase alphanumeric runs joined by `-`.
        fn name() -> impl Strategy<Value = String> {
            "[a-z][a-z0-9]{0,4}(-[a-z0-9]{1,3})?"
        }

        fn requirement() -> impl Strategy<Value = (String, String)> {
            let tail = prop::sample::select(vec!["", ">=1", "[cli]", "==2.0; python_version < '3.12'"]);
            (name(), tail).prop_map(|(name, tail)| (name.clone(), format!("{name}{tail}")))
        }

        /// Bodies that do not parse as Python, or are no Python at all.
        const BODIES: &[&str] = &["", "import os\n", "def broken(:\n", "\"\"\"\n", "if x\n  y)\n"];

        proptest! {
            /// The block lines found are those the PEP's regex finds.
            #[test]
            fn blocks_match_the_reference_regex(
                lines in prop::collection::vec(prop::sample::select(LINES), 0..16),
            ) {
                let text = lines.iter().map(|line| format!("{line}\n")).collect::<String>();
                prop_assert_eq!(blocks(&text), reference_blocks(&text), "{:?}", text);
            }

            /// The `dependencies` a generated block lists are read back in
            /// order, each on the line it is written on, whatever the body
            /// below the block holds.
            #[test]
            fn script_dependencies_roundtrip(
                requires in prop::option::of(prop::sample::select(vec![">=3.11", ">=3.9,<4"])),
                requirements in prop::collection::vec(requirement(), 0..6),
                before in 0usize..3,
                body in prop::sample::select(BODIES),
            ) {
                let mut text = "# leading comment\n".repeat(before);
                text.push_str("# /// script\n");
                if let Some(requires) = requires {
                    writeln!(text, "# requires-python = \"{requires}\"").expect("write");
                }
                text.push_str("# dependencies = [\n");
                for (_, raw) in &requirements {
                    writeln!(text, "#   \"{raw}\",").expect("write");
                }
                text.push_str("# ]\n# ///\n");
                text.push_str(body);

                let mut warnings = Vec::new();
                let script = parse_inline_script("run.py", &text, &mut warnings)
                    .expect("a valid script block");
                prop_assert_eq!(warnings, []);
                prop_assert_eq!(script.requires_python.as_deref(), requires);
                let first = before + usize::from(requires.is_some()) + 3;
                let expected: Vec<(String, Option<u32>)> = requirements
                    .iter()
                    .enumerate()
                    .map(|(index, (name, _))| (name.clone(), u32::try_from(first + index).ok()))
                    .collect();
                let got: Vec<(String, Option<u32>)> = script
                    .dependencies
                    .iter()
                    .map(|dep| (dep.name.clone(), dep.origin.line))
                    .collect();
                prop_assert_eq!(got, expected, "{}", text);
            }
        }
    }
}
