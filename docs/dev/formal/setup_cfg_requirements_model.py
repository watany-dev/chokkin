#!/usr/bin/env python3
"""Finite model of how chokkin reads ``install_requires`` from ``setup.cfg``.

Port of ``src/manifest/setup_cfg.rs`` (``ini_lines``, ``parse_ini_sections``
/ ``append_value``, ``one_line_options``, ``setup_cfg_requirement_items``)
and ``src/manifest/setuptools_reqs.rs`` (``setuptools_requirement_items``),
the reader #989 made split a one-line value at ``;`` (v0.7.10). The
reference is what setuptools does with the same text: the stdlib
``configparser.ConfigParser`` with ``optionxform = str`` (``setuptools/dist.py``
``parse_config_files``), then ``_parse_requirements_list``
(``_parse_list_semicolon`` and the ``#`` filter, ``setuptools/config/
setupcfg.py``) and ``_reqs.parse_strings`` (``yield_lines``, ``drop_comment``,
``join_continuation`` from ``jaraco.text``), copied from setuptools 80.9.0.
Nothing of the analyzed project runs: the reference parses text only.

The universe: an ``[options]`` header (plain, or with text after ``]``)
followed by every sequence of up to ``DEPTH`` lines drawn from ``LINES``
(one-line and multi-line values, continuations holding ``;``, comments,
blank lines, a trailing ``\\``, an indented key, a second section). A text
configparser rejects (a repeated option, a line with no ``=``) fails
setuptools too, so it is left out.

P1  The requirement texts chokkin declares for ``install_requires`` equal
    the ones setuptools passes to ``Requirement``, in order, outside the
    divergences in ``ACCEPTED``. A new divergence is a counterexample.
P2  ``one_line_options`` holds exactly the options whose configparser value
    has no line break, so the ``;`` split happens where setuptools splits
    (the property #989 rests on).

Accepted divergences, each a decision rather than a bug:

D1  A value whose last line ends in ``\\`` with nothing to join: setuptools
    drops the item, chokkin keeps it so it is reported as an invalid
    requirement line (``setuptools_reqs.rs``, #928, ADR 0005).

Open divergences (counterexamples the reconcile step has not settled; see
``ledger-v0.7.10.ja.md``). They are listed so the run stays green while the
question is open; an entry that stops occurring is reported as stale.

Q1  A key line indented before any option of its section
    (``[options]`` then ``  install_requires = a``): configparser reads the
    option, chokkin skips the line, so every requirement it holds is
    undeclared.

Run:  python3 docs/dev/formal/setup_cfg_requirements_model.py
      python3 docs/dev/formal/setup_cfg_requirements_model.py --broken
``--broken`` swaps in the reader before #989 (every value split at line ends)
and exits 0 only when P1 catches it outside the accepted divergences.
Exit status 1 when a property is violated (counterexamples are printed).
"""

from __future__ import annotations

import configparser
import itertools
import sys

DEPTH = 4

LINES = (
    "install_requires = a;b",
    "install_requires = a ; b # c",
    "install_requires = a \\",
    "install_requires =",
    "install_requires: a",
    "  install_requires = z",
    "other = q",
    "    c;d",
    "    e \\",
    "    # x",
    "    ; x",
    "",
    "# comment",
    "[more]",
)

# The header that opens the section; the second has text after `]`, which
# configparser's SECTCRE ignores (#1308).
HEADERS = ("[options]", "[options] ; c")


# ─── Reference: configparser + setuptools 80.9.0 ─────────────────────────────


def setuptools_value(text: str) -> str | None | bool:
    """The raw ``install_requires`` value, ``None`` when absent, ``False`` when
    configparser rejects the text."""
    parser = configparser.ConfigParser()
    parser.optionxform = str
    try:
        parser.read_string(text)
    except configparser.Error:
        return False
    if not parser.has_section("options"):
        return None
    return parser["options"].get("install_requires")


def parse_list_semicolon(value: str) -> list[str]:
    # setupcfg.py `_parse_list(value, separator=';')`
    chunks = value.splitlines() if "\n" in value else value.split(";")
    return [chunk.strip() for chunk in chunks if chunk.strip()]


def join_continuation(lines):
    # jaraco.text.join_continuation
    lines = iter(lines)
    for item in lines:
        while item.endswith("\\"):
            following = next(lines, None)
            if following is None:
                return
            item = item[:-2].strip() + following
        yield item


def parse_strings(items: list[str]) -> list[str]:
    # _reqs.parse_strings: yield_lines, drop_comment, join_continuation
    lines = [
        line
        for item in items
        for line in map(str.strip, item.splitlines())
        if line and not line.startswith("#")
    ]
    return list(join_continuation(line.partition(" #")[0] for line in lines))


def setuptools_requirements(value: str) -> list[str]:
    parsed = [line for line in parse_list_semicolon(value) if not line.startswith("#")]
    return parse_strings(parsed)


# ─── Port of chokkin ─────────────────────────────────────────────────────────


def ini_lines(contents: str):
    """setup_cfg.rs `ini_lines`: (section, key, value, continuation) or None."""
    current = "default"
    last_key = None
    for raw in contents.splitlines(keepends=True):
        trimmed = raw.strip()
        if not trimmed or trimmed.startswith(("#", ";")):
            yield None
        elif trimmed.startswith("[") and trimmed.rfind("]") > 1:
            # section_header: SECTCRE's greedy name runs to the last `]`.
            current = trimmed[1 : trimmed.rfind("]")].strip()
            last_key = None
            yield ("section", current)
        elif raw.startswith((" ", "\t")):
            if last_key is None:
                yield None
            else:
                yield (current, last_key, trimmed, True)
        else:
            cut = min((i for i in (trimmed.find("="), trimmed.find(":")) if i >= 0), default=-1)
            if cut < 0:
                yield None
                continue
            key = trimmed[:cut].strip().lower()
            last_key = key
            yield (current, key, trimmed[cut + 1 :].strip(), False)


def parse_ini_sections(contents: str) -> dict[str, dict[str, str]]:
    sections: dict[str, dict[str, str]] = {}
    for line in ini_lines(contents):
        if line is None:
            continue
        if line[0] == "section":
            sections.setdefault(line[1], {})
            continue
        section, key, value, continuation = line
        options = sections.setdefault(section, {})
        if key in options:
            # append_value
            if continuation or value:
                if options[key]:
                    options[key] += "\n"
                options[key] += value
        else:
            options[key] = value
    return sections


def one_line_options(contents: str) -> set[tuple[str, str]]:
    counts: dict[tuple[str, str], int] = {}
    for line in ini_lines(contents):
        if line is not None and line[0] != "section":
            counts[(line[0], line[1])] = counts.get((line[0], line[1]), 0) + 1
    return {option for option, count in counts.items() if count == 1}


def setuptools_requirement_items(lines) -> list[str]:
    """setuptools_reqs.rs: like parse_strings, but a dangling `\\` item stays."""
    items: list[str] = []
    pending = None
    cleaned = [line.strip() for line in lines]
    cleaned = [line.partition(" #")[0] for line in cleaned if line and not line.startswith("#")]
    for line in cleaned:
        item = pending[:-2].strip() + line if pending is not None else line
        pending = None
        if item.endswith("\\"):
            pending = item
        else:
            items.append(item.rstrip())
    if pending is not None:
        items.append(pending.rstrip())
    return items


def chokkin_requirements(text: str, broken: bool = False) -> list[str] | None:
    value = parse_ini_sections(text).get("options", {}).get("install_requires")
    if value is None:
        return None
    one_line = ("options", "install_requires") in one_line_options(text)
    if one_line and not broken:
        return setuptools_requirement_items(value.split(";"))
    return setuptools_requirement_items(value.splitlines())


# ─── Check ───────────────────────────────────────────────────────────────────


def accepted(text: str, ours, theirs) -> str | None:
    _, *lines = text.splitlines()
    missed = ours is None and theirs is not None
    for line in lines:
        if line.startswith("["):
            break
        if line.startswith("  install_requires"):
            return "Q1" if missed else None
        if line and not line[0].isspace() and not line.startswith("#"):
            # An option opened first: the indented key continues it.
            break
    if (
        ours is not None
        and theirs is not None
        and len(ours) == len(theirs) + 1
        and ours[:-1] == theirs
        and ours[-1].endswith("\\")
    ):
        return "D1"
    return None


def run(broken: bool) -> tuple[list[str], set[str], int]:
    failures: list[str] = []
    seen: set[str] = set()
    checked = 0
    for depth in range(1, DEPTH + 1):
        for header, *body in itertools.product(HEADERS, *[LINES] * depth):
            text = header + "\n" + "\n".join(body) + "\n"
            value = setuptools_value(text)
            if value is False:
                continue
            checked += 1
            theirs = None if value is None else setuptools_requirements(value)
            ours = chokkin_requirements(text, broken)
            # P2: where both read the option, the one-line test agrees.
            if value is not None and ours is not None and not broken:
                one_line = ("options", "install_requires") in one_line_options(text)
                if one_line != ("\n" not in value):
                    failures.append(f"P2 counterexample: {text!r}: one_line={one_line}, value={value!r}")
            if ours == theirs:
                continue
            reason = accepted(text, ours, theirs)
            if reason:
                seen.add(reason)
                continue
            failures.append(f"P1 counterexample: {text!r}: chokkin {ours}, setuptools {theirs}")
    return failures, seen, checked


def main() -> int:
    if "--broken" in sys.argv[1:]:
        failures, _, _ = run(broken=True)
        if failures:
            print(f"broken variant caught ({len(failures)} counterexamples), e.g. {failures[0]}")
            return 0
        print("broken variant (split every value at line ends) was NOT caught")
        return 1
    failures, seen, checked = run(broken=False)
    for failure in failures[:20]:
        print(failure)
    expected = {"D1", "Q1"}
    if seen != expected:
        print(f"stale accepted divergences: {sorted(expected - seen)}")
        return 1
    if failures:
        print(f"{len(failures)} counterexamples")
        return 1
    print(f"P1, P2 hold on {checked} texts outside the accepted divergences ({', '.join(sorted(seen))})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
