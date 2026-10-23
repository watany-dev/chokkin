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
    Ok(match decode_wide_bom(&bytes) {
        Some(decoded) => decoded,
        None => crate::parser::decode_python_source(bytes),
    })
}

/// Decode `bytes` that start with a UTF-32 or UTF-16 BOM, which is dropped.
///
/// `None` when there is no such BOM; `Some(None)` when there is one but the
/// rest is not valid in that encoding (odd length, lone surrogate). UTF-32
/// is tried first because its little-endian BOM starts with UTF-16LE's.
fn decode_wide_bom(bytes: &[u8]) -> Option<Option<String>> {
    if let Some(rest) = bytes.strip_prefix(b"\xff\xfe\0\0") {
        return Some(decode_utf32(rest, u32::from_le_bytes));
    }
    if let Some(rest) = bytes.strip_prefix(b"\0\0\xfe\xff") {
        return Some(decode_utf32(rest, u32::from_be_bytes));
    }
    if let Some(rest) = bytes.strip_prefix(b"\xff\xfe") {
        return Some(decode_utf16(rest, u16::from_le_bytes));
    }
    if let Some(rest) = bytes.strip_prefix(b"\xfe\xff") {
        return Some(decode_utf16(rest, u16::from_be_bytes));
    }
    None
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

    fn bom_encoded(text: &str, encoding: &str) -> Vec<u8> {
        let units: Vec<u16> = text.encode_utf16().collect();
        match encoding {
            "utf-16le" => [
                vec![0xFF, 0xFE],
                units.iter().flat_map(|u| u.to_le_bytes()).collect(),
            ]
            .concat(),
            "utf-16be" => [
                vec![0xFE, 0xFF],
                units.iter().flat_map(|u| u.to_be_bytes()).collect(),
            ]
            .concat(),
            "utf-32le" => [
                vec![0xFF, 0xFE, 0, 0],
                text.chars()
                    .flat_map(|c| u32::from(c).to_le_bytes())
                    .collect(),
            ]
            .concat(),
            "utf-32be" => [
                vec![0, 0, 0xFE, 0xFF],
                text.chars()
                    .flat_map(|c| u32::from(c).to_be_bytes())
                    .collect(),
            ]
            .concat(),
            _ => unreachable!("{encoding}"),
        }
    }

    #[test]
    fn wide_boms_decide_the_codec() {
        let text = "requests==2.31.0\r\n# caf\u{e9} \u{1f600}\r\n";
        for encoding in ["utf-16le", "utf-16be", "utf-32le", "utf-32be"] {
            assert_eq!(
                decode_wide_bom(&bom_encoded(text, encoding)),
                Some(Some(text.to_owned())),
                "{encoding}"
            );
        }
    }

    #[test]
    fn invalid_wide_text_is_undecodable_not_misread() {
        // Odd length, a lone surrogate, and a code point past U+10FFFF.
        assert_eq!(decode_wide_bom(b"\xff\xfer\0e"), Some(None));
        assert_eq!(decode_wide_bom(b"\xff\xfe\x00\xd8r\0"), Some(None));
        assert_eq!(decode_wide_bom(b"\0\0\xfe\xff\0\x11\0\0"), Some(None));
    }

    #[test]
    fn utf8_and_pep263_bytes_have_no_wide_bom() {
        assert_eq!(decode_wide_bom(b"requests\n"), None);
        assert_eq!(decode_wide_bom(b"\xef\xbb\xbfrequests\n"), None);
        assert_eq!(decode_wide_bom(b""), None);
    }

    #[test]
    fn read_text_reads_powershell_pip_freeze_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("requirements.txt");
        std::fs::write(&path, bom_encoded("requests\r\n", "utf-16le")).unwrap();
        assert_eq!(read_text(&path).unwrap().as_deref(), Some("requests\r\n"));
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
