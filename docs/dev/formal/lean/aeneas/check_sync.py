#!/usr/bin/env python3
"""Check that extract/src copies chokkin's functions token for token.

Each entry names a function in chokkin's src/ and the copy in extract/src/.
Both bodies are cut out by brace matching, visibility is dropped, and
whitespace is collapsed; the listed stand-in substitutions are applied to
the source side first. Any other difference fails.

Usage: python3 check_sync.py   (from any directory)
"""

import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[4]

# (chokkin source, function, copy, stand-in substitutions applied to the source)
ITEMS = [
    ("src/manifest/python_range.rs", "all", "python_range.rs", []),
    ("src/manifest/python_range.rs", "from_intervals", "python_range.rs", []),
    (
        "src/manifest/python_range.rs",
        "union",
        "python_range.rs",
        [
            (
                "Self::from_intervals(self.0.iter().chain(&other.0).copied().collect())",
                "Self::from_intervals(chain_copied_collect(&self.0, &other.0))",
            )
        ],
    ),
    ("src/manifest/python_range.rs", "complement", "python_range.rs", []),
    ("src/manifest/python_range.rs", "intersect", "python_range.rs", []),
    ("src/manifest/python_range.rs", "is_subset_of", "python_range.rs", []),
    (
        "src/manifest/python_range.rs",
        "specifier_range",
        "python_range.rs",
        [
            (
                "let version = specifier.text.trim_start_matches(['<', '>', '=', '!', '~']); "
                'let digits = version.strip_suffix(".*").unwrap_or(version); '
                "if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit() || c == '.') { "
                "return None; }",
                "if !specifier.digits_only { return None; }",
            )
        ],
    ),
    ("src/manifest/python_range.rs", "exact", "python_range.rs", []),
    ("src/manifest/python_range.rs", "pad", "python_range.rs", []),
    ("src/manifest/python_range.rs", "successor", "python_range.rs", []),
    ("src/manifest/python_range.rs", "prefix_range", "python_range.rs", []),
    ("src/manifest/python_range.rs", "minor_blocks", "python_range.rs", []),
    ("src/parser/version_guard.rs", "holds", "version_guard.rs", []),
    ("src/parser/version_guard.rs", "minor_runs", "version_guard.rs", []),
    ("src/manifest/pep508_util.rs", "normalize_distribution_name", "names.rs", []),
    ("src/manifest/uv_lock.rs", "normalized", "names.rs", []),
    ("src/rules/types.rs", "target_part", "target.rs", []),
    ("src/path_util.rs", "normalize_user_path", "paths.rs", []),
    ("src/resolver/pytest_path.rs", "normalize_dir", "paths.rs", []),
    ("src/manifest/wheel.rs", "normalize_path", "paths.rs", []),
    ("src/config/workspace.rs", "normalize_relative_path", "paths.rs", []),
]


def function_text(source: str, name: str) -> str:
    match = re.search(
        rf"^\s*(?:pub(?:\([a-z]+\))?\s+)?fn {re.escape(name)}\b", source, re.MULTILINE
    )
    if match is None:
        raise LookupError(name)
    start = source.index("fn ", match.start())
    depth = 0
    for i in range(source.index("{", start), len(source)):
        if source[i] == "{":
            depth += 1
        elif source[i] == "}":
            depth -= 1
            if depth == 0:
                return source[start : i + 1]
    raise LookupError(name)


def squash(text: str) -> str:
    text = re.sub(r"//[^\n]*", "", text)
    return " ".join(text.split())


def main() -> int:
    failures = 0
    for src, name, copy, substitutions in ITEMS:
        try:
            original = squash(function_text((REPO / src).read_text(), name))
            copied = squash(
                function_text((HERE / "extract/src" / copy).read_text(), name)
            )
        except LookupError:
            print(f"MISSING {src}::{name}")
            failures += 1
            continue
        for old, new in substitutions:
            if squash(old) not in original:
                print(f"STALE   {src}::{name}: stand-in no longer applies")
                failures += 1
            original = original.replace(squash(old), squash(new))
        if original != copied:
            print(f"DIFF    {src}::{name}")
            failures += 1
        else:
            print(f"ok      {src}::{name}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
