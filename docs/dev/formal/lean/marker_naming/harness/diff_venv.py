"""O7: which installed distribution an import uses (PEP 420 / 503 / 794,
Core Metadata, RECORD, egg-info).

Builds a project with a `.venv` whose distributions the Lean spec also
reads, imports some of their modules from `main.py`, and declares every
distribution. Expected: CHK002 exactly for the distributions no import uses
(the spec's `used`), and no CHK003 / CHK010 (every import exists).
"""

import os
import random
import sys

from common import chokkin, issues, oracle, pyproject

SP = ".venv/lib/python3.12/site-packages/"
WORDS = [
    "alpha",
    "bravo",
    "carbon",
    "delta",
    "ember",
    "flint",
    "garnet",
    "harbor",
    "iris",
    "jade",
]
EXT = [".cpython-312-x86_64-linux-gnu.so", ".abi3.so", ".so"]
LAYOUTS = ["package", "module", "ext", "portion", "portion", "editable", "egg"]
# Features to leave out once their disagreement is filed, so new ones show:
# SKIP=ext,egg,portion,private,body,lc
SKIP = set(filter(None, os.environ.get("SKIP", "").split(",")))


def spelling(rng, words):
    sep = rng.choice(["-", "_", ".", "-"])
    name = sep.join(words)
    return name.title() if rng.random() < 0.3 else name


def make_dist(rng, idx, namespace):
    words = ["zz", WORDS[idx]]
    canonical = "-".join(words)
    mod = "zz" + WORDS[idx]
    layout = rng.choice([l for l in LAYOUTS if l not in SKIP])
    meta_name = spelling(rng, words)
    field = "name" if rng.random() < 0.15 and "lc" not in SKIP else "Name"
    headers = ["Metadata-Version: 2.4", f"{field}: {meta_name}", "Version: 1.0"]
    files, top_level, kind, imports = [], None, "wheel", []
    if layout == "package":
        files = [f"{mod}/__init__.py", f"{mod}/core.py"]
        imports = [mod, f"{mod}.core"]
    elif layout == "module":
        files = [f"{mod}.py"]
        imports = [mod]
    elif layout == "ext":
        files = [f"{mod}{rng.choice(EXT)}"]
        imports = [mod]
    elif layout == "portion":
        files = [f"{namespace}/{WORDS[idx]}/__init__.py"]
        imports = [f"{namespace}.{WORDS[idx]}"]
    elif layout == "editable":
        kind = "editable"
        files = [f"__editable__.{canonical.replace('-', '_')}-1.0.pth"]
        value = rng.choice(
            [mod] if "private" in SKIP else [mod, f"{mod} ; private", f"{mod};private"]
        )
        headers.append(f"Import-Name: {value}")
        imports = [mod]
    else:
        kind = "egg"
        files = [f"../{mod}/__init__.py"]
        top_level = mod + "\n"
        imports = [mod]
    if rng.random() < 0.3:
        top_level = (
            (top_level or "") + (mod if layout != "portion" else namespace) + "\n"
        )
    body = ""
    if rng.random() < 0.25 and "body" not in SKIP:
        body = "\nUsage notes.\n\nImport-Name: {other}\n"
    return {
        "canonical": canonical,
        "layout": layout,
        "kind": kind,
        "headers": headers,
        "body": body,
        "files": files,
        "top_level": top_level,
        "imports": imports,
        "mod": mod,
    }


def build(rng):
    namespace = "zz" + rng.choice(["corp", "org"])
    n = rng.randint(2, 4)
    dists = [make_dist(rng, i, namespace) for i in rng.sample(range(len(WORDS)), n)]
    others = [d["mod"] for d in dists]
    for d in dists:
        d["body"] = d["body"].replace("{other}", rng.choice(others))
    imports = [rng.choice(d["imports"]) for d in dists if rng.random() < 0.6]
    return dists, imports


def files_of(dists, imports):
    out = {".venv/pyvenv.cfg": "version = 3.12.1\n"}
    for d in dists:
        stem = d["canonical"].replace("-", "_")
        meta = "\n".join(d["headers"]) + "\n" + d["body"]
        if d["kind"] == "egg":
            info = f"{SP}{stem}-1.0-py3.12.egg-info/"
            out[info + "PKG-INFO"] = meta
            out[info + "installed-files.txt"] = "\n".join(d["files"]) + "\n"
            for f in d["files"]:
                out[SP + f[3:]] = ""
        else:
            info = f"{SP}{stem}-1.0.dist-info/"
            out[info + "METADATA"] = meta
            out[info + "RECORD"] = (
                "".join(f"{f},,\n" for f in d["files"])
                + f"{stem}-1.0.dist-info/RECORD,,\n"
            )
            for f in d["files"]:
                out[SP + f] = ""
        if d["top_level"] is not None:
            out[info + "top_level.txt"] = d["top_level"]
    out["main.py"] = "".join(f"import {m}\n" for m in imports) or "print(1)\n"
    out["pyproject.toml"] = pyproject([d["canonical"] for d in dists])
    return out


def query(dists, imports):
    return {
        "kind": "venv",
        "imports": imports,
        "dists": [
            {
                "kind": d["kind"],
                "metadata": "\n".join(d["headers"]) + "\n" + d["body"],
                "files": "\n".join(f"{f},," for f in d["files"]),
                "dir_name": d["canonical"],
                **({"top_level": d["top_level"]} if d["top_level"] is not None else {}),
            }
            for d in dists
        ],
    }


def main(seed, rounds):
    rng = random.Random(seed)
    cases = [build(rng) for _ in range(rounds)]
    answers = oracle([query(*c) for c in cases])
    bad = 0
    for (dists, imports), answer in zip(cases, answers, strict=True):
        used = {u for a in answer for u in a["used"]}
        declared = {d["canonical"] for d in dists}
        report = chokkin(files_of(dists, imports))
        got002 = issues(report, "CHK002")
        extra = sorted(issues(report, "CHK003") | issues(report, "CHK010"))
        fp = sorted(got002 & used)
        fn = sorted((declared - used) - got002)
        if fp or fn or extra:
            bad += 1
            layouts = {
                d["canonical"]: d["layout"]
                + ("+body" if d["body"] else "")
                + ("+lc" if d["headers"][1].startswith("name") else "")
                for d in dists
            }
            print(
                f"FP={fp} FN={fn} CHK003/010={extra} imports={imports} layouts={layouts}"
            )
    print(f"# {rounds} venvs, {bad} disagree", file=sys.stderr)


if __name__ == "__main__":
    main(
        int(sys.argv[1]) if len(sys.argv) > 1 else 0,
        int(sys.argv[2]) if len(sys.argv) > 2 else 20,
    )
