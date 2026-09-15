#!/usr/bin/env python3
"""Exhaustive model of relative-import normalization vs. CPython semantics.

Faithful ports of:

- ``src/parser/relative.rs``            (``file_module_name``, ``resolve_relative_import``)
- ``src/reachability/module_index.rs``  (``path_to_module``)

The reference model is CPython's ``importlib._bootstrap._resolve_name`` plus the
``__package__`` derivation rule (PEP 366): for ``pkg/__init__.py`` the package is
the module itself, otherwise it is the parent of the module.

Two properties are checked over a finite universe of paths / levels / layouts:

P1  (soundness of relative resolution)
    resolve_relative_import(path, layout, level, suffix, name) == Some(m)
        ==>  CPython also resolves the same import to m.
    Violations mean chokkin fabricates a module name CPython would reject.

P2  (module-name consistency)
    file_module_name(path, layout) == path_to_module(path, layout)
    whenever path_to_module is Some.  Every relative import is resolved through
    ``file_module_name`` and then looked up in the ``ModuleIndex`` which is keyed
    by ``path_to_module``; a divergence means the relative import can never hit
    the index (silent unreachability / CHK001 false positives).

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
def file_module_name(path: str, layout: Layout) -> str | None:
    if not path.endswith(".py"):
        return None
    path = path[: -len(".py")]
    if path == "":
        return None
    parts = [p for p in path.split("/") if p]
    if not parts:
        return None
    if layout.kind == "Src":
        module_parts = parts[1:] if (parts[0] == "src" and len(parts) > 1) else parts
    else:  # Flat | Unknown
        module_parts = parts
    name_parts = list(module_parts)
    if name_parts and name_parts[-1] == "__init__":
        name_parts.pop()
    if not name_parts:
        return None
    return ".".join(name_parts)


def containing_package(module: str, is_init: bool) -> str:
    if is_init:
        return module
    if "." in module:
        return module.rsplit(".", 1)[0]
    return ""


def parent_of(package: str) -> str | None:
    if package == "":
        return None
    if "." in package:
        return package.rsplit(".", 1)[0]
    return ""


def ascend_package(package: str, level: int) -> str | None:
    if level == 0:
        return package
    current = package
    for _ in range(1, level):  # Rust: for _ in 1..level
        if current == "":
            return None
        current = parent_of(current)
        if current is None:
            return None
    return current


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
    current_module = file_module_name(file_path, layout)
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
# Port of src/reachability/module_index.rs::path_to_module
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


def cpython_module_name(path: str, layout: Layout) -> str | None:
    """Module name a file would get when the project is installed / on sys.path.

    Src layout: ``src`` is the sys.path root.  Flat: the project root is.  Unknown:
    chokkin itself picks ``src/`` first and otherwise falls back to flat packages,
    which we accept as the intended semantics (``path_to_module``).
    """
    return path_to_module(path, layout)


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


def check_p1() -> list[str]:
    failures = []
    for layout, path, level, suffix, name in itertools.product(
        LAYOUTS, PATHS, LEVELS, SUFFIXES, NAMES
    ):
        if level == 0:
            continue
        if not suffix and name is None:
            continue
        got = resolve_relative_import(path, layout, level, suffix, name)
        if got is None:
            continue  # chokkin declines -> diagnostic; always sound
        module = cpython_module_name(path, layout)
        if module is None:
            # File is not importable under this layout; chokkin should not
            # invent a module (covered by P2), skip here.
            continue
        pkg = module if path.endswith("__init__.py") else containing_package(module, False)
        try:
            expected = cpython_resolve(pkg, level, suffix, name)
        except ImportErrorModel as exc:
            failures.append(
                f"P1 layout={layout.kind}{list(layout.packages)} file={path} "
                f"level={level} suffix={suffix!r} name={name!r}: chokkin={got!r} "
                f"but CPython raises ImportError({exc})"
            )
            continue
        if expected != got:
            failures.append(
                f"P1 layout={layout.kind} file={path} level={level} suffix={suffix!r} "
                f"name={name!r}: chokkin={got!r} cpython={expected!r}"
            )
    return failures


def check_p2() -> list[str]:
    failures = []
    for layout, path in itertools.product(LAYOUTS, PATHS):
        fm = file_module_name(path, layout)
        pm = path_to_module(path, layout)
        if pm is None:
            # Index has no entry.  Relative imports from this file resolve to a
            # name that can never be found; only a problem when chokkin still
            # produces a module name for it.
            if fm is not None and layout.kind in ("Src", "Unknown"):
                failures.append(
                    f"P2 layout={layout.kind}{list(layout.packages)} file={path}: "
                    f"file_module_name={fm!r} but ModuleIndex has no key for this "
                    f"file (path_to_module=None) -> relative imports from it can "
                    f"never resolve"
                )
            continue
        if fm != pm:
            failures.append(
                f"P2 layout={layout.kind}{list(layout.packages)} file={path}: "
                f"file_module_name={fm!r} != path_to_module={pm!r}"
            )
    return failures


def classify(line: str) -> str:
    if "beyond top-level" in line:
        return "A: bare-name fabricated beyond top-level package (level == depth+1)"
    if line.startswith("P1") and "chokkin='src." in line:
        return "B: Unknown layout keeps `src.` prefix (file_module_name vs path_to_module)"
    if line.startswith("P2") and "path_to_module=None" in line:
        return "C: file outside index root gets a module name that ModuleIndex never contains"
    if line.startswith("P2"):
        return "D: file_module_name != path_to_module"
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
