"""Differential run: Lean binding spec vs chokkin (black box, --reporter json).

usage: python3 -I diff.py <chokkin> <oracle> <workdir> [--filter SUBSTR] [--jobs N]

For each generated case the project is written to disk, converted to IR
(py2ir, ``ast.parse`` only) and fed to the Lean oracle, and chokkin is run on
it. Outcomes per case:

  FP           the spec derives a reference to a symbol chokkin reports as
               CHK006 / CHK007 (chokkin claims unused, the spec shows a read)
  FN           negative case: by the spec nothing reads ``pkg.m:target``, yet
               chokkin does not report it
  SPEC_MISS    positive case where the spec derives no reference (spec gap)
  SPEC_CONTROL the spec derives a reference to a control symbol (spec bug)
  CONTROL      chokkin does not report the control ``pkg.m:other``
"""

import json
import os
import shutil
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gen
import py2ir

CONTROLS = {("pkg.m", "other"), ("pkg.m", "Other"), ("pkg.m", "OTHER")}


def write(root, files):
    shutil.rmtree(root, ignore_errors=True)
    for rel, text in files.items():
        p = os.path.join(root, *rel.split("/"))
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w") as fh:
            fh.write(text)


def spec_refs(oracle, root):
    ir = py2ir.project(root, py2ir.walk(root))
    path = root + ".ir.json"
    with open(path, "w") as fh:
        json.dump(ir, fh)
    out = subprocess.run([oracle, path], capture_output=True, text=True, timeout=120, check=False)
    if out.returncode != 0:
        raise RuntimeError(out.stderr)
    return {(m, n) for (_i, m, n) in json.loads(out.stdout)["refs"]}


def chokkin_unused(chokkin, root):
    out = subprocess.run(
        [chokkin, "--reporter", "json", "--no-cache", root],
        capture_output=True, text=True, timeout=120, check=False,
    )
    data = json.loads(out.stdout)
    got = {}
    for i in data["issues"]:
        if i["code"] in ("CHK006", "CHK007") and i.get("symbol"):
            m, _, n = i["symbol"].partition(":")
            got[(m, n)] = (i["code"], i["confidence"])
    return got, data


def run_case(args):
    chokkin, oracle, work, (name, files, expected) = args
    root = os.path.join(work, name.replace(":", "_").replace("+", "__"))
    write(root, files)
    res = {"case": name, "outcomes": []}
    try:
        refs = spec_refs(oracle, root)
        unused, _ = chokkin_unused(chokkin, root)
    except Exception as exc:  # noqa: BLE001
        res["outcomes"].append(("ERROR", str(exc)[:300]))
        return res
    for sym in refs & set(unused):
        res["outcomes"].append(("FP", sym, unused[sym]))
    for sym in expected:
        if sym not in refs:
            res["outcomes"].append(("SPEC_MISS", sym))
    for sym in refs & CONTROLS:
        res["outcomes"].append(("SPEC_CONTROL", sym))
    if name.startswith("neg:"):
        t = ("pkg.m", "target")
        if t in refs:
            res["outcomes"].append(("SPEC_CONTROL", t))
        elif t not in unused:
            res["outcomes"].append(("FN", t))
    if ("pkg.m", "other") not in unused:
        res["outcomes"].append(("CONTROL", ("pkg.m", "other")))
    if not res["outcomes"]:
        shutil.rmtree(root, ignore_errors=True)
        os.remove(root + ".ir.json")
    return res


def main():
    chokkin, oracle, work = sys.argv[1:4]
    flt = None
    jobs = os.cpu_count() or 4
    rest = sys.argv[4:]
    while rest:
        a = rest.pop(0)
        if a == "--filter":
            flt = rest.pop(0)
        elif a == "--jobs":
            jobs = int(rest.pop(0))
    os.makedirs(work, exist_ok=True)
    cases = [c for c in gen.all_cases() if flt is None or flt in c[0]]
    tally = {}
    with ThreadPoolExecutor(jobs) as ex:
        for res in ex.map(run_case, [(chokkin, oracle, work, c) for c in cases]):
            for o in res["outcomes"]:
                tally[o[0]] = tally.get(o[0], 0) + 1
                print(json.dumps({"case": res["case"], "outcome": o}))
    print(json.dumps({"cases": len(cases), "tally": tally}), file=sys.stderr)


if __name__ == "__main__":
    main()
