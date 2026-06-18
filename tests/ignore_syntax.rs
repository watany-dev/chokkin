//! Regression tests for frozen ignore directive syntax (v0.3 draft).

#![allow(clippy::expect_used)]

use chokkin::extract_ignores;

#[test]
fn inline_ignore_requires_code_on_same_line() {
    let source = "import sys  # chokkin: ignore[CHK003]\n";
    let directives = extract_ignores(source);
    assert_eq!(directives.len(), 1);
    assert!(!directives[0].file_level);
    assert_eq!(directives[0].codes, vec!["CHK003".to_owned()]);
    assert_eq!(directives[0].line, 1);
}

#[test]
fn file_ignore_must_precede_first_statement() {
    let source = "# chokkin: file-ignore[CHK001,CHK006]\nimport os\n";
    let directives = extract_ignores(source);
    assert_eq!(directives.len(), 1);
    assert!(directives[0].file_level);
    assert_eq!(
        directives[0].codes,
        vec!["CHK001".to_owned(), "CHK006".to_owned()]
    );
}

#[test]
fn standalone_comment_before_code_is_not_inline_ignore() {
    let source = "# chokkin: ignore[CHK003]\nimport sys\n";
    let directives = extract_ignores(source);
    assert!(directives.is_empty());
}

#[test]
fn file_ignore_after_code_is_ignored() {
    let source = "import os\n# chokkin: file-ignore[CHK001]\n";
    let directives = extract_ignores(source);
    assert!(directives.is_empty());
}

#[test]
fn unknown_rule_codes_are_still_parsed() {
    let source = "import sys  # chokkin: ignore[CHK999]\n";
    let directives = extract_ignores(source);
    assert_eq!(directives.len(), 1);
    assert_eq!(directives[0].codes, vec!["CHK999".to_owned()]);
}

#[test]
fn comma_separated_codes_must_not_include_spaces() {
    let source = "import sys  # chokkin: ignore[CHK003, CHK010]\n";
    assert!(extract_ignores(source).is_empty());
}
