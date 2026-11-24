"""Run the spec and chokkin on a real checkout and list CHK006 / CHK007
findings whose symbol the spec shows being read from another module.

usage: python3 -I oss.py <chokkin> <oracle> <repo> [source-root ...]

Source roots are the directories module names are relative to (``src`` for a
src layout); the repository root is always added for tests and scripts.
"""

import json
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import py2ir


def main():
    chokkin, oracle, repo, *roots = sys.argv[1:]
    roots = [os.path.join(repo, r) for r in roots] + [repo]
    seen, mods = set(), []
    for root in roots:
        for f in py2ir.walk(root):
            if f in seen:
                continue
            seen.add(f)
            part = py2ir.project(root, [f])["modules"]
            for m in part:
                m["file"] = os.path.relpath(f, repo)
            mods += part
    ir = repo.rstrip("/") + ".ir.json"
    with open(ir, "w") as fh:
        json.dump({"modules": mods}, fh)
    out = subprocess.run([oracle, ir], capture_output=True, text=True, check=True)
    refs = {}
    for imp, m, n in json.loads(out.stdout)["refs"]:
        refs.setdefault((m, n), imp)
    file_of = {m["name"]: m["file"] for m in mods}
    spec = {(file_of.get(m, m), n): imp for (m, n), imp in refs.items()}
    data = json.loads(subprocess.run([chokkin, "--reporter", "json", "--no-cache", repo],
                                     capture_output=True, text=True, check=False).stdout)
    n = 0
    for i in data["issues"]:
        if i["code"] not in ("CHK006", "CHK007") or not i.get("symbol"):
            continue
        n += 1
        name = i["symbol"].partition(":")[2]
        key = (i["file"], name)
        if key in spec:
            print(json.dumps({"code": i["code"], "severity": i["severity"], "confidence": i["confidence"],
                              "file": i["file"], "name": name, "read_in": spec[key]}))
    print(json.dumps({"chk006_007": n, "spec_refs": len(spec)}), file=sys.stderr)


if __name__ == "__main__":
    main()
