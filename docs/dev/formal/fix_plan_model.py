#!/usr/bin/env python3
"""Exhaustive model of the CHK005 ``--fix`` planner against the CHK005 rule.

Ports (spec §10 CHK005 and §13 fix spec):

- the rule: ``detect_misplaced_dependencies`` in ``src/rules/deps/misplaced.rs``
  with ``ImportDeclarations::{lookup, member_satisfies, governing}`` from
  ``src/rules/deps/missing.rs`` and the buckets of ``src/rules/deps/context.rs``;
- the planner: ``plan_move_to_runtime`` in ``src/fix/plan.rs``;
- the edit: ``move_group_to_runtime`` in ``src/fix/pyproject.rs`` (remove the
  group entry, which fails when it is already gone, then append to
  ``[project].dependencies`` without duplicating).

One distribution X, a root and one workspace member, imported at runtime from
the root, the member, or both.  What each side declares for X:

    R  [project].dependencies                      runtime bucket
    X  [project.optional-dependencies]             optional bucket
    D  [dependency-groups].dev in pyproject.toml   dev bucket, movable
    Q  requirements-dev.txt                        dev bucket, not TOML
    O  opaque dev entry                            dev bucket, the planner ignores it
    E  ``Y[extra]`` in a dev group whose lockfile extra pulls in X: indexed under
       X (``build_import_declared_index``) but not a declaration of X

``static`` says the side's pyproject has a static ``[project].dependencies``
(``require_static_project_dependencies``).

Properties of the planner on every state:

    F1 converges  a finding whose move was applied is gone on the re-run
    F2 target     the move edits the manifest that governs the finding
    F3 complete   the entry is moved when the governing manifest has, among
                  its declarations of X, exactly one pyproject dev entry and
                  no runtime one, and a static [project].dependencies
    F4 clean      every planned move applies (no stale "could not remove")

Run:  python3 docs/dev/formal/fix_plan_model.py
Exit status 1 when the fixed planner breaks a property or a broken variant
is not caught.
"""

from __future__ import annotations

import itertools
import sys
from collections.abc import Callable
from dataclasses import dataclass

KINDS = "RXDQOE"
RUNTIME = frozenset("RX")
SIDES = ("root", "api")


@dataclass(frozen=True)
class State:
    decls: tuple[tuple[str, ...], tuple[str, ...]]  # per side, in manifest order
    static: tuple[bool, bool]
    strict: bool
    sites: frozenset[str]  # sides importing X at runtime


@dataclass(frozen=True)
class Finding:
    member: str | None  # issue.workspace_member
    governing: str  # side whose declarations decided the finding


@dataclass(frozen=True)
class Move:
    finding: Finding
    side: str
    index: int  # position of the D entry in the side's declarations


def side_index(side: str) -> int:
    return SIDES.index(side)


def chk005(state: State) -> list[Finding]:
    """Port of the CHK005 rule: one finding per (member, X)."""
    findings = []
    for site in SIDES:
        if site not in state.sites:
            continue
        member = None if site == "root" else site
        member_decls = state.decls[side_index(site)] if member else ()
        # member_satisfies: an R/X entry of the member's own.
        if member and RUNTIME & set(member_decls):
            continue
        # governing: the member's index entries win only under --strict.
        governing = site if member and member_decls and state.strict else "root"
        decls = state.decls[side_index(governing)]
        if not decls or RUNTIME & set(decls):
            continue
        findings.append(Finding(member, governing))
    return findings


Targeter = Callable[[State, Finding], str]


def target_root(state: State, finding: Finding) -> str:
    """``plan_move_to_runtime`` before the fix: always the root manifest."""
    return "root"


def target_governing(state: State, finding: Finding) -> str:
    """Fixed: the member when its declared index (opaque and lockfile-extra
    entries included) has X under --strict, as ``governing`` does."""
    member = finding.member
    if member and state.strict and state.decls[side_index(member)]:
        return member
    return "root"


def target_member_ignoring_strict(state: State, finding: Finding) -> str:
    """Broken: the member whenever it declares X, --strict or not."""
    member = finding.member
    if member and state.decls[side_index(member)]:
        return member
    return "root"


def target_direct_only(state: State, finding: Finding) -> str:
    """Broken: the member only when it declares X itself, missing the
    entries a lockfile extra adds to the index."""
    member = finding.member
    if member and state.strict and set(state.decls[side_index(member)]) - {"E"}:
        return member
    return "root"


def plan(state: State, targeter: Targeter, dedupe: bool) -> list[Move]:
    """Port of ``plan_move_to_runtime`` for each finding, in report order."""
    moves: list[Move] = []
    for finding in chk005(state):
        side = targeter(state, finding)
        decls = state.decls[side_index(side)]
        # Declarations of X itself, opaque ones dropped.
        direct = [(i, kind) for i, kind in enumerate(decls) if kind in "RXDQ"]
        if any(kind in RUNTIME for _, kind in direct):
            continue  # Ambiguous: runtime declaration already exists
        groups = [(i, kind) for i, kind in direct if kind in "DQ"]
        if len(groups) != 1:
            continue  # Ambiguous: only a single dev-group entry moves
        index, kind = groups[0]
        if kind == "Q" or not state.static[side_index(side)]:
            continue  # UnsupportedTarget
        move = Move(finding, side, index)
        if dedupe and any((m.side, m.index) == (side, index) for m in moves):
            continue
        moves.append(move)
    return moves


def apply(state: State, moves: list[Move]) -> tuple[State, list[Move], list[Move]]:
    """Port of ``move_group_to_runtime``; returns (state, applied, failed)."""
    decls = [list(side) for side in state.decls]
    removed: set[tuple[str, int]] = set()
    applied, failed = [], []
    for move in moves:
        key = (move.side, move.index)
        if key in removed:
            failed.append(move)  # remove_label_in_document finds no entry
            continue
        removed.add(key)
        applied.append(move)
    for side, index in sorted(removed, key=lambda key: -key[1]):
        side_decls = decls[side_index(side)]
        del side_decls[index]
        if "R" not in side_decls:
            side_decls.append("R")
    after = State(
        (tuple(decls[0]), tuple(decls[1])), state.static, state.strict, state.sites
    )
    return after, applied, failed


def violations(state: State, targeter: Targeter, dedupe: bool) -> list[str]:
    moves = plan(state, targeter, dedupe)
    after, applied, failed = apply(state, moves)
    remaining = {f.member for f in chk005(after)}
    out = [
        f"F1 finding {move.finding} remains after the move"
        for move in applied
        if move.finding.member in remaining
    ]
    out.extend(
        f"F2 move edits {move.side}, {move.finding.governing} governs"
        for move in moves
        if move.side != move.finding.governing
    )
    for finding in chk005(state):
        side = side_index(finding.governing)
        decls = state.decls[side]
        movable = (
            decls.count("D") == 1
            and not set(decls) & set("RXQ")
            and state.static[side]
        )
        # A finding that shares its entry with an earlier one rides its move.
        target = (finding.governing, decls.index("D") if movable else -1)
        if movable and not any((m.side, m.index) == target for m in moves):
            out.append(f"F3 no move planned for {finding}")
    out.extend(
        f"F4 move for {move.finding} fails: entry already moved" for move in failed
    )
    return out


def states() -> list[State]:
    multisets = [
        combo
        for size in range(4)
        for combo in itertools.product(KINDS, repeat=size)
        if list(combo) == sorted(combo, key=KINDS.index)
    ]
    sites = [frozenset(s) for s in ({"root"}, {"api"}, {"root", "api"})]
    return [
        State((root, member), static, strict, site)
        for root, member in itertools.product(multisets, repeat=2)
        for static in itertools.product((False, True), repeat=2)
        for strict in (False, True)
        for site in sites
    ]


VARIANTS: list[tuple[str, Targeter, bool, bool]] = [
    # (name, targeter, dedupe, expected to hold)
    ("fixed planner", target_governing, True, True),
    ("planner before the fix (root only, no dedupe)", target_root, False, False),
    ("broken: member regardless of --strict", target_member_ignoring_strict, True, False),
    ("broken: member only on a direct entry", target_direct_only, True, False),
    ("broken: no dedupe of a shared move", target_governing, False, False),
]


def main() -> int:
    all_states = states()
    failures = []
    for name, targeter, dedupe, expected in VARIANTS:
        # The first witness of each property the variant breaks.
        witnesses: dict[str, tuple[State, str]] = {}
        for state in all_states:
            for found in violations(state, targeter, dedupe):
                witnesses.setdefault(found[:2], (state, found))
        holds = not witnesses
        print(f"{'OK' if holds else 'VIOLATE':8} {name}")
        for prop in sorted(witnesses):
            state, found = witnesses[prop]
            print(f"         {found}")
            print(f"           in {state}")
        if holds != expected:
            failures.append(f"{name}: expected {'OK' if expected else 'VIOLATE'}")
    if failures:
        print(f"{len(failures)} counterexample(s):")
        for failure in failures:
            print(f"  {failure}")
        return 1
    print(
        f"OK: F1-F4 hold for the fixed planner over {len(all_states)} states;"
        " every broken variant is caught"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
