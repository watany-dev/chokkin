#!/usr/bin/env python3
"""Exhaustive model of relative-import normalization vs. CPython semantics.

Faithful ports of:

- ``src/parser/relative.rs``  (``resolve_relative_import``)
- ``src/sources/layout.rs``   (``path_to_module``)

The reference model is CPython's ``importlib._bootstrap._resolve_name`` plus the
``__package__`` derivation rule (PEP 366): for ``pkg/__init__.py`` the package is
the module itself, otherwise it is the parent of the module.

Two properties are checked over a finite universe of paths / levels / layouts:

P1  (soundness of relative resolution)
    resolve_relative_import(path, layout, level, suffix, name) == Some(m)
        ==>  CPython also resolves the same import to m.
    Violations mean chokkin fabricates a module name CPython would reject.

P2  (module-name consistency)
    path_to_module(path, layout) is None  ==>  resolve_relative_import declines.
    A relative import is resolved against the current file's module name and the
    result is looked up in the ``ModuleIndex``, which is keyed by
    ``path_to_module``; deriving the current name any other way lets the parser
    fabricate a name the index can never contain (silent unreachability /
    CHK001 false positives).

Run:  python3 docs/dev/formal/relative_import_model.py
Exit status 1 when any property is violated (counterexamples are printed).
"""

from __future__ import annotations

import itertools
import sys
from dataclasses import dataclass, field


# --------------------------------------------------------------------------
# Layout description (subset of ``crate::sources::LayoutInfo``)
# --------------------------------------------------------------------------
@dataclass(frozen=True)
class Layout:
    kind: str  # "Src" | "Flat" | "Unknown"
    packages: tuple[str, ...] = field(default_factory=tuple)


# --------------------------------------------------------------------------
# Port of src/parser/relative.rs
# --------------------------------------------------------------------------
def containing_package(module: str, is_init: bool) -> str:
    if is_init:
        return module
    if "." in module:
        return module.rsplit(".", 1)[0]
    return ""


def parent_of(package: str) -> str | None:
    if "." in package:
        return package.rsplit(".", 1)[0]
    return None


def ascend_package(package: str, level: int) -> str | None:
    if level == 0:
        return package
    current = package
    for _ in range(1, level):  # Rust: for _ in 1..level
        current = parent_of(current)
        if current is None:
            return None
    return None if current == "" else current


def join_module(base: str, suffix: str) -> str:
    return suffix if base == "" else f"{base}.{suffix}"


def resolve_relative_import(
    file_path: str,
    layout: Layout,
    level: int,
    module_suffix: str | None,
    imported_name: str | None,
) -> str | None:
    if level == 0:
        return module_suffix
    current_module = path_to_module(file_path, layout)
    if current_module is None:
        return None
    is_init = file_path.endswith("__init__.py")
    pkg = containing_package(current_module, is_init)
    if pkg == "" and level > 0:
        return None
    base = ascend_package(pkg, level)
    if base is None:
        return None
    if module_suffix:
        return join_module(base, module_suffix)
    if imported_name is not None:
        return join_module(base, imported_name)
    return None


# --------------------------------------------------------------------------
# Port of src/sources/layout.rs::path_to_module
# --------------------------------------------------------------------------
def flat_module_name(path: str, layout: Layout) -> str | None:
    for package in layout.packages:
        if path == package:
            return package
        if path.startswith(package + "/"):
            return package + "." + path[len(package) + 1 :].replace("/", ".")
    return None


def path_to_module(path: str, layout: Layout) -> str | None:
    if not path.endswith(".py"):
        return None
    stem = path[: -len(".py")]
    module_path = stem[: -len("/__init__")] if stem.endswith("/__init__") else stem
    if layout.kind == "Src":
        if module_path.startswith("src/"):
            return module_path[len("src/") :].replace("/", ".")
        return None
    if layout.kind == "Flat":
        return flat_module_name(module_path, layout)
    # Unknown
    if module_path.startswith("src/"):
        return module_path[len("src/") :].replace("/", ".")
    return flat_module_name(module_path, layout)


# --------------------------------------------------------------------------
# Reference: CPython semantics
# --------------------------------------------------------------------------
class ImportErrorModel(Exception):
    pass


def cpython_resolve(package: str, level: int, module_suffix: str | None, name: str | None) -> str:
    """``from <'.'*level><module_suffix> import <name>`` inside ``__package__ == package``.

    Mirrors ``importlib._bootstrap._sanity_check`` + ``_resolve_name`` and the
    fall-back that ``from . import name`` binds ``package.name`` when ``name`` is
    a submodule (``_handle_fromlist``).
    """
    if level > 0 and not package:
        raise ImportErrorModel("attempted relative import with no known parent package")
    bits = package.rsplit(".", level - 1)
    if len(bits) < level:
        raise ImportErrorModel("attempted relative import beyond top-level package")
    base = bits[0]
    if module_suffix:
        return f"{base}.{module_suffix}"
    if name is not None:
        return f"{base}.{name}"
    raise ImportErrorModel("nothing to import")


# --------------------------------------------------------------------------
# Universe
# --------------------------------------------------------------------------
LAYOUTS = [
    Layout("Src", ("acme",)),
    Layout("Flat", ("acme",)),
    Layout("Unknown", ()),
    Layout("Unknown", ("acme",)),
]

PATHS = [
    "src/acme/__init__.py",
    "src/acme/core.py",
    "src/acme/api/__init__.py",
    "src/acme/api/routes.py",
    "src/acme/api/v1/handlers.py",
    "acme/__init__.py",
    "acme/core.py",
    "acme/api/__init__.py",
    "acme/api/routes.py",
    "tests/__init__.py",
    "tests/conftest.py",
    "tests/unit/test_core.py",
    "scripts/run.py",
    "main.py",
]

LEVELS = range(0, 5)
SUFFIXES = [None, "", "models", "models.user"]
NAMES = [None, "thing"]


def cpython_package(path: str, layout: Layout) -> str | None:
    """``__package__`` of ``path`` when the project is on sys.path (PEP 366).

    ``path_to_module`` is the module name chokkin's index assigns (src/ first,
    then flat packages), accepted here as the intended semantics.  ``None`` when
    the file is not importable under the layout (covered by P2, skipped by P1).
    """
    module = path_to_module(path, layout)
    if module is None:
        return None
    return module if path.endswith("__init__.py") else containing_package(module, False)


def check_p1() -> list[str]:
    failures = []
    for layout, path in itertools.product(LAYOUTS, PATHS):
        pkg = cpython_package(path, layout)
        if pkg is None:
            continue
        for level, suffix, name in itertools.product(LEVELS, SUFFIXES, NAMES):
            if level == 0 or (not suffix and name is None):
                continue
            got = resolve_relative_import(path, layout, level, suffix, name)
            if got is None:
                continue  # chokkin declines -> diagnostic; always sound
            mismatch = p1_mismatch(pkg, level, suffix, name, got)
            if mismatch is not None:
                failures.append(
                    f"P1 layout={layout.kind}{list(layout.packages)} file={path} "
                    f"level={level} suffix={suffix!r} name={name!r}: chokkin={got!r} "
                    f"but {mismatch}"
                )
    return failures


def p1_mismatch(pkg: str, level: int, suffix, name, got: str) -> str | None:
    """Describe how CPython disagrees with chokkin's result, or None if it agrees."""
    try:
        expected = cpython_resolve(pkg, level, suffix, name)
    except ImportErrorModel as exc:
        return f"CPython raises ImportError({exc})"
    return None if expected == got else f"CPython resolves to {expected!r}"


def check_p2() -> list[str]:
    failures = []
    for layout, path in itertools.product(LAYOUTS, PATHS):
        if path_to_module(path, layout) is not None:
            continue
        # The index has no entry for this file.  Any module name the parser
        # invents for it resolves to a key ``ModuleIndex`` can never contain.
        for level, suffix, name in itertools.product(LEVELS, SUFFIXES, NAMES):
            if level == 0 or (not suffix and name is None):
                continue
            got = resolve_relative_import(path, layout, level, suffix, name)
            if got is not None:
                failures.append(
                    f"P2 layout={layout.kind}{list(layout.packages)} file={path} "
                    f"level={level} suffix={suffix!r} name={name!r}: "
                    f"path_to_module=None but resolve_relative_import={got!r} -> "
                    f"the name can never be found in the ModuleIndex"
                )
    return failures


def classify(line: str) -> str:
    if "beyond top-level" in line:
        return "A: bare-name fabricated beyond top-level package (level == depth+1)"
    if line.startswith("P1") and "chokkin='src." in line:
        return "B: Unknown layout keeps `src.` prefix (parser name vs path_to_module)"
    if line.startswith("P2"):
        return "C: file outside index root gets a module name that ModuleIndex never contains"
    return "E: other"


def main() -> int:
    failures = check_p1() + check_p2()
    if not failures:
        print("OK: P1 and P2 hold on the finite universe")
        return 0
    groups: dict[str, list[str]] = {}
    for line in failures:
        groups.setdefault(classify(line), []).append(line)
    print(f"{len(failures)} counterexample(s) in {len(groups)} class(es):")
    for name, lines in sorted(groups.items()):
        print(f"\n[{name}] {len(lines)} case(s); first 4:")
        for line in lines[:4]:
            print("  " + line)
    return 1


if __name__ == "__main__":
    sys.exit(main())
