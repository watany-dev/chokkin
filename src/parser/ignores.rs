//! `# chokkin: ignore[…]` directive extraction from source text.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use ruff_python_ast::token::{TokenKind, Tokens};
use ruff_text_size::{Ranged, TextSize};

use super::lines::LineIndex;
use super::types::{IgnoreDirective, ParseDiagnostic, ParseSeverity};
use crate::rules::RuleId;

#[expect(
    clippy::expect_used,
    reason = "the pattern is a literal; `Regex::new` fails only on a typo, which every run hits"
)]
static IGNORE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"#[ \t]*chokkin:[ \t]*(file-)?ignore\[([A-Z][A-Z0-9_]*(?:,[A-Z][A-Z0-9_]*)*)\]")
        .expect("valid ignore regex")
});

/// Any comment that starts like a directive, read or not (#1132). Unlike
/// [`IGNORE_RE`] it takes any white space, so a directive spelled with NBSP
/// or U+3000 warns instead of vanishing (#1439).
#[expect(
    clippy::expect_used,
    reason = "the pattern is a literal; `Regex::new` fails only on a typo, which every run hits"
)]
static DIRECTIVE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"#\s*chokkin\s*:").expect("valid directive regex"));

const UNRECOGNIZED: &str = "unrecognized `# chokkin:` directive suppresses nothing; the forms are `# chokkin: ignore[CHK001,CHK003]` and `# chokkin: file-ignore[CHK001]`";
const UNKNOWN_CODE: &str = "`# chokkin:` directive names a rule code that is not CHK001-CHK010; that code suppresses nothing";
const LATE_FILE_IGNORE: &str = "`# chokkin: file-ignore[…]` after the first statement suppresses nothing; move it above the code";
const EARLY_IGNORE: &str = "`# chokkin: ignore[…]` before the first statement is on no code line and suppresses nothing; use `file-ignore[…]` for the whole file";

/// Extract chokkin ignore directives from the comment tokens among the
/// lexer's `tokens` for `source`, whose line index the parser already built,
/// adding to `diagnostics` a warning for each
/// `# chokkin:` comment that suppresses nothing, so a user who wrote one
/// knows (ADR 0005, #1132). No other spelling is accepted: the syntax is
/// frozen as a draft (spec §18), so lower case or a space before `[` stays
/// unrecognized. Only comment tokens are read: text in a string literal is
/// not a comment (Language Reference §2.1.3, #1438), and a comment ends at
/// its physical line, so a directive never spans lines (#1439). The header
/// scan runs only in a file that names chokkin, which most files never do.
pub(crate) fn extract_ignores_with(
    source: &str,
    tokens: &Tokens,
    lines: &LineIndex,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Vec<IgnoreDirective> {
    let mut directives = Vec::new();
    if !source.contains("chokkin") {
        return directives;
    }
    let first_stmt_offset = first_statement_offset(source);
    let line_of = |offset: usize| TextSize::try_from(offset).map_or(0, |offset| lines.line(offset));
    let mut found = Vec::new();
    let mut warn = |offset: usize, message: &str| {
        found.push(ParseDiagnostic {
            line: line_of(offset),
            message: message.to_owned(),
            severity: ParseSeverity::Warning,
        });
    };
    let mut read = BTreeSet::new();
    let comment_texts = || comments(source, tokens);

    for (start, caps) in comment_texts()
        .flat_map(|(start, text)| IGNORE_RE.captures_iter(text).map(move |caps| (start, caps)))
    {
        let Some(full) = caps.get(0) else {
            continue;
        };
        let offset = start.saturating_add(full.start());
        read.insert(offset);
        let file_level = caps.get(1).is_some();
        let Some(codes_match) = caps.get(2) else {
            continue;
        };
        let codes_raw = codes_match.as_str();
        if codes_raw
            .split(',')
            .any(|code| RuleId::parse_code(code).is_none())
        {
            warn(offset, UNKNOWN_CODE);
        }
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
            if offset >= first_stmt_offset {
                warn(offset, LATE_FILE_IGNORE);
                continue;
            }
        } else if offset < first_stmt_offset {
            // Standalone comment lines before code are not inline ignores.
            warn(offset, EARLY_IGNORE);
            continue;
        }

        let line = if file_level { 0 } else { line_of(offset) };

        directives.push(IgnoreDirective {
            file_level,
            codes,
            line,
        });
    }

    for (start, text) in comment_texts() {
        for comment in DIRECTIVE_RE.find_iter(text) {
            let offset = start.saturating_add(comment.start());
            if !read.contains(&offset) {
                warn(offset, UNRECOGNIZED);
            }
        }
    }
    found.sort_by_key(|diagnostic| diagnostic.line);
    diagnostics.extend(found);

    directives
}

/// Each comment token's start offset and text.
fn comments<'s>(source: &'s str, tokens: &'s Tokens) -> impl Iterator<Item = (usize, &'s str)> {
    tokens
        .iter()
        .filter(|token| token.kind() == TokenKind::Comment)
        .filter_map(|token| {
            let start = usize::from(token.start());
            source
                .get(start..usize::from(token.end()))
                .map(|text| (start, text))
        })
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
    use super::{EARLY_IGNORE, LATE_FILE_IGNORE, UNKNOWN_CODE, UNRECOGNIZED, extract_ignores_with};
    use crate::internals::extract_ignores;
    use crate::parser::LineIndex;

    fn warnings(source: &str) -> (usize, Vec<(u32, String)>) {
        let mut diagnostics = Vec::new();
        let parsed = ruff_python_parser::parse_module(source).expect("test source parses");
        let directives = extract_ignores_with(
            source,
            parsed.tokens(),
            &LineIndex::new(source),
            &mut diagnostics,
        );
        let warnings = diagnostics
            .into_iter()
            .map(|diagnostic| (diagnostic.line, diagnostic.message))
            .collect();
        (directives.len(), warnings)
    }

    /// Issue #1132: every directive that suppresses nothing says so, and none
    /// of the malformed spellings is accepted.
    #[test]
    fn malformed_directives_warn_and_suppress_nothing() {
        for (comment, expected) in [
            ("# chokkin: ignore[CHK03]", UNKNOWN_CODE),
            ("# chokkin: ignore[CHK999]", UNKNOWN_CODE),
            ("# chokkin: ignore[chk003]", UNRECOGNIZED),
            ("# chokkin: ignore [CHK003]", UNRECOGNIZED),
            ("# chokkin: ignore[CHK003,]", UNRECOGNIZED),
            ("# chokkin: ignore", UNRECOGNIZED),
            ("# chokkin : ignore[CHK003]", UNRECOGNIZED),
            ("# chokkin: noqa", UNRECOGNIZED),
        ] {
            let source = format!("import os\nimport yaml  {comment}\n");
            let (read, warnings) = warnings(&source);
            let suppressed = extract_ignores(&source)
                .iter()
                .any(|directive| directive.codes == ["CHK003"]);
            assert!(!suppressed, "{comment}");
            assert_eq!(
                warnings,
                [(2, expected.to_owned())],
                "{comment} ({read} read)"
            );
        }
    }

    /// Issue #1438: a string literal is not a comment, so the directive
    /// text in it neither suppresses nor warns.
    #[test]
    fn directive_text_in_a_string_literal_is_not_read() {
        for source in [
            "import yaml; s = \"# chokkin: ignore[CHK003]\"\n",
            "import yaml; s = f\"{yaml}  # chokkin: ignore[CHK003]\"\n",
            "import yaml; s = br'# chokkin: ignore[CHK003]'\n",
            "import yaml; s = rf\"\\\\#chokkin:ignore[CHK003]\"\n",
            "import yaml\ns = \"\"\"\n# chokkin: ignore[CHK003]\n\"\"\"\n",
            "s = \"# chokkin: file-ignore[CHK001]\"\nimport os\n",
        ] {
            assert_eq!(warnings(source), (0, vec![]), "{source}");
        }
        assert_eq!(
            warnings("s = '# chokkin: x'  # chokkin: ignore[CHK003]\n"),
            (1, vec![])
        );
    }

    /// Issue #1439: a directive ends at its physical line, and white space
    /// other than space and tab is an unrecognized form, not a directive.
    #[test]
    fn directive_stays_on_its_line_and_takes_only_space_or_tab() {
        let spill = "import yaml\nimport requests  #\nchokkin: ignore[CHK003]\n";
        assert_eq!(warnings(spill), (0, vec![]));
        for comment in [
            "#\u{3000}chokkin: ignore[CHK003]",
            "#\u{a0}chokkin: ignore[CHK003]",
            "# chokkin:\u{a0}ignore[CHK003]",
            "# chokkin: ignore\u{3000}[CHK003]",
            "#\u{c}chokkin: ignore[CHK003]",
        ] {
            let source = format!("import os\nimport yaml  {comment}\n");
            assert_eq!(
                warnings(&source),
                (0, vec![(2, UNRECOGNIZED.to_owned())]),
                "{comment:?}"
            );
        }
    }

    #[test]
    fn well_formed_directives_do_not_warn() {
        for source in [
            "import yaml  # chokkin: ignore[CHK003]\n",
            "import yaml  #chokkin:ignore[CHK003,CHK010]\n",
            "# chokkin: file-ignore[CHK001]\nimport os\n",
            "import os\n",
        ] {
            assert_eq!(warnings(source).1, [], "{source}");
        }
    }

    proptest::proptest! {
        /// A `# chokkin:` comment either suppresses something or warns.
        #[test]
        fn chokkin_comment_is_never_silently_dropped(
            lead in "[ \t]{0,2}",
            tail in "[a-zA-Z0-9_ ,\\[\\]-]{0,24}",
            header in proptest::bool::ANY,
        ) {
            let comment = format!("#{lead}chokkin:{tail}");
            let source = if header {
                format!("{comment}\nimport os\n")
            } else {
                format!("import os\nimport yaml  {comment}\n")
            };
            let (read, warnings) = warnings(&source);
            proptest::prop_assert!(read > 0 || !warnings.is_empty(), "{}", source);
        }
    }

    #[test]
    fn misplaced_directives_warn() {
        assert_eq!(
            warnings("import os\n# chokkin: file-ignore[CHK001]\n"),
            (0, vec![(2, LATE_FILE_IGNORE.to_owned())])
        );
        assert_eq!(
            warnings("# chokkin: ignore[CHK001]\nimport os\n"),
            (0, vec![(1, EARLY_IGNORE.to_owned())])
        );
    }

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
    fn drops_codes_that_are_not_chk_and_three_digits() {
        let source = "import os  # chokkin: ignore[FOO,ABC123,CHK0001,CHK03,CHK003]\n";
        let directives = extract_ignores(source);
        assert_eq!(directives.len(), 1);
        assert_eq!(directives[0].codes, vec!["CHK003".to_owned()]);
        assert_eq!(
            extract_ignores("import os  # chokkin: ignore[ABC123]\n"),
            []
        );
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
