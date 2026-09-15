#!/usr/bin/env python3
"""Z3 model of exit-status computation with and without a baseline.

Ported from:

- ``src/rules/filter.rs::counts_toward_exit``
- ``src/rules/emit.rs::compute_exit_status``      (used by step 12)
- ``src/baseline/store.rs::compute_exit_status``  (used by ``apply_baseline``)

Spec (docs/dev/spec.ja.md line 608): exit code 1 is triggered by
``severity >= error && confidence >= likely`` (default) or
``severity >= warning && confidence >= maybe`` (``--strict``); line 117:
``--no-exit-code`` forces exit 0.  Line 1064: a baseline silences existing
issues so that *only new issues* fail CI.

Property E1: after ``apply_baseline`` the exit status must equal the status
that ``emit.rs`` would compute over the remaining (non-suppressed) issues.

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

    # emit.rs::compute_exit_status over the full issue list -> "previous"
    any_counts_all = Or(*[counts_toward_exit(sev[i], conf[i], strict) for i in range(n)])
    emit_issues_found = And(Not(no_exit_code), any_counts_all)

    # baseline/store.rs::compute_exit_status(remaining, previous)
    remaining_nonempty = Or(*[Not(suppressed[i]) for i in range(n)])
    baseline_issues_found = And(emit_issues_found, remaining_nonempty)

    # expected: emit.rs semantics over the remaining issues
    any_counts_remaining = Or(
        *[And(Not(suppressed[i]), counts_toward_exit(sev[i], conf[i], strict)) for i in range(n)]
    )
    expected_issues_found = And(Not(no_exit_code), any_counts_remaining)

    solver = Solver()
    solver.add(domain, baseline_issues_found != expected_issues_found)
    if solver.check() == sat:
        m = solver.model()
        print("VIOLATED E1: baseline exit status differs from emit.rs semantics")
        print(f"  strict={m.eval(strict, True)} no_exit_code={m.eval(no_exit_code, True)}")
        for i in range(n):
            print(
                f"  issue{i}: severity={m.eval(sev[i], True)} confidence={m.eval(conf[i], True)} "
                f"suppressed_by_baseline={m.eval(suppressed[i], True)}"
            )
        print(
            f"  baseline_exit=IssuesFound:{m.eval(baseline_issues_found, True)} "
            f"expected=IssuesFound:{m.eval(expected_issues_found, True)}"
        )
        return 1
    print("OK E1")
    return 0


if __name__ == "__main__":
    sys.exit(main())
