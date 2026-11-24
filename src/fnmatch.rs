//! Python's `fnmatch.fnmatchcase`, the pattern dialect of setuptools
//! `packages.find`, pytest `python_files` and (one component at a time)
//! `glob.glob` and the `glob` crate uv expands workspace members with.
//!
//! `*` matches any run of characters, `/` and `.` included; `?` any one
//! character; `[seq]` / `[!seq]` a class, where a `]` right after `[` or
//! `[!` is a member and `a-z` a range (a reversed range matches nothing).
//! A `[` that no `]` closes is a literal `[`. Nothing else has a meaning:
//! there is no escape character, `{a,b}` is literal, and `[^x]` is the class
//! of `^` and `x`, not a negation. This is globset's dialect only in part,
//! which is why these settings do not go through it (#1419, #1424, #1448).
//! The specification is `docs/dev/formal/lean/patterns/Patterns/Fnmatch.lean`.

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Literal(char),
    Any,
    Star,
    Class {
        negated: bool,
        items: Vec<(char, char)>,
    },
}

/// A compiled `fnmatch` pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fnmatch {
    tokens: Vec<Token>,
    unclosed_class: bool,
}

impl Fnmatch {
    pub(crate) fn new(pattern: &str) -> Self {
        let chars: Vec<char> = pattern.chars().collect();
        let mut tokens = Vec::with_capacity(chars.len());
        let mut unclosed_class = false;
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '*' => {
                    if tokens.last() != Some(&Token::Star) {
                        tokens.push(Token::Star);
                    }
                },
                '?' => tokens.push(Token::Any),
                '[' => {
                    if let Some((token, next)) = class(&chars, i + 1) {
                        tokens.push(token);
                        i = next;
                        continue;
                    }
                    unclosed_class = true;
                    tokens.push(Token::Literal('['));
                },
                c => tokens.push(Token::Literal(c)),
            }
            i += 1;
        }
        Self {
            tokens,
            unclosed_class,
        }
    }

    /// Whether a `[` was left unclosed and read as a literal. Python does
    /// that; the `glob` crate rejects the pattern instead.
    pub(crate) fn has_unclosed_class(&self) -> bool {
        self.unclosed_class
    }

    /// Whether the whole of `name` matches.
    pub(crate) fn is_match(&self, name: &str) -> bool {
        let text: Vec<char> = name.chars().collect();
        let (mut p, mut t) = (0, 0);
        // The last `*` seen and the text position it was reached at: every
        // other token takes exactly one character, so one backtrack point
        // suffices. Backtracking returns to the `*`, which records the next
        // position.
        let mut star: Option<(usize, usize)> = None;
        while t < text.len() {
            match self.tokens.get(p) {
                Some(Token::Star) => {
                    star = Some((p, t));
                    p += 1;
                },
                Some(token) if hits(token, text[t]) => {
                    p += 1;
                    t += 1;
                },
                _ => match star {
                    Some((at, from)) => {
                        p = at;
                        t = from + 1;
                    },
                    None => return false,
                },
            }
        }
        self.tokens[p..].iter().all(|token| *token == Token::Star)
    }
}

impl Fnmatch {
    /// The same pattern in `globset` syntax, for a matcher built without
    /// backslash escapes. With `component`, the glob is meant for a matcher
    /// built with `literal_separator`, where `*` and `?` stay within one path
    /// component, and a negated class is kept off `/` too: the pattern is
    /// matched against a basename. `None` for a class `globset` cannot spell
    /// (a positive class of only `^` and `!`, a range ending in `]` or `-`),
    /// or for a `\` without `component`, which `path_glob` reads as `/`.
    pub(crate) fn to_glob(&self, component: bool) -> Option<String> {
        let mut out = String::new();
        for token in &self.tokens {
            match token {
                // `path_glob` reads `\\` as a separator.
                Token::Literal('\\') if !component => return None,
                Token::Literal(c) => push_literal(&mut out, *c),
                Token::Any => out.push('?'),
                Token::Star => out.push('*'),
                Token::Class { negated, items } => {
                    push_class(&mut out, *negated, items, component)?;
                },
            }
        }
        Some(out)
    }
}

fn push_literal(out: &mut String, c: char) {
    if matches!(c, '*' | '?' | '[' | ']' | '{' | '}') {
        out.push('[');
        out.push(c);
        out.push(']');
    } else {
        out.push(c);
    }
}

/// `globset` reads `!` or `^` first as negation, `]` first as a member and
/// `-` first or last as a member: order the members so each keeps its
/// meaning.
fn push_class(
    out: &mut String,
    negated: bool,
    items: &[(char, char)],
    component: bool,
) -> Option<()> {
    let mut members: Vec<String> = Vec::new();
    let (mut close, mut dash) = (false, false);
    for (lo, hi) in items {
        if lo != hi && [lo, hi].iter().any(|c| matches!(c, ']' | '-')) {
            return None;
        }
        let member = match (lo == hi, *lo) {
            (true, ']') => {
                close = true;
                continue;
            },
            (true, '-') => {
                dash = true;
                continue;
            },
            (true, c) => c.to_string(),
            (false, _) => format!("{lo}-{hi}"),
        };
        if !members.contains(&member) {
            members.push(member);
        }
    }
    if negated && component && !members.iter().any(|member| member == "/") {
        members.push("/".to_owned());
    }
    // A positive class must not lead with `!` / `^`, which would negate it:
    // lead with another member, or with `-`.
    let mut lead_dash = false;
    if !negated && !close {
        match members
            .iter()
            .position(|member| !member.starts_with(['!', '^']))
        {
            Some(index) => {
                let lead = members.remove(index);
                members.insert(0, lead);
            },
            None if dash => lead_dash = true,
            // `[^]` is the literal `^`.
            None if members.len() == 1 && members[0].chars().count() == 1 => {
                out.push_str(&members[0]);
                return Some(());
            },
            None => return None,
        }
    }
    out.push('[');
    if negated {
        out.push('!');
    }
    if close {
        out.push(']');
    }
    if lead_dash {
        out.push('-');
    }
    out.push_str(&members.concat());
    if dash && !lead_dash {
        out.push('-');
    }
    out.push(']');
    Some(())
}

/// `fnmatch.fnmatchcase(name, pattern)`.
pub(crate) fn fnmatch(pattern: &str, name: &str) -> bool {
    Fnmatch::new(pattern).is_match(name)
}

/// The class whose body starts at `start` (just after `[`) and the index
/// after its `]`; `None` when no `]` closes it.
fn class(chars: &[char], start: usize) -> Option<(Token, usize)> {
    let mut i = start;
    let negated = chars.get(i) == Some(&'!');
    if negated {
        i += 1;
    }
    let body_start = i;
    if chars.get(i) == Some(&']') {
        i += 1;
    }
    let close = i + chars.get(i..)?.iter().position(|c| *c == ']')?;
    let body = &chars[body_start..close];
    let mut items = Vec::new();
    let mut j = 0;
    while j < body.len() {
        if j + 2 < body.len() && body[j + 1] == '-' {
            items.push((body[j], body[j + 2]));
            j += 3;
        } else {
            items.push((body[j], body[j]));
            j += 1;
        }
    }
    Some((Token::Class { negated, items }, close + 1))
}

fn hits(token: &Token, c: char) -> bool {
    match token {
        Token::Literal(d) => c == *d,
        Token::Any | Token::Star => true,
        Token::Class { negated, items } => {
            items.iter().any(|(lo, hi)| *lo <= c && c <= *hi) != *negated
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_crosses_slash_and_dot() {
        assert!(fnmatch("check_*.py", "check_x/helper.py"));
        assert!(fnmatch("acme*", "acme.sub.deep"));
        assert!(fnmatch("*", ""));
        assert!(fnmatch("a*b*c", "aXbYbZc"));
        assert!(!fnmatch("a*b*c", "aXbYbZ"));
    }

    #[test]
    fn braces_and_backslashes_are_literal() {
        assert!(!fnmatch("check_{a,b}.py", "check_a.py"));
        assert!(fnmatch("check_{a,b}.py", "check_{a,b}.py"));
        assert!(fnmatch(r"a\*", r"a\xyz"));
        assert!(!fnmatch(r"a\*", "a*x"));
    }

    #[test]
    fn classes_follow_python() {
        // `^` is a member, `!` negates.
        assert!(fnmatch("acme.[^c]*", "acme.^x"));
        assert!(fnmatch("acme.[^c]*", "acme.core"));
        assert!(!fnmatch("acme.[^c]*", "acme.internal"));
        assert!(fnmatch("acme.[!c]*", "acme.internal"));
        assert!(!fnmatch("acme.[!c]*", "acme.core"));
        // A leading `]` is a member; ranges, and reversed ones match nothing.
        assert!(fnmatch("[]a]", "]"));
        assert!(fnmatch("[!]a]", "b"));
        assert!(fnmatch("[a-c]", "b"));
        assert!(!fnmatch("[c-a]", "b"));
        assert!(fnmatch("[a-]", "-"));
        assert!(fnmatch("?", "/"));
    }

    #[test]
    fn unclosed_class_is_a_literal_bracket() {
        let pattern = Fnmatch::new("a[");
        assert!(pattern.has_unclosed_class());
        assert!(pattern.is_match("a["));
        assert!(fnmatch("check_[a.py", "check_[a.py"));
        assert!(!Fnmatch::new("a[b]").has_unclosed_class());
    }

    /// The globset spelling matches the same basenames.
    #[test]
    fn to_glob_keeps_the_meaning() {
        let cases = [
            ("check_*.py", "check_*.py"),
            ("check_{a,b}.py", "check_[{]a,b[}].py"),
            ("a[^x]", "a[x^]"),
            ("a[!x]", "a[!x/]"),
            ("a[^]", "a^"),
            ("a[]x-]", "a[]x-]"),
            ("a[!a-c]", "a[!a-c/]"),
            ("a[", "a[[]"),
            ("a[-]", "a[-]"),
            ("a[^-bx]", "a[x^-b]"),
            ("a[^-]", "a[-^]"),
            ("a[]^]", "a[]^]"),
        ];
        for (pattern, glob) in cases {
            assert_eq!(
                Fnmatch::new(pattern).to_glob(true).as_deref(),
                Some(glob),
                "{pattern}"
            );
        }
        assert_eq!(
            Fnmatch::new("a[!x]").to_glob(false).as_deref(),
            Some("a[!x]")
        );
        assert_eq!(
            Fnmatch::new("a[!^]").to_glob(false).as_deref(),
            Some("a[!^]")
        );
        assert_eq!(Fnmatch::new("[^!]").to_glob(true), None);
        assert_eq!(Fnmatch::new("x[^-a]").to_glob(true), None);
        assert_eq!(Fnmatch::new(r"a\b").to_glob(true).as_deref(), Some(r"a\b"));
        assert_eq!(Fnmatch::new(r"a/\b").to_glob(false), None);
        let names = [
            "a!",
            "a_",
            "a^",
            "ax",
            "ab",
            "a]",
            "a-",
            "a/",
            "a[",
            "ac",
            "check_{a,b}.py",
            "check_a.py",
        ];
        for (pattern, _) in cases {
            let glob =
                globset::GlobBuilder::new(&Fnmatch::new(pattern).to_glob(true).expect("glob"))
                    .literal_separator(true)
                    .backslash_escape(false)
                    .build()
                    .expect("compile")
                    .compile_matcher();
            for name in names {
                let expected = fnmatch(pattern, name) && !name.contains('/');
                assert_eq!(glob.is_match(name), expected, "{pattern} on {name}");
            }
        }
    }
}
