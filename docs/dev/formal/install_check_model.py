#!/usr/bin/env python3
"""Finite model of first-party install checks (#1269, #1272).

Port of ``import_checks`` / ``import_failure`` in ``src/parser/visit.rs`` and
of the gate ``apply_import_checks`` (``src/parser/import_check.rs``) puts on
the imports after a call: a statement call (``check_x()``) gates when the
function is a check that raises, and a call a branch test needs truthy
(``if check_x():``, the code after ``if not check_x(): return``) gates when
it is any check. A gated import is optional, whatever it imports.

The doc comment of ``import_checks`` claims the shape decides: a top-level
function whose body *unconditionally* tries to import packages and raises
from the handler a failed import lands in returns only with them installed,
and one whose handler returns ``None`` / ``False`` returns truthy only with
them installed.

The universe: function bodies of one to three top-level statements, each a
``try: import p_i`` with one ``except ImportError:`` handler (nine handler
bodies, with or without ``finally: return``), ``if cond: return``,
``if cond: return True`` or ``pass``, then an implicit ``return None`` or a
``return True``. The reference runs each body with every subset of the
``p_i`` installed and every value of every ``cond``.

G1  Sound gate: when the port gates a call, some ``p_i`` makes it, when
    missing, raise on every path (a statement call) or never return a truthy
    value (a test). A gate the reference refutes is a counterexample, and
    each must fall under one of the divergences below; a new kind fails.

Known divergences (#1272 follow-up; see ``ledger-v0.7.12.ja.md``). Each
lowers findings: the imports after the call turn optional, so a certain
CHK003 / CHK005 drops to an info note.

D1  An early ``return`` before the ``try`` (``if name != "x": return``,
    or an earlier ``try`` whose handler returns ``True``) makes the call
    return without checking anything.
D2  A handler that may ``return`` before its final ``raise``
    (``if not strict: return True``) is read as raising.
D3  A statement call of a function whose first ``try`` returns ``False``
    and a later one raises: with the first package missing it returns.
D4  ``finally: return`` on any ``try`` swallows the ``raise`` (and a later
    check never runs).

G1 also requires every divergence to have a witness, so fixing one fails
the model until this docstring and ``KNOWN`` drop it.

``--broken`` runs variants that must break G1: any top-level ``try`` counted
whatever its handler does, and a falsy-returning check gating statement
calls.

Run:  python3 docs/dev/formal/install_check_model.py [--broken]
Exit status 1 when a property is violated (counterexamples are printed).
"""

from __future__ import annotations

import itertools
import sys

KNOWN = {"D1", "D2", "D3", "D4"}
BROKEN: set[str] = set()

HANDLERS = (
    ("raise",),
    ("log", "raise"),
    ("return False",),
    ("return None",),
    ("return",),
    ("if_return_true", "raise"),
    ("if_return", "raise"),
    ("if_raise", "return False"),
    ("log",),
    ("return True",),
)
STATEMENTS = [
    ("try", handler, fin) for handler in HANDLERS for fin in (False, True)
] + [("ret_if",), ("ret_true_if",), ("pass",)]
ENDINGS = ((), (("ret_true",),))

RAISES, FALSY = "raises", "falsy"


# ─── Port ────────────────────────────────────────────────────────────────────


def import_failure(stmt) -> str | None:
    """``import_failure``: how the handler's last statement leaves."""
    if "count-every-try" in BROKEN:
        return RAISES
    last = stmt[1][-1]
    if last == "raise":
        return RAISES
    if last in ("return False", "return None", "return"):
        return FALSY
    return None


def port_check(body):
    """``import_checks``: None, or whether the check returns falsy."""
    tries = [import_failure(s) for s in body if s[0] == "try"]
    tries = [t for t in tries if t is not None]
    if not tries:
        return None
    return all(t == FALSY for t in tries)


def port_gates(body, call: str) -> bool:
    returns_falsy = port_check(body)
    if returns_falsy is None:
        return False
    if call == "statement":
        return not returns_falsy or "falsy-gates-statements" in BROKEN
    return True


# ─── Reference ───────────────────────────────────────────────────────────────


def outcomes(body, installed) -> set:
    """Every way the function leaves: "raise", or ("return", truthy)."""
    out: set = set()

    def run(position: int, try_index: int) -> None:
        if position == len(body):
            out.add(("return", False))
            return
        stmt = body[position]
        kind = stmt[0]
        if kind == "pass":
            run(position + 1, try_index)
        elif kind == "ret_true":
            out.add(("return", True))
        elif kind in ("ret_if", "ret_true_if"):
            out.add(("return", kind == "ret_true_if"))
            run(position + 1, try_index)
        else:
            _, handler, fin = stmt
            for left in handler_paths(handler, installed[try_index]):
                if fin:
                    out.add(("return", False))
                elif left == "continue":
                    run(position + 1, try_index + 1)
                else:
                    out.add(left)

    run(0, 0)
    return out


def handler_paths(handler, installed: bool) -> set:
    if installed:
        return {"continue"}
    paths: set = set()

    def step(i: int) -> None:
        if i == len(handler):
            paths.add("continue")
            return
        s = handler[i]
        if s == "raise":
            paths.add("raise")
        elif s == "log":
            step(i + 1)
        elif s.startswith("return"):
            paths.add(("return", s == "return True"))
        else:
            leave = {
                "if_return_true": ("return", True),
                "if_return": ("return", False),
                "if_raise": "raise",
            }[s]
            paths.add(leave)
            step(i + 1)

    step(0)
    return paths


def reference_gates(body, call: str) -> bool:
    n = sum(1 for s in body if s[0] == "try")
    for i in range(n):
        ok = True
        for installed in itertools.product((True, False), repeat=n):
            if installed[i]:
                continue
            for left in outcomes(body, installed):
                if call == "statement" and left != "raise":
                    ok = False
                if call == "test" and left == ("return", True):
                    ok = False
        if ok:
            return True
    return False


# ─── Classification ──────────────────────────────────────────────────────────


def divergence(body, call: str) -> str | None:
    counted = [
        i for i, s in enumerate(body) if s[0] == "try" and import_failure(s) is not None
    ]
    if any(s[0] == "try" and s[2] for s in body):
        return "D4"
    early_exit = ("ret_if", "ret_true_if")
    if any(
        s[0] in early_exit or (s[0] == "try" and s[1] == ("return True",))
        for s in body[: counted[-1]]
    ):
        return "D1"
    if any({"if_return", "if_return_true"} & set(body[i][1]) for i in counted):
        return "D2"
    failures = [import_failure(body[i]) for i in counted]
    if (
        call == "statement"
        and FALSY in failures
        and RAISES in failures
        and failures.index(FALSY) < failures.index(RAISES)
    ):
        return "D3"
    return None


def bodies():
    for length in (1, 2, 3):
        for stmts in itertools.product(STATEMENTS, repeat=length):
            if sum(1 for s in stmts if s[0] == "try") > 2:
                continue
            for ending in ENDINGS:
                yield tuple(stmts) + ending


def check() -> tuple[list[str], dict, int]:
    bad: list[str] = []
    witnesses: dict = {}
    incomplete = 0
    for body in bodies():
        for call in ("statement", "test"):
            port, ref = port_gates(body, call), reference_gates(body, call)
            if port and not ref:
                kind = divergence(body, call)
                if kind in KNOWN:
                    witnesses.setdefault(kind, (body, call))
                else:
                    bad.append(
                        f"G1 {call} call of {body}: gated, but a missing package can return"
                    )
            elif ref and not port:
                incomplete += 1
    if not BROKEN:
        bad.extend(
            f"G1 {kind} has no witness any more: drop it from KNOWN and the docstring"
            for kind in sorted(KNOWN - witnesses.keys())
        )
    return bad, witnesses, incomplete


def main() -> int:
    if "--broken" in sys.argv:
        status = 0
        for variant in ("count-every-try", "falsy-gates-statements"):
            BROKEN.clear()
            BROKEN.add(variant)
            bad, _, _ = check()
            print(
                f"broken variant {variant}: {'caught (' + str(len(bad)) + ')' if bad else 'NOT caught'}"
            )
            status |= not bad
        BROKEN.clear()
        return status
    bad, witnesses, incomplete = check()
    for line in bad[:50]:
        print(line)
    if bad:
        print(f"install_check_model: {len(bad)} counterexamples")
        return 1
    for kind in sorted(witnesses):
        body, call = witnesses[kind]
        print(f"  {kind} witness ({call} call): {body}")
    print(
        f"install_check_model: G1 holds outside D1-D4 ({incomplete} sound gates the port does not take)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
