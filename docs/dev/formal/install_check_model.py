#!/usr/bin/env python3
"""Finite model of first-party install checks (#1269, #1272).

Port of ``import_checks`` / ``import_failure`` in ``src/parser/visit.rs`` and
of the gate ``apply_import_checks`` (``src/parser/import_check.rs``) puts on
the imports after a call: a statement call (``check_x()``) gates when the
function is a check that raises, and a call a branch test needs truthy
(``if check_x():``, the code after ``if not check_x(): return``) gates when
it is any check. A gated import is optional, whatever it imports.

The doc comment of ``import_checks`` claims the shape decides: the first
top-level ``try`` that imports packages and raises from the handler a failed
import lands in makes the call return only with them installed, and one
whose handler returns ``None`` / ``False`` makes it return truthy only with
them installed. A ``return`` before that ``try``, in its handler or in its
``finally`` is a check only when falsy, and then only for a test.

The universe: function bodies of one to three top-level statements, each a
``try: import p_i`` with one ``except ImportError:`` handler (nine handler
bodies, with or without ``finally: return``), ``if cond: return``,
``if cond: return True`` or ``pass``, then an implicit ``return None`` or a
``return True``. The reference runs each body with every subset of the
``p_i`` installed and every value of every ``cond``.

G1  Sound gate: when the port gates a call, some ``p_i`` makes it, when
    missing, raise on every path (a statement call) or never return a truthy
    value (a test). A gate the reference refutes is a counterexample.

G1 held with four known divergences until they were fixed (D1-D4 in the
``ledger-v0.7.12.ja.md``): an early ``return``, a handler that may return
before its ``raise``, a statement call past a first ``try`` that returns
``False``, and ``finally: return``.

``--broken`` runs variants that must break G1: any top-level ``try`` counted
whatever its handler does, a falsy-returning check gating statement calls,
and the statements before the first counted ``try`` left unread.

Run:  python3 docs/dev/formal/install_check_model.py [--broken]
Exit status 1 when a property is violated (counterexamples are printed).
"""

from __future__ import annotations

import itertools
import sys

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
    """``import_failure``: how the handler leaves, ``None`` when it may
    return a truthy value or ``finally`` returns."""
    if "count-every-try" in BROKEN:
        return RAISES
    _, handler, fin = stmt
    if fin:
        return None
    *rest, last = handler
    early = {"if_return_true": "truthy", "if_return": "falsy"}
    early = {early[s] for s in rest if s in early}
    early |= {"truthy" for s in rest if s == "return True"}
    if "truthy" in early:
        return None
    if last == "raise":
        return FALSY if early else RAISES
    if last in ("return False", "return None", "return"):
        return FALSY
    return None


def early_returns(stmt) -> set:
    """The kinds of ``return`` in a statement: "falsy" and "truthy"."""
    kinds = {"ret_if": "falsy", "ret_true_if": "truthy", "ret_true": "truthy"}
    if stmt[0] in kinds:
        return {kinds[stmt[0]]}
    if stmt[0] != "try":
        return set()
    _, handler, fin = stmt
    found = {"falsy"} if fin else set()
    for s in handler:
        if s in ("return True", "if_return_true"):
            found.add("truthy")
        elif s.startswith(("return", "if_return")):
            found.add("falsy")
    return found


def port_check(body):
    """``import_checks``: None, or whether the check returns falsy."""
    returns_early = False
    for stmt in body:
        if stmt[0] == "try" and (failure := import_failure(stmt)) is not None:
            return returns_early or failure == FALSY
        early = early_returns(stmt)
        if "early-return" in BROKEN:
            continue
        if "truthy" in early:
            return None
        returns_early |= bool(early)
    return None


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


# ─── Check ───────────────────────────────────────────────────────────────────


def bodies():
    for length in (1, 2, 3):
        for stmts in itertools.product(STATEMENTS, repeat=length):
            if sum(1 for s in stmts if s[0] == "try") > 2:
                continue
            for ending in ENDINGS:
                yield tuple(stmts) + ending


def check() -> tuple[list[str], int]:
    bad: list[str] = []
    incomplete = 0
    for body in bodies():
        for call in ("statement", "test"):
            port, ref = port_gates(body, call), reference_gates(body, call)
            if port and not ref:
                bad.append(
                    f"G1 {call} call of {body}: gated, but a missing package can return"
                )
            elif ref and not port:
                incomplete += 1
    return bad, incomplete


def main() -> int:
    if "--broken" in sys.argv:
        status = 0
        for variant in ("count-every-try", "falsy-gates-statements", "early-return"):
            BROKEN.clear()
            BROKEN.add(variant)
            bad, _ = check()
            print(
                f"broken variant {variant}: {'caught (' + str(len(bad)) + ')' if bad else 'NOT caught'}"
            )
            status |= not bad
        BROKEN.clear()
        return status
    bad, incomplete = check()
    for line in bad[:50]:
        print(line)
    if bad:
        print(f"install_check_model: {len(bad)} counterexamples")
        return 1
    print(
        f"install_check_model: G1 holds ({incomplete} sound gates the port does not take)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
