"""Real-file check of symbol lines: each CHK006 / CHK007 chokkin reports for a
module must sit on a physical line where CPython's AST binds that name at
module level (`def` / `class` line, assignment target line).

    python3 -I harness/oss_symbols.py DIR... --limit 400
"""

import argparse
import ast
import collections
import json
import os
import pathlib
import random
import sys

sys.path.insert(0, os.path.dirname(__file__))
from common import chokkin, project

PYPROJECT = (
    '[project]\nname = "t"\nversion = "0"\ndependencies = []\n'
    '[tool.chokkin]\nmode = "app"\nentry = ["main.py"]\n'
)


def bindings(tree):
    out = collections.defaultdict(set)

    def targets(t, line):
        if isinstance(t, ast.Name):
            out[t.id].add(t.lineno)
        elif isinstance(t, (ast.Tuple, ast.List)):
            for e in t.elts:
                targets(e, line)
        elif isinstance(t, ast.Starred):
            targets(t.value, line)

    def walk(body):
        for n in body:
            if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                out[n.name].add(n.lineno)
            elif isinstance(n, ast.Assign):
                for t in n.targets:
                    targets(t, n.lineno)
            elif isinstance(n, (ast.AnnAssign, ast.AugAssign)):
                targets(n.target, n.lineno)
            elif isinstance(n, ast.TypeAlias):
                targets(n.name, n.lineno)
            elif isinstance(n, (ast.If, ast.Try, ast.With, ast.For, ast.While)):
                for field in ("body", "orelse", "finalbody"):
                    walk(getattr(n, field, []) or [])
                for h in getattr(n, "handlers", []) or []:
                    walk(h.body)
            elif isinstance(n, (ast.Import, ast.ImportFrom)):
                for a in n.names:
                    out[(a.asname or a.name).split(".")[0]].add(n.lineno)

    walk(tree.body)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dirs", nargs="+")
    ap.add_argument("--limit", type=int, default=400)
    ap.add_argument("--out", default="oss-symbol-mismatches.jsonl")
    args = ap.parse_args()
    files = [
        os.path.join(d, f)
        for top in args.dirs
        for d, _, fs in os.walk(top)
        for f in fs
        if f.endswith(".py") and f != "__init__.py"
    ]
    random.Random(1).shuffle(files)
    stats, rows = collections.Counter(), []
    for path in files[: args.limit]:
        with open(path, "rb") as fh:
            data = fh.read()
        try:
            tree = ast.parse(data)
        except (SyntaxError, ValueError):
            continue
        b = bindings(tree)
        root = project(b"import pkg.mod\n", pyproject=PYPROJECT)
        os.makedirs(os.path.join(root, "pkg"))
        pathlib.Path(root, "pkg", "__init__.py").write_bytes(b"")
        pathlib.Path(root, "pkg", "mod.py").write_bytes(data)
        got = chokkin(root)
        for i in got["issues"]:
            if i["code"] not in ("CHK006", "CHK007") or i["file"] != "pkg/mod.py":
                continue
            name = (i["symbol"] or "").split(":")[-1].split(".")[0]
            stats["checked"] += 1
            if name in b and i["line"] not in b[name]:
                rows.append(
                    {
                        "path": path,
                        "code": i["code"],
                        "symbol": i["symbol"],
                        "got": i["line"],
                        "ast": sorted(b[name]),
                    }
                )
            elif name not in b:
                stats["name-not-module-level"] += 1
    with open(args.out, "w") as f:
        f.writelines(json.dumps(r) + "\n" for r in rows)
    print(dict(stats), "mismatches:", len(rows))


if __name__ == "__main__":
    main()
