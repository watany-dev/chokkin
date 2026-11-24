"""Shared pieces of the O3 differential: run the Lean oracle, CPython's
tokenizer and chokkin on one `main.py`, and say what each reports.

chokkin is a black box here: `--reporter json`, nothing else.
"""

import io
import json
import os
import pathlib
import re
import subprocess
import tempfile
import tokenize

HERE = pathlib.Path(__file__).resolve().parent
ORACLE = str(HERE.parent / ".lake" / "build" / "bin" / "oracle")
CHOKKIN = os.environ.get(
    "CHOKKIN", str(HERE.parents[5] / "target" / "release" / "chokkin")
)
WORK = os.environ.get("O3_WORK", tempfile.gettempdir())

PYPROJECT = (
    '[project]\nname = "t"\nversion = "0"\ndependencies = []\n'
    '[tool.chokkin]\nmode = "app"\nentry = ["main.py"]\n'
)

WARNINGS = {
    "unrecognized `# chokkin:` directive": "unrecognized",
    "names a rule code that is not": "unknown-code",
    "after the first statement suppresses nothing": "late-file-ignore",
    "before the first statement is on no code line": "early-ignore",
}


def oracle(paths):
    # The specs recurse once per character (they are written for proofs, not
    # speed); a large file needs more than the default 8 MiB stack.
    out = subprocess.run(
        ["sh", "-c", 'ulimit -s unlimited; exec "$0" "$@"', ORACLE, *paths],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return [json.loads(line) for line in out.splitlines()]


def chokkin(root):
    out = subprocess.run(
        [CHOKKIN, "--reporter", "json", "--no-cache", root],
        capture_output=True,
        text=True,
        check=False,
    )
    report = json.loads(out.stdout)
    chk003 = sorted(
        (i["symbol"] or i["distribution"], i["line"])
        for i in report["issues"]
        if i["code"] == "CHK003" and i["file"] == "main.py"
    )
    warns, skipped = [], False
    for d in report["diagnostics"]:
        m = d["message"]
        if "skipped `main.py`" in m:
            skipped = True
        hit = re.match(r"parse: `main\.py:(\d+)`: (.*)", m)
        if hit:
            for needle, kind in WARNINGS.items():
                if needle in hit.group(2):
                    warns.append((int(hit.group(1)), kind))
    return {
        "chk003": chk003,
        "warnings": sorted(warns),
        "skipped": skipped,
        "issues": report["issues"],
        "diagnostics": report["diagnostics"],
    }


def project(source: bytes, extra=None, pyproject=PYPROJECT):
    root = pathlib.Path(tempfile.mkdtemp(prefix="o3-", dir=WORK))
    (root / "pyproject.toml").write_text(pyproject)
    (root / "main.py").write_bytes(source)
    for name, data in (extra or {}).items():
        (root / name).write_bytes(data)
    return str(root)


def univ(text: str) -> str:
    return text.replace("\r\n", "\n").replace("\r", "\n")


def cpython_comments(text: str):
    """COMMENT tokens per CPython's tokenizer, or None when it rejects the text."""
    try:
        toks = list(tokenize.generate_tokens(io.StringIO(univ(text)).readline))
    except (SyntaxError, tokenize.TokenError):
        return None
    return [[t.start[0], t.string] for t in toks if t.type == tokenize.COMMENT]


def cpython_compiles(text: str) -> bool:
    try:
        compile(text, "main.py", "exec", dont_inherit=True)
        return True
    except (SyntaxError, ValueError):
        return False


def line_of(text: str, offset: int) -> int:
    """Language Reference §2.1.2 physical line (CR, LF, CRLF), 1-based."""
    return univ(text[:offset]).count("\n") + 1
