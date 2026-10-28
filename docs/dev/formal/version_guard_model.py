#!/usr/bin/env python3
"""Finite model of ``sys.version_info`` branch tests vs. CPython (#997, #1272).

Port of ``src/parser/version_guard.rs`` (``VersionTest::read`` / ``holds``,
``branch_minors``, ``version_names``) and the three-valued reading of
``src/parser/condition.rs`` (``can`` / ``fixed`` / ``excluded``) it runs on.
The reference is CPython itself: each generated test is evaluated with
``eval`` against a ``sys`` whose ``version_info`` is ``(3, m, 0, "final", 0)``
(a tuple with ``.major``), with every opaque operand (``flag``) taking both
values. The strings are this script's own; no analyzed code is run.

The universe: every read form (``sys.version_info``, ``[:2]``, ``[0:2]``,
``[0]``, ``.major``) and forms left opaque (``[1]``, ``.minor``, ``[:1]``,
``[:3]``, three-part tuples, ``==`` on the full tuple) × six operators × the
version on either side × literals around the boundaries (``(2,)``, ``(3,)``,
``(3, 8)`` .. ``(3, 10)``, ``(3, 200)``, ``(4,)``, majors 0..4), then pairs of
read tests under ``not`` / ``and`` / ``or`` with an opaque ``flag``, two-test
``if`` / ``elif`` / ``else`` chains, and module-level names bound to tests and
to ``not`` / ``or`` of earlier names (six's ``PY2`` / ``PY3``). Minors 0..30
and 127 (the last bit, which stands for every later minor) and 200.

V1  A test ``read`` accepts holds on 3.m exactly when CPython says so, and
    every form outside the documented ones stays opaque.
V2  Sound: a branch runs on 3.m for some value of the opaque operands only
    if ``branch_minors`` keeps m. A branch it leaves no minor runs only on
    Python 2 (#1272).
V3  Exact without opaque operands: with every leaf read (or a name bound to
    read leaves), ``branch_minors`` keeps exactly the minors the branch runs
    on.

``--broken`` runs variants that must each break a property: the full tuple
read like ``[:2]`` (``(3, 9, 0) > (3, 9)`` is true), the major compared with
the minor, and an opaque leaf that may take whichever value excludes a branch.

``--trace BIN`` also runs a chokkin binary on one generated project per minor
(``requires-python = "==3.m.*"``) and checks each branch's unresolved import
is reported at info exactly where the model says the branch never runs
(§6 trace-checking; not part of ``make formal``).

Scoping is outside the model: ``version_names`` counts only module-level
bindings, so a ``global`` rebinding in a function or a ``*`` import still
reads the name as its test (see ``docs/dev/formal/ledger-v0.7.12.ja.md``).

Run:  python3 docs/dev/formal/version_guard_model.py [--broken] [--trace BIN]
Exit status 1 when a property is violated (counterexamples are printed).
"""

from __future__ import annotations

import ast
import itertools
import json
import os
import random
import subprocess
import sys
import tempfile

LAST_MINOR = 127
MINORS = list(range(31)) + [LAST_MINOR]
ALL = frozenset(range(LAST_MINOR + 1))

# Variants for --broken.
BROKEN: set[str] = set()


# ─── Port ────────────────────────────────────────────────────────────────────


def is_version_info(node: ast.expr) -> bool:
    return (
        isinstance(node, ast.Attribute)
        and node.attr == "version_info"
        and isinstance(node.value, ast.Name)
        and node.value.id == "sys"
    )


def small_int(node: ast.expr | None) -> int | None:
    """A non-negative integer literal below ``LAST_MINOR`` (``bool`` is not)."""
    if (
        isinstance(node, ast.Constant)
        and type(node.value) is int
        and 0 <= node.value < LAST_MINOR
    ):
        return node.value
    return None


def is_major_value(node: ast.expr) -> bool:
    if isinstance(node, ast.Attribute):
        return node.attr == "major" and is_version_info(node.value)
    if isinstance(node, ast.Subscript):
        return small_int(node.slice) == 0 and is_version_info(node.value)
    return False


def version_value(node: ast.expr) -> bool | None:
    """True for ``sys.version_info``, False for ``[:2]`` / ``[0:2]``."""
    if is_version_info(node):
        return True
    if not (isinstance(node, ast.Subscript) and isinstance(node.slice, ast.Slice)):
        return None
    s = node.slice
    lower_is_zero = s.lower is None or small_int(s.lower) == 0
    if (
        lower_is_zero
        and s.step is None
        and small_int(s.upper) == 2
        and is_version_info(node.value)
    ):
        return False
    return None


SWAP = {
    ast.Lt: ast.Gt,
    ast.LtE: ast.GtE,
    ast.Gt: ast.Lt,
    ast.GtE: ast.LtE,
    ast.Eq: ast.Eq,
    ast.NotEq: ast.NotEq,
}


def swapped(op: type) -> type | None:
    return SWAP.get(op)


def version_tuple(node: ast.expr):
    if not isinstance(node, ast.Tuple):
        return None
    elts = node.elts
    if len(elts) == 1:
        major = small_int(elts[0])
        return None if major is None else (major, None)
    if len(elts) == 2:
        major, minor = small_int(elts[0]), small_int(elts[1])
        return None if major is None or minor is None else (major, minor)
    return None


class VersionTest:
    def __init__(self, op, major, minor, full, major_only):
        self.op, self.major, self.minor, self.full, self.major_only = (
            op,
            major,
            minor,
            full,
            major_only,
        )

    @staticmethod
    def read(node: ast.expr) -> VersionTest | None:
        if not isinstance(node, ast.Compare) or len(node.ops) != 1:
            return None
        op, left, right = type(node.ops[0]), node.left, node.comparators[0]
        major = VersionTest.read_major(op, left, right)
        if major is not None:
            return major
        full = version_value(left)
        if full is not None:
            tup = right
        else:
            full = version_value(right)
            if full is None:
                return None
            tup, op = left, swapped(op)
            if op is None:
                return None
        parsed = version_tuple(tup)
        if parsed is None:
            return None
        if op in (ast.Lt, ast.LtE, ast.Gt, ast.GtE):
            ok = True
        elif op in (ast.Eq, ast.NotEq):
            ok = not full
        else:
            ok = False
        if "full-as-slice" in BROKEN:
            full = False
        return VersionTest(op, parsed[0], parsed[1], full, False) if ok else None

    @staticmethod
    def read_major(op, left, right):
        if is_major_value(left):
            major = small_int(right)
        elif is_major_value(right):
            major, op = small_int(left), swapped(op)
        else:
            return None
        if major is None or op is None or swapped(op) is None:
            return None
        return VersionTest(op, major, None, False, True)

    def holds(self, minor: int) -> bool:
        def cmp(a, b):
            return (a > b) - (a < b)

        ordering = cmp(3, self.major)
        if ordering == 0:
            if self.major_only and "major-reads-minor" not in BROKEN:
                ordering = 0
            elif self.minor is not None:
                ordering = cmp(minor, self.minor)
            else:
                ordering = 1
        if ordering == 0:
            ordering = 1 if self.full else 0
        return {
            ast.Lt: ordering < 0,
            ast.LtE: ordering <= 0,
            ast.Gt: ordering > 0,
            ast.GtE: ordering >= 0,
            ast.Eq: ordering == 0,
        }.get(self.op, ordering != 0)


OPAQUE, FREE = "opaque", "free"


def can(node: ast.expr, value: bool, leaf) -> bool:
    """``condition::can``: the known leaves can make ``node`` ``value``."""
    if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.Not):
        return can(node.operand, not value, leaf)
    if isinstance(node, ast.BoolOp):
        if isinstance(node.op, ast.And) == value:
            return all(can(v, value, leaf) for v in node.values)
        return any(can(v, value, leaf) for v in node.values)
    got = leaf(node)
    if got == OPAQUE:
        return "opaque-is-free" in BROKEN
    if got == FREE:
        return True
    return got == value


def fixed(node: ast.expr, leaf) -> bool | None:
    t, f = can(node, True, leaf), can(node, False, leaf)
    return True if (t and not f) else False if (f and not t) else None


def excluded(earlier, test, leaf) -> bool:
    return any(can(t, True, leaf) for t in earlier) or (
        test is not None and can(test, False, leaf)
    )


def leaves(nodes):
    stack, out = list(nodes), []
    while stack:
        node = stack.pop()
        if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.Not):
            stack.append(node.operand)
        elif isinstance(node, ast.BoolOp):
            stack.extend(node.values)
        else:
            out.append(node)
    return out


def version_leaf(node, minor, names):
    if isinstance(node, ast.Name):
        if node.id in names:
            true_on, false_on = names[node.id]
            if minor in true_on:
                return True
            if minor in false_on:
                return False
        return OPAQUE
    read = VersionTest.read(node)
    return OPAQUE if read is None else read.holds(minor)


def reads_version(nodes, names) -> bool:
    return any(
        (isinstance(leaf, ast.Name) and leaf.id in names)
        or (not isinstance(leaf, ast.Name) and VersionTest.read(leaf) is not None)
        for leaf in leaves(nodes)
    )


def branch_minors(earlier, test, names) -> frozenset[int]:
    tests = list(earlier) + ([test] if test is not None else [])
    if not reads_version(tests, names):
        return ALL
    return frozenset(
        m
        for m in range(LAST_MINOR + 1)
        if not excluded(earlier, test, lambda n, m=m: version_leaf(n, m, names))
    )


def version_names(defs) -> dict:
    """``version_names`` over plain module-level ``name = <test>`` bound once."""
    names: dict = {}
    for name, node in defs:
        if not reads_version([node], names):
            continue
        true_on, false_on = set(), set()
        for m in range(LAST_MINOR + 1):
            v = fixed(node, lambda n, m=m: version_leaf(n, m, names))
            if v is True:
                true_on.add(m)
            elif v is False:
                false_on.add(m)
        names[name] = (frozenset(true_on), frozenset(false_on))
    return names


# ─── Reference (CPython) ─────────────────────────────────────────────────────


class VersionInfo(tuple):
    @property
    def major(self):
        return self[0]

    @property
    def minor(self):
        return self[1]


class FakeSys:
    def __init__(self, minor: int):
        self.version_info = VersionInfo((3, minor, 0, "final", 0))


def runs(chain, branch, defs, minor, flag) -> bool:
    env = {"sys": FakeSys(minor), "flag": flag, "minor": 0}
    for name, text in defs:
        env[name] = eval(text, env)
    for position, text in enumerate(chain):
        if eval(text, env):
            return position == branch
    return branch == len(chain)


def can_run(chain, branch, defs, minor) -> bool:
    return any(runs(chain, branch, defs, minor, flag) for flag in (True, False))


# ─── Universe ────────────────────────────────────────────────────────────────

TUPLE_FORMS = ["sys.version_info", "sys.version_info[:2]", "sys.version_info[0:2]"]
OPAQUE_TUPLE_FORMS = ["sys.version_info[:1]", "sys.version_info[:3]"]
MAJOR_FORMS = ["sys.version_info[0]", "sys.version_info.major"]
OPAQUE_INT_FORMS = ["sys.version_info[1]", "sys.version_info.minor"]
TUPLES = [
    "(2,)",
    "(3,)",
    "(4,)",
    "(2, 7)",
    "(3, 0)",
    "(3, 8)",
    "(3, 9)",
    "(3, 10)",
    "(3, 200)",
    "(4, 0)",
]
OPAQUE_TUPLES = ["(3, 9, 1)", "(3, minor)"]
INTS = ["0", "2", "3", "4", "True"]
OPS = ["<", "<=", ">", ">=", "==", "!="]


def atoms():
    """(text, documented): every comparison, and whether the docs say it is read."""
    groups = [
        (TUPLE_FORMS, TUPLES, True),
        (TUPLE_FORMS, OPAQUE_TUPLES, False),
        (OPAQUE_TUPLE_FORMS, TUPLES, False),
        (MAJOR_FORMS, INTS, True),
        (OPAQUE_INT_FORMS, INTS, False),
    ]
    for forms, literals, documented in groups:
        for form, literal, op in itertools.product(forms, literals, OPS):
            read = documented
            # `==` / `!=` on the whole tuple reads like a typo for `[:2]`;
            # `(3, 200)` is past the last minor bit; `True` is not an int.
            if form == "sys.version_info" and op in ("==", "!="):
                read = False
            if literal in ("(3, 200)", "True"):
                read = False
            yield f"{form} {op} {literal}", read
            yield f"{literal} {op} {form}", read


def expr(text: str) -> ast.expr:
    return ast.parse(text, mode="eval").body


def cases():
    """(chain, defs, has_opaque): chains of tests and names bound before them."""
    read = [text for text, documented in atoms() if documented]
    rnd = random.Random(1272)
    out = [([text], [], not documented) for text, documented in atoms()]
    shapes = [
        ("not ({a})", False),
        ("({a}) and flag", True),
        ("({a}) or flag", True),
        ("({a}) and ({b})", False),
        ("not (({a}) or ({b}))", False),
        ("({a}) or not ({b})", False),
    ]
    for _ in range(500):
        a, b = rnd.sample(read, 2)
        shape, opaque = rnd.choice(shapes)
        out.append(([shape.format(a=a, b=b)], [], opaque))
        out.append(([a, b], [], False))
    for i in range(120):
        a, b = rnd.sample(read, 2)
        defs = [(f"N{i}", a), (f"M{i}", f"not N{i}"), (f"K{i}", f"N{i} or ({b})")]
        for test, opaque in [
            (f"N{i}", False),
            (f"M{i}", False),
            (f"not M{i}", False),
            (f"K{i}", False),
            (f"N{i} and flag", True),
        ]:
            out.append(([test], defs, opaque))
    return out


# ─── Properties ──────────────────────────────────────────────────────────────


def check_v1() -> list[str]:
    bad = []
    for text, documented in atoms():
        read = VersionTest.read(expr(text))
        if (read is not None) != documented:
            bad.append(f"V1 {text!r}: read={read is not None}, documented={documented}")
            continue
        if read is None:
            continue
        for m in MINORS + [200]:
            want = bool(eval(text, {"sys": FakeSys(m), "minor": 0}))
            if read.holds(min(m, LAST_MINOR)) != want:
                bad.append(f"V1 {text!r} on 3.{m}: model {not want}, CPython {want}")
    return bad


def check_v2_v3() -> list[str]:
    bad = []
    for chain, defs, opaque in cases():
        names = version_names([(n, expr(t)) for n, t in defs])
        nodes = [expr(t) for t in chain]
        for branch in range(len(chain) + 1):
            kept = branch_minors(
                nodes[:branch], nodes[branch] if branch < len(chain) else None, names
            )
            for m in MINORS:
                real = can_run(chain, branch, defs, m)
                if real and m not in kept:
                    bad.append(
                        f"V2 {chain} {defs} branch {branch} runs on 3.{m} but is dropped"
                    )
                if not opaque and not real and m in kept:
                    bad.append(
                        f"V3 {chain} {defs} branch {branch} never runs on 3.{m} but is kept"
                    )
    return bad


def check_all() -> list[str]:
    return check_v1() + check_v2_v3()


# ─── Trace-check against a chokkin binary (§6) ───────────────────────────────


def trace(binary: str) -> list[str]:
    all_cases = [(c, d) for c, d, _ in cases()]
    lines = ["import sys", "flag = __import__('os').environ.get('X')"]
    bound = set()
    for _, defs in all_cases:
        for name, text in defs:
            if name not in bound:
                bound.add(name)
                lines.append(f"{name} = {text}")
    sites = []
    for i, (chain, _) in enumerate(all_cases):
        for branch in range(len(chain) + 1):
            lines.append(
                ("if" if branch == 0 else "elif") + f" {chain[branch]}:"
                if branch < len(chain)
                else "else:"
            )
            lines.append(f"    import zz_{i}_{branch}")
            sites.append((i, branch))
    model = {}
    for i, branch in sites:
        chain, defs = all_cases[i]
        names = version_names([(n, expr(t)) for n, t in defs])
        nodes = [expr(t) for t in chain]
        model[(i, branch)] = branch_minors(
            nodes[:branch], nodes[branch] if branch < len(chain) else None, names
        )
    bad = []
    with tempfile.TemporaryDirectory() as tmp:
        with open(os.path.join(tmp, "main.py"), "w") as f:
            f.write("\n".join(lines) + "\n")
        for m in range(21):
            with open(os.path.join(tmp, "pyproject.toml"), "w") as f:
                f.write(
                    f'[project]\nname = "probe"\nversion = "0"\nrequires-python = "==3.{m}.*"\n'
                )
            out = subprocess.run(
                [binary, "--no-cache", "--no-exit-code", "--reporter", "json", tmp],
                capture_output=True,
                text=True,
                check=True,
            )
            severity = {
                issue["target"].split(":", 1)[1]: issue["severity"]
                for issue in json.loads(out.stdout)["issues"]
                if issue.get("target", "").startswith("main.py:zz_")
            }
            for i, branch in sites:
                chain, defs = all_cases[i]
                kept = model[(i, branch)]
                want_info = m not in kept or not kept
                got = severity.get(f"zz_{i}_{branch}")
                if got is None or (got == "info") != want_info:
                    bad.append(
                        f"trace 3.{m} {chain} {defs} branch {branch}: binary {got}, model {'info' if want_info else 'warning'}"
                    )
    return bad


def main() -> int:
    if "--broken" in sys.argv:
        status = 0
        for variant in ("full-as-slice", "major-reads-minor", "opaque-is-free"):
            BROKEN.clear()
            BROKEN.add(variant)
            caught = check_all()
            print(
                f"broken variant {variant}: {'caught (' + str(len(caught)) + ')' if caught else 'NOT caught'}"
            )
            status |= not caught
        BROKEN.clear()
        return status
    bad = check_all()
    if "--trace" in sys.argv:
        bad += trace(sys.argv[sys.argv.index("--trace") + 1])
    for line in bad[:50]:
        print(line)
    if bad:
        print(f"version_guard_model: {len(bad)} counterexamples")
        return 1
    print(
        f"version_guard_model: V1 V2 V3 hold ({len(cases())} chains, minors {MINORS[0]}..{MINORS[-2]} and {LAST_MINOR})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
