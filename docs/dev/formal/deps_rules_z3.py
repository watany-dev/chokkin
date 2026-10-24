#!/usr/bin/env python3
"""Z3 model of the §10 dependency-context decision table (CHK003/CHK004/CHK005),
workspace members, lockfile scopes and ``--strict`` included.

Ported from:

- ``src/rules/deps/context.rs``   (``declaration_matches_usage``, ``is_directly_declared``)
- ``src/rules/deps/missing.rs``   (``detect_missing_dependencies``,
  ``ImportDeclarations``, ``undeclared_candidate``, ``lock_evidence``)
- ``src/rules/deps/misplaced.rs`` (``detect_misplaced_dependencies`` up to the
  bucket check; the CHK005 confidence is ``misplaced.rs`` ``mod verification``)

One reachable third-party import of a single distribution ``D`` is modelled
symbolically.  The declaration side is the set of buckets ``D`` is declared in
(runtime / dev / type / optional) at the root and, for an import inside a
workspace member, in the member.  The usage side is the file/import context.
The environment is ``--strict``, whether the import is optional (``try`` or a
platform guard), and what each lockfile says about ``D``.

Lockfiles (``lock_evidence``): a member import is checked against the member's
own lockfile (#653) and the root lockfile, both walked from the *member's*
declarations (#649); a root import against the root lockfile from the root's
declarations.  Per lockfile ``edge`` means a lock edge from a declaration other
than ``D`` reaches ``D`` (``LockEvidence::TransitiveEdge``), ``listed`` that
the lockfile has an entry for ``D`` (``LockEvidence::LockedOnly`` when no edge
reaches it).  The root lockfile walked from the root's own declarations is a
separate input, so the scope rule is checked rather than assumed.

Tie to the implementation: the exhaustive test ``missing.rs`` ``decisions_follow_the_formal_model`` builds every
combination of these inputs as real declarations and lockfiles, runs
``detect_missing_dependencies`` and ``detect_misplaced_dependencies``, and
compares rule, severity and confidence with ``decide`` below transcribed to
Rust.  Change both together.

Properties (spec.ja.md §10):

S1  CHK004 ("only available as a transitive dependency") is emitted only when
    ``D`` is absent from the governing declarations (root, and under
    ``--strict`` the member's own entry).

S2  A dev/type-only declaration used at runtime is CHK005 and nothing else:
    never also missing (CHK003) or transitive (CHK004).

W1  CHK003 (plain, optional-import or workspace), CHK004 and CHK005 are
    mutually exclusive for one import.

W2  ``--strict``, runtime import in a member that does not mention ``D``, root
    declares ``D`` only as dev/type: the root is the fallback, so CHK005 alone.

W3  ``--strict``, runtime import in a member that declares ``D`` as runtime or
    optional: nothing fires, whatever the root declares.

W4  A member that declares ``D`` for the usage context is satisfied, with or
    without ``--strict``, whatever the root declares (``member_satisfies``).

L1  A lock edge proves a transitive dependency: an undeclared import with an
    edge in scope is CHK004 / certain even under ``try`` or a platform guard
    (#504), and then a warning instead of an error (#582).

L2  A lockfile entry without an edge (stale lock, ``pylock.toml``) is weaker:
    CHK004 / likely for a plain import, the conditional CHK003 for an optional
    one.

L3  A member import's CHK004 / certain needs an edge from the member's own
    declarations; a package only the root's declarations reach is not
    transitive for the member (#649).

L4  Without any lockfile in scope there is no CHK004.

Run:  python3 docs/dev/formal/deps_rules_z3.py
Exit status 1 when Z3 finds a counterexample for any property.
"""

from __future__ import annotations

import sys

from z3 import And, Bool, If, Implies, Not, Or, Solver, sat

INFO, WARNING, ERROR = 0, 1, 2
MAYBE, LIKELY, CERTAIN = 0, 1, 2
BUCKETS = ("runtime", "dev", "type", "optional")


def buckets(prefix):
    """Declaration buckets D is declared in (DeclarationBucket)."""
    return {kind: Bool(f"{prefix}_{kind}") for kind in BUCKETS}


def matches_usage(decl, usage):
    """context.rs::is_directly_declared over a set of buckets."""
    return Or(
        And(usage["runtime"], Or(decl["runtime"], decl["optional"])),
        And(usage["type"], Or(decl["type"], decl["runtime"], decl["optional"])),
        And(usage["dev_like"], Or(decl["dev"], decl["runtime"], decl["optional"])),
    )


def any_bucket(decl):
    return Or(*decl.values())


def dev_or_type_only(decl):
    return And(Or(decl["dev"], decl["type"]), Not(decl["runtime"]), Not(decl["optional"]))


def decide(i):
    """The findings for one import, as ``{name: (fires, severity, confidence)}``.

    ``i`` maps every input name to a Z3 Bool; see ``inputs``.
    """
    usage = {"runtime": i["usage_runtime"], "type": i["usage_type"], "dev_like": i["usage_dev_like"]}
    root, member = i["root"], i["member"]
    in_member, strict, optional = i["in_member"], i["strict"], i["optional_import"]
    root_any, member_any = any_bucket(root), any_bucket(member)

    # ---- missing.rs::ImportDeclarations ------------------------------------
    member_satisfies = And(in_member, member_any, matches_usage(member, usage))
    member_governs = And(strict, in_member, member_any)

    def governing(kind):
        return If(member_governs, member[kind], root[kind])

    governing_some = Or(member_governs, root_any)

    # ---- missing.rs::detect_missing_dependencies ---------------------------
    reaches_checks = And(Not(member_satisfies), Or(strict, usage["runtime"]))
    emits_workspace_chk003 = And(
        reaches_checks, strict, in_member, Not(member_any), matches_usage(root, usage)
    )
    # Declared in any governing bucket, just not one matching the usage
    # context: §10 leaves that to CHK005 (misplaced).
    undeclared = And(reaches_checks, Not(emits_workspace_chk003), Not(governing_some))

    # ---- missing.rs::lock_evidence -----------------------------------------
    # Seeds are the member's declarations for a member import (#649).
    root_lock_edge = If(in_member, i["root_lock_edge_from_member"], i["root_lock_edge_from_root"])
    member_lock = And(in_member, i["member_lock"])
    edge = Or(
        And(member_lock, i["member_lock_edge"]),
        And(i["root_lock"], root_lock_edge),
    )
    listed = Or(
        And(member_lock, i["member_lock_listed"]),
        And(i["root_lock"], i["root_lock_listed"]),
    )

    # ---- missing.rs::undeclared_candidate ----------------------------------
    emits_optional_chk003 = And(undeclared, optional, Not(edge))
    emits_chk004 = And(undeclared, Or(edge, And(Not(optional), listed)))
    emits_chk003 = And(undeclared, Not(optional), Not(edge), Not(listed))

    # ---- misplaced.rs::detect_misplaced_dependencies -----------------------
    emits_chk005 = And(
        usage["runtime"],
        Not(member_satisfies),
        governing_some,
        Not(Or(governing("runtime"), governing("optional"))),
        Or(governing("dev"), governing("type")),
    )

    return {
        "CHK003": (emits_chk003, ERROR, CERTAIN),
        "CHK003 optional": (emits_optional_chk003, If(strict, WARNING, INFO), LIKELY),
        "CHK003 workspace": (emits_workspace_chk003, ERROR, CERTAIN),
        "CHK004": (emits_chk004, If(optional, WARNING, ERROR), If(edge, CERTAIN, LIKELY)),
        "CHK005": (emits_chk005, None, None),
    }


def inputs():
    """Every input of ``decide`` as a Z3 Bool."""
    names = {
        name: Bool(name)
        for name in (
            "usage_runtime", "usage_type", "usage_dev_like",
            "in_member", "strict", "optional_import",
            "root_lock", "root_lock_listed", "root_lock_edge_from_root", "root_lock_edge_from_member",
            "member_lock", "member_lock_listed", "member_lock_edge",
        )
    }
    names["root"] = buckets("root")
    names["member"] = buckets("member")
    return names


def well_formed(i):
    u = (i["usage_runtime"], i["usage_type"], i["usage_dev_like"])
    return And(
        Or(*u),
        *[Not(And(a, b)) for k, a in enumerate(u) for b in u[k + 1 :]],
        # Only a workspace member import has member declarations or a member lockfile.
        Implies(Not(i["in_member"]), And(Not(any_bucket(i["member"])), Not(i["member_lock"]))),
    )


def flat(i):
    return [v for k, v in i.items() if not isinstance(v, dict)] + [
        v for k in ("root", "member") for v in i[k].values()
    ]


def build():
    i = inputs()
    out = decide(i)
    fires = {name: f for name, (f, _, _) in out.items()}
    chk003 = Or(fires["CHK003"], fires["CHK003 optional"], fires["CHK003 workspace"])
    chk004, chk005 = fires["CHK004"], fires["CHK005"]
    _, chk004_sev, chk004_conf = out["CHK004"]
    usage = {"runtime": i["usage_runtime"], "type": i["usage_type"], "dev_like": i["usage_dev_like"]}
    root, member = i["root"], i["member"]
    nothing = Not(Or(chk003, chk004, chk005))
    governing_some = Or(And(i["strict"], i["in_member"], any_bucket(member)), any_bucket(root))
    member_lock = And(i["in_member"], i["member_lock"])
    edge_from_member = Or(
        And(member_lock, i["member_lock_edge"]),
        And(i["root_lock"], i["root_lock_edge_from_member"]),
    )
    edge_in_scope = Or(
        And(member_lock, i["member_lock_edge"]),
        And(i["root_lock"], If(i["in_member"], i["root_lock_edge_from_member"], i["root_lock_edge_from_root"])),
    )
    undeclared_import = Or(chk003, chk004)  # reached undeclared_candidate or the workspace CHK003
    fs = list(fires.values())

    props = {
        "S1 CHK004 only when D is undeclared in the governing manifest": Implies(
            chk004, Not(governing_some)
        ),
        "S2 dev/type-only D used at runtime -> CHK005 only": Implies(chk005, Not(Or(chk003, chk004))),
        "W1 CHK003/CHK004/CHK005 are mutually exclusive": And(
            *[Not(And(a, b)) for k, a in enumerate(fs) for b in fs[k + 1 :]]
        ),
        "W2 strict member without D falls back to root -> CHK005": Implies(
            And(i["strict"], i["in_member"], Not(any_bucket(member)), i["usage_runtime"], dev_or_type_only(root)),
            And(chk005, Not(Or(chk003, chk004))),
        ),
        "W3 strict member declares D at runtime -> nothing": Implies(
            And(i["strict"], i["in_member"], i["usage_runtime"], Or(member["runtime"], member["optional"])),
            nothing,
        ),
        "W4 member declaring D for the usage is satisfied": Implies(
            And(i["in_member"], matches_usage(member, usage)), nothing
        ),
        "L1 a lock edge keeps an undeclared optional import on CHK004 / certain": Implies(
            And(undeclared_import, Not(fires["CHK003 workspace"]), edge_in_scope),
            And(chk004, chk004_conf == CERTAIN, chk004_sev == If(i["optional_import"], WARNING, ERROR)),
        ),
        "L2 a lock entry without an edge is CHK004 / likely, or the optional CHK003": Implies(
            And(chk004, Not(edge_in_scope)),
            And(chk004_conf == LIKELY, Not(i["optional_import"])),
        ),
        "L3 a member's CHK004 / certain needs an edge from the member's declarations": Implies(
            And(i["in_member"], chk004, chk004_conf == CERTAIN), edge_from_member
        ),
        "L4 no lockfile in scope -> no CHK004": Implies(
            And(Not(i["root_lock"]), Not(member_lock)), Not(chk004)
        ),
    }
    return well_formed(i), props, i, out


def main() -> int:
    well_formed_, props, i, out = build()
    failed = 0
    for label, prop in props.items():
        solver = Solver()
        solver.add(well_formed_, Not(prop))
        if solver.check() == sat:
            failed += 1
            model = solver.model()
            print(f"VIOLATED {label}")
            print("  inputs : " + ", ".join(f"{n}={model.eval(n, True)}" for n in flat(i)))
            print("  fires  : " + ", ".join(f"{k}={model.eval(f, True)}" for k, (f, _, _) in out.items()))
        else:
            print(f"OK       {label}")
    # A finding no input can produce would make every property above about it
    # hold vacuously.
    for name, (fires, _, _) in out.items():
        solver = Solver()
        solver.add(well_formed_, fires)
        if solver.check() != sat:
            failed += 1
            print(f"VACUOUS  {name} never fires")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
