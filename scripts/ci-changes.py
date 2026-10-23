#!/usr/bin/env python3
"""Tell the PR jobs whether a change can affect a build or a test.

A PR that only edits prose (Markdown under docs/, or at the repository root)
skips the build and test jobs in ci.yml and release.yml: their checks report
success without running. Every other file counts as code, including:

- README.md, README.ja.md and docs/dev/spec.ja.md: tests/cli_analyze.rs
  compares their sample report with the binary's output, and the sdist ships
  README.md;
- Markdown anywhere else (tests/, src/): fixtures and inputs;
- anything under docs/ that is not Markdown (docs/schema/ is read by tests).

An empty list counts as code too, so a diff that could not be read never
skips a check.

Usage: git diff --name-only BASE HEAD | ci-changes.py   (prints code=true|false)
"""

import sys

# Prose that a test or the packaging reads, so a change to it is a code change.
READ_BY_TESTS = {"README.md", "README.ja.md", "docs/dev/spec.ja.md"}


def is_prose(path: str) -> bool:
    if path in READ_BY_TESTS or not path.endswith(".md"):
        return False
    return "/" not in path or path.startswith("docs/")


def code_changed(paths: list[str]) -> bool:
    return not paths or not all(is_prose(p) for p in paths)


def main() -> None:
    paths = [line.strip() for line in sys.stdin if line.strip()]
    print(f"code={str(code_changed(paths)).lower()}")


if __name__ == "__main__":
    main()
