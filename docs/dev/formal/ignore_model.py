#!/usr/bin/env python3
"""Exhaustive model of config ignore matching (spec §18 vs src/rules/ignore.rs).

Port of the dependency-rule branches of ``config_pattern_matches`` from
``src/rules/ignore.rs`` (the File / Symbol / Binary branches are irrelevant to
I1 and omitted).  ``glob_match`` is approximated with ``fnmatch`` (the patterns
used here contain only ``*`` and literals, on which ``globset`` and ``fnmatch``
agree).

Spec (docs/dev/spec.ja.md line 1044):

    dependency系ruleではdistribution名のglob、file系ruleではpath glob、
    symbol系ruleでは `path:symbol_glob` 形式とする。

Reference semantics I1: for every dependency rule (CHK002-005, 008, 009) a
config pattern ignores a candidate iff it glob-matches the *distribution name*
of that candidate.

Run:  python3 docs/dev/formal/ignore_model.py
Exit status 1 when any (rule, pattern, candidate) disagrees with I1.
"""

from __future__ import annotations

import itertools
import sys
from dataclasses import dataclass
from fnmatch import fnmatchcase


@dataclass(frozen=True)
class Subject:
    kind: str  # "Import" | "Distribution"
    name: str = ""  # distribution name
    module: str = ""
    file: str = ""


@dataclass(frozen=True)
class Candidate:
    rule: str
    subject: Subject
    distribution: str  # what the spec calls the rule's target (not stored in subject)


DIST_RULES = {"CHK002", "CHK003", "CHK004", "CHK005", "CHK008", "CHK009"}


def glob_match(pattern: str, value: str) -> bool:
    return fnmatchcase(value, pattern)


def config_pattern_matches(rule: str, pattern: str, subject: Subject) -> bool:
    if subject.kind == "Distribution" and rule in DIST_RULES:
        return glob_match(pattern, subject.name)
    if subject.kind == "Import":
        return glob_match(pattern, subject.file) or glob_match(pattern, subject.module)
    return False


# Candidates as actually constructed by the rule modules:
#   CHK002/CHK005/CHK009 -> IssueSubject::Distribution
#   CHK003/CHK004        -> IssueSubject::Import { module, file, line }  (missing.rs)
CANDIDATES = [
    Candidate("CHK002", Subject("Distribution", name="pyyaml"), "pyyaml"),
    Candidate("CHK005", Subject("Distribution", name="pyyaml"), "pyyaml"),
    Candidate("CHK003", Subject("Import", module="yaml", file="src/acme/app.py"), "pyyaml"),
    Candidate("CHK004", Subject("Import", module="yaml", file="src/acme/app.py"), "pyyaml"),
    Candidate(
        "CHK003",
        Subject("Import", module="google.cloud.storage", file="src/acme/app.py"),
        "google-cloud-storage",
    ),
    Candidate(
        "CHK004",
        Subject("Import", module="google.cloud.storage", file="src/acme/app.py"),
        "google-cloud-storage",
    ),
    Candidate("CHK003", Subject("Import", module="pkg_resources", file="src/acme/app.py"), "setuptools"),
]

PATTERNS = ["pyyaml", "google-cloud-*", "setuptools", "yaml", "src/acme/*", "google-cloud-storage"]


def main() -> int:
    failures = []
    for cand, pattern in itertools.product(CANDIDATES, PATTERNS):
        got = config_pattern_matches(cand.rule, pattern, cand.subject)
        expected = glob_match(pattern, cand.distribution)
        if got != expected:
            failures.append(
                f"I1 rule={cand.rule} pattern={pattern!r} subject={cand.subject.kind}"
                f"(module={cand.subject.module!r}, file={cand.subject.file!r}) "
                f"distribution={cand.distribution!r}: ignored={got} expected={expected}"
            )
    if not failures:
        print("OK: I1 holds")
        return 0
    print(f"{len(failures)} counterexample(s):")
    for line in failures:
        print("  " + line)
    return 1


if __name__ == "__main__":
    sys.exit(main())
