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

A second answer gates the wheels release.yml cross-compiles or tests under
QEMU or a 32-bit container (i686, armv7, ppc64le, s390x). What breaks those
is the toolchain, the dependencies, or the build itself, so a PR rebuilds them
only when it touches one of CROSS_INPUTS; the push to main after the merge
builds and tests every target, as a tag does.

An empty list counts as a change to both, so a diff that could not be read
never skips a check.

Usage: git diff --name-only BASE HEAD | ci-changes.py
       (prints code=true|false and cross=true|false)
"""

import sys

# Prose that a test or the packaging reads, so a change to it is a code change.
READ_BY_TESTS = {"README.md", "README.ja.md", "docs/dev/spec.ja.md"}

# Files that decide how a wheel is built or smoke-tested on every target.
CROSS_INPUTS = {
    "Cargo.toml",
    "Cargo.lock",
    "pyproject.toml",
    ".cargo/config.toml",
    ".github/workflows/release.yml",
    "scripts/wheel-smoke.py",
}


def is_prose(path: str) -> bool:
    if path in READ_BY_TESTS or not path.endswith(".md"):
        return False
    return "/" not in path or path.startswith("docs/")


def code_changed(paths: list[str]) -> bool:
    return not paths or not all(is_prose(p) for p in paths)


def cross_changed(paths: list[str]) -> bool:
    return not paths or any(p in CROSS_INPUTS for p in paths)


def main() -> None:
    paths = [line.strip() for line in sys.stdin if line.strip()]
    print(f"code={str(code_changed(paths)).lower()}")
    print(f"cross={str(cross_changed(paths)).lower()}")


if __name__ == "__main__":
    main()
