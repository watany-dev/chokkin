"""Run the closure oracle on real lockfiles from OSS clones.

For each clone, read its lockfile with `tomllib` into the oracle's model (by
each format's own rules, written here independently of chokkin), pick random
declaration sets from the lock's packages, and compare chokkin's verdict for
an `import` of every other locked package.

    python3 -I real.py --chokkin ... --oracle ... --clones /path/to/clones \
        --samples 5 --out real-mismatches.jsonl
"""

from __future__ import annotations

import argparse
import json
import random
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import tomllib

sys.path.insert(0, str(Path(__file__).parent))
from gen import RANK, chokkin_verdicts, norm, toml_str

NAME = re.compile(r"^\s*([A-Za-z0-9][A-Za-z0-9._-]*)\s*(?:\[([^\]]*)\])?")


def pep508_head(spec: str) -> tuple[str, list[str]] | None:
    m = NAME.match(spec)
    if not m:
        return None
    extras = [norm(e.strip()) for e in (m.group(2) or "").split(",") if e.strip()]
    return norm(m.group(1)), extras


def model_uv(data: dict) -> list[dict]:
    def dep(d):
        if isinstance(d, str):
            return {"name": norm(d), "extras": []}
        return {
            "name": norm(d["name"]),
            "extras": [norm(e) for e in d.get("extra", [])],
        }

    return [
        {
            "name": norm(p["name"]),
            "deps": [dep(d) for d in p.get("dependencies", [])],
            "optional": {
                norm(e): [dep(d) for d in ds]
                for e, ds in p.get("optional-dependencies", {}).items()
            },
        }
        for p in data.get("package", [])
        if "name" in p
    ]


def model_poetry(data: dict) -> list[dict]:
    out = []
    for p in data.get("package", []):
        base, optional_names = [], {}
        for key, value in p.get("dependencies", {}).items():
            values = value if isinstance(value, list) else [value]
            extras, optional = set(), False
            for v in values:
                if isinstance(v, dict):
                    extras.update(norm(e) for e in v.get("extras", []))
                    optional = optional or bool(v.get("optional", False))
            d = {"name": norm(key), "extras": sorted(extras)}
            if optional:
                optional_names[norm(key)] = d
            else:
                base.append(d)
        opt = {}
        for extra, reqs in p.get("extras", {}).items():
            deps = []
            for r in reqs:
                head = pep508_head(r)
                if head:
                    name, extras = head
                    d = dict(optional_names.get(name, {"name": name, "extras": []}))
                    d["extras"] = sorted(set(d["extras"]) | set(extras))
                    deps.append(d)
            opt[norm(extra)] = deps
        out.append({"name": norm(p["name"]), "deps": base, "optional": opt})
    return out


def model_pdm(data: dict) -> list[dict]:
    out = []
    for p in data.get("package", []):
        name = norm(p["name"])
        deps = []
        for r in p.get("dependencies", []):
            head = pep508_head(r)
            if head and head[0] != name:
                deps.append({"name": head[0], "extras": head[1]})
        extras = [norm(e) for e in p.get("extras", [])]
        if extras:
            out.append(
                {"name": name, "deps": [], "optional": dict.fromkeys(extras, deps)}
            )
        else:
            out.append({"name": name, "deps": deps, "optional": {}})
    return out


def model_pylock(data: dict) -> list[dict]:
    return [
        {
            "name": norm(p["name"]),
            "deps": [
                {"name": norm(d["name"]), "extras": []}
                for d in p.get("dependencies", [])
                if "name" in d
            ],
            "optional": {},
        }
        for p in data.get("packages", [])
        if "name" in p
    ]


def pick_lock(clone: Path):
    for name, fmt in [("uv.lock", "uv"), ("pylock.toml", "pylock")]:
        if (clone / name).is_file():
            return clone / name, fmt
    named = sorted(p for p in clone.glob("pylock.*.toml") if p.name.count(".") == 2)
    if named:
        return named[0], "pylock"
    for name, fmt in [("poetry.lock", "poetry"), ("pdm.lock", "pdm")]:
        if (clone / name).is_file():
            return clone / name, fmt
    return None


MODEL = {
    "uv": model_uv,
    "poetry": model_poetry,
    "pdm": model_pdm,
    "pylock": model_pylock,
}
STDLIB = set(sys.stdlib_module_names)


def importable(name: str) -> bool:
    mod = name.replace("-", "_")
    return mod.isidentifier() and mod not in STDLIB and not mod.startswith("_")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--chokkin", required=True)
    ap.add_argument("--oracle", required=True)
    ap.add_argument("--clones", required=True)
    ap.add_argument("--samples", type=int, default=5)
    ap.add_argument("--max-imports", type=int, default=150)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--out", default="real-mismatches.jsonl")
    args = ap.parse_args()

    oracle = subprocess.Popen(
        [args.oracle], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True
    )
    stats: dict[str, int] = {}
    with open(args.out, "w") as out:
        for clone in sorted(Path(args.clones).iterdir()):
            picked = pick_lock(clone)
            if not picked:
                continue
            path, fmt = picked
            lock = MODEL[fmt](tomllib.loads(path.read_text()))
            names = sorted({p["name"] for p in lock})
            by_name: dict[str, set[str]] = {}
            for p in lock:
                by_name.setdefault(p["name"], set()).update(p["optional"])
            for s in range(args.samples):
                rng = random.Random(f"{args.seed}:{clone.name}:{s}")
                declared = []
                for name in rng.sample(names, k=min(len(names), rng.randint(1, 4))):
                    extras = sorted(by_name[name])
                    chosen = (
                        rng.sample(extras, k=min(len(extras), rng.randint(0, 2)))
                        if extras
                        else []
                    )
                    declared.append({"name": name, "extras": chosen})
                dnames = {d["name"] for d in declared}
                imports = [n for n in names if n not in dnames and importable(n)]
                rng.shuffle(imports)
                imports = sorted(imports[: args.max_imports])
                sc = {
                    "lock": lock,
                    "declared": declared,
                    "imports": imports,
                    "extrasKnown": fmt != "pylock",
                }
                oracle.stdin.write(json.dumps(sc) + "\n")
                oracle.stdin.flush()
                expected = json.loads(oracle.stdout.readline())
                if not expected.get("closed"):
                    print(f"{clone.name}: oracle did not converge", file=sys.stderr)
                    continue
                with tempfile.TemporaryDirectory() as tmp:
                    root = Path(tmp)
                    reqs = ", ".join(
                        toml_str(
                            d["name"]
                            + (f"[{','.join(d['extras'])}]" if d["extras"] else "")
                        )
                        for d in declared
                    )
                    (root / "pyproject.toml").write_text(
                        f'[project]\nname = "oq-app"\nversion = "0.1.0"\nrequires-python = ">=3.9"\n'
                        f'dependencies = [{reqs}]\n\n[tool.chokkin]\nmode = "app"\nentry = ["main.py"]\n'
                    )
                    (root / "main.py").write_text(
                        "".join(f"import {n.replace('-', '_')}\n" for n in imports)
                    )
                    shutil.copy(path, root / path.name)
                    actual = chokkin_verdicts(args.chokkin, root)
                for dist, want in expected["verdicts"].items():
                    got = actual.get(dist, "none")
                    key = f"{fmt}:{want}->{got}"
                    stats[key] = stats.get(key, 0) + 1
                    if got != want:
                        out.write(
                            json.dumps(
                                {
                                    "clone": clone.name,
                                    "format": fmt,
                                    "dist": dist,
                                    "want": want,
                                    "got": got,
                                    "direction": "under"
                                    if RANK[got] < RANK[want]
                                    else "over",
                                    "declared": declared,
                                }
                            )
                            + "\n"
                        )
    for key, count in sorted(stats.items()):
        a, b = key.split(":", 1)[1].split("->")
        print(f"{key:45} {count}{'' if a == b else '  <-- mismatch'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
