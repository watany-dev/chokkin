"""Real-file run: check the spec against CPython, then chokkin against both.

For every `.py` under the given directories:

1. spec vs CPython: the Lean oracle's decision and COMMENT tokens must match
   CPython's `tokenize` on the decoded text (a mismatch is a spec bug to fix
   before anything else is believed);
2. chokkin, one file per project as `main.py`: every CHK003 / CHK010 it
   reports must sit on a line where CPython's AST has an import of that
   module (`Import` / `ImportFrom` lineno), and a marker import appended after
   the last physical line must be reported on the line the spec counts.

    python3 -I harness/oss.py DIR... --limit 3000
"""

import argparse
import ast
import collections
import io
import json
import os
import random
import sys
import tokenize

sys.path.insert(0, os.path.dirname(__file__))
from common import chokkin, oracle, project, univ

MARK = "zzlexmarker"


def py_decode(data):
    try:
        enc, _ = tokenize.detect_encoding(io.BytesIO(data).readline)
        return data.decode(enc)
    except (SyntaxError, LookupError, UnicodeError):
        return None


def import_lines(text):
    try:
        tree = ast.parse(text)
    except (SyntaxError, ValueError):
        return None
    out = collections.defaultdict(set)
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for a in node.names:
                out[a.name.split(".")[0]].add(node.lineno)
        elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
            out[node.module.split(".")[0]].add(node.lineno)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dirs", nargs="+")
    ap.add_argument("--limit", type=int, default=2000)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--out", default="oss-mismatches.jsonl")
    args = ap.parse_args()
    files = [
        os.path.join(d, f)
        for top in args.dirs
        for d, _, fs in os.walk(top)
        for f in fs
        if f.endswith(".py")
    ]
    random.Random(args.seed).shuffle(files)
    files = files[: args.limit]
    stats, rows = collections.Counter(), []
    exps = {}
    for i in range(0, len(files), 200):
        for e in oracle(files[i : i + 200]):
            exps[e["path"]] = e
    for path in files:
        with open(path, "rb") as fh:
            data = fh.read()
        exp = exps[path]
        text = py_decode(data)
        if not exp["decoded"]:
            stats["spec-not-decoded:" + exp["decision"]] += 1
            continue
        if text is None:
            stats["cpython-cannot-decode"] += 1
            continue
        try:
            toks = list(tokenize.generate_tokens(io.StringIO(univ(text)).readline))
            cp = [[t.start[0], t.string] for t in toks if t.type == tokenize.COMMENT]
        except (SyntaxError, tokenize.TokenError):
            cp = None
        if cp is None:
            stats["cpython-tokenize-error"] += 1
            continue
        if exp["error"] is not None or exp["comments"] != cp:
            stats["spec-vs-cpython"] += 1
            rows.append(
                {
                    "bucket": "spec-vs-cpython",
                    "path": path,
                    "error": exp["error"],
                    "first-diff": next(
                        (
                            [a, b]
                            for a, b in zip(exp["comments"], cp, strict=False)
                            if a != b
                        ),
                        None,
                    ),
                }
            )
            continue
        stats["spec-ok"] += 1
        imports = import_lines(text)
        if imports is None or MARK in text:
            continue
        sep = b"" if data.endswith((b"\n", b"\r")) else b"\n"
        root = project(data + sep + f"import {MARK}\n".encode())
        got = chokkin(root)
        if got["skipped"]:
            stats["chokkin-skipped"] += 1
            rows.append(
                {
                    "bucket": "chokkin-skipped",
                    "path": path,
                    "diag": [
                        d["message"]
                        for d in got["diagnostics"]
                        if "skipped" in d["message"]
                    ],
                }
            )
            continue
        want_mark = univ(text + sep.decode()).count("\n") + 1
        for issue in got["issues"]:
            if issue["code"] not in ("CHK003", "CHK010") or issue["file"] != "main.py":
                continue
            sym, line = issue["symbol"], issue["line"]
            top = (sym or "").split(".")[0]
            if top == MARK:
                if line != want_mark:
                    rows.append(
                        {
                            "bucket": "marker-line",
                            "path": path,
                            "want": want_mark,
                            "got": line,
                        }
                    )
                continue
            if top in imports and line not in imports[top]:
                rows.append(
                    {
                        "bucket": "import-line",
                        "path": path,
                        "symbol": sym,
                        "got": line,
                        "ast": sorted(imports[top]),
                    }
                )
        stats["chokkin-checked"] += 1
    with open(args.out, "w") as f:
        f.writelines(json.dumps(r, ensure_ascii=False) + "\n" for r in rows)
    print(dict(stats), collections.Counter(r["bucket"] for r in rows))


if __name__ == "__main__":
    main()
