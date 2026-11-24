#!/usr/bin/env python3
"""Differential run of the Lean shell oracle against the chokkin binary.

Each case is a shell script written to `ci.sh` at the root of a throwaway app
project that has `pkg/tool.py` and `pkg/gen.py` (reached by nothing else) and
declares no tool. The Lean oracle (`.lake/build/bin/oracle`) says what the
script runs; chokkin's report is read back as:

* a tracked tool binary is run      <=> CHK008 for it (undeclared, root file)
* `python -m pkg.tool` is run       <=> no CHK001 on pkg/tool.py
* `python pkg/gen.py` is run        <=> no CHK001 on pkg/gen.py

Scripts come from a random grammar (`--generate N`) or from files (`--files`,
one script per file, e.g. `.sh` files from the OSS clones). Mismatches are
written as JSON lines for triage; the grammar's own intent is never used as the
expectation, only the oracle's answer.

Usage: harness.py --chokkin PATH [--generate N --seed S] [--files F...] [--out mismatches.jsonl]
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ORACLE = HERE / ".lake" / "build" / "bin" / "oracle"
TRACKED = ["black", "isort", "ruff", "mypy", "flake8", "twine"]

PYPROJECT = """[project]
name = "pkg"
version = "0"
requires-python = ">=3.10"
dependencies = []

[tool.chokkin]
mode = "app"
entry = ["main.py"]
"""


# ---------------------------------------------------------------- generator


def pick(r: random.Random, xs):
    return xs[r.randrange(len(xs))]


def py_opts(r: random.Random) -> str:
    opts = [
        "-u",
        "-B",
        "-O",
        "-OO",
        "-I",
        "-E",
        "-s",
        "-S",
        "-q",
        "-b",
        "-bb",
        "-W ignore",
        "-Wignore",
        "-X dev",
        "-Xdev",
        "-W error::DeprecationWarning",
        "-uB",
        "-X importtime",
        "-P",
        "-d",
        "-v",
        "--check-hash-based-pycs never",
        "-R",
        "-x",
    ]
    return " ".join(pick(r, opts) for _ in range(r.randrange(3)))


def python_head(r: random.Random) -> str:
    return pick(
        r,
        [
            "python",
            "python3",
            "python3.12",
            "/usr/bin/python3",
            ".venv/bin/python",
            "py -3.11",
            "pypy3",
            "python3.9",
        ],
    )


def target(r: random.Random) -> str:
    kind = r.randrange(3)
    if kind == 0:
        tool = pick(r, TRACKED)
        return (
            f"{tool} {pick(r, ['.', 'src', '--check .', '-q src tests', ''])}".strip()
        )
    head = python_head(r)
    opts = py_opts(r)
    if kind == 1:
        form = pick(
            r,
            [
                "-m pkg.tool",
                "-mpkg.tool",
                "-m pkg.tool --flag",
                "-um pkg.tool",
                "-Om pkg.tool",
            ],
        )
        return " ".join(x for x in [head, opts, form] if x)
    return " ".join(
        x
        for x in [
            head,
            opts,
            pick(r, ["pkg/gen.py", "./pkg/gen.py", "pkg/gen.py --out x"]),
        ]
        if x
    )


def wrap(r: random.Random, cmd: str) -> str:
    w = r.randrange(32)
    q = cmd.replace("'", "'\\''")
    if w == 0:
        return f"env {pick(r, ['', '-i', '-u HOME', '-uHOME', '-C /tmp', '--chdir=/tmp', '-v', 'A=1 B=2', '--unset=X'])} {cmd}"
    if w == 1:
        return f"nice {pick(r, ['', '-n 5', '-10', '--adjustment=3', '-n5'])} {cmd}"
    if w == 2:
        return f"nohup {cmd}"
    if w == 3:
        return f"timeout {pick(r, ['', '-s KILL', '-sKILL', '-k 5', '--signal=TERM', '--preserve-status', '-v', '--foreground'])} 60 {cmd}"
    if w == 4:
        return (
            f"stdbuf {pick(r, ['-oL', '-o L', '-i0 -o0', '--output=L', '-eL'])} {cmd}"
        )
    if w == 5:
        return f"setsid {pick(r, ['', '-f', '-w'])} {cmd}"
    if w == 6:
        return f"ionice {pick(r, ['-c 3', '-c3', '-c 2 -n 7', '-t -c 3'])} {cmd}"
    if w == 7:
        return f"taskset {pick(r, ['-c 0', '-c 0-3', '0x1', '-a -c 1'])} {cmd}"
    if w == 8:
        return f"flock {pick(r, ['', '-w 5', '-n', '-x'])} /tmp/l {cmd}"
    if w == 9:
        return f"time {pick(r, ['', '-p'])} {cmd}"
    if w == 10:
        return f"command {pick(r, ['', '-p'])} {cmd}"
    if w == 11:
        return f"exec {pick(r, ['', '-c', '-a name'])} {cmd}"
    if w == 12:
        return f"sudo {pick(r, ['', '-E', '-u app', '-H -u app', '--preserve-env', '-n'])} {cmd}"
    if w == 13:
        return f"xargs {pick(r, ['', '-0', '-n 1', '-r', '-P 4', '-I{{}}'.format()])} {cmd} < files"
    if w == 14:
        return f"uv run {pick(r, ['', '--frozen', '--with x', '-p 3.12', '--no-sync', '--extra dev', '--group test', '--isolated'])} {cmd}"
    if w == 15:
        return f"poetry run {cmd}"
    if w == 16:
        return f"pipenv run {cmd}"
    if w == 17:
        return f"pdm run {cmd}"
    if w == 18:
        return f"hatch run {pick(r, ['', 'test:', 'lint:'])}{cmd}"
    if w == 19:
        return f"conda run {pick(r, ['-n base', '--no-capture-output -n x', '-p /opt/env'])} {cmd}"
    if w == 20:
        return f"bash -c '{q}'"
    if w == 21:
        return f"sh {pick(r, ['-ec', '-c', '-xc', '-euc', '-c -e', '-o pipefail -c', '-lc'])} '{q}'"
    if w == 22:
        return f"watch {pick(r, ['', '-n 1', '-n1', '-d', '-t'])} {cmd}"
    if w == 23:
        return f"watch -x {cmd}"
    if w == 24:
        return f"find . -name '*.py' -exec {cmd} {{}} \\;"
    if w == 25:
        return f"find . {pick(r, ['-execdir', '-exec'])} {cmd} {{}} +"
    if w == 26:
        return f"eval '{q}'"
    if w == 27:
        return f"trap '{q}' EXIT"
    if w == 28:
        return f"/usr/bin/env {cmd}"
    if w == 29:
        return f"doas {cmd}"
    if w == 30:
        return f"env -S '{q}'"
    return cmd


def context(r: random.Random, cmd: str) -> str:
    c = r.randrange(48)
    forms = [
        "{c}",
        "X=1 {c}",
        "if true; then {c}; fi",
        "if {c}; then :; fi",
        "if ! {c}; then exit 1; fi",
        "while {c}; do :; done",
        "until {c}; do sleep 1; done",
        "{{ {c}; }}",
        "( {c} )",
        "{c} &",
        "true && {c}",
        "false || {c}",
        "{c} | tee log",
        "x=$({c})",
        'echo "$({c})"',
        "x=`{c}`",
        "f() {{ {c}; }}",
        "function f {{ {c}; }}",
        "case $1 in a) {c};; esac",
        "case $1 in (a|b) {c} ;; esac",
        "for i in 1 2; do {c}; done",
        "! {c}",
        "time {c}",
        "cat <<EOF\n$({c})\nEOF",
        "{c} 2>&1 >/dev/null",
        ">log {c}",
        "[[ -n $x ]] && {c}",
        "(( 1 )) && {c}",
        "{c}; # done",
        "if [ -f x ]\nthen\n  {c}\nfi",
        "echo {c}",
        "echo '{c}'",
        'echo "a\n{c}"',
        "# {c}",
        "cat <<EOF\n{c}\nEOF",
        "cat <<'EOF'\n$({c})\nEOF",
        "x='{c}'",
        "printf '%s\\n' \"{c}\"",
        ": {c}",
        "[[ $x == '{c}' ]]",
        "alias x='{c}'",
        "cat <<-EOF\n\t{c}\n\tEOF",
        "echo a \\\n  && {c}",
        "[ -x /bin/sh ] || {c}",
        "{c} <<< 'in'",
        "{c} < <(echo x)",
        "set -e\n{c}",
        "case x in\n  a|b)\n    {c}\n    ;;\nesac",
    ]
    return forms[c % len(forms)].format(c=cmd)


def generate(r: random.Random) -> str:
    cmd = target(r)
    for _ in range(r.choice([0, 1, 1, 2])):
        cmd = wrap(r, cmd)
    return context(r, cmd)


# ---------------------------------------------------------------- runners


def oracle(scripts: list[str]) -> list[list | None]:
    data = "".join(json.dumps(s) + "\n" for s in scripts)
    out = subprocess.run(
        [str(ORACLE)], input=data, capture_output=True, text=True, check=True
    ).stdout
    return [json.loads(line) for line in out.splitlines()]


def expectation(events: list) -> dict:
    bins = {e[1] for e in events if e[0] == "bin"}
    mods = {e[1] for e in events if e[0] == "module"}
    scripts = {e[1].removeprefix("./") for e in events if e[0] == "script"}
    return {
        "tools": sorted(b for b in TRACKED if b in bins),
        "module": "pkg.tool" in mods,
        "script": "pkg/gen.py" in scripts,
        "unknown": any(e[0] == "unknown" for e in events),
    }


def observe(chokkin: str, script: str) -> dict:
    d = Path(tempfile.mkdtemp(prefix="o1-"))
    try:
        (d / "pyproject.toml").write_text(PYPROJECT)
        (d / "main.py").write_text("print(1)\n")
        (d / "pkg").mkdir()
        (d / "pkg" / "__init__.py").write_text("")
        (d / "pkg" / "tool.py").write_text("print(2)\n")
        (d / "pkg" / "gen.py").write_text("print(3)\n")
        (d / "ci.sh").write_text("#!/bin/sh\n" + script + "\n")
        res = subprocess.run(
            [chokkin, "--reporter", "json", "--no-cache", str(d)],
            capture_output=True,
            text=True,
            check=False,
        )
        issues = json.loads(res.stdout)["issues"]
    finally:
        shutil.rmtree(d, ignore_errors=True)
    chk008 = {i["target"] for i in issues if i["code"] == "CHK008"}
    chk001 = {i["target"]: i["confidence"] for i in issues if i["code"] == "CHK001"}
    return {
        "tools": sorted(b for b in TRACKED if b in chk008),
        "module": "pkg/tool.py" not in chk001,
        "module_conf": chk001.get("pkg/tool.py"),
        "script": "pkg/gen.py" not in chk001,
        "script_conf": chk001.get("pkg/gen.py"),
    }


def compare(exp: dict, obs: dict) -> list[str]:
    diffs = [
        f"{'FN' if t in exp['tools'] else 'FP'}:bin:{t}"
        for t in TRACKED
        if (t in exp["tools"]) != (t in obs["tools"])
    ]
    diffs.extend(
        f"{'FP' if exp[k] else 'FN'}:{k}:{obs[k + '_conf']}" for k in ("module", "script") if exp[k] != obs[k]
    )
    return diffs


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--chokkin", required=True)
    ap.add_argument("--generate", type=int, default=0)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--files", nargs="*", default=[])
    ap.add_argument("--out", default="mismatches.jsonl")
    ap.add_argument("--jobs", type=int, default=os.cpu_count() or 4)
    args = ap.parse_args()

    r = random.Random(args.seed)
    scripts = [generate(r) for _ in range(args.generate)]
    scripts.extend(
        Path(f).read_text(errors="replace") for f in args.files if Path(f).is_file()
    )
    expected = oracle(scripts)
    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        observed = list(pool.map(lambda s: observe(args.chokkin, s), scripts))

    skipped = mismatched = 0
    with open(args.out, "w") as out:
        for script, ev, obs in zip(scripts, expected, observed, strict=True):
            if ev is None:
                skipped += 1
                continue
            exp = expectation(ev)
            diffs = compare(exp, obs)
            if diffs:
                mismatched += 1
                out.write(
                    json.dumps(
                        {
                            "script": script,
                            "diffs": diffs,
                            "expected": exp,
                            "observed": obs,
                        }
                    )
                    + "\n"
                )
    print(
        f"cases={len(scripts)} unparsed={skipped} mismatched={mismatched}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
