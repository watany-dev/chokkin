#!/usr/bin/env python3
"""Finite model of pytest ``testpaths`` glob expansion (#998, v0.7.10).

Port of ``src/sources/layout.rs`` (``expand_testpath``, ``matching_children``,
``dirs_from``, ``testpath_entries``, ``unmatched_testpaths``, with
``in_root_dir`` normalizing the entry first) and of the one-component
``globset`` 0.4 matcher it calls (``GLOBSET`` below; the port was compared
with globset 0.4.20 on every component and name of this universe). The
reference is what pytest does with ``testpaths`` when it runs from the
rootdir (``Config._decide_args``): ``sorted(glob.iglob(entry,
recursive=True))``, with the stdlib ``glob`` run on a real directory tree.
Nothing of an analyzed project runs.

The universe: one directory tree whose names exercise every class syntax
(``a``, ``b``, ``ab``, ``.h``, ``t[1``, ``t[1]``, ``^``, ``]``, ``{a,b}``,
``a.py``),
three levels deep, with a symlink to a directory, and every entry of 1 to 3
components drawn from ``COMPONENTS``.

The observable is what an entry makes test code: the files of the tree at
or below a path the expansion yields (chokkin gives a file test context by
its path's prefix, pytest collects below each argument).

T1  The files chokkin's expansion of an entry covers equal the files below
    the paths pytest's glob yields, outside ``ACCEPTED``.
T2  chokkin warns that an entry names nothing exactly when pytest's glob
    yields nothing for it, outside ``ACCEPTED``.

Accepted divergences, each a decision rather than a bug:

D1  ``**`` does not descend through a symlink in chokkin, so a link loop
    cannot hang the walk (``dirs_from``); Python's glob follows it.
D2  An entry the glob matches nothing for stays as written, so a directory
    literally named ``t[1]`` is test code and no warning is raised
    (``testpath_entries``); pytest's glob reads ``[1]`` as a class and
    collects nothing from that entry.

Open divergences (README "v0.7.10 の台帳"):

Q3  A component globset rejects but Python reads: an unclosed ``[`` (a
    directory named ``t[1``) or a reversed range ``[z-a]`` (Python: matches
    nothing). chokkin matches nothing for that component, so the entry falls
    back to D2. (``a**`` is ``a*`` in both.)
Q4  ``[^a]``: globset negates the class, Python's ``fnmatch`` reads ``^`` as
    a literal character, as ``[\\^a]``.
Q5  ``**`` after a component that matched a file (``tests/*/**`` with
    ``tests/test_x.py``) keeps the file: ``dirs_from`` pushes its base without
    asking whether it is a directory, while Python's ``_glob2`` yields the
    base only for a directory. The file becomes test code through an entry
    pytest collects nothing from.
Q6  A last ``**`` component yields only directories; Python's yields the
    files too. Below a directory it makes no difference (the directory
    covers them), but ``testpaths = **`` covers no file at the root.

Run:  python3 docs/dev/formal/testpaths_glob_model.py
      python3 docs/dev/formal/testpaths_glob_model.py --broken
``--broken`` swaps in an expansion that lets a wildcard match names starting
with ``.`` and exits 0 only when T1 catches it outside the accepted
divergences.
Exit status 1 when a property is violated (counterexamples are printed).
"""

from __future__ import annotations

import fnmatch
import glob
import itertools
import os
import re
import sys
import tempfile

COMPONENTS = (
    "a",
    "*",
    "?",
    "**",
    "a*",
    "[ab]",
    "[!a]",
    ".*",
    "*.py",
    "{a,b}",
    "[]]",
    "t[1",
    "t[1]",
    "a**",
    "[z-a]",
    "[^a]",
)

NAMES = ("a", "b", "ab", ".h", "t[1", "t[1]", "^", "]", "{a,b}")

BROKEN = False


# ─── globset 0.4 one-component matcher ───────────────────────────────────────


class GlobError(Exception):
    pass


def globset_regex(pattern: str) -> str:
    """The regex globset compiles a one-component pattern to, or GlobError
    where `Glob::new` fails (UnclosedClass, InvalidRange). Braces never reach
    it: ``matching_children`` escapes them as ``[{]`` / ``[}]`` first."""
    out = []
    i, n = 0, len(pattern)
    while i < n:
        c = pattern[i]
        if c == "*":
            # `**` beside other text in a component is two `*` (`a**` is
            # `a*`); alone it matches every name.
            while i < n and pattern[i] == "*":
                i += 1
            out.append(".*")
        elif c == "?":
            out.append(".")
            i += 1
        elif c == "[":
            i += 1
            negated = i < n and pattern[i] in "!^"
            if negated:
                i += 1
            items = []
            first = True
            while True:
                if i >= n:
                    raise GlobError("UnclosedClass")
                ch = pattern[i]
                if ch == "]" and not first:
                    i += 1
                    break
                first = False
                if i + 2 < n and pattern[i + 1] == "-" and pattern[i + 2] != "]":
                    lo, hi = ch, pattern[i + 2]
                    if lo > hi:
                        raise GlobError("InvalidRange")
                    items.append(f"{re.escape(lo)}-{re.escape(hi)}")
                    i += 3
                else:
                    items.append(re.escape(ch))
                    i += 1
            out.append(f"[{'^' if negated else ''}{''.join(items)}]")
        else:
            out.append(re.escape(c))
            i += 1
    return "".join(out)


def globset_match(pattern: str, name: str) -> bool:
    return re.fullmatch(globset_regex(pattern), name, re.DOTALL) is not None


# ─── Port of layout.rs ───────────────────────────────────────────────────────


def in_root_dir(path: str) -> str | None:
    if os.path.isabs(path):
        return None
    parts = []
    for part in re.split(r"[/\\]", path):
        if part in ("", "."):
            continue
        if part == "..":
            return None
        parts.append(part)
    return "/".join(parts) if parts else None


def join_rel(base: str, rel: str) -> str:
    return f"{base}/{rel}" if base else rel


# Single changes to the port that each settle one open or accepted divergence;
# `accepted` names the divergence by the change that removes it.
FIXES: set[str] = set()


def matching_children(root: str, base: str, component: str) -> list[str]:
    literal_braces = component.replace("{", "[{]").replace("}", "[}]")
    if "fnmatch" in FIXES and "[" in component:
        matches = lambda name: fnmatch.fnmatchcase(name, component)
    else:
        try:
            globset_regex(literal_braces)
        except GlobError:
            return []
        matches = lambda name: globset_match(literal_braces, name)
    dot = component.startswith(".") or BROKEN
    try:
        names = os.listdir(os.path.join(root, base))
    except OSError:
        return []
    return [
        join_rel(base, name)
        for name in names
        if (dot or not name.startswith(".")) and matches(name)
    ]


def dirs_from(root: str, base: str, out: list[str], last: bool = False) -> None:
    if "isdir" in FIXES and base and not os.path.isdir(os.path.join(root, base)):
        return
    out.append(base)
    try:
        entries = list(os.scandir(os.path.join(root, base)))
    except OSError:
        return
    for entry in entries:
        if entry.name.startswith("."):
            continue
        if entry.is_dir(follow_symlinks="links" in FIXES):
            dirs_from(root, join_rel(base, entry.name), out, last)
        elif last and "files" in FIXES:
            out.append(join_rel(base, entry.name))


def expand_testpath(root: str, pattern: str) -> list[str]:
    found = [""]
    components = pattern.split("/")
    for at, component in enumerate(components):
        nxt: list[str] = []
        for base in found:
            if component == "**":
                dirs_from(root, base, nxt, last=at == len(components) - 1)
            else:
                nxt.extend(matching_children(root, base, component))
        found = sorted(set(nxt))
    return [path for path in found if path]


def chokkin_entry(root: str, raw: str) -> tuple[list[str], bool]:
    """(paths the entry stands for, whether it is an unmatched-entry warning)."""
    entry = in_root_dir(raw)
    if entry is None:
        return [], False
    matched = expand_testpath(root, entry)
    if matched or "nolit" in FIXES:
        return matched, not matched
    return [entry], not os.path.lexists(os.path.join(root, entry))


# ─── Reference: pytest + stdlib glob ─────────────────────────────────────────


def pytest_entry(root: str, raw: str) -> list[str]:
    return sorted(p.rstrip("/") for p in glob.glob(raw, root_dir=root, recursive=True))


# ─── Check ───────────────────────────────────────────────────────────────────


def build_tree(root: str) -> list[str]:
    """Create the universe's tree and return its files (paths through the
    symlink included, as pytest would see them)."""
    for top in NAMES:
        for sub in ("", "a", ".h", "a/b"):
            d = os.path.join(root, top, sub)
            os.makedirs(d, exist_ok=True)
            open(os.path.join(d, "f.py"), "w").close()
    open(os.path.join(root, "a.py"), "w").close()
    os.symlink("a", os.path.join(root, "ln"), target_is_directory=True)
    files = []
    for dirpath, _, filenames in os.walk(root, followlinks=True):
        rel = os.path.relpath(dirpath, root)
        files.extend(
            name if rel == "." else f"{rel}/{name}".replace(os.sep, "/") for name in filenames
        )
    return sorted(files)


def covered(files: list[str], paths: list[str]) -> frozenset[str]:
    return frozenset(
        f for f in files if any(f == p or f.startswith(p + "/") for p in paths)
    )


def outcome(root: str, files: list[str], entry: str) -> tuple[frozenset[str], bool]:
    paths, warned = chokkin_entry(root, entry)
    return covered(files, paths), warned


REASONS = {"links": "D1", "nolit": "D2", "isdir": "Q5", "files": "Q6"}


def accepted(root: str, files: list[str], entry: str, theirs: frozenset, yields: bool) -> str | None:
    """The divergences an entry shows, named by the smallest set of changes
    to the port that removes them (``fnmatch``: Python's matcher for the
    components holding a ``[`` class, Q3 for a component globset rejects and
    Q4 otherwise)."""
    names = list(REASONS) + ["fnmatch"]
    for size in range(1, len(names) + 1):
        for fixes in itertools.combinations(names, size):
            FIXES.clear()
            FIXES.update(fixes)
            try:
                ours, warned = outcome(root, files, entry)
            finally:
                FIXES.clear()
            if ours != theirs or warned != (not yields):
                continue
            reasons = [REASONS[fix] for fix in fixes if fix in REASONS]
            if "fnmatch" in fixes:
                rejected = any(c in ("t[1", "[z-a]") for c in entry.split("/"))
                reasons.append("Q3" if rejected else "Q4")
            return "+".join(reasons)
    return None


def run() -> tuple[list[str], set[str], int]:
    failures: list[str] = []
    seen: set[str] = set()
    checked = 0
    with tempfile.TemporaryDirectory() as root:
        files = build_tree(root)
        for depth in (1, 2, 3):
            for parts in itertools.product(COMPONENTS, repeat=depth):
                entry = "/".join(parts)
                checked += 1
                ours_paths, warned = chokkin_entry(root, entry)
                theirs_paths = pytest_entry(root, entry)
                ours, theirs = covered(files, ours_paths), covered(files, theirs_paths)
                problems = []
                if ours != theirs:
                    problems.append(
                        f"T1 counterexample: {entry!r}: chokkin covers {sorted(ours - theirs)[:4]} "
                        f"more, {sorted(theirs - ours)[:4]} fewer"
                    )
                if warned != (not theirs_paths):
                    problems.append(
                        f"T2 counterexample: {entry!r}: chokkin warns={warned}, pytest yields {theirs_paths[:4]}"
                    )
                if not problems:
                    continue
                reason = accepted(root, files, entry, theirs, bool(theirs_paths))
                if reason:
                    seen.update(reason.split("+"))
                    continue
                failures.extend(problems)
    return failures, seen, checked


def main() -> int:
    global BROKEN
    if "--broken" in sys.argv[1:]:
        BROKEN = True
        failures, _, _ = run()
        if failures:
            print(f"broken variant caught ({len(failures)} counterexamples), e.g. {failures[0]}")
            return 0
        print("broken variant (wildcards match dot names) was NOT caught")
        return 1
    failures, seen, checked = run()
    for failure in failures[:20]:
        print(failure)
    expected = {"D1", "D2", "Q3", "Q4", "Q5", "Q6"}
    if seen != expected:
        print(f"stale accepted divergences: {sorted(expected - seen)}")
        return 1
    if failures:
        print(f"{len(failures)} counterexamples")
        return 1
    print(f"T1, T2 hold on {checked} entries outside the accepted divergences ({', '.join(sorted(seen))})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
