//! Requirements files commands and configs install from (#1273).
//!
//! `pip install -r path` in a command, tox `deps = -r path`, nox
//! `session.install("-r", path)` and a file in a `requirements` directory
//! below the root each install what the file lists, wherever the file is.
//! Read on every run, outside the config scan cache, so an edit to the file
//! is seen. Its names count like a requirements file outside the fixed names
//! (they provide binaries and make an import third-party, never a
//! declaration in a context). A reference that cannot be read (a URL, a
//! computed path, a missing file) is kept as unread: the gap stage lowers
//! the findings its names may have answered (ADR 0005).

use std::collections::BTreeSet;

use crate::manifest::{LoadedManifest, referenced_requirement_names};

use super::types::RequirementsRef;

/// What the referenced files declare and what could not be read.
#[derive(Debug, Default)]
pub(super) struct ReferencedRequirements {
    pub(super) names: Vec<String>,
    pub(super) unread: Vec<RequirementsRef>,
}

pub(super) fn read(manifest: &LoadedManifest, refs: &[RequirementsRef]) -> ReferencedRequirements {
    let mut found = ReferencedRequirements::default();
    let mut seen = BTreeSet::new();
    for reference in refs {
        if !seen.insert(reference.path.as_str()) {
            continue;
        }
        let names = if is_literal_path(&reference.path) {
            referenced_requirement_names(manifest, &reference.path).ok()
        } else {
            None
        };
        match names {
            Some(names) => found.names.extend(names),
            None => found.unread.push(reference.clone()),
        }
    }
    found.names.sort();
    found.names.dedup();
    found
}

/// A relative path written out in full: not empty (a computed argument),
/// not a URL, and free of shell or tox substitutions and globs.
fn is_literal_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains("://")
        && !path.contains(['$', '{', '%', '*', '?', '`'])
        && !std::path::Path::new(path).is_absolute()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_paths_exclude_urls_substitutions_and_globs() {
        for path in ["requirements.txt", ".ci/requirements-mypy.txt", "../x.txt"] {
            assert!(is_literal_path(path), "{path}");
        }
        for path in [
            "",
            "https://example.com/r.txt",
            "$REQS",
            "{env:REQS}",
            "reqs/*.txt",
            "%REQS%",
        ] {
            assert!(!is_literal_path(path), "{path}");
        }
    }
}
