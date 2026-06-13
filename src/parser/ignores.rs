//! `# yokei: ignore[…]` directive extraction from source text.

use regex::Regex;

use super::types::IgnoreDirective;

#[allow(clippy::expect_used)]
fn ignore_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"#\s*yokei:\s*(file-)?ignore\[([A-Z][A-Z0-9_]*(?:,[A-Z][A-Z0-9_]*)*)\]")
            .expect("valid ignore regex")
    })
}

/// Extract yokei ignore directives from raw source.
#[must_use]
pub fn extract_ignores(source: &str) -> Vec<IgnoreDirective> {
    let mut directives = Vec::new();
    let first_stmt_offset = first_statement_offset(source);

    for caps in ignore_re().captures_iter(source) {
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
            u32::try_from(
                source[..full.start()]
                    .chars()
                    .filter(|ch| *ch == '\n')
                    .count(),
            )
            .unwrap_or(0)
            .saturating_add(1)
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
    code.len() == 6 && code.starts_with("YOK") && code[3..].chars().all(|ch| ch.is_ascii_digit())
}

fn first_statement_offset(source: &str) -> usize {
    let mut offset: usize = 0;
    let lines = source.lines();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            offset = offset.saturating_add(line.len()).saturating_add(1);
            continue;
        }
        break;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inline_ignore() {
        let source = "import sys  # yokei: ignore[YOK003]\n";
        let directives = extract_ignores(source);
        assert_eq!(directives.len(), 1);
        assert!(!directives[0].file_level);
        assert_eq!(directives[0].codes, vec!["YOK003".to_owned()]);
        assert_eq!(directives[0].line, 1);
    }

    #[test]
    fn parses_file_ignore_before_code() {
        let source = "# yokei: file-ignore[YOK001,YOK006]\nimport os\n";
        let directives = extract_ignores(source);
        assert_eq!(directives.len(), 1);
        assert!(directives[0].file_level);
        assert_eq!(
            directives[0].codes,
            vec!["YOK001".to_owned(), "YOK006".to_owned()]
        );
    }
}
