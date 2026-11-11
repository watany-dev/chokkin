#!/usr/bin/env python3
"""Finite model of when a ``sys.version_info`` guard makes an import never run (#997, v0.7.10).

``version_guard_model.py`` checks the reading of each test against CPython
(including names bound to tests, #1272). This model shares its V1-V3 ground
only as a precondition and carries the reading on to the decision that
matters for findings: the nesting in ``visit.rs`` and ``excludes_minors``
against the ``requires-python`` window (V4), and the run encoding (V5).

Port of ``src/parser/version_guard.rs`` (``VersionTest::read`` / ``holds``,
``branch_minors``, ``minor_runs``), ``src/parser/condition.rs`` (``can``,
``excluded``), the nesting in ``src/parser/visit.rs`` (each enclosing
branch's minors are intersected) and ``StdlibRange::excludes_minors``
(``src/resolver/stdlib.rs``), which together decide that an import "never
runs" on any Python ``requires-python`` allows, so it is no CHK003 / CHK004
/ CHK005. The reference is Python's own tuple comparison on
``sys.version_info == (3, m, micro, "final", 0)`` and the ``if`` / ``elif`` /
``else`` semantics of ``not`` / ``and`` / ``or`` on truth values.

The universe:

* every comparison chokkin reads or refuses: ``sys.version_info``,
  ``sys.version_info[:2]`` and ``sys.version_info[0]`` on either side of
  ``<`` ``<=`` ``>`` ``>=`` ``==`` ``!=``, against ``(M,)`` / ``(M, N)`` /
  ``M`` with ``M`` in 0..4 and ``N`` in 0..15, on minors 0..20 and on the
  last bit (127), which stands for every later minor;
* every boolean tree up to depth 2 over two fixed leaves and two opaque
  ones (one of them possibly repeated), for ``can`` / ``fixed`` /
  ``excluded``;
* every ``if`` / ``elif`` / ``else`` chain of up to 3 tests drawn from a pool
  of version comparisons, their negations, an opaque test and their
  ``and`` / ``or``, nested 1 or 2 deep, against every ``requires-python``
  window ``[floor, max]`` with 6 <= floor <= max <= 16, and with no upper
  bound (``max == u32::MAX``).

Out of scope: module-level names bound to a version test (``PY2 =
sys.version_info[0] == 2``, #1272, after v0.7.10). ``version_leaf`` reads
them through ``VersionNames``; this model does not.

V1  ``holds`` is Python's comparison: for every comparison chokkin reads and
    every minor, ``holds(minor)`` equals the comparison's value; the last
    bit's answer is every later minor's; ``sys.version_info == (3, 9)``
    (never true, a likely typo for ``[:2]``) is not read.
V2  ``can(e, v)`` is sound: when it is true, ``e`` has truth value ``v`` for
    every value of the opaque leaves. It is exact when no opaque leaf repeats.
V3  ``excluded`` is sound: an excluded branch runs for no value of the
    opaque leaves (a branch runs when every earlier test is false and its
    own is true).
V4  ``never_runs`` is sound end to end: when chokkin says an import never
    runs, then on every minor in the window and every value of the opaque
    tests, the branches enclosing it do not all run.
V5  ``minor_runs`` is a faithful encoding: the runs cover exactly the set
    bits, a run through the last bit ends at ``u32::MAX``.

Run:  python3 docs/dev/formal/never_runs_model.py
      python3 docs/dev/formal/never_runs_model.py --broken
``--broken`` runs three broken variants (``holds`` forgetting that
``sys.version_info`` is longer than the tuple; ``can`` treating an opaque
leaf as free; ``excludes_minors`` with an inclusive ``max``) and exits 0
only when each is caught.
Exit status 1 when a property is violated (counterexamples are printed).
"""

from __future__ import annotations

import itertools
import sys
from dataclasses import dataclass

LAST_MINOR = 127  # Minors::BITS - 1
U32_MAX = 2**32 - 1
OPS = ("<", "<=", ">", ">=", "==", "!=")
SWAPPED = {"<": ">", "<=": ">=", ">": "<", ">=": "<=", "==": "==", "!=": "!="}

BROKEN: set[str] = set()


def compare(op: str, left, right) -> bool:
    return {
        "<": left < right,
        "<=": left <= right,
        ">": left > right,
        ">=": left >= right,
        "==": left == right,
        "!=": left != right,
    }[op]


# ─── Port of version_guard.rs ────────────────────────────────────────────────


@dataclass(frozen=True)
class VersionTest:
    op: str
    major: int
    minor: int | None
    full: bool
    major_only: bool

    def holds(self, minor: int) -> bool:
        def cmp(a, b):
            return (a > b) - (a < b)

        ordering = cmp(3, self.major)
        if ordering == 0:
            if self.major_only:
                ordering = 0
            elif self.minor is not None:
                ordering = cmp(minor, self.minor)
            else:
                ordering = 1
        if ordering == 0 and self.full and "full" not in BROKEN:
            ordering = 1
        return {
            "<": ordering < 0,
            "<=": ordering <= 0,
            ">": ordering > 0,
            ">=": ordering >= 0,
            "==": ordering == 0,
            "!=": ordering != 0,
        }[self.op]


def small_int(value: int) -> int | None:
    return value if 0 <= value < LAST_MINOR else None


def read(form: str, side: str, op: str, literal) -> VersionTest | None:
    """VersionTest::read for `<form> <op> <literal>` (side "left") or
    `<literal> <op> <form>` (side "right"). `literal` is an int for the
    major form and a tuple otherwise."""
    if side == "right":
        op = SWAPPED[op]
    if form == "major":
        if not isinstance(literal, int) or small_int(literal) is None:
            return None
        return VersionTest(op, literal, None, False, True)
    if not isinstance(literal, tuple) or len(literal) not in (1, 2):
        return None
    if any(small_int(part) is None for part in literal):
        return None
    full = form == "full"
    if op in ("==", "!=") and full:
        return None
    major, minor = literal[0], (literal[1] if len(literal) == 2 else None)
    return VersionTest(op, major, minor, full, False)


def python_value(form: str, side: str, op: str, literal, minor: int, micro: int = 0) -> bool:
    info = (3, minor, micro, "final", 0)
    value = {"full": info, "slice": info[:2], "major": info[0]}[form]
    return compare(op, value, literal) if side == "left" else compare(op, literal, value)


def minor_runs(minors: set[int]) -> list[list[int]]:
    runs: list[list[int]] = []
    for minor in sorted(minors):
        high = U32_MAX if minor == LAST_MINOR else minor
        if runs and runs[-1][1] + 1 == minor:
            runs[-1][1] = high
        else:
            runs.append([minor, high])
    return runs


def excludes_minors(floor: int | None, max_: int, runs) -> bool:
    if floor is None:
        return False
    if "max" in BROKEN:
        return all(high < floor or max_ <= low for low, high in runs)
    return all(high < floor or max_ < low for low, high in runs)


# ─── Port of condition.rs ────────────────────────────────────────────────────
# An expression is ("leaf", name) | ("not", e) | ("and", [e...]) | ("or", [e...]).
# `leaf(name)` returns ("fixed", bool) or ("opaque",).


def can(expr, value: bool, leaf) -> bool:
    kind = expr[0]
    if kind == "not":
        return can(expr[1], not value, leaf)
    if kind in ("and", "or"):
        if (kind == "and") == value:
            return all(can(operand, value, leaf) for operand in expr[1])
        return any(can(operand, value, leaf) for operand in expr[1])
    known = leaf(expr[1])
    if known[0] == "opaque":
        return "opaque" in BROKEN
    return known[1] == value


def fixed(expr, leaf) -> bool | None:
    t, f = can(expr, True, leaf), can(expr, False, leaf)
    if t and not f:
        return True
    if f and not t:
        return False
    return None


def excluded(earlier, test, leaf) -> bool:
    return any(can(e, True, leaf) for e in earlier) or (
        test is not None and can(test, False, leaf)
    )


def evaluate(expr, env) -> bool:
    kind = expr[0]
    if kind == "not":
        return not evaluate(expr[1], env)
    if kind == "and":
        return all(evaluate(e, env) for e in expr[1])
    if kind == "or":
        return any(evaluate(e, env) for e in expr[1])
    return env[expr[1]]


def leaf_names(expr) -> list[str]:
    if expr[0] == "leaf":
        return [expr[1]]
    if expr[0] == "not":
        return leaf_names(expr[1])
    return [name for e in expr[1] for name in leaf_names(e)]


def runs_branch(earlier, test, env) -> bool:
    return not any(evaluate(e, env) for e in earlier) and (test is None or evaluate(test, env))


# ─── Properties ──────────────────────────────────────────────────────────────


def check_v1(failures: list[str]) -> None:
    minors = list(range(21)) + [LAST_MINOR]
    for form, side, op in itertools.product(("full", "slice", "major"), ("left", "right"), OPS):
        literals = (
            list(range(5))
            if form == "major"
            else [(m,) for m in range(5)] + [(m, n) for m in range(5) for n in range(16)]
        )
        for literal in literals:
            test = read(form, side, op, literal)
            if form == "full" and op in ("==", "!=") and test is not None:
                failures.append(f"V1: {form} {op} {literal} is read; it should stay opaque")
            if test is None:
                continue
            for minor in minors:
                later = [minor] if minor < LAST_MINOR else range(LAST_MINOR, LAST_MINOR + 40)
                for actual, micro in itertools.product(later, (0, 7)):
                    want = python_value(form, side, op, literal, actual, micro)
                    if test.holds(minor) != want:
                        failures.append(
                            f"V1: {side} {form} {op} {literal} on 3.{actual}.{micro}: "
                            f"holds({minor})={test.holds(minor)}, Python {want}"
                        )
                        break


def trees(leaves, depth):
    if depth == 0:
        return [("leaf", name) for name in leaves]
    smaller = trees(leaves, depth - 1)
    out = list(smaller)
    out += [("not", e) for e in smaller]
    for kind in ("and", "or"):
        out += [(kind, [a, b]) for a, b in itertools.product(smaller, repeat=2)]
    return out


def check_v2_v3(failures: list[str]) -> None:
    known = {"T": ("fixed", True), "F": ("fixed", False), "O": ("opaque",), "P": ("opaque",)}
    leaf = known.__getitem__
    exprs = trees(("T", "F", "O", "P"), 1) + [
        ("and", [("or", [("leaf", "O"), ("leaf", "T")]), ("not", ("leaf", "P"))]),
        ("or", [("and", [("leaf", "O"), ("leaf", "F")]), ("not", ("leaf", "T"))]),
    ]
    envs = [
        {"T": True, "F": False, "O": o, "P": p}
        for o, p in itertools.product((False, True), repeat=2)
    ]
    for expr in exprs:
        for value in (True, False):
            values = {evaluate(expr, env) for env in envs}
            if can(expr, value, leaf) and values != {value}:
                failures.append(f"V2 soundness: can({expr}, {value}) but it takes {values}")
            names = leaf_names(expr)
            opaque = [n for n in names if n in "OP"]
            exact = len(opaque) == len(set(opaque))
            if exact and not can(expr, value, leaf) and values == {value}:
                failures.append(f"V2 exactness: not can({expr}, {value}) yet it is always {value}")
    pool = trees(("T", "F", "O", "P"), 0) + [("not", ("leaf", "O")), ("and", [("leaf", "O"), ("leaf", "T")])]
    for n in range(3):
        for earlier in itertools.product(pool, repeat=n):
            failures.extend(
                f"V3: excluded({earlier}, {test}) but the branch runs"
                for test in [*pool, None]
                if excluded(list(earlier), test, leaf)
                and any(runs_branch(earlier, test, env) for env in envs)
            )


# Version comparisons of the V4 pool, as (form, side, op, literal).
POOL = [
    ("full", "left", ">=", (3, 9)),
    ("slice", "left", "<", (3, 11)),
    ("full", "right", "<=", (3, 12)),
    ("slice", "left", "==", (3, 10)),
    ("major", "left", "==", 2),
    ("full", "left", ">=", (4,)),
]


def check_v4(failures: list[str]) -> None:
    names = [f"v{i}" for i in range(len(POOL))] + ["X"]
    atoms = [("leaf", n) for n in names]
    tests = atoms + [("not", ("leaf", n)) for n in names[:3]] + [
        ("and", [("leaf", "v0"), ("leaf", "X")]),
        ("or", [("leaf", "v1"), ("leaf", "X")]),
    ]

    def leaf_at(minor):
        def leaf(name):
            if name == "X":
                return ("opaque",)
            test = read(*POOL[int(name[1:])])
            return ("opaque",) if test is None else ("fixed", test.holds(minor))

        return leaf

    def branch_minors(earlier, test) -> set[int]:
        return {
            minor
            for minor in range(LAST_MINOR + 1)
            if not excluded(earlier, test, leaf_at(minor))
        }

    # One level: a chain and a branch in it.
    chains = [
        (list(chain[:at]), chain[at] if at < n else None)
        for n in range(1, 3)
        for chain in itertools.product(tests, repeat=n)
        for at in range(n + 1)  # `at == n` is the `else`
    ]
    # `top = U32_MAX` is a `requires-python` with no upper bound.
    windows = [(floor, top) for floor in range(6, 17) for top in [*range(floor, 17), U32_MAX]]
    checked = 0
    nests = [[c] for c in chains] + [
        [a, b] for a, b in itertools.product(chains[:: max(1, len(chains) // 40)], repeat=2)
    ]
    for nest in nests:
        minors = set(range(LAST_MINOR + 1))
        for earlier, test in nest:
            minors &= branch_minors(earlier, test)
        runs = minor_runs(minors)
        for floor, top in windows:
            if not excludes_minors(floor, top, runs):
                continue
            checked += 1
            for minor in range(floor, min(top, LAST_MINOR + 8) + 1):
                for x in (False, True):
                    env = {"X": x}
                    for i, comparison in enumerate(POOL):
                        env[f"v{i}"] = python_value(*comparison, minor)
                    if all(runs_branch(earlier, test, env) for earlier, test in nest):
                        failures.append(
                            f"V4: never_runs for {nest} with requires-python >=3.{floor}"
                            f"{'' if top == U32_MAX else f',<3.{top + 1}'}, but it runs on 3.{minor} (X={x})"
                        )
    if checked == 0:
        failures.append("V4: no import was ever found to never run; the check is vacuous")


def check_v5(failures: list[str]) -> None:
    samples = [set(), {0}, {LAST_MINOR}, set(range(LAST_MINOR + 1)), {3, 4, 5, 9, 126, 127}]
    samples += [set(c) for c in itertools.combinations([0, 1, 2, 5, 126, 127], 3)]
    for minors in samples:
        runs = minor_runs(minors)
        covered = {m for low, high in runs for m in range(low, min(high, LAST_MINOR) + 1)}
        if covered != minors:
            failures.append(f"V5: minor_runs({sorted(minors)}) covers {sorted(covered)}")
        if LAST_MINOR in minors and runs[-1][1] != U32_MAX:
            failures.append(f"V5: run through the last bit ends at {runs[-1][1]}")


def run() -> list[str]:
    failures: list[str] = []
    check_v1(failures)
    check_v2_v3(failures)
    check_v4(failures)
    check_v5(failures)
    return failures


def main() -> int:
    if "--broken" in sys.argv[1:]:
        status = 0
        for variant in ("full", "opaque", "max"):
            BROKEN.clear()
            BROKEN.add(variant)
            failures = run()
            if failures:
                print(f"broken variant {variant!r} caught: {failures[0]}")
            else:
                print(f"broken variant {variant!r} was NOT caught")
                status = 1
        return status
    failures = run()
    for failure in failures[:20]:
        print(failure)
    if failures:
        print(f"{len(failures)} counterexamples")
        return 1
    print("V1-V5 hold")
    return 0


if __name__ == "__main__":
    sys.exit(main())
