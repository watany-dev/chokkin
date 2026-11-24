"""Convert Python source to the JSON IR read by the Lean binding spec.

Static only: the files are read with ``ast.parse`` and never imported or run.
Constructs the IR has no node for become ``["other", ...]`` with their
sub-expressions kept, so loads inside them still count; nothing is dropped.
"""

import ast
import json
import os
import sys


def e(n):
    if n is None:
        return None
    if isinstance(n, ast.Name):
        return ["name", n.id]
    if isinstance(n, ast.Attribute):
        return ["attr", e(n.value), n.attr]
    if isinstance(n, ast.Call):
        return [
            "call",
            e(n.func),
            [e(a) for a in n.args],
            [[k.arg, e(k.value)] for k in n.keywords],
        ]
    if isinstance(n, ast.Constant):
        if isinstance(n.value, str):
            return ["str", n.value]
        if isinstance(n.value, int) and not isinstance(n.value, bool):
            return ["int", n.value]
        return ["other", []]
    if isinstance(n, (ast.Tuple, ast.List)):
        return ["tuple", [e(x) for x in n.elts]]
    if isinstance(n, ast.NamedExpr):
        return ["walrus", n.target.id, e(n.value)]
    if isinstance(n, ast.Lambda):
        return ["lambda", params(n.args), e(n.body)]
    if isinstance(n, (ast.ListComp, ast.SetComp, ast.GeneratorExp)):
        return ["comp", gens(n.generators), [e(n.elt)]]
    if isinstance(n, ast.DictComp):
        return ["comp", gens(n.generators), [e(n.key), e(n.value)]]
    if isinstance(n, ast.Starred):
        return ["starred", e(n.value)]
    if isinstance(n, ast.Subscript):
        return ["subscript", e(n.value), e(n.slice)]
    if isinstance(n, ast.IfExp):
        return ["ifexp", e(n.test), e(n.body), e(n.orelse)]
    if isinstance(n, ast.BoolOp):
        return ["or", [e(x) for x in n.values]]
    kids = [e(c) for c in ast.iter_child_nodes(n) if isinstance(c, ast.expr)]
    for c in ast.iter_child_nodes(n):
        if isinstance(c, ast.comprehension):
            return ["comp", gens([c for c in ast.iter_child_nodes(n) if isinstance(c, ast.comprehension)]), kids]
    return ["other", kids]


def gens(gs):
    return [[e(g.target), e(g.iter), [e(i) for i in g.ifs]] for g in gs]


def params(a):
    pos = a.posonlyargs + a.args
    defaults = [None] * (len(pos) - len(a.defaults)) + list(a.defaults)
    out = [[p.arg, e(d), e(p.annotation)] for p, d in zip(pos, defaults, strict=False)]
    if a.vararg:
        out.append(["*" + a.vararg.arg, None, e(a.vararg.annotation)])
    for p, d in zip(a.kwonlyargs, a.kw_defaults, strict=False):
        out.append([p.arg, e(d), e(p.annotation)])
    if a.kwarg:
        out.append(["**" + a.kwarg.arg, None, e(a.kwarg.annotation)])
    return out


def tparams(n):
    return [t.name for t in getattr(n, "type_params", [])]


def pat(p):
    if isinstance(p, ast.MatchAs):
        return ["capture", p.name, pat(p.pattern) if p.pattern else None]
    if isinstance(p, ast.MatchValue):
        return ["value", e(p.value)]
    if isinstance(p, ast.MatchSequence):
        return ["seq", [pat(x) for x in p.patterns]]
    if isinstance(p, ast.MatchStar):
        return ["star", p.name]
    if isinstance(p, ast.MatchOr):
        return ["or", [pat(x) for x in p.patterns]]
    if isinstance(p, ast.MatchClass):
        return [
            "class",
            e(p.cls),
            [pat(x) for x in p.patterns] + [pat(x) for x in p.kwd_patterns],
        ]
    if isinstance(p, ast.MatchMapping):
        return ["mapping", [e(k) for k in p.keys], [pat(x) for x in p.patterns], p.rest]
    return ["other"]


def body(ss):
    return [s(x) for x in ss]


def s(n):
    if isinstance(n, ast.Import):
        return ["import", [[a.name, a.asname] for a in n.names]]
    if isinstance(n, ast.ImportFrom):
        return ["from", n.module, n.level, [[a.name, a.asname] for a in n.names]]
    if isinstance(n, ast.Assign):
        return ["assign", [e(t) for t in n.targets], e(n.value)]
    if isinstance(n, ast.AugAssign):
        return ["aug", e(n.target), e(n.value)]
    if isinstance(n, ast.AnnAssign):
        return ["ann", e(n.target), e(n.annotation), e(n.value)]
    if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef)):
        return [
            "def",
            n.name,
            params(n.args),
            [e(d) for d in n.decorator_list],
            e(n.returns),
            body(n.body),
            tparams(n),
        ]
    if isinstance(n, ast.ClassDef):
        return [
            "class",
            n.name,
            [e(b) for b in n.bases] + [e(k.value) for k in n.keywords],
            [e(d) for d in n.decorator_list],
            body(n.body),
            tparams(n),
        ]
    if isinstance(n, ast.Return):
        return ["return", e(n.value)]
    if isinstance(n, ast.Expr):
        return ["expr", e(n.value)]
    if isinstance(n, ast.If):
        return ["if", e(n.test), body(n.body), body(n.orelse)]
    if isinstance(n, ast.While):
        return ["while", e(n.test), body(n.body), body(n.orelse)]
    if isinstance(n, (ast.For, ast.AsyncFor)):
        return ["for", e(n.target), e(n.iter), body(n.body), body(n.orelse)]
    if isinstance(n, (ast.With, ast.AsyncWith)):
        return ["with", [[e(i.context_expr), e(i.optional_vars)] for i in n.items], body(n.body)]
    if isinstance(n, (ast.Try, getattr(ast, "TryStar", ast.Try))):
        return [
            "try",
            body(n.body),
            [[e(h.type), h.name, body(h.body)] for h in n.handlers],
            body(n.orelse),
            body(n.finalbody),
        ]
    if isinstance(n, ast.Global):
        return ["global", n.names]
    if isinstance(n, ast.Nonlocal):
        return ["nonlocal", n.names]
    if isinstance(n, ast.Delete):
        return ["del", [e(t) for t in n.targets]]
    if isinstance(n, ast.Match):
        return [
            "match",
            e(n.subject),
            [[pat(c.pattern), e(c.guard), body(c.body)] for c in n.cases],
        ]
    if type(n).__name__ == "TypeAlias":
        return ["typealias", n.name.id, tparams(n), e(n.value)]
    return ["other", [e(c) for c in ast.iter_child_nodes(n) if isinstance(c, ast.expr)], []]


def module_name(root, path):
    rel = os.path.relpath(path, root)
    parts = rel[: -len(".py")].split(os.sep)
    is_pkg = parts[-1] == "__init__"
    if is_pkg:
        parts = parts[:-1]
    return ".".join(parts), is_pkg


def project(root, files):
    mods = []
    for f in files:
        name, is_pkg = module_name(root, f)
        if not name:
            continue
        try:
            with open(f, "rb") as fh:
                tree = ast.parse(fh.read())
        except (SyntaxError, ValueError):
            continue
        mods.append({"name": name, "is_pkg": is_pkg, "file": os.path.relpath(f, root), "body": body(tree.body)})
    return {"modules": mods}


def walk(root):
    out = []
    for d, dirs, fs in os.walk(root):
        dirs[:] = sorted(x for x in dirs if not x.startswith(".") and x not in ("node_modules", "__pycache__"))
        out.extend(os.path.join(d, f) for f in sorted(fs) if f.endswith(".py"))
    return out


if __name__ == "__main__":
    root = sys.argv[1]
    json.dump(project(root, walk(root)), sys.stdout)
