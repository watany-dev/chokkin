#!/usr/bin/env python3
"""Model of the exit status with and without a baseline, baseline matching
included.

Ported from:

- ``src/rules/filter.rs::counts_toward_exit``
- ``src/rules/emit.rs::compute_exit_status``      (used by step 12 and ``apply_baseline``)
- ``src/rules/types.rs::issue_stable_target`` / ``target_part`` / ``issue_fingerprint``
- ``src/baseline/store.rs::write_baseline`` / ``apply_baseline``

Spec (docs/dev/spec.ja.md §16 / §18): exit code 1 is triggered by
``severity >= error && confidence >= likely`` (default) or
``severity >= warning && confidence >= maybe`` (``--strict``);
``--no-exit-code`` forces exit 0.  A baseline silences existing issues so
that *only new issues* fail CI.  An issue is "the same" as a frozen one when
its rule, workspace member and subject agree: the forward-slash path plus the
symbol or module of a file, symbol or import issue, never the line (§18, and
§22 for imports: a line added to an import subject leaves the baseline as it
was, #830).  So an issue that moved, or a second import of the same module in
the same file, is not new.

F1  (exhaustive) ``issue_fingerprint`` is injective on that key, over every
    path and member id of up to 3 characters from an alphabet that holds the
    separator ``:`` and the encoding's ``%``, ``2``, ``5``, ``3``, ``A``.  A
    POSIX path may hold ``:``, so without ``target_part`` member ``a`` with
    ``b.py`` and the root-level ``a:b.py`` share ``CHK001:a:b.py`` (#1108).
    Z3's string theory answers ``unknown`` for these concatenations, hence
    the enumeration.

The rest is Z3 over a baseline written from one run (``frozen``, 2 issues)
and a later run (``current``, 2 issues).  ``write_baseline`` stores each
frozen fingerprint; ``apply_baseline`` drops the current issues whose
fingerprint is stored and recomputes the exit status.  A fingerprint is a
function of the key alone (no line), equal for two issues only when their
keys are, which is F1.

E1  After ``apply_baseline`` the exit status is the status ``emit.rs`` computes
    over the current issues that are *new*: whose key matches no frozen
    issue.

E2  Applying a baseline never turns a passing run into a failing one.

E3  An issue that differs from a frozen one only in its line is suppressed.

E4  Applying a baseline to the run it was written from leaves exit 0.

Run:  python3 docs/dev/formal/exit_status_z3.py
Exit status 1 when a counterexample exists.
"""

from __future__ import annotations

import itertools
import sys

from z3 import (
    And,
    Bool,
    BoolSort,
    Function,
    If,
    Implies,
    Int,
    IntSort,
    Not,
    Or,
    Solver,
    sat,
)

INFO, WARNING, ERROR = 0, 1, 2
MAYBE, LIKELY, CERTAIN = 0, 1, 2
FILE, DISTRIBUTION, IMPORT, SYMBOL = 1, 2, 3, 6  # rule number == subject kind


# ---- F1: the fingerprint string -------------------------------------------

def target_part(raw: str) -> str:
    """types.rs::target_part"""
    if "%" in raw or ":" in raw:
        return raw.replace("%", "%25").replace(":", "%3A")
    return raw


def fingerprint(rule: int, member: str | None, path: str, name: str) -> str:
    """types.rs::issue_fingerprint over issue_stable_target (normalized path)."""
    if rule == FILE:
        target = target_part(path)
    elif rule == DISTRIBUTION:
        target = name
    else:
        target = f"{target_part(path)}:{name}"
    if member is not None:
        target = f"{target_part(member)}:{target}"
    return f"CHK00{rule}:{target}"


def check_injective() -> tuple[tuple, tuple] | None:
    alphabet = "a:%253A"
    parts = [
        "".join(chars)
        for length in range(4)
        for chars in itertools.product(alphabet, repeat=length)
    ]
    # Dotted modules, symbols and distribution names never hold ':'.
    names = ["x", "x.y"]
    seen: dict[str, tuple] = {}
    for rule in (FILE, DISTRIBUTION, IMPORT, SYMBOL):
        for member in [None, *parts]:
            for path in [""] if rule == DISTRIBUTION else parts:
                for name in [""] if rule == FILE else names:
                    key = (rule, member, path, name)
                    other = seen.setdefault(fingerprint(*key), key)
                    if other != key:
                        return other, key
    return None


# ---- E1-E4: the exit status ------------------------------------------------

def counts_toward_exit(severity, confidence, strict):
    min_sev = If(strict, WARNING, ERROR)
    min_conf = If(strict, MAYBE, LIKELY)
    return And(severity >= min_sev, confidence >= min_conf)


class Issue:
    def __init__(self, tag):
        self.tag = tag
        self.key = Int(f"{tag}_key")  # (rule, member, subject), line left out
        self.line = Int(f"{tag}_line")
        self.sev = Int(f"{tag}_severity")
        self.conf = Int(f"{tag}_confidence")

    def domain(self):
        return And(
            self.sev >= INFO, self.sev <= ERROR,
            self.conf >= MAYBE, self.conf <= CERTAIN,
        )

    def names(self):
        return [self.key, self.line, self.sev, self.conf]


def main() -> int:
    failed = 0
    collision = check_injective()
    if collision is None:
        print("OK       F1 the fingerprint is injective on (rule, member, subject)")
    else:
        failed += 1
        print("VIOLATED F1 the fingerprint is injective on (rule, member, subject)")
        print(f"  {collision[0]} and {collision[1]} -> {fingerprint(*collision[0])}")

    strict = Bool("strict")
    no_exit_code = Bool("no_exit_code")
    frozen = [Issue("frozen0"), Issue("frozen1")]
    current = [Issue("current0"), Issue("current1")]
    issues = frozen + current
    # issue_fingerprint reads no line; F1 makes it injective on the key.
    fp = Function("fingerprint", IntSort(), IntSort())
    stored = Function("stored", IntSort(), BoolSort())  # the baseline's fingerprint set
    domain = And(
        *[i.domain() for i in issues],
        *[(fp(a.key) == fp(b.key)) == (a.key == b.key) for a, b in itertools.combinations(issues, 2)],
        # write_baseline stores exactly the frozen fingerprints.
        *[stored(fp(i.key)) == Or(*[fp(i.key) == fp(f.key) for f in frozen]) for i in issues],
    )

    def exit_issues_found(run, kept):
        """``emit.rs::compute_exit_status`` over the issues selected by ``kept``."""
        return And(
            Not(no_exit_code),
            Or(*[And(kept(i), counts_toward_exit(i.sev, i.conf, strict)) for i in run]),
        )

    def suppressed(issue):
        return stored(fp(issue.key))

    def is_new(issue):
        return Not(Or(*[issue.key == f.key for f in frozen]))

    emit_issues_found = exit_issues_found(current, lambda i: True)
    baseline_issues_found = exit_issues_found(current, lambda i: Not(suppressed(i)))
    a, b = current[0], frozen[0]

    props = {
        "E1 baseline exit status == emit.rs over the new issues": (
            baseline_issues_found == exit_issues_found(current, is_new)
        ),
        "E2 a baseline never turns a passing run into a failing one": Implies(
            baseline_issues_found, emit_issues_found
        ),
        "E3 an issue that moved to another line stays suppressed": Implies(
            And(a.key == b.key, a.line != b.line), suppressed(a)
        ),
        "E4 a baseline applied to its own run leaves exit 0": Not(
            exit_issues_found(frozen, lambda i: Not(suppressed(i)))
        ),
    }

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
        for i in issues:
            print(f"  {i.tag}: " + " ".join(f"{n}={m.eval(n, True)}" for n in i.names()))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
