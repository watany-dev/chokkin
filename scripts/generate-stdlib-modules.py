#!/usr/bin/env python3
"""Generate versioned stdlib module lists for chokkin resolver.

Each list is ``sys.stdlib_module_names`` of the matching interpreter, so
version differences (3.11 ``tomllib``, 3.12 ``distutils`` removal, 3.13
PEP 594 removals) come from CPython itself. The list is a static table in
CPython, so it is identical across platforms (``msvcrt`` / ``winreg`` are
listed on Linux too).

Every ``python3.X`` in VERSIONS must be on PATH (CI installs them with
actions/setup-python).
"""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT_DIR = ROOT / "src" / "resolver" / "stdlib"

# py310.txt also serves older targets: `sys.stdlib_module_names` is 3.10+.
VERSIONS = ["3.10", "3.11", "3.12", "3.13", "3.14"]

# `__main__` is always importable but `sys.stdlib_module_names` omits it.
ALWAYS_PRESENT = ["__main__"]

DUMP = "import sys; print('\\n'.join(sorted(sys.stdlib_module_names)))"


def stdlib_module_names(version: str) -> list[str]:
    python = shutil.which(f"python{version}")
    if python is None:
        raise SystemExit(f"python{version} not found on PATH")
    out = subprocess.run(
        [python, "-c", DUMP],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return sorted({*out.split(), *ALWAYS_PRESENT})


def main() -> None:
    for version in VERSIONS:
        modules = stdlib_module_names(version)
        path = OUT_DIR / f"py{version.replace('.', '')}.txt"
        path.write_text("\n".join(modules) + "\n", encoding="utf-8")
        print(f"wrote {path} ({len(modules)} modules)")


if __name__ == "__main__":
    main()
