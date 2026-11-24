"""Generated-input differential for inline / file-level ignores and line numbers.

Each case is a `main.py` built from random lines: imports of modules nobody
declares (so each is a CHK003 at its physical line), string literals of every
prefix and quote holding directive text, comments holding well-formed and
malformed directives, and LF / CRLF / CR endings. For each case:

1. the Lean oracle gives the COMMENT tokens, the first statement's line and the
   directives' effects (spec §18 over COMMENT tokens);
2. CPython's tokenizer must report the same COMMENT tokens (checks the spec);
3. chokkin's CHK003 lines and `# chokkin:` warnings must equal the expected.

    python3 -I harness/gen_ignores.py --cases 3000 --seed 1
"""

import argparse
import collections
import json
import os
import random
import sys

sys.path.insert(0, os.path.dirname(__file__))
from common import chokkin, cpython_comments, cpython_compiles, line_of, oracle, project

MODULES = [
    "yaml",
    "requests",
    "numpy",
    "attr",
    "six",
    "toml",
    "jwt",
    "dateutil",
    "pytz",
    "click",
    "flask",
    "redis",
    "lxml",
    "bs4",
    "sqlalchemy",
    "jinja2",
    "markdown",
    "pydantic",
    "httpx",
    "rich",
]

DIRECTIVES = [
    "# chokkin: ignore[CHK003]",
    "#chokkin:ignore[CHK003]",
    "# chokkin: ignore[CHK003,CHK010]",
    "# chokkin: ignore[CHK010]",
    "# chokkin: ignore[CHK03]",
    "# chokkin: ignore[CHK999,CHK003]",
    "# chokkin: ignore [CHK003]",
    "# chokkin : ignore[CHK003]",
    "# chokkin: ignore[chk003]",
    "# chokkin: file-ignore[CHK003]",
    "#\tchokkin:\tignore[CHK003]",
    "# noqa # chokkin: ignore[CHK003]",
    "# chokkin: ignore[CHK003] trailing",
    "# chokkin:",
    "#  chokkin: noqa",
    "# chokkin: ignore[CHK003]",
    "# chokkin: ignore [CHK003]",
    "# chokkin:　ignore[CHK003]",
]

KNOWN = set()  # root causes already filed; --exclude drops their triggers


def rand_content(r, quote, triple, fstr, depth=0):
    parts = []
    for _ in range(r.randint(0, 4)):
        k = r.random()
        if k < 0.3:
            if "string" in KNOWN:
                parts.append(r.choice(["#", "# x", "#chokkin"]))
            else:
                parts.append(
                    r.choice(DIRECTIVES[:4] + ["#", "# x", "#chokkin:ignore[CHK003]"])
                )
        elif k < 0.4:
            parts.append(r.choice(["\\\\", "\\n", "\\" + quote, "\\t"]))
        elif k < 0.5 and triple:
            parts.append(
                r.choice(
                    ["\n", "\n#", "\n"]
                    + ([] if "string" in KNOWN else ["\n# chokkin: ignore[CHK003]\n"])
                )
            )
        elif k < 0.65 and fstr and depth < 2:
            inner_q = r.choice(["'", '"'])
            expr = r.choice(["1", "x", "a[0]", "{'k': 1}['k']", "f(1, 2)"])
            if r.random() < 0.5:
                expr = rand_string(r, depth + 1, inner_q)
            spec = r.choice(["", "", ":>10", "!r", ":{w}"])
            if triple and r.random() < 0.3:
                expr = expr + (
                    "  # c\n" if "string" in KNOWN else "  # chokkin: ignore[CHK003]\n"
                )
            parts.append("{" + expr + spec + "}")
        elif k < 0.7 and fstr:
            parts.append(
                r.choice(
                    ["{{", "}}"]
                    + ([] if "string" in KNOWN else ["{{# chokkin: ignore[CHK003]}}"])
                )
            )
        else:
            parts.append(r.choice(["a", " ", "x y", "é", "()", "[", ":"]))
    return "".join(parts)


def rand_string(r, depth=0, quote=None):
    prefix = r.choice(["", "", "r", "b", "u", "f", "F", "rf", "fR", "Rb", "br"])
    quote = quote or r.choice(["'", '"'])
    triple = r.random() < 0.35
    fstr = "f" in prefix.lower()
    if "b" in prefix.lower():
        content = r.choice(
            ["#", "x"] + ([] if "string" in KNOWN else ["# chokkin: ignore[CHK003]"])
        )
    else:
        content = rand_content(r, quote, triple, fstr, depth)
    q = quote * 3 if triple else quote
    # Keep the literal well-formed: no trailing backslash or quote run.
    if content.endswith(("\\", quote)):
        content += " "
    return prefix + q + content + q


def directives():
    ds = DIRECTIVES
    if "whitespace" in KNOWN:
        ds = [d for d in ds if "\u00a0" not in d and "\u3000" not in d]
    return ds


HEADERS = [
    "",
    "# header",
    "#!/usr/bin/env python",
    "\x0c",
    "   # indented",
    "# -*- coding: utf-8 -*-",
    "\t",
    "\x0c# ff comment",
    "# chokkin: file-ignore[CHK003]",
    "# chokkin: ignore[CHK003]",
    "# chokkin: file-ignore[CHK010]",
    '"""doc"""',
    '"""\n# chokkin: file-ignore[CHK003]\n"""',
    "\\",
    "if True:\n    pass",
    "@dec\ndef f(): pass",
    "(\n# chokkin: file-ignore[CHK003]\n)",
]


def build(r):
    lines, imports = [], []
    mods = MODULES[:]
    r.shuffle(mods)
    lines.extend(r.choice(HEADERS) for _ in range(r.randint(0, 3)))
    for _ in range(r.randint(1, 7)):
        k = r.random()
        tail = ("  " + r.choice(directives())) if r.random() < 0.5 else ""
        if k < 0.35 and mods:
            m = mods.pop()
            lines.append(("IMPORT", m, tail))
        elif k < 0.55 and mods:
            m = mods.pop()
            lines.append(("IMPORT", m, "; x = " + rand_string(r) + tail))
        elif k < 0.8:
            lines.append("x = " + rand_string(r) + tail)
        elif k < 0.9:
            lines.append(r.choice(directives()))
        else:
            lines.append(
                r.choice(["", "\x0c", "y = (1 +\n     2)", "z = 1 + \\\n    2"])
            )
    if r.random() < 0.1 and "whitespace" not in KNOWN:
        lines.append(("IMPORT", mods.pop(), "  #"))
        lines.append("chokkin: ignore[CHK003]")
    eol = r.choice(["\n", "\n", "\r\n", "\r", "mixed"])
    text = ""
    for _i, item in enumerate(lines):
        end = r.choice(["\n", "\r\n", "\r"]) if eol == "mixed" else eol
        if isinstance(item, tuple):
            _, m, tail = item
            imports.append((m, len(text)))
            text += f"import {m}{tail}{end}"
        else:
            text += item.replace("\n", end) + end
    return text, [(m, line_of(text, off)) for m, off in imports]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", type=int, default=500)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--out", default="ignores-mismatches.jsonl")
    ap.add_argument(
        "--exclude", default="", help="comma list of filed causes: string,whitespace"
    )
    args = ap.parse_args()
    KNOWN.update(x for x in args.exclude.split(",") if x)
    r = random.Random(args.seed)
    stats = collections.Counter()
    buckets = collections.defaultdict(list)
    for _n in range(args.cases):
        text, imports = build(r)
        if not cpython_compiles(text):
            stats["cpython-rejects"] += 1
            continue
        root = project(text.encode())
        [exp] = oracle([os.path.join(root, "main.py")])
        cp = cpython_comments(text)
        if exp["error"] is not None or cp != exp["comments"]:
            stats["spec-vs-cpython"] += 1
            buckets["spec-vs-cpython"].append(
                {
                    "text": text,
                    "lean": exp["comments"],
                    "cpython": cp,
                    "error": exp["error"],
                }
            )
            continue
        got = chokkin(root)
        inline = {(l, c) for l, cs in exp["inline"] for c in cs}
        want = sorted(
            (m, l)
            for m, l in imports
            if "CHK003" not in exp["fileCodes"] and (l, "CHK003") not in inline
        )
        want_w = sorted(tuple(w) for w in exp["warnings"])
        stats["cases"] += 1
        if got["chk003"] != want:
            missing = sorted(set(want) - set(got["chk003"]))
            extra = sorted(set(got["chk003"]) - set(want))
            key = (
                "chk003-"
                + ("suppressed-wrongly" if missing else "")
                + ("not-suppressed" if extra else "")
            )
            buckets[key].append({"text": text, "want": want, "got": got["chk003"]})
        if got["warnings"] != want_w:
            buckets["warnings"].append(
                {"text": text, "want": want_w, "got": got["warnings"]}
            )
    with open(args.out, "w") as f:
        for k, v in buckets.items():
            f.writelines(
                json.dumps({"bucket": k, **case}, ensure_ascii=False) + "\n"
                for case in v
            )
    print(dict(stats), {k: len(v) for k, v in buckets.items()})


if __name__ == "__main__":
    main()
