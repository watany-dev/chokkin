"""Run target/release/chokkin as a black box on a generated project."""
import json
import os
import pathlib
import shutil
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[6]
BIN = os.environ.get("CHOKKIN", str(ROOT / "target/release/chokkin"))
SCRATCH = os.environ.get("SCRATCH", tempfile.gettempdir())

APP_PYPROJECT = '[project]\nname="t"\nversion="0"\n[tool.chokkin]\nmode="app"\nentry=["main.py"]\n'


def run(files, git=True, args=()):
    """files: {relpath: content}; returns parsed JSON report."""
    d = tempfile.mkdtemp(dir=SCRATCH)
    try:
        for rel, text in files.items():
            p = os.path.join(d, rel)
            os.makedirs(os.path.dirname(p), exist_ok=True)
            if text is None:
                os.makedirs(p, exist_ok=True)
                continue
            with open(p, "w") as f:
                f.write(text)
        if git:
            subprocess.run(["git", "init", "-q", d], check=True)
        out = subprocess.run([BIN, "--reporter", "json", "--no-cache", *args, d],
                             capture_output=True, text=True, check=False)
        try:
            rep = json.loads(out.stdout)
        except json.JSONDecodeError:
            rep = {"issues": [], "error": out.stderr}
        rep["_dir"] = d
        rep["_exit"] = out.returncode
        return rep
    finally:
        if not os.environ.get("KEEP"):
            shutil.rmtree(d, ignore_errors=True)


def codes(rep, code):
    return sorted(i.get("target") or "" for i in rep["issues"] if i["code"] == code)


def brief(rep):
    return sorted((i["code"], i.get("target")) for i in rep["issues"])
