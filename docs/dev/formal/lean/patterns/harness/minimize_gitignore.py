"""Shrink gitignore mismatches from diff_chokkin.py to a minimal case."""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import chk
from oracle import Oracle

o = Oracle()


def mismatch(ignores, files):
    proj = {"pyproject.toml": chk.APP_PYPROJECT, "main.py": ""}
    for f in files:
        proj[f] = ""
    for d, text in ignores.items():
        proj[os.path.join(d, ".gitignore")] = text
    want = set(files) - set(o.gitignore(ignores, files))
    rep = chk.run(proj)
    got = set(files) - set(chk.codes(rep, "CHK001"))
    return (sorted(want), sorted(got)) if want != got else None


def shrink(ignores, files):
    changed = True
    while changed:
        changed = False
        for f in list(files):
            rest = [x for x in files if x != f]
            if rest and mismatch(ignores, rest):
                files, changed = rest, True
        for d in list(ignores):
            lines = ignores[d].splitlines()
            for i in range(len(lines)):
                trial = dict(ignores)
                trial[d] = "\n".join(lines[:i] + lines[i + 1:]) + "\n"
                if not trial[d].strip("\n"):
                    del trial[d]
                if mismatch(trial, files):
                    ignores, changed = trial, True
                    break
    return ignores, files


with open(sys.argv[1]) as f:
    rows = [json.loads(line)["case"] for line in f]
for case in rows:
    ig, fs = shrink(case["ignores"], case["files"])
    print(json.dumps({"ignores": ig, "files": fs, "spec_ignored_vs_chokkin": mismatch(ig, fs)}))
