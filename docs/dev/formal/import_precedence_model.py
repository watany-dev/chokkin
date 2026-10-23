#!/usr/bin/env python3
"""Finite model of absolute-import precedence vs. CPython's ``sys.path`` order.

Port of the order ``src/resolver/resolve.rs`` (``resolve_import_root``) tries
for an absolute ``import X``: a declared backport (#939), then the stdlib of
the minors in the window (§7 step 1), then first-party modules. The reference
is CPython 3: ``sys.meta_path`` asks ``BuiltinImporter`` and
``FrozenImporter`` first, modules imported at startup are already in
``sys.modules``, and only then ``PathFinder`` walks ``sys.path``, whose first
entry is the script's directory (``python main.py``) or the working
directory (``python -m``), ahead of the stdlib directories and site-packages.

The universe: a module name of each kind (builtin, imported at startup,
plain stdlib, stdlib only in older minors, not stdlib) × where the project
holds a module of that name (nowhere, at the root, under ``src/``) × flat or
src layout × how the code runs (a root script, ``-m`` from the root, an
installed console script) × the running minor.

P1  chokkin's origin for the import equals CPython's, outside the divergences
    listed in ``ACCEPTED``. A new divergence is a counterexample.

Accepted divergences, each a decision rather than a bug:

D1  A flat-layout root module that shadows a plain stdlib module
    (``json.py`` next to ``main.py``) is what ``python main.py`` imports,
    while chokkin resolves the stdlib and reports the file as unreachable.
    Whether the code runs from the root or installed is not static, and
    shadowing the stdlib is almost always a mistake; the CPython-checked
    reachability tests (#1112) are where this gets measured.

Run:  python3 docs/dev/formal/import_precedence_model.py
Exit status 1 when a property is violated (counterexamples are printed).
"""

from __future__ import annotations

import itertools
import sys

MINORS = (12, 13)

# name -> (kind, minors whose stdlib ships it)
NAMES = {
    "sys": ("builtin", set(range(8, 15))),
    "os": ("startup", set(range(8, 15))),
    "json": ("stdlib", set(range(8, 15))),
    "cgi": ("stdlib", set(range(8, 13))),
    "acme_util": ("none", set()),
}
PLACES = ("nowhere", "root", "src")
LAYOUTS = ("flat", "src")
RUNS = ("script", "module", "installed")


def project_module(place: str, layout: str) -> bool:
    """Whether chokkin indexes the project's module as first-party."""
    if place == "root":
        return layout == "flat"
    if place == "src":
        return layout == "src"
    return False


def chokkin(name: str, place: str, layout: str, minor: int) -> str:
    """Port of resolve_import_root for an undeclared name, window = {minor}."""
    _, ships = NAMES[name]
    if minor in ships:
        return "stdlib"
    if project_module(place, layout):
        return "first-party"
    return "unresolved"


def cpython(name: str, place: str, layout: str, run: str, minor: int) -> str:
    kind, ships = NAMES[name]
    if kind in ("builtin", "startup") and minor in ships:
        return "stdlib"
    path = []
    if run in ("script", "module"):
        # sys.path[0]: the root, holding main.py or the cwd of `python -m`.
        path.append("root")
    path.append("stdlib")
    # site-packages: the installed project, flat or (editable) src.
    if project_module(place, layout):
        path.append("site")
    for entry in path:
        if entry == "root" and place == "root":
            return "first-party"
        if entry == "stdlib" and minor in ships:
            return "stdlib"
        if entry == "site":
            return "first-party"
    return "unresolved"


def accepted(name: str, place: str, layout: str, run: str) -> str | None:
    kind, _ = NAMES[name]
    if kind == "stdlib" and place == "root" and layout == "flat" and run != "installed":
        return "D1"
    return None


def main() -> int:
    failures = []
    seen = set()
    for name, place, layout, run, minor in itertools.product(
        NAMES, PLACES, LAYOUTS, RUNS, MINORS
    ):
        if place == "root" and layout == "src":
            # Discovery reads only `src/` of a src layout, so chokkin sees
            # no import of the root module and reports no file for it.
            continue
        ours = chokkin(name, place, layout, minor)
        theirs = cpython(name, place, layout, run, minor)
        if ours == theirs:
            continue
        reason = accepted(name, place, layout, run)
        if reason:
            seen.add(reason)
            continue
        failures.append((name, place, layout, run, minor, ours, theirs))
    for failure in failures:
        name, place, layout, run, minor, ours, theirs = failure
        print(
            f"P1 counterexample: name={name} place={place} layout={layout} run={run} "
            f"3.{minor}: chokkin {ours}, CPython {theirs}"
        )
    # An accepted divergence that never occurs is a stale entry.
    if seen != {"D1"}:
        print(f"stale accepted divergences: {sorted({'D1'} - seen)}")
        return 1
    if failures:
        return 1
    print("P1 holds outside the accepted divergences (D1)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
