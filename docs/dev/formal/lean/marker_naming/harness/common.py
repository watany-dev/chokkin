"""Shared helpers: run the Lean oracle and chokkin (as a black box) on temp projects."""

import json
import os
import pathlib
import subprocess
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
ORACLE = HERE.parent / ".lake" / "build" / "bin" / "oracle"
CHOKKIN = os.environ.get(
    "CHOKKIN", str(HERE.parents[5] / "target" / "release" / "chokkin")
)


def oracle(queries):
    """One answer per query, from the Lean executable."""
    text = "".join(json.dumps(q) + "\n" for q in queries)
    out = subprocess.run(
        [str(ORACLE)], input=text, capture_output=True, text=True, check=True
    )
    return [json.loads(line) for line in out.stdout.splitlines()]


def chokkin(files, *args):
    """chokkin --reporter json on a temp project made of `files`."""
    with tempfile.TemporaryDirectory() as d:
        for rel, content in files.items():
            path = pathlib.Path(d, rel)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        out = subprocess.run(
            [CHOKKIN, "--reporter", "json", "--no-cache", *args],
            cwd=d,
            capture_output=True,
            text=True,
            check=False,
        )
        return json.loads(out.stdout)


def issues(report, code):
    return {i["target"] for i in report["issues"] if i["code"] == code}


def pyproject(deps, requires_python=">=3.9"):
    return (
        '[project]\nname = "zzproj"\nversion = "0"\n'
        f"requires-python = {json.dumps(requires_python)}\n"
        f"dependencies = {json.dumps(deps)}\n"
        '[tool.chokkin]\nmode = "app"\nentry = ["main.py"]\n'
    )
