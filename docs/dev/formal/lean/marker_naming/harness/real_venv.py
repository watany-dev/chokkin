"""O7 on real wheels: read every `*.dist-info` / `*.egg-info` of a real
site-packages into the Lean spec, ask which distributions the given imports
use, and run chokkin on a project that declares those distributions and
makes those imports with that venv.

usage: real_venv.py <venv> <import> [<import> ...]
"""

import json
import os
import pathlib
import subprocess
import sys
import tempfile

from common import CHOKKIN, oracle


def read_dists(site):
    dists = []
    for info in sorted(site.iterdir()):
        if info.suffix == ".dist-info":
            meta, files, kind = info / "METADATA", info / "RECORD", "wheel"
        elif info.suffix == ".egg-info":
            meta, files, kind = info / "PKG-INFO", info / "installed-files.txt", "egg"
        else:
            continue
        top = info / "top_level.txt"
        dist = {
            "kind": kind,
            "metadata": meta.read_text(errors="replace") if meta.exists() else "",
            "files": files.read_text(errors="replace") if files.exists() else "",
            "dir_name": info.name.split("-")[0],
        }
        if top.exists():
            dist["top_level"] = top.read_text()
        dists.append(dist)
    return dists


def main(venv, imports):
    venv = pathlib.Path(venv).resolve()
    site = next(venv.glob("lib/python3*/site-packages"))
    [answer] = oracle([{"kind": "venv", "dists": read_dists(site), "imports": imports}])
    used = sorted({u for a in answer for u in a["used"]})
    with tempfile.TemporaryDirectory() as d:
        os.symlink(venv, pathlib.Path(d, ".venv"))
        pathlib.Path(d, "pyproject.toml").write_text(
            '[project]\nname = "zzproj"\nversion = "0"\nrequires-python = ">=3.9"\n'
            f"dependencies = {json.dumps(used)}\n"
            '[tool.chokkin]\nmode = "app"\nentry = ["main.py"]\n'
        )
        pathlib.Path(d, "main.py").write_text("".join(f"import {m}\n" for m in imports))
        out = subprocess.run(
            [CHOKKIN, "--reporter", "json", "--no-cache"],
            cwd=d,
            capture_output=True,
            text=True,
            check=False,
        )
        report = json.loads(out.stdout)
    print(
        json.dumps(
            {
                "spec_used": used,
                "chokkin": [
                    (i["code"], i["target"], i["confidence"]) for i in report["issues"]
                ],
            },
            indent=1,
        )
    )


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2:])
