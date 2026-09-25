#!/usr/bin/env python3
"""Z3 model of the §10 dependency-context decision table (CHK003/CHK004/CHK005).

Ported from:

- ``src/rules/deps/context.rs``   (``declaration_matches_usage``, ``declaration_bucket``)
- ``src/rules/deps/missing.rs``   (``detect_missing_dependencies``, ``is_transitive_only``)
- ``src/rules/deps/misplaced.rs`` (``detect_misplaced_dependencies``)

One reachable third-party import of a single distribution ``D`` is modelled
symbolically.  The declaration side is the set of buckets ``D`` is declared in
(runtime / dev / type / optional), the usage side is the file/import context,
plus the environment flags (lockfile present, ``D`` reachable in the lockfile
closure of *other* declared deps, optional try-import, ``--strict``).

Properties (spec.ja.md §10, lines 498-524):

S1  CHK004 ("only available as a transitive dependency") is emitted only when
    ``D`` is not directly declared in *any* context.  The message and the spec
    (``src/ で import urllib3 / requests のtransitive dependencyとして入っているだけ``)
    both require that ``D`` is absent from the manifest.

S2  ``src/ で import pytest`` with pytest only in ``dependency-groups.dev`` yields
    CHK005 and nothing else (spec line 511-513).  A dev/type-only declaration
    used at runtime must not additionally be reported as *missing* (CHK003) or
    *transitive* (CHK004).

Run:  python3 docs/dev/formal/deps_rules_z3.py
Exit status 1 when Z3 finds a counterexample for any property.
"""

from __future__ import annotations

import sys

from z3 import And, Bool, Implies, Not, Or, Solver, sat


def build():
    # usage context (one-hot over UsageContext)
    usage_runtime = Bool("usage_runtime")
    usage_type = Bool("usage_type")
    usage_dev_like = Bool("usage_dev_like")  # Test | Docs | Dev

    # declaration buckets D is declared in (DeclarationBucket)
    decl_runtime = Bool("decl_runtime")
    decl_dev = Bool("decl_dev")
    decl_type = Bool("decl_type")
    decl_optional = Bool("decl_optional")
    declared_any = Or(decl_runtime, decl_dev, decl_type, decl_optional)

    has_lockfile = Bool("has_lockfile")
    # D is in the lockfile transitive closure of some *other* declared dep
    in_closure_of_others = Bool("in_closure_of_others")
    optional_import = Bool("optional_import")
    strict = Bool("strict")

    # ---- context.rs::declaration_matches_usage ----------------------------
    root_declared = Or(
        And(usage_runtime, Or(decl_runtime, decl_optional)),
        And(usage_type, Or(decl_type, decl_runtime, decl_optional)),
        And(usage_dev_like, Or(decl_dev, decl_runtime, decl_optional)),
    )

    # ---- missing.rs::is_transitive_only -------------------------------------
    # The queue is seeded with the declared dependency names *other than* D, so
    # a directly declared D is only "transitive" when some other declared dep
    # pulls it in.
    is_transitive_only = in_closure_of_others

    # ---- missing.rs::detect_missing_dependencies (no workspace member) ----
    skip_declared = root_declared  # member_declared=false; (!strict && root) || (root && no member)
    skip_non_runtime = And(Not(strict), Or(usage_type, usage_dev_like))
    # D declared in any bucket, just not one matching the usage context: §10
    # leaves that to CHK005 (misplaced).
    skip_context_mismatch = declared_any
    proceeds = And(Not(skip_declared), Not(skip_non_runtime), Not(skip_context_mismatch))
    emits_optional_chk003 = And(proceeds, optional_import)
    emits_chk004 = And(proceeds, Not(optional_import), has_lockfile, is_transitive_only)
    emits_chk003 = And(
        proceeds, Not(optional_import), Not(And(has_lockfile, is_transitive_only))
    )

    # ---- misplaced.rs::detect_misplaced_dependencies -----------------------
    emits_chk005 = And(
        usage_runtime,
        declared_any,
        Not(Or(decl_runtime, decl_optional)),
        Or(decl_dev, decl_type),
    )

    well_formed = And(
        Or(usage_runtime, usage_type, usage_dev_like),
        Not(And(usage_runtime, usage_type)),
        Not(And(usage_runtime, usage_dev_like)),
        Not(And(usage_type, usage_dev_like)),
    )

    props = {
        "S1 CHK004 only when D is undeclared": Implies(emits_chk004, Not(declared_any)),
        "S2 dev/type-only D used at runtime -> CHK005 only": Implies(
            emits_chk005, Not(Or(emits_chk003, emits_chk004, emits_optional_chk003))
        ),
    }
    names = [
        usage_runtime, usage_type, usage_dev_like,
        decl_runtime, decl_dev, decl_type, decl_optional,
        has_lockfile, in_closure_of_others, optional_import, strict,
    ]
    derived = {
        "root_declared": root_declared,
        "is_transitive_only": is_transitive_only,
        "emits_chk003": emits_chk003,
        "emits_optional_chk003": emits_optional_chk003,
        "emits_chk004": emits_chk004,
        "emits_chk005": emits_chk005,
    }
    return well_formed, props, names, derived


def main() -> int:
    well_formed, props, names, derived = build()
    failed = 0
    for label, prop in props.items():
        solver = Solver()
        solver.add(well_formed, Not(prop))
        if solver.check() == sat:
            failed += 1
            model = solver.model()
            print(f"VIOLATED {label}")
            print("  inputs : " + ", ".join(f"{n}={model.eval(n, True)}" for n in names))
            print(
                "  derived: "
                + ", ".join(f"{k}={model.eval(v, True)}" for k, v in derived.items())
            )
        else:
            print(f"OK       {label}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
