//! `# chokkin: ignore[…]` directive extraction from source text.

use std::sync::LazyLock;

use regex::Regex;
use ruff_text_size::TextSize;

use super::lines::LineIndex;
use super::types::IgnoreDirective;

#[allow(clippy::expect_used)]
static IGNORE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"#\s*chokkin:\s*(file-)?ignore\[([A-Z][A-Z0-9_]*(?:,[A-Z][A-Z0-9_]*)*)\]")
        .expect("valid ignore regex")
});

/// Extract chokkin ignore directives from raw source.
#[must_use]
pub fn extract_ignores(source: &str) -> Vec<IgnoreDirective> {
    let mut directives = Vec::new();
    let first_stmt_offset = first_statement_offset(source);
    let lines = LineIndex::new(source);

    for caps in IGNORE_RE.captures_iter(source) {
        let Some(full) = caps.get(0) else {
            continue;
        };
        let file_level = caps.get(1).is_some();
        let Some(codes_match) = caps.get(2) else {
            continue;
        };
        let codes_raw = codes_match.as_str();
        let codes: Vec<String> = codes_raw
            .split(',')
            .map(str::trim)
            .filter(|code| is_valid_code(code))
            .map(str::to_owned)
            .collect();
        if codes.is_empty() {
            continue;
        }

        if file_level {
            if full.start() >= first_stmt_offset {
                continue;
            }
        } else if full.start() < first_stmt_offset {
            // Standalone comment lines before code are not inline ignores.
            continue;
        }

        let line = if file_level {
            0
        } else {
            TextSize::try_from(full.start()).map_or(0, |offset| lines.line(offset))
        };

        directives.push(IgnoreDirective {
            file_level,
            codes,
            line,
        });
    }

    directives
}

fn is_valid_code(code: &str) -> bool {
    code.len() == 6 && code.starts_with("CHK") && code[3..].chars().all(|ch| ch.is_ascii_digit())
}

/// Byte offset where the leading comment/blank block ends. Lines split at
/// `\r` too, so a CRLF ending counts both bytes and a lone `\r` ends a line.
fn first_statement_offset(source: &str) -> usize {
    let mut offset: usize = 0;
    for line in source.split_inclusive(['\n', '\r']) {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            break;
        }
        offset = offset.saturating_add(line.len());
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inline_ignore() {
        let source = "import sys  # chokkin: ignore[CHK003]\n";
        let directives = extract_ignores(source);
        assert_eq!(directives.len(), 1);
        assert!(!directives[0].file_level);
        assert_eq!(directives[0].codes, vec!["CHK003".to_owned()]);
        assert_eq!(directives[0].line, 1);
    }

    #[test]
    fn file_ignore_after_long_crlf_header_is_kept() {
        let source = format!(
            "{}# chokkin: file-ignore[CHK001]\r\nimport os\r\n",
            "# license\r\n".repeat(40)
        );
        let directives = extract_ignores(&source);
        assert_eq!(directives.len(), 1);
        assert!(directives[0].file_level);
    }

    #[test]
    fn inline_ignore_line_counts_lone_cr() {
        let directives = extract_ignores("\rimport os\rimport sys  # chokkin: ignore[CHK003]\r");
        assert_eq!(directives.len(), 1);
        assert_eq!(directives[0].line, 3);
    }

    #[test]
    fn parses_file_ignore_before_code() {
        let source = "# chokkin: file-ignore[CHK001,CHK006]\nimport os\n";
        let directives = extract_ignores(source);
        assert_eq!(directives.len(), 1);
        assert!(directives[0].file_level);
        assert_eq!(
            directives[0].codes,
            vec!["CHK001".to_owned(), "CHK006".to_owned()]
        );
    }
}
