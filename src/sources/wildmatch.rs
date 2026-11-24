//! `.gitignore` lines rewritten so the `ignore` crate matches what git does.
//!
//! `ignore` compiles a line into one `globset` glob, which departs from git's
//! `wildmatch` and `dir.c` in a few places. [`git_rules`] rewrites a line into
//! the rules that give git's answer (the specification is
//! `docs/dev/formal/lean/patterns/Patterns/Gitignore.lean`, checked against
//! git 2.43):
//!
//! - braces are literal: wildmatch has no alternation (#1149);
//! - a `!` with no pattern after it negates the empty pattern, which matches
//!   nothing, so the line is no rule (#1432);
//! - a run of three or more `*` between a `/` (or the start) and a `/` (or
//!   the end) is `**`, and elsewhere `*` (#1433);
//! - in a pattern holding a `/`, git compares the text before the first
//!   glob character literally and matches the rest as a pattern of its own,
//!   so `a**/b` spans directories as `**/b` does: it matches `ab` and
//!   `ax/y/b` (#1437). gitignore(5) says otherwise; git is followed;
//! - a negated class (`[!x]`, `[^x]`) never matches `/` (#1447).

use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token<'a> {
    /// `\` and the character it escapes, as written.
    Escape(&'a str),
    /// A closed class, brackets included.
    Class {
        text: &'a str,
        negated: bool,
    },
    Stars(usize),
    Char(char),
}

/// The rules `ignore` should compile for one `.gitignore` line.
pub(crate) fn git_rules(line: &str) -> Vec<Cow<'_, str>> {
    if line.is_empty() || line.starts_with('#') {
        return vec![Cow::Borrowed(line)];
    }
    let (negation, body) = match line.strip_prefix('!') {
        Some(rest) => ("!", rest),
        None => ("", line),
    };
    if !negation.is_empty() && trim_trailing_spaces(body).is_empty() {
        return Vec::new();
    }
    if !body.contains(['{', '}', '*', '[']) {
        return vec![Cow::Borrowed(line)];
    }
    rules(&tokenize(body))
        .into_iter()
        .map(|rule| Cow::Owned(format!("{negation}{rule}")))
        .collect()
}

fn rules(tokens: &[Token<'_>]) -> Vec<String> {
    let Some(split) = spanning_run(tokens) else {
        return vec![render(&normalize_stars(tokens))];
    };
    let prefix = render(&tokens[..split]);
    match &tokens[split + 1..] {
        // `P**` (`/` or not): `P` and anything below it.
        after @ ([] | [Token::Char('/')]) => {
            let dir = if after.is_empty() { "" } else { "/" };
            vec![format!("{prefix}*{dir}"), format!("{prefix}*/**{dir}")]
        },
        // `P**/R`: `PR`, read again as git would, and `P` + `*/` + any
        // directories + `R`.
        [_, remainder @ ..] => {
            let mut joined = tokens[..split].to_vec();
            joined.extend_from_slice(remainder);
            let mut out: Vec<String> = rules(&joined).into_iter().map(anchored).collect();
            out.push(format!(
                "{prefix}*/**/{}",
                render(&normalize_stars(remainder))
            ));
            out
        },
    }
}

/// `rule` kept relative to the `.gitignore`'s directory: joining `P` and
/// `R` can drop the only `/` that anchored the original pattern.
fn anchored(rule: String) -> String {
    if rule.trim_end_matches('/').contains('/') {
        rule
    } else {
        format!("/{rule}")
    }
}

/// `body` without the trailing spaces git drops (an escaped one stays).
fn trim_trailing_spaces(body: &str) -> &str {
    let mut end = body.len();
    while body[..end].ends_with(' ') {
        let before = &body[..end - 1];
        let backslashes = before.len() - before.trim_end_matches('\\').len();
        if backslashes % 2 == 1 {
            break;
        }
        end -= 1;
    }
    &body[..end]
}

fn tokenize(body: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut rest = body;
    while let Some(ch) = rest.chars().next() {
        let len = match ch {
            '\\' => {
                let len = 1 + rest[1..].chars().next().map_or(0, char::len_utf8);
                tokens.push(Token::Escape(&rest[..len]));
                len
            },
            '*' => {
                let run = rest.len() - rest.trim_start_matches('*').len();
                tokens.push(Token::Stars(run));
                run
            },
            // `ignore` allows unclosed classes: that `[` is a literal.
            '[' => {
                if let Some(close) = class_close(&rest[1..]) {
                    let text = &rest[..close + 2];
                    let negated = text[1..].starts_with(['!', '^']);
                    tokens.push(Token::Class { text, negated });
                    text.len()
                } else {
                    tokens.push(Token::Char('['));
                    1
                }
            },
            ch => {
                tokens.push(Token::Char(ch));
                ch.len_utf8()
            },
        };
        rest = &rest[len..];
    }
    tokens
}

/// Byte offset in `rest`, the text after a class's `[`, of the `]` that
/// closes it, as `globset`'s `parse_class` reads it: an optional `!` or `^`,
/// a leading `]` taken literally, no escapes. `None` when it never closes.
fn class_close(rest: &str) -> Option<usize> {
    let skipped = usize::from(rest.starts_with(['!', '^']));
    let body = &rest[skipped..];
    let first = body.strip_prefix(']').map_or(0, |_| 1);
    let close = body[first..].find(']')?;
    Some(skipped + first + close)
}

fn is_slash(token: Option<&Token<'_>>) -> bool {
    token == Some(&Token::Char('/'))
}

/// Runs of `*` as wildmatch reads them: `**` between boundaries, else `*`.
fn normalize_stars<'a>(tokens: &[Token<'a>]) -> Vec<Token<'a>> {
    tokens
        .iter()
        .enumerate()
        .map(|(index, token)| match token {
            Token::Stars(run) if *run > 1 => {
                let before = index == 0 || is_slash(tokens.get(index - 1));
                let after = index + 1 == tokens.len() || is_slash(tokens.get(index + 1));
                Token::Stars(if before && after { 2 } else { 1 })
            },
            token => token.clone(),
        })
        .collect()
}

/// The index of a run of two or more `*` that git reads as spanning
/// directories although a literal prefix not ending in `/` comes before it:
/// the first glob character of a pattern holding a `/`, followed by `/` or
/// the end.
fn spanning_run(tokens: &[Token<'_>]) -> Option<usize> {
    let end = tokens.len() - usize::from(is_slash(tokens.last()));
    if !tokens[..end].iter().any(|token| is_slash(Some(token))) {
        return None;
    }
    let first = tokens
        .iter()
        .position(|token| !matches!(token, Token::Char(ch) if *ch != '?'))?;
    let spans = matches!(tokens[first], Token::Stars(run) if run > 1)
        && first > 0
        && !is_slash(tokens.get(first - 1))
        && (first + 1 == tokens.len() || is_slash(tokens.get(first + 1)));
    spans.then_some(first)
}

fn render(tokens: &[Token<'_>]) -> String {
    let mut out = String::new();
    for token in tokens {
        match token {
            Token::Escape(text) => out.push_str(text),
            Token::Class { text, negated } => {
                if *negated && !text.contains('/') {
                    // `[!x]` → `[!x/]`; before a trailing `-`, which would
                    // otherwise start a range.
                    let body = &text[..text.len() - 1];
                    if let Some(head) = body.strip_suffix('-') {
                        out.push_str(head);
                        out.push_str("/-]");
                    } else {
                        out.push_str(body);
                        out.push_str("/]");
                    }
                } else {
                    out.push_str(text);
                }
            },
            Token::Stars(run) => out.push_str(&"*".repeat(*run)),
            Token::Char(ch @ ('{' | '}')) => {
                out.push('\\');
                out.push(*ch);
            },
            Token::Char(ch) => out.push(*ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use ignore::gitignore::{Gitignore, GitignoreBuilder};

    use super::*;

    fn rendered(line: &str) -> Vec<String> {
        git_rules(line).into_iter().map(Cow::into_owned).collect()
    }

    fn matcher(lines: &[&str]) -> Gitignore {
        let mut builder = GitignoreBuilder::new(".");
        for line in lines {
            for rule in git_rules(line) {
                builder.add_line(None, &rule).expect("rule");
            }
        }
        builder.build().expect("gitignore")
    }

    /// Whether `path` (a file) is ignored, through its parents as the walk
    /// would prune them.
    fn ignored(gitignore: &Gitignore, path: &str) -> bool {
        gitignore
            .matched_path_or_any_parents(path, false)
            .is_ignore()
    }

    #[test]
    fn braces_are_escaped_and_classes_copied() {
        for (line, expected) in [
            ("*.py", "*.py"),
            ("{}", "\\{\\}"),
            ("*.{py,pyc}", "*.\\{py,pyc\\}"),
            ("\\{a}", "\\{a\\}"),
            ("a\\\\{b}", "a\\\\\\{b\\}"),
            ("[{]x}", "[{]x\\}"),
            ("[!{}]", "[!{}/]"),
            ("[]{]}", "[]{]\\}"),
            ("[{x", "[\\{x"),
            ("[{x[}]", "[{x[}]"),
            ("[!]{]x}", "[!]{/]x\\}"),
            ("[!{]}", "[!{/]\\}"),
            ("[x{", "[x\\{"),
            ("{\\", "\\{\\"),
        ] {
            assert_eq!(rendered(line), [expected], "line {line}");
        }
    }

    /// `!` with nothing after it is no rule (#1432).
    #[test]
    fn empty_negation_is_dropped() {
        assert!(git_rules("!").is_empty());
        assert!(git_rules("!   ").is_empty());
        assert_eq!(rendered("!\\ "), ["!\\ "]);
        let gitignore = matcher(&["*.py", "!", "! "]);
        assert!(ignored(&gitignore, "b.py"));
        assert!(ignored(&gitignore, "a/b.py"));
    }

    /// Runs of `*` read as wildmatch reads them (#1433).
    #[test]
    fn star_runs_follow_wildmatch() {
        assert_eq!(rendered("***/b.py"), ["**/b.py"]);
        assert_eq!(rendered("a/***"), ["a/**"]);
        assert_eq!(rendered("x***y"), ["x*y"]);
        assert_eq!(rendered("a/*/b"), ["a/*/b"]);
        assert_eq!(rendered("*"), ["*"]);
        let gitignore = matcher(&["***/b.py"]);
        for path in ["b.py", "a/b.py", "c/d/b.py"] {
            assert!(ignored(&gitignore, path), "{path}");
        }
    }

    /// A `**` right after a literal prefix spans directories (#1437).
    #[test]
    fn prefix_then_double_star_spans_directories() {
        assert_eq!(rendered("a**/b.py"), ["/ab.py", "a*/**/b.py"]);
        assert_eq!(rendered("!x/a**/b"), ["!x/ab", "!x/a*/**/b"]);
        assert_eq!(rendered("x/a**"), ["x/a*", "x/a*/**"]);
        assert_eq!(rendered("x/a**/"), ["x/a*/", "x/a*/**/"]);
        // Not the first glob character, or no `/`: plain `*`.
        assert_eq!(rendered("a?**/b"), ["a?*/b"]);
        assert_eq!(rendered("a**b"), ["a*b"]);
        assert_eq!(rendered("a**/"), ["a*/"]);
        let gitignore = matcher(&["a**/b.py"]);
        assert!(ignored(&gitignore, "ab.py"));
        assert!(ignored(&gitignore, "ax/y/b.py"));
        assert!(ignored(&gitignore, "a/b.py"));
        assert!(!ignored(&gitignore, "ab/c.py"));
        assert!(!ignored(&gitignore, "x/ab.py"));
    }

    /// Negated classes do not match `/` (#1447).
    #[test]
    fn negated_classes_skip_slash() {
        assert_eq!(rendered("a[!x]b"), ["a[!x/]b"]);
        assert_eq!(rendered("a[^x-]b"), ["a[^x/-]b"]);
        assert_eq!(rendered("a[!-]b"), ["a[!/-]b"]);
        for line in ["a[!x]b", "a[^x]b", "a[!x]b/"] {
            let gitignore = matcher(&[line]);
            assert!(!ignored(&gitignore, "a/b/m.py"), "{line}");
            assert!(!ignored(&gitignore, "x/a/b/m.py"), "{line}");
            assert!(ignored(&gitignore, "acb/m.py"), "{line}");
        }
    }
}
