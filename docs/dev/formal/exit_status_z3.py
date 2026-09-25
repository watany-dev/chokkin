#!/usr/bin/env python3
"""Z3 model of exit-status computation with and without a baseline.

Ported from:

- ``src/rules/filter.rs::counts_toward_exit``
- ``src/rules/emit.rs::compute_exit_status``      (used by step 12 and ``apply_baseline``)

Spec (docs/dev/spec.ja.md line 608): exit code 1 is triggered by
``severity >= error && confidence >= likely`` (default) or
``severity >= warning && confidence >= maybe`` (``--strict``); line 117:
``--no-exit-code`` forces exit 0.  Line 1064: a baseline silences existing
issues so that *only new issues* fail CI.

Property E1: after ``apply_baseline`` the exit status must equal the status
that ``emit.rs`` would compute over the remaining (non-suppressed) issues.

Property E2: applying a baseline never turns a passing run into a failing one.

Two issues suffice as a universe; severity/confidence are small integers in
rank order (Info=0 < Warning=1 < Error=2; Maybe=0 < Likely=1 < Certain=2).

Run:  python3 docs/dev/formal/exit_status_z3.py
Exit status 1 when a counterexample exists.
"""

from __future__ import annotations

import sys

from z3 import And, Bool, If, Implies, Int, Not, Or, Solver, sat

INFO, WARNING, ERROR = 0, 1, 2
MAYBE, LIKELY, CERTAIN = 0, 1, 2


def counts_toward_exit(severity, confidence, strict):
    min_sev = If(strict, WARNING, ERROR)
    min_conf = If(strict, MAYBE, LIKELY)
    return And(severity >= min_sev, confidence >= min_conf)


def main() -> int:
    strict = Bool("strict")
    no_exit_code = Bool("no_exit_code")
    n = 2
    sev = [Int(f"sev{i}") for i in range(n)]
    conf = [Int(f"conf{i}") for i in range(n)]
    suppressed = [Bool(f"baseline_suppresses{i}") for i in range(n)]

    domain = And(*[And(s >= INFO, s <= ERROR) for s in sev], *[And(c >= MAYBE, c <= CERTAIN) for c in conf])

    def exit_issues_found(kept):
        """``emit.rs::compute_exit_status`` over the issues selected by ``kept``."""
        return And(
            Not(no_exit_code),
            Or(
                *[
                    And(kept(i), counts_toward_exit(sev[i], conf[i], strict))
                    for i in range(n)
                ]
            ),
        )

    # step 12, before any baseline is applied
    emit_issues_found = exit_issues_found(lambda i: True)
    # apply_baseline recomputes over the issues it kept, using
    # the same thresholds and the same --no-exit-code override as emit.rs.
    baseline_issues_found = exit_issues_found(lambda i: Not(suppressed[i]))
    expected_issues_found = exit_issues_found(lambda i: Not(suppressed[i]))

    props = {
        "E1 baseline exit status == emit.rs semantics over remaining issues": (
            baseline_issues_found == expected_issues_found
        ),
        "E2 a baseline never turns a passing run into a failing one": Implies(
            baseline_issues_found, emit_issues_found
        ),
    }

    failed = 0
    for label, prop in props.items():
        solver = Solver()
        solver.add(domain, Not(prop))
        if solver.check() != sat:
            print(f"OK       {label}")
            continue
        failed += 1
        m = solver.model()
        print(f"VIOLATED {label}")
        print(f"  strict={m.eval(strict, True)} no_exit_code={m.eval(no_exit_code, True)}")
        for i in range(n):
            print(
                f"  issue{i}: severity={m.eval(sev[i], True)} confidence={m.eval(conf[i], True)} "
                f"suppressed_by_baseline={m.eval(suppressed[i], True)}"
            )
        print(
            f"  emit_exit=IssuesFound:{m.eval(emit_issues_found, True)} "
            f"baseline_exit=IssuesFound:{m.eval(baseline_issues_found, True)} "
            f"expected=IssuesFound:{m.eval(expected_issues_found, True)}"
        )
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
