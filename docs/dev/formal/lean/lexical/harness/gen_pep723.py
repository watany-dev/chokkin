"""PEP 723 differential: which `# /// script` blocks count.

Random scripts built from block-shaped lines (opening lines with valid and
invalid types, content lines, closing lines with and without trailing
blanks, `# ///` inside the content run, code between) and LF / CRLF / CR
endings. The Lean oracle applies the PEP's reference regex to the text as
Python reads it (universal newlines). Every script imports `requests`; a block
whose TOML lists it declares it.

Expected of chokkin: exactly one `script` block that lists `requests` means no
CHK003 for it; no `script` block means CHK003. (Several `script` blocks are an
error per the PEP and are left out.)

    python3 -I harness/gen_pep723.py --cases 1500
"""

import argparse
import collections
import json
import os
import random
import sys

import tomllib

sys.path.insert(0, os.path.dirname(__file__))
from common import chokkin, oracle, project

CONTENT = [
    *['# dependencies = ["requests"]'] * 3,
    "#",
    "# ",
    "#\t",
    '# requires-python = ">=3.8"',
    "#x",
    "# ///",
    "",
    '#  dependencies = ["requests"]',
]


def build(r):
    lines = []
    if r.random() < 0.3:
        lines.append(
            r.choice(["#!/usr/bin/env python", '"""doc"""', "import os", "# comment"])
        )
    for _ in range(r.randint(1, 2)):
        lines.append(
            r.choice(
                ["# /// script"] * 6
                + [
                    "# /// script ",
                    "#/// script",
                    "# ///  script",
                    "# /// pyproject",
                    "# /// Script",
                    "# /// sc ript",
                ]
            )
        )
        lines.extend(r.choice(CONTENT) for _ in range(r.randint(0, 3)))
        lines.append(r.choice(["# ///"] * 6 + ["# /// ", "#///", "# ////", "  # ///"]))
        if r.random() < 0.3:
            lines.append(r.choice(["x = 1", "#", "# ///", ""]))
    lines.append("import requests")
    eol = r.choice(["\n", "\r\n", "\r", "mixed"])
    text = ""
    for item in lines:
        text += item + (r.choice(["\n", "\r\n", "\r"]) if eol == "mixed" else eol)
    return text


def lists_requests(text, block):
    ls = text.replace("\r\n", "\n").replace("\r", "\n").split("\n")
    body = "\n".join(l[2:] for l in ls[block[0] : block[1] - 1])
    try:
        return "requests" in tomllib.loads(body).get("dependencies", [])
    except tomllib.TOMLDecodeError:
        return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", type=int, default=1000)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--out", default="pep723-mismatches.jsonl")
    args = ap.parse_args()
    r = random.Random(args.seed)
    stats, rows = collections.Counter(), []
    for _ in range(args.cases):
        text = build(r)
        root = project(text.encode())
        [exp] = oracle([os.path.join(root, "main.py")])
        scripts = [b for b in exp["scripts"] if b[2] == "script"]
        if len(scripts) > 1:
            stats["several-blocks"] += 1
            continue
        declared = bool(scripts) and lists_requests(text, scripts[0])
        if declared is None:
            stats["toml-error"] += 1
            continue
        got = chokkin(root)
        flagged = any(s == "requests" for s, _ in got["chk003"])
        stats["cases"] += 1
        if flagged == declared:
            rows.append(
                {
                    "text": text,
                    "blocks": exp["scripts"],
                    "declared": declared,
                    "chk003": flagged,
                    "diagnostics": [d["message"] for d in got["diagnostics"]],
                }
            )
    with open(args.out, "w") as f:
        f.writelines(json.dumps(row, ensure_ascii=False) + "\n" for row in rows)
    print(dict(stats), "mismatches:", len(rows))


if __name__ == "__main__":
    main()
