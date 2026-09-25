//! PEP 723 inline script metadata (`# /// script` blocks).
//!
//! Blocks are read from the file text, not the AST, so a script whose body
//! does not parse still gets its own dependency scope.

use std::path::Path;

use toml::Value;

use crate::config::TargetVersion;

use super::extract::infer_target_version_from_requires_python;
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
pub fn parse_inline_script(
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
pub fn inline_script_target(text: &str) -> Option<TargetVersion> {
    if !text.contains(SCRIPT_OPENING) {
        return None;
    }
    let block = read_script_block(text).ok()??;
    match block.table.get("requires-python") {
        Some(Value::String(value)) => infer_target_version_from_requires_python(value),
        _ => None,
    }
}

/// Read every listed Python file below `root` and collect its script block.
///
/// Unreadable files are skipped here; parse (step 5) reports them.
pub fn discover_inline_scripts<'a>(
    root: &Path,
    paths: impl IntoIterator<Item = &'a str>,
) -> (Vec<InlineScript>, Vec<ManifestWarning>) {
    let mut scripts = Vec::new();
    let mut warnings = Vec::new();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(root.join(path)) else {
            continue;
        };
        if let Some(script) = parse_inline_script(path, &text, &mut warnings) {
            scripts.push(script);
        }
    }
    (scripts, warnings)
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
    let dependencies = declared_dependencies(path, block, &raw_dependencies, warnings);
    let target_version = requires_python
        .as_deref()
        .and_then(infer_target_version_from_requires_python);
    Ok(InlineScript {
        path: path.to_owned(),
        dependencies,
        requires_python,
        target_version,
    })
}

fn declared_dependencies(
    path: &str,
    block: &ScriptBlock,
    raw_dependencies: &[String],
    warnings: &mut Vec<ManifestWarning>,
) -> Vec<DeclaredDependency> {
    let mut dependencies = Vec::new();
    let mut cursor = 0;
    for (index, raw) in raw_dependencies.iter().enumerate() {
        if let Some(offset) = block
            .content
            .iter()
            .skip(cursor)
            .position(|line| line.contains(raw.as_str()))
        {
            cursor += offset;
        }
        let line = u32::try_from(block.first_content + cursor + 1).ok();
        let origin = DependencyOrigin {
            file: path.to_owned(),
            line,
            label: format!("script.dependencies[{index}]"),
        };
        match parse_pep508_requirement(raw, DependencyContext::Runtime, origin) {
            Ok(dependency) => dependencies.push(dependency),
            Err(warning) => warnings.push(warning),
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
        assert!(warnings.is_empty());
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
    fn crlf_block_matches_lf_block() {
        let crlf = SPEC_EXAMPLE.replace('\n', "\r\n");
        let (lf_script, _) = parse(SPEC_EXAMPLE);
        let (crlf_script, warnings) = parse(&crlf);
        assert!(warnings.is_empty());
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
        assert!(warnings.is_empty());
    }

    #[test]
    fn other_block_kinds_do_not_count_as_script() {
        let text = format!("# /// pyproject\n# [tool.x]\n# ///\n\n{SPEC_EXAMPLE}");
        let (script, warnings) = parse(&text);
        assert!(script.is_some());
        assert!(warnings.is_empty());
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
    }
}
