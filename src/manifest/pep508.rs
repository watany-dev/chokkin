//! Minimal PEP 508 requirement and PEP 440 specifier parsing.
//!
//! Only the structure chokkin reads is recovered (name, extras, version
//! specifier or URL, marker); input is still validated against the grammar so
//! malformed lines keep surfacing as warnings. Markers and URLs are kept
//! verbatim instead of being normalized. `packaging` is the reference for what
//! is accepted; `tests/pep508_packaging_golden.rs` pins that.

use std::sync::LazyLock;

use regex::Regex;

use super::pep508_util::{is_strict_pep508_name, leading_name_token, normalize_distribution_name};

/// Components of a syntactically valid PEP 508 requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Requirement {
    pub name: String,
    pub extras: Vec<String>,
    /// Version specifiers (`>=1.0, <2`) or the direct URL.
    pub version_or_url: Option<String>,
    pub marker: Option<String>,
}

/// PEP 440 comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Equal,
    EqualStar,
    NotEqual,
    NotEqualStar,
    TildeEqual,
    LessThan,
    LessThanEqual,
    GreaterThan,
    GreaterThanEqual,
    ExactEqual,
}

/// One PEP 440 version specifier such as `>=3.10`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionSpecifier {
    pub operator: Operator,
    /// Release segments (`3.10.1` -> `[3, 10, 1]`), saturating at `u64::MAX`
    /// since PEP 440 allows segments of any size.
    pub release: Vec<u64>,
    /// Operator and version as written, without inner whitespace.
    pub text: String,
}

/// Parse a PEP 508 requirement; `None` when it does not match the grammar.
pub(super) fn parse_requirement(input: &str) -> Option<Requirement> {
    let input = input.trim();
    let name = leading_name_token(input);
    if !is_strict_pep508_name(name) {
        return None;
    }
    let mut rest = input[name.len()..].trim_start();

    let mut extras = Vec::new();
    if let Some(after) = rest.strip_prefix('[') {
        let end = after.find(']')?;
        extras = parse_extras(&after[..end])?;
        rest = after[end + 1..].trim_start();
    }

    let (version_or_url, marker) = if let Some(after) = rest.strip_prefix('@') {
        let after = after.trim_start();
        let (url, tail) = after
            .split_once([' ', '\t'])
            .map_or((after, ""), |(url, _)| (url, &after[url.len()..]));
        if !has_url_scheme(url) {
            return None;
        }
        let tail = tail.trim_start();
        let marker = if tail.is_empty() {
            None
        } else {
            Some(tail.strip_prefix(';')?)
        };
        (Some(url.to_owned()), marker)
    } else {
        let (spec, marker) = match rest.split_once(';') {
            Some((spec, marker)) => (spec, Some(marker)),
            None => (rest, None),
        };
        let spec = spec.trim();
        let (spec, parenthesized) = match spec.strip_prefix('(') {
            Some(inner) => (inner.strip_suffix(')')?.trim(), true),
            None => (spec, false),
        };
        let version = if spec.is_empty() {
            if parenthesized {
                return None;
            }
            None
        } else {
            let specifiers = parse_version_specifiers(spec)?;
            Some(
                specifiers
                    .iter()
                    .map(|specifier| specifier.text.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        };
        (version, marker)
    };

    let marker = match marker {
        Some(raw) => {
            let raw = raw.trim();
            if !is_valid_marker(raw) {
                return None;
            }
            Some(raw.to_owned())
        },
        None => None,
    };

    Some(Requirement {
        name: name.to_owned(),
        extras,
        version_or_url,
        marker,
    })
}

fn parse_extras(inner: &str) -> Option<Vec<String>> {
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    inner
        .split(',')
        .map(|extra| {
            let extra = extra.trim();
            (leading_name_token(extra) == extra && is_strict_pep508_name(extra))
                .then(|| normalize_distribution_name(extra))
        })
        .collect()
}

/// pip reads a bare name with an archive extension (`foo.whl`) as a file.
pub(super) fn looks_like_archive(name: &str) -> bool {
    [
        ".whl",
        ".tbz",
        ".txz",
        ".tlz",
        ".zip",
        ".tgz",
        ".tar",
        ".tar.bz2",
        ".tar.xz",
        ".tar.lz",
        ".tar.lzma",
        ".tar.gz",
    ]
    .iter()
    .any(|extension| name.len() > extension.len() && name.ends_with(extension))
}

/// RFC 3986 scheme (`[A-Za-z][A-Za-z0-9+.-]*:`); `packaging` accepts any URL.
fn has_url_scheme(url: &str) -> bool {
    url.split_once(':').is_some_and(|(scheme, _)| {
        let mut chars = scheme.chars();
        chars.next().is_some_and(|ch| ch.is_ascii_alphabetic())
            && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '.' | '-'))
    })
}

/// Parse a comma-separated PEP 440 specifier set (`>=3.10, <4`).
pub fn parse_version_specifiers(input: &str) -> Option<Vec<VersionSpecifier>> {
    input.split(',').map(parse_version_specifier).collect()
}

fn parse_version_specifier(input: &str) -> Option<VersionSpecifier> {
    let input = input.trim();
    let (op_text, operator) = [
        ("===", Operator::ExactEqual),
        ("==", Operator::Equal),
        ("!=", Operator::NotEqual),
        ("~=", Operator::TildeEqual),
        ("<=", Operator::LessThanEqual),
        (">=", Operator::GreaterThanEqual),
        ("<", Operator::LessThan),
        (">", Operator::GreaterThan),
    ]
    .into_iter()
    .find(|(op, _)| input.starts_with(op))?;
    let version = input[op_text.len()..].trim();
    if version.contains(char::is_whitespace) {
        return None;
    }
    // `===` compares strings, so it accepts versions PEP 440 cannot parse.
    if operator == Operator::ExactEqual {
        let release = VERSION_RE
            .captures(version)
            .and_then(|captures| captures.name("release"))
            .map(|release| parse_release(release.as_str()))
            .unwrap_or_default();
        return Some(VersionSpecifier {
            operator,
            release,
            text: format!("{op_text}{version}"),
        });
    }
    let (operator, body) = match (operator, version.strip_suffix(".*")) {
        (Operator::Equal, Some(prefix)) => (Operator::EqualStar, prefix),
        (Operator::NotEqual, Some(prefix)) => (Operator::NotEqualStar, prefix),
        (_, Some(_)) => return None,
        (_, None) => (operator, version),
    };
    let captures = VERSION_RE.captures(body)?;
    let has_suffix = ["pre", "post", "dev"]
        .iter()
        .any(|group| captures.name(group).is_some());
    let has_local = captures.name("local").is_some();
    let release = parse_release(captures.name("release")?.as_str());

    let valid = match operator {
        Operator::EqualStar | Operator::NotEqualStar => !has_suffix && !has_local,
        Operator::TildeEqual => release.len() >= 2 && !has_local,
        Operator::Equal | Operator::NotEqual => true,
        _ => !has_local,
    };
    if !valid {
        return None;
    }

    let op_text = match operator {
        Operator::EqualStar => "==",
        Operator::NotEqualStar => "!=",
        _ => op_text,
    };
    Some(VersionSpecifier {
        operator,
        release,
        text: format!("{op_text}{version}"),
    })
}

/// Release segments matched by [`VERSION_RE`] (ASCII digits only), so the
/// only parse failure is overflow.
fn parse_release(release: &str) -> Vec<u64> {
    release
        .split('.')
        .map(|segment| segment.parse::<u64>().unwrap_or(u64::MAX))
        .collect()
}

/// PEP 440 version (the `packaging` reference pattern, anchored).
#[allow(clippy::expect_used)]
static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)^
        v?
        (?:[0-9]+!)?
        (?P<release>[0-9]+(?:\.[0-9]+)*)
        (?P<pre>[-_.]?(?:alpha|beta|preview|pre|rc|a|b|c)[-_.]?[0-9]*)?
        (?P<post>-[0-9]+|[-_.]?(?:post|rev|r)[-_.]?[0-9]*)?
        (?P<dev>[-_.]?dev[-_.]?[0-9]*)?
        (?P<local>\+[a-z0-9]+(?:[-_.][a-z0-9]+)*)?
        $",
    )
    .expect("valid PEP 440 regex")
});

const MARKER_VARIABLES: &[&str] = &[
    "python_version",
    "python_full_version",
    "os_name",
    "sys_platform",
    "platform_release",
    "platform_system",
    "platform_version",
    "platform_machine",
    "platform_python_implementation",
    "implementation_name",
    "implementation_version",
    "extra",
    // PEP 751 (lock files); `packaging` accepts them in any marker.
    "extras",
    "dependency_groups",
    // Legacy PEP 345 spellings still accepted by pip.
    "os.name",
    "sys.platform",
    "platform.version",
    "platform.machine",
    "platform.python_implementation",
    "python_implementation",
];

/// Nesting cap so adversarial `((((…` input cannot exhaust the stack.
const MAX_MARKER_DEPTH: usize = 64;

#[derive(Debug, PartialEq, Eq)]
enum MarkerToken<'a> {
    Open,
    Close,
    Value,
    Op,
    Word(&'a str),
}

fn is_valid_marker(input: &str) -> bool {
    let Some(tokens) = tokenize_marker(input) else {
        return false;
    };
    let mut pos = 0;
    parse_marker_or(&tokens, &mut pos, 0) && pos == tokens.len()
}

fn tokenize_marker(input: &str) -> Option<Vec<MarkerToken<'_>>> {
    let mut tokens = Vec::new();
    let mut rest = input.trim_start();
    while let Some(ch) = rest.chars().next() {
        let len = match ch {
            '(' => {
                tokens.push(MarkerToken::Open);
                1
            },
            ')' => {
                tokens.push(MarkerToken::Close);
                1
            },
            '\'' | '"' => {
                let close = rest[1..].find(ch)?;
                tokens.push(MarkerToken::Value);
                close + 2
            },
            '<' | '>' | '=' | '!' | '~' => {
                let len = rest
                    .find(|c: char| !matches!(c, '<' | '>' | '=' | '!' | '~'))
                    .unwrap_or(rest.len());
                if !matches!(
                    &rest[..len],
                    "<" | "<=" | "==" | "!=" | ">=" | ">" | "~=" | "==="
                ) {
                    return None;
                }
                tokens.push(MarkerToken::Op);
                len
            },
            _ => {
                let len = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.')))
                    .unwrap_or(rest.len());
                if len == 0 {
                    return None;
                }
                tokens.push(MarkerToken::Word(&rest[..len]));
                len
            },
        };
        rest = rest[len..].trim_start();
    }
    Some(tokens)
}

fn parse_marker_or(tokens: &[MarkerToken<'_>], pos: &mut usize, depth: usize) -> bool {
    if !parse_marker_and(tokens, pos, depth) {
        return false;
    }
    while tokens.get(*pos) == Some(&MarkerToken::Word("or")) {
        *pos += 1;
        if !parse_marker_and(tokens, pos, depth) {
            return false;
        }
    }
    true
}

fn parse_marker_and(tokens: &[MarkerToken<'_>], pos: &mut usize, depth: usize) -> bool {
    if !parse_marker_expr(tokens, pos, depth) {
        return false;
    }
    while tokens.get(*pos) == Some(&MarkerToken::Word("and")) {
        *pos += 1;
        if !parse_marker_expr(tokens, pos, depth) {
            return false;
        }
    }
    true
}

fn parse_marker_expr(tokens: &[MarkerToken<'_>], pos: &mut usize, depth: usize) -> bool {
    if tokens.get(*pos) == Some(&MarkerToken::Open) {
        if depth >= MAX_MARKER_DEPTH {
            return false;
        }
        *pos += 1;
        if !parse_marker_or(tokens, pos, depth + 1) || tokens.get(*pos) != Some(&MarkerToken::Close)
        {
            return false;
        }
        *pos += 1;
        return true;
    }
    if !parse_marker_value(tokens, pos) {
        return false;
    }
    match tokens.get(*pos) {
        Some(MarkerToken::Op | MarkerToken::Word("in")) => *pos += 1,
        Some(MarkerToken::Word("not"))
            if tokens.get(*pos + 1) == Some(&MarkerToken::Word("in")) =>
        {
            *pos += 2;
        },
        _ => return false,
    }
    parse_marker_value(tokens, pos)
}

fn parse_marker_value(tokens: &[MarkerToken<'_>], pos: &mut usize) -> bool {
    let valid = match tokens.get(*pos) {
        Some(MarkerToken::Value) => true,
        Some(MarkerToken::Word(word)) => MARKER_VARIABLES.contains(word),
        _ => false,
    };
    if valid {
        *pos += 1;
    }
    valid
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Option<Requirement> {
        parse_requirement(input)
    }

    #[test]
    fn parses_name_extras_specifier_and_marker() {
        let requirement = parse("Foo_Bar [Sec, tests] (>=1.0,<2) ; python_version < '3.11'")
            .expect("valid requirement");
        assert_eq!(requirement.name, "Foo_Bar");
        assert_eq!(requirement.extras, ["sec", "tests"]);
        assert_eq!(requirement.version_or_url.as_deref(), Some(">=1.0, <2"));
        assert_eq!(
            requirement.marker.as_deref(),
            Some("python_version < '3.11'")
        );
    }

    #[test]
    fn parses_direct_url_with_marker() {
        let requirement =
            parse("pkg @ git+https://h/r.git@v1 ; sys_platform == 'linux'").expect("valid");
        assert_eq!(
            requirement.version_or_url.as_deref(),
            Some("git+https://h/r.git@v1")
        );
        assert_eq!(
            requirement.marker.as_deref(),
            Some("sys_platform == 'linux'")
        );
    }

    #[test]
    fn url_ends_at_whitespace() {
        let requirement = parse("pkg @ https://h/x\t; os_name == 'nt'").expect("valid");
        assert_eq!(requirement.version_or_url.as_deref(), Some("https://h/x"));
        assert_eq!(requirement.marker.as_deref(), Some("os_name == 'nt'"));
        // Like `packaging`, a `;` with no whitespace before it stays in the URL.
        let requirement = parse("pkg @ https://h/x;os_name=='nt'").expect("valid");
        assert_eq!(
            requirement.version_or_url.as_deref(),
            Some("https://h/x;os_name=='nt'")
        );
        assert_eq!(requirement.marker, None);
        for input in [
            "pkg @ https://h/a b.whl",
            "pkg @ file:///tmp/my dir/x ; os_name == 'nt'",
        ] {
            assert_eq!(parse(input), None, "{input:?}");
        }
    }

    #[test]
    fn url_scheme_follows_rfc3986_case_insensitively() {
        for input in [
            "pkg @ HTTPS://h/foo.whl",
            // `packaging` ends a URL only at a space or tab.
            "pkg @ https://h/x\u{a0}y",
            "pkg @ Git+HTTPS://h/r.git",
            "pkg @ ftp://h/foo.tar.gz",
        ] {
            assert!(parse(input).is_some(), "{input:?}");
        }
        for input in ["pkg @ ./local", "pkg @ 1http://h/x", "pkg @ :x"] {
            assert_eq!(parse(input), None, "{input:?}");
        }
    }

    #[test]
    fn rejects_malformed_requirements() {
        for input in [
            "pkg()",
            "pkg (>=1.0",
            "pkg[x",
            "pkg[x y]",
            "pkg >= 1.0 extra",
            "pkg ~=1",
            "pkg >=1.0.*",
            "pkg ==1.0a1.*",
            "pkg >=1.0+local",
            "pkg ; ",
            "pkg ; python_version",
            "pkg ; unknown_var == '1'",
            "pkg ; python_version << '3'",
            "pkg ; (python_version == '3'",
            "pkg ; python_version == '3",
            "pkg[tests,]",
            "pkg[,tests]",
            "pkg ==",
        ] {
            assert_eq!(parse(input), None, "{input:?}");
        }
    }

    #[test]
    fn accepts_edge_cases_from_the_grammar() {
        for input in [
            "pkg[]",
            "pkg ===1.0",
            "legacy===2013b-custom",
            "pkg ===foobar",
            "pkg ===1.*",
            "pkg ===",
            "pkg ==1.99999999999999999999",
            "pkg ==1.0+local",
            "pkg ~=1.0.post1",
            "pkg !=2.0.*",
            "pkg ; extra not in 'a' or (os.name == 'nt' and 'x' in sys_platform)",
            "foo.tar.bz3",
            // pip reads these as files; that is applied in `pep508_util`.
            "foo.tar.gz",
            "foo.whl[tests]",
        ] {
            assert!(parse(input).is_some(), "{input:?}");
        }
    }

    #[test]
    fn invalid_extra_names_are_rejected_without_panicking() {
        // Regression: pep508_rs 0.9 panics on these.
        for input in ["pkg[a-]", "1[1-", "pkg[a-]>=1", "pkg[-a]"] {
            assert_eq!(parse(input), None, "{input:?}");
        }
    }

    #[test]
    fn marker_is_kept_verbatim_even_when_always_true() {
        // A written marker counts as conditional, so CHK002 stays conservative.
        let requirement = parse("pkg ; python_version >= '0'").expect("valid");
        assert_eq!(requirement.marker.as_deref(), Some("python_version >= '0'"));
    }

    #[test]
    fn accepts_pep751_marker_variables() {
        for input in [
            "pkg ; 'dev' in dependency_groups",
            "pkg ; 'cli' not in extras and python_version >= '3.9'",
        ] {
            assert!(parse(input).is_some(), "{input:?}");
        }
    }

    #[test]
    fn marker_nesting_is_capped() {
        let deep = format!(
            "pkg ; {}python_version == '3'{}",
            "(".repeat(MAX_MARKER_DEPTH + 1),
            ")".repeat(MAX_MARKER_DEPTH + 1)
        );
        assert_eq!(parse(&deep), None);
        let shallow = format!(
            "pkg ; {}python_version == '3'{}",
            "(".repeat(MAX_MARKER_DEPTH),
            ")".repeat(MAX_MARKER_DEPTH)
        );
        assert!(parse(&shallow).is_some());
    }

    #[test]
    fn version_specifiers_expose_operator_and_release() {
        let specifiers = parse_version_specifiers(">=3.10.1, ==3.11.*, ~=3.9").expect("valid");
        let summary: Vec<_> = specifiers
            .iter()
            .map(|spec| (spec.operator, spec.release.clone(), spec.text.clone()))
            .collect();
        assert_eq!(
            summary,
            [
                (
                    Operator::GreaterThanEqual,
                    vec![3, 10, 1],
                    ">=3.10.1".to_owned()
                ),
                (Operator::EqualStar, vec![3, 11], "==3.11.*".to_owned()),
                (Operator::TildeEqual, vec![3, 9], "~=3.9".to_owned()),
            ]
        );
        assert_eq!(parse_version_specifiers(""), None);
        assert_eq!(parse_version_specifiers(">=3.8,"), None);
    }

    #[test]
    fn oversized_release_segment_saturates() {
        let specifiers = parse_version_specifiers(">=3.99999999999999999999").expect("valid");
        assert_eq!(specifiers[0].release, [3, u64::MAX]);
        let specifiers = parse_version_specifiers("===3.11.*").expect("valid");
        assert_eq!(specifiers[0].release, Vec::<u64>::new());
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn direct_url_never_contains_space_or_tab(input in "a @ [a:/;# \t\u{3000}é]{0,16}") {
                if let Some(url) = parse_requirement(&input).and_then(|req| req.version_or_url) {
                    prop_assert!(!url.contains([' ', '\t']));
                }
            }

            #[test]
            fn parse_requirement_never_panics(input in "\\PC{0,80}") {
                let _ = parse_requirement(&input);
            }
        }
    }
}
