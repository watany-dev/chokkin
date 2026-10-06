//! Decode Python source bytes (UTF-8, or a PEP 263 single-byte coding).

/// Decode `bytes` as Python would, for the encodings chokkin supports.
///
/// Valid UTF-8 is used as is, minus a leading BOM as Python drops it.
/// Otherwise the PEP 263 declaration decides: latin-1 and cp1252 are
/// decoded, anything else (or no declaration) is `None` so the caller can
/// skip the file instead of guessing.
#[must_use]
pub(crate) fn decode_python_source(bytes: Vec<u8>) -> Option<String> {
    let bytes = match String::from_utf8(bytes) {
        Ok(text) => {
            return Some(match text.strip_prefix('\u{feff}') {
                Some(rest) => rest.to_owned(),
                None => text,
            });
        },
        Err(error) => error.into_bytes(),
    };
    match single_byte_codec(&declared_coding(&bytes)?)? {
        Codec::Latin1 => Some(bytes.iter().copied().map(char::from).collect()),
        Codec::Cp1252 => bytes.iter().copied().map(cp1252_char).collect(),
    }
}

enum Codec {
    Latin1,
    Cp1252,
}

/// The codec a declared coding `name` selects, by `CPython`'s rules: the
/// tokenizer maps `latin-1-unix` and similar spellings itself, the rest goes
/// through `encodings`, which folds every run of `-`/`_` to one `_` and looks
/// the result up as an alias (also with `.` read as `_`) or a codec module.
fn single_byte_codec(name: &str) -> Option<Codec> {
    let tokenizer_latin1 = ["latin-1", "iso-8859-1", "iso-latin-1"]
        .iter()
        .any(|spelling| {
            name.strip_prefix(spelling)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
        });
    if tokenizer_latin1 {
        return Some(Codec::Latin1);
    }
    let normalized = name
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    let alias = |key: &str| match key {
        "latin1" | "l1" | "latin" | "8859" | "cp819" | "ibm819" | "csisolatin1" | "iso8859"
        | "iso8859_1" | "iso_8859_1" | "iso_8859_1_1987" | "iso_ir_100" => Some(Codec::Latin1),
        "1252" | "windows_1252" => Some(Codec::Cp1252),
        _ => None,
    };
    alias(&normalized)
        .or_else(|| alias(&normalized.replace('.', "_")))
        .or(match normalized.as_str() {
            "latin_1" => Some(Codec::Latin1),
            "cp1252" => Some(Codec::Cp1252),
            _ => None,
        })
}

/// The PEP 263 coding name, lowercased with `_` folded to `-`.
///
/// Only the first two lines count, and the second only when the first is a
/// comment or blank (it may be a shebang).
fn declared_coding(bytes: &[u8]) -> Option<String> {
    for line in lines(bytes).take(2) {
        let line = String::from_utf8_lossy(line);
        let trimmed = line.trim_start_matches([' ', '\t', '\x0c']);
        if let Some(comment) = trimmed.strip_prefix('#') {
            if let Some(name) = coding_in_comment(comment) {
                return Some(name);
            }
        } else if !trimmed.is_empty() {
            return None;
        }
    }
    None
}

/// Lines ending at `\n`, `\r\n` or a lone `\r`: Python reads source with
/// universal newlines, so a classic-Mac file has lines too.
fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = bytes;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = rest
            .iter()
            .position(|&byte| matches!(byte, b'\n' | b'\r'))
            .unwrap_or(rest.len());
        let (line, tail) = rest.split_at(end);
        rest = tail
            .strip_prefix(b"\r\n")
            .or_else(|| tail.get(1..))
            .unwrap_or_default();
        Some(line)
    })
}

fn coding_in_comment(comment: &str) -> Option<String> {
    let mut rest = comment;
    while let Some(at) = rest.find("coding") {
        rest = &rest[at + "coding".len()..];
        let Some(value) = rest.strip_prefix([':', '=']) else {
            continue;
        };
        let name: String = value
            .trim_start_matches([' ', '\t'])
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
            .collect();
        if !name.is_empty() {
            return Some(name.to_ascii_lowercase().replace('_', "-"));
        }
    }
    None
}

/// Map one cp1252 byte; the five bytes Python leaves undefined are `None`.
fn cp1252_char(byte: u8) -> Option<char> {
    const HIGH: [Option<char>; 32] = [
        Some('\u{20AC}'),
        None,
        Some('\u{201A}'),
        Some('\u{0192}'),
        Some('\u{201E}'),
        Some('\u{2026}'),
        Some('\u{2020}'),
        Some('\u{2021}'),
        Some('\u{02C6}'),
        Some('\u{2030}'),
        Some('\u{0160}'),
        Some('\u{2039}'),
        Some('\u{0152}'),
        None,
        Some('\u{017D}'),
        None,
        None,
        Some('\u{2018}'),
        Some('\u{2019}'),
        Some('\u{201C}'),
        Some('\u{201D}'),
        Some('\u{2022}'),
        Some('\u{2013}'),
        Some('\u{2014}'),
        Some('\u{02DC}'),
        Some('\u{2122}'),
        Some('\u{0161}'),
        Some('\u{203A}'),
        Some('\u{0153}'),
        None,
        Some('\u{017E}'),
        Some('\u{0178}'),
    ];
    match byte {
        0x80..=0x9F => HIGH[usize::from(byte - 0x80)],
        _ => Some(char::from(byte)),
    }
}

#[cfg(test)]
mod tests {
    use super::decode_python_source;

    #[test]
    fn utf8_passes_through_without_a_declaration() {
        assert_eq!(
            decode_python_source("s = 'café'\n".as_bytes().to_vec()).as_deref(),
            Some("s = 'café'\n")
        );
    }

    #[test]
    fn latin1_declaration_decodes_high_bytes() {
        let source = b"# -*- coding: latin-1 -*-\ns = 'caf\xe9'\n".to_vec();
        assert_eq!(
            decode_python_source(source).as_deref(),
            Some("# -*- coding: latin-1 -*-\ns = 'café'\n")
        );
    }

    #[test]
    fn cp1252_declaration_on_second_line_after_shebang() {
        let source =
            b"#!/usr/bin/env python\n# vim: set fileencoding=Windows_1252 :\nq = '\x93x\x94'\n"
                .to_vec();
        assert_eq!(
            decode_python_source(source).as_deref(),
            Some(
                "#!/usr/bin/env python\n# vim: set fileencoding=Windows_1252 :\nq = '\u{201C}x\u{201D}'\n"
            )
        );
    }

    #[test]
    fn cp1252_undefined_byte_is_rejected() {
        assert_eq!(
            decode_python_source(b"# coding: cp1252\nq = '\x81'\n".to_vec()),
            None
        );
    }

    #[test]
    fn unsupported_or_missing_declaration_is_rejected() {
        assert_eq!(
            decode_python_source(b"# coding: iso-8859-5\nu = '\xd0'\n".to_vec()),
            None
        );
        assert_eq!(decode_python_source(b"u = '\xd0'\n".to_vec()), None);
    }

    #[test]
    fn declaration_after_code_is_ignored() {
        assert_eq!(
            decode_python_source(b"import os\n# coding: latin-1\nu = '\xe9'\n".to_vec()),
            None
        );
    }

    #[test]
    fn python_alias_spellings_are_recognised() {
        for name in [
            "latin-1-unix",
            "cp819",
            "8859",
            "iso-ir-100",
            "iso8859.1",
            "windows.1252",
            // Not the tokenizer's `latin-1` prefix; `encodings` folds it to the
            // `latin_1` codec module.
            "latin--1",
        ] {
            let source = format!("# coding: {name}\ns = '").into_bytes();
            let decoded = decode_python_source([source, b"\xe9'\n".to_vec()].concat());
            assert_eq!(
                decoded.as_deref(),
                Some(format!("# coding: {name}\ns = '\u{e9}'\n").as_str()),
                "{name}"
            );
        }
    }

    #[test]
    fn lone_cr_ends_lines() {
        assert_eq!(
            decode_python_source(b"\r# coding=l1\rs = '\xe9'\r".to_vec()).as_deref(),
            Some("\r# coding=l1\rs = '\u{e9}'\r")
        );
        assert_eq!(
            decode_python_source(b"# plain\rx = 1\r# coding: latin\rs = '\xe9'\r".to_vec()),
            None
        );
    }

    #[test]
    fn utf8_bom_is_dropped() {
        assert_eq!(
            decode_python_source(b"\xef\xbb\xbf# chokkin: file-ignore[CHK001]\n".to_vec())
                .as_deref(),
            Some("# chokkin: file-ignore[CHK001]\n")
        );
    }

    mod props {
        use proptest::prelude::*;

        use super::decode_python_source;

        /// Spellings `CPython` 3.11 accepts for latin-1 and cp1252.
        const LATIN1: &[&str] = &[
            "latin-1",
            "latin1",
            "l1",
            "latin",
            "iso-8859-1",
            "iso8859-1",
            "iso-latin-1",
            "latin-1-unix",
            "iso-8859-1-dos",
            "cp819",
            "ibm819",
            "8859",
            "csisolatin1",
            "iso-ir-100",
            "iso8859",
            "iso-8859-1-1987",
        ];
        const CP1252: &[&str] = &["cp1252", "windows-1252", "1252"];
        const CP1252_UNDEFINED: [u8; 5] = [0x81, 0x8D, 0x8F, 0x90, 0x9D];

        /// Case and `-`/`_` are irrelevant to Python's lookup.
        fn respell(names: &'static [&'static str]) -> impl Strategy<Value = String> {
            (
                prop::sample::select(names),
                prop::collection::vec(any::<bool>(), 24),
            )
                .prop_map(|(name, flips)| {
                    name.chars()
                        .zip(flips.into_iter().chain(std::iter::repeat(false)))
                        .map(|(ch, flip)| match (ch, flip) {
                            ('-', true) => '_',
                            (_, true) => ch.to_ascii_uppercase(),
                            _ => ch,
                        })
                        .collect()
                })
        }

        fn eol() -> impl Strategy<Value = &'static str> {
            prop::sample::select(vec!["\n", "\r\n", "\r"])
        }

        /// A cookie on line 1, or line 2 behind a shebang, with a body that
        /// is never valid UTF-8 (it starts with `0xFF`).
        fn declared(name: &str, shebang: bool, eol: &str, body: &[u8]) -> Vec<u8> {
            let lead = if shebang {
                format!("#!/usr/bin/env python{eol}")
            } else {
                String::new()
            };
            let mut bytes = format!("{lead}# -*- coding: {name} -*-{eol}").into_bytes();
            bytes.push(0xFF);
            bytes.extend_from_slice(body);
            bytes
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
                let _ = decode_python_source(bytes);
            }

            #[test]
            fn utf8_is_identity_minus_bom(text in ".{0,64}", bom in any::<bool>()) {
                let source = if bom { format!("\u{feff}{text}") } else { text.clone() };
                let expected = text.strip_prefix('\u{feff}').filter(|_| !bom).unwrap_or(&text);
                let decoded = decode_python_source(source.into_bytes());
                prop_assert_eq!(decoded.as_deref(), Some(expected));
            }

            #[test]
            fn latin1_matches_byte_to_char(
                name in respell(LATIN1),
                shebang in any::<bool>(),
                eol in eol(),
                body in prop::collection::vec(any::<u8>(), 0..64),
            ) {
                let bytes = declared(&name, shebang, eol, &body);
                let expected: String = bytes.iter().copied().map(char::from).collect();
                prop_assert_eq!(decode_python_source(bytes), Some(expected));
            }

            #[test]
            fn cp1252_differs_from_latin1_only_in_c1(
                name in respell(CP1252),
                shebang in any::<bool>(),
                eol in eol(),
                body in prop::collection::vec(any::<u8>(), 0..64),
            ) {
                let bytes = declared(&name, shebang, eol, &body);
                let decoded = decode_python_source(bytes.clone());
                if bytes.iter().any(|byte| CP1252_UNDEFINED.contains(byte)) {
                    prop_assert_eq!(decoded, None);
                } else {
                    let decoded = decoded.unwrap_or_default();
                    prop_assert_eq!(decoded.chars().count(), bytes.len());
                    for (byte, ch) in bytes.iter().zip(decoded.chars()) {
                        if (0x80..=0x9F).contains(byte) {
                            prop_assert!(u32::from(ch) > 0xFF, "{byte:#x} -> {ch:?}");
                        } else {
                            prop_assert_eq!(ch, char::from(*byte));
                        }
                    }
                }
            }

            /// A cookie after a code line, or on line 3 or later, is not a
            /// declaration, whatever the line endings.
            #[test]
            fn late_cookie_is_ignored(
                name in respell(LATIN1),
                first in prop::sample::select(vec!["x = 1", "# one", "", "#!/bin/py"]),
                second in prop::sample::select(vec!["x = 1", "# two", ""]),
                eol in eol(),
            ) {
                let lead = if first.starts_with('#') || first.is_empty() {
                    format!("{first}{eol}{second}{eol}")
                } else {
                    format!("{first}{eol}")
                };
                let bytes = [lead.into_bytes(), declared(&name, false, eol, b"")].concat();
                prop_assert_eq!(decode_python_source(bytes), None);
            }
        }
    }
}
