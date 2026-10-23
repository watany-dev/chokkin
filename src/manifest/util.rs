//! Shared helpers for manifest extraction modules.

use std::path::Path;

use super::error::ManifestError;
use super::pep508_util::parse_pep508_requirement;
use super::types::{DeclaredDependency, DependencyContext, DependencyOrigin};
use super::warnings::ManifestWarning;

/// Returns `true` when `path` resolves under `root`.
#[must_use]
pub(crate) fn path_is_within_root(root: &Path, path: &Path) -> bool {
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let canonical_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical_path.starts_with(&canonical_root)
}

/// Read a manifest file as UTF-8 text.
pub(crate) fn read_to_string(path: &Path) -> Result<String, ManifestError> {
    std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Read a manifest file as pip's `auto_decode` does: a UTF-16 or UTF-32 BOM
/// decides the codec, otherwise UTF-8 or the PEP 263 coding it declares.
///
/// Windows PowerShell 5.1 writes `pip freeze > requirements.txt` as UTF-16LE
/// with a BOM (#1153). `None` when the bytes cannot be decoded; the caller
/// skips the file.
pub(crate) fn read_text(path: &Path) -> Result<Option<String>, ManifestError> {
    let bytes = std::fs::read(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(match wide_bom(&bytes) {
        Some((rest, decode)) => decode(rest),
        None => crate::parser::decode_python_source(bytes),
    })
}

/// Decodes the text after a BOM; `None` when it is not valid in that
/// encoding (odd length, lone surrogate).
type WideDecoder = fn(&[u8]) -> Option<String>;

/// The bytes after a leading UTF-32 or UTF-16 BOM, and their decoder. UTF-32
/// is tried first because its little-endian BOM starts with UTF-16LE's.
fn wide_bom(bytes: &[u8]) -> Option<(&[u8], WideDecoder)> {
    let boms: [(&[u8], WideDecoder); 4] = [
        (b"\xff\xfe\0\0", |rest| {
            decode_utf32(rest, u32::from_le_bytes)
        }),
        (b"\0\0\xfe\xff", |rest| {
            decode_utf32(rest, u32::from_be_bytes)
        }),
        (b"\xff\xfe", |rest| decode_utf16(rest, u16::from_le_bytes)),
        (b"\xfe\xff", |rest| decode_utf16(rest, u16::from_be_bytes)),
    ];
    boms.into_iter()
        .find_map(|(bom, decode)| Some((bytes.strip_prefix(bom)?, decode)))
}

fn decode_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> Option<String> {
    let chunks = bytes.chunks_exact(2);
    if !chunks.remainder().is_empty() {
        return None;
    }
    let units = chunks.map(|pair| unit([pair[0], pair[1]]));
    char::decode_utf16(units).collect::<Result<_, _>>().ok()
}

fn decode_utf32(bytes: &[u8], unit: fn([u8; 4]) -> u32) -> Option<String> {
    let chunks = bytes.chunks_exact(4);
    if !chunks.remainder().is_empty() {
        return None;
    }
    chunks
        .map(|quad| char::from_u32(unit([quad[0], quad[1], quad[2], quad[3]])))
        .collect()
}

/// Context for pushing a parsed dependency declaration.
pub(crate) struct DependencyPush<'a> {
    /// Target dependency list.
    pub dependencies: &'a mut Vec<DeclaredDependency>,
    /// Warning sink.
    pub warnings: &'a mut Vec<ManifestWarning>,
    /// Raw PEP 508 or requirements line.
    pub raw: &'a str,
    /// Declaration context.
    pub context: DependencyContext,
    /// Root-relative manifest file path.
    pub file: &'a str,
    /// TOML key path or requirements label.
    pub label: String,
    /// 1-based line number when available.
    pub line: Option<u32>,
}

/// Parse `raw` and append either a dependency or a non-fatal warning.
pub(crate) fn push_dependency(push: DependencyPush<'_>) {
    let origin = DependencyOrigin {
        file: push.file.to_owned(),
        line: push.line,
        label: push.label,
    };
    match parse_pep508_requirement(push.raw, push.context, origin) {
        Ok(dep) => push.dependencies.push(dep),
        Err(warning) => push.warnings.push(warning),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(bom: [u8; 2], text: &str, unit: fn(u16) -> [u8; 2]) -> Vec<u8> {
        bom.into_iter()
            .chain(text.encode_utf16().flat_map(unit))
            .collect()
    }

    fn utf32(bom: [u8; 4], text: &str, unit: fn(u32) -> [u8; 4]) -> Vec<u8> {
        bom.into_iter()
            .chain(text.chars().flat_map(|ch| unit(u32::from(ch))))
            .collect()
    }

    /// The decoded text, `Ok(None)` when it is invalid, `Err` without a BOM.
    fn decode(bytes: &[u8]) -> Result<Option<String>, &'static str> {
        wide_bom(bytes)
            .map(|(rest, decode)| decode(rest))
            .ok_or("no BOM")
    }

    #[test]
    fn wide_boms_decide_the_codec() {
        let text = "requests==2.31.0\r\n# caf\u{e9} \u{1f600}\r\n";
        for (label, bytes) in [
            ("utf-16le", utf16([0xFF, 0xFE], text, u16::to_le_bytes)),
            ("utf-16be", utf16([0xFE, 0xFF], text, u16::to_be_bytes)),
            (
                "utf-32le",
                utf32([0xFF, 0xFE, 0, 0], text, u32::to_le_bytes),
            ),
            (
                "utf-32be",
                utf32([0, 0, 0xFE, 0xFF], text, u32::to_be_bytes),
            ),
        ] {
            assert_eq!(decode(&bytes), Ok(Some(text.to_owned())), "{label}");
        }
    }

    #[test]
    fn invalid_wide_text_is_undecodable_not_misread() {
        // Odd length, a lone surrogate, and a code point past U+10FFFF.
        assert_eq!(decode(b"\xff\xfer\0e"), Ok(None));
        assert_eq!(decode(b"\xff\xfe\x00\xd8r\0"), Ok(None));
        assert_eq!(decode(b"\0\0\xfe\xff\0\x11\0\0"), Ok(None));
    }

    #[test]
    fn utf8_and_pep263_bytes_have_no_wide_bom() {
        assert_eq!(decode(b"requests\n"), Err("no BOM"));
        assert_eq!(decode(b"\xef\xbb\xbfrequests\n"), Err("no BOM"));
        assert_eq!(decode(b""), Err("no BOM"));
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn push_dependency_appends_exactly_one_item(raw in "\\PC{0,120}") {
                let mut dependencies = Vec::new();
                let mut warnings = Vec::new();
                push_dependency(DependencyPush {
                    dependencies: &mut dependencies,
                    warnings: &mut warnings,
                    raw: &raw,
                    context: DependencyContext::Runtime,
                    file: "requirements.txt",
                    label: "requirements.txt".to_string(),
                    line: Some(1),
                });

                prop_assert_eq!(dependencies.len() + warnings.len(), 1);
            }
        }
    }
}
