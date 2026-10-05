//! Decode Python source bytes (UTF-8, or a PEP 263 single-byte coding).

/// Decode `bytes` as Python would, for the encodings chokkin supports.
///
/// Valid UTF-8 is used as is. Otherwise the PEP 263 declaration decides:
/// latin-1 and cp1252 are decoded, anything else (or no declaration) is
/// `None` so the caller can skip the file instead of guessing.
#[must_use]
pub fn decode_python_source(bytes: Vec<u8>) -> Option<String> {
    let bytes = match String::from_utf8(bytes) {
        Ok(text) => return Some(text),
        Err(error) => error.into_bytes(),
    };
    match declared_coding(&bytes)?.as_str() {
        "latin-1" | "latin1" | "l1" | "iso-8859-1" | "iso8859-1" | "iso-latin-1" => {
            Some(bytes.iter().copied().map(char::from).collect())
        },
        "cp1252" | "windows-1252" => bytes.iter().copied().map(cp1252_char).collect(),
        _ => None,
    }
}

/// The PEP 263 coding name, lowercased with `_` folded to `-`.
///
/// Only the first two lines count, and the second only when the first is a
/// comment or blank (it may be a shebang).
fn declared_coding(bytes: &[u8]) -> Option<String> {
    for line in bytes.split(|&byte| byte == b'\n').take(2) {
        let line = String::from_utf8_lossy(line);
        let trimmed = line.trim_start_matches([' ', '\t', '\x0c']);
        if let Some(comment) = trimmed.strip_prefix('#') {
            if let Some(name) = coding_in_comment(comment) {
                return Some(name);
            }
        } else if !trimmed.trim_end().is_empty() {
            return None;
        }
    }
    None
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
}
