#!/usr/bin/env python3
"""Z3 model of the §10 dependency-context decision table (CHK003/CHK004/CHK005),
workspace members and ``--strict`` included.

Ported from:

- ``src/rules/deps/context.rs``   (``declaration_matches_usage``, ``declaration_bucket``)
- ``src/rules/deps/missing.rs``   (``detect_missing_dependencies``, ``is_transitive_only``)
- ``src/rules/deps/misplaced.rs`` (``detect_misplaced_dependencies``)

One reachable third-party import of a single distribution ``D`` is modelled
symbolically.  The declaration side is the set of buckets ``D`` is declared in
(runtime / dev / type / optional), the usage side is the file/import context,
plus the environment flags (lockfile present, ``D`` reachable in the lockfile
closure of *other* declared deps, optional try-import, ``--strict``).

Workspace members (``missing.rs``/``misplaced.rs`` with ``workspace_declared``):
the import may sit in a workspace member, which has its own declaration buckets.
``--strict`` lets the member's own entry govern and falls back to the root when
the member does not mention ``D`` (``governing_declarations``); a member that
omits a root-declared ``D`` is a strict-only CHK003.

Properties (spec.ja.md §10, lines 498-524):

S1  CHK004 ("only available as a transitive dependency") is emitted only when
    ``D`` is absent from the governing declarations (root, and under
    ``--strict`` the member's own entry).  The message and the spec
    (``src/ で import urllib3 / requests のtransitive dependencyとして入っているだけ``)
    both require that ``D`` is absent from the manifest.

S2  ``src/ で import pytest`` with pytest only in ``dependency-groups.dev`` yields
    CHK005 and nothing else (spec line 511-513).  A dev/type-only declaration
    used at runtime must not additionally be reported as *missing* (CHK003) or
    *transitive* (CHK004).

W1  CHK003 (plain, optional-import or workspace), CHK004 and CHK005 are
    mutually exclusive for one import.

W2  ``--strict``, runtime import in a member that does not mention ``D``, root
    declares ``D`` only as dev/type: the root is the fallback, so CHK005 alone.

W3  ``--strict``, runtime import in a member that declares ``D`` as runtime or
    optional: nothing fires, whatever the root declares.

Run:  python3 docs/dev/formal/deps_rules_z3.py
Exit status 1 when Z3 finds a counterexample for any property.
"""

from __future__ import annotations

import sys

from z3 import And, Bool, If, Implies, Not, Or, Solver, sat


def buckets(prefix):
    """Declaration buckets D is declared in (DeclarationBucket)."""
    return {
        kind: Bool(f"{prefix}_{kind}") for kind in ("runtime", "dev", "type", "optional")
    }


def matches_usage(decl, usage_runtime, usage_type, usage_dev_like):
    """context.rs::declaration_matches_usage over a set of buckets."""
    return Or(
        And(usage_runtime, Or(decl["runtime"], decl["optional"])),
        And(usage_type, Or(decl["type"], decl["runtime"], decl["optional"])),
        And(usage_dev_like, Or(decl["dev"], decl["runtime"], decl["optional"])),
    )


def any_bucket(decl):
    return Or(*decl.values())


def dev_or_type_only(decl):
    return And(Or(decl["dev"], decl["type"]), Not(decl["runtime"]), Not(decl["optional"]))


def build():
    # usage context (one-hot over UsageContext)
    usage_runtime = Bool("usage_runtime")
    usage_type = Bool("usage_type")
    usage_dev_like = Bool("usage_dev_like")  # Test | Docs | Dev

    root = buckets("root")
    member = buckets("member")
    in_member = Bool("in_member")  # import.workspace_member.is_some()
    root_any = any_bucket(root)
    member_any = any_bucket(member)  # member_entry.is_some()

    has_lockfile = Bool("has_lockfile")
    # D is in the lockfile transitive closure of some *other* declared dep
    in_closure_of_others = Bool("in_closure_of_others")
    optional_import = Bool("optional_import")
    strict = Bool("strict")

    usage = (usage_runtime, usage_type, usage_dev_like)
    root_declared = matches_usage(root, *usage)
    member_declared = And(in_member, matches_usage(member, *usage))

    # ---- missing.rs::governing_declarations --------------------------------
    member_governs = And(strict, in_member, member_any)

    def governing(kind):
        return If(member_governs, member[kind], root[kind])

    governing_some = Or(member_governs, root_any)

    # ---- missing.rs::is_transitive_only -------------------------------------
    # The queue is seeded with the declared dependency names *other than* D, so
    # a directly declared D is only "transitive" when some other declared dep
    # pulls it in.
    is_transitive_only = in_closure_of_others

    # ---- missing.rs::detect_missing_dependencies ---------------------------
    skip_declared = Or(
        member_declared,
        And(Not(strict), root_declared),
        And(root_declared, Not(in_member)),
    )
    skip_non_runtime = And(Not(strict), Not(usage_runtime))
    reaches_workspace = And(Not(skip_declared), Not(skip_non_runtime))
    emits_workspace_chk003 = And(
        reaches_workspace, strict, root_declared, Not(member_any), in_member
    )
    # D declared in a governing bucket, just not one matching the usage
    # context: §10 leaves that to CHK005 (misplaced).
    proceeds = And(reaches_workspace, Not(emits_workspace_chk003), Not(governing_some))
    emits_optional_chk003 = And(proceeds, optional_import)
    emits_chk004 = And(proceeds, Not(optional_import), has_lockfile, is_transitive_only)
    emits_chk003 = And(
        proceeds, Not(optional_import), Not(And(has_lockfile, is_transitive_only))
    )

    # ---- misplaced.rs::detect_misplaced_dependencies -----------------------
    emits_chk005 = And(
        usage_runtime,
        governing_some,
        Not(Or(governing("runtime"), governing("optional"))),
        Or(governing("dev"), governing("type")),
    )

    well_formed = And(
        Or(usage_runtime, usage_type, usage_dev_like),
        Not(And(usage_runtime, usage_type)),
        Not(And(usage_runtime, usage_dev_like)),
        Not(And(usage_type, usage_dev_like)),
        # Only a workspace member import has member declarations.
        Implies(Not(in_member), Not(member_any)),
    )

    findings = [
        emits_chk003, emits_optional_chk003, emits_workspace_chk003, emits_chk004, emits_chk005,
    ]
    at_most_one = And(
        *[Not(And(a, b)) for i, a in enumerate(findings) for b in findings[i + 1 :]]
    )
    any_chk003 = Or(emits_chk003, emits_optional_chk003, emits_workspace_chk003)

    props = {
        "S1 CHK004 only when D is undeclared in the governing manifest": Implies(
            emits_chk004, Not(governing_some)
        ),
        "S2 dev/type-only D used at runtime -> CHK005 only": Implies(
            emits_chk005, Not(Or(any_chk003, emits_chk004))
        ),
        "W1 CHK003/CHK004/CHK005 are mutually exclusive": at_most_one,
        "W2 strict member without D falls back to root -> CHK005": Implies(
            And(strict, in_member, Not(member_any), usage_runtime, dev_or_type_only(root)),
            And(emits_chk005, Not(Or(any_chk003, emits_chk004))),
        ),
        "W3 strict member declares D at runtime -> nothing": Implies(
            And(strict, in_member, usage_runtime, Or(member["runtime"], member["optional"])),
            Not(Or(any_chk003, emits_chk004, emits_chk005)),
        ),
    }
    names = [
        usage_runtime, usage_type, usage_dev_like,
        *root.values(), in_member, *member.values(),
        has_lockfile, in_closure_of_others, optional_import, strict,
    ]
    derived = {
        "root_declared": root_declared,
        "member_declared": member_declared,
        "member_governs": member_governs,
        "is_transitive_only": is_transitive_only,
        "emits_chk003": emits_chk003,
        "emits_optional_chk003": emits_optional_chk003,
        "emits_workspace_chk003": emits_workspace_chk003,
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
