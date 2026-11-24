"""Metamorphic check: chokkin's verdicts on a real uv.lock must not change when
the file is rewritten in a TOML-equivalent layout (a leading comment, CRLF
line endings), since TOML gives all of them the same table."""

import json
import random
import sys
import tempfile
from pathlib import Path

import tomllib

sys.path.insert(0, str(Path(__file__).parent))
from gen import chokkin_verdicts, toml_str
from real import importable, model_uv

VARIANTS = {
    "comment": lambda t: "# layout probe\n" + t,
    "crlf": lambda t: t.replace("\n", "\r\n"),
    "trailing-newlines": lambda t: t + "\n\n",
}


def main():
    binary, clones = sys.argv[1], Path(sys.argv[2])
    bad = 0
    for clone in sorted(clones.iterdir()):
        lock = clone / "uv.lock"
        if not lock.is_file():
            continue
        text = lock.read_text()
        assert all(
            tomllib.loads(v(text)) == tomllib.loads(text) for v in VARIANTS.values()
        )
        model = model_uv(tomllib.loads(text))
        names = sorted({p["name"] for p in model})
        rng = random.Random(clone.name)
        declared = []
        for name in rng.sample(names, k=min(3, len(names))):
            extras = sorted(
                {e for p in model if p["name"] == name for e in p["optional"]}
            )
            declared.append(name + (f"[{extras[0]}]" if extras else ""))
        imports = [n for n in names if importable(n)][:200]
        results = {}
        for label, rewrite in [("original", lambda t: t), *VARIANTS.items()]:
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                (root / "pyproject.toml").write_text(
                    f'[project]\nname = "oq-app"\nversion = "0.1.0"\ndependencies = [{", ".join(toml_str(d) for d in declared)}]\n'
                    '[tool.chokkin]\nmode = "app"\nentry = ["main.py"]\n'
                )
                (root / "main.py").write_text(
                    "".join(f"import {n.replace('-', '_')}\n" for n in imports)
                )
                (root / "uv.lock").write_bytes(rewrite(text).encode())
                results[label] = chokkin_verdicts(binary, root)
        for label in VARIANTS:
            if results[label] != results["original"]:
                bad += 1
                diff = {
                    k: (results["original"].get(k), results[label].get(k))
                    for k in set(results[label]) | set(results["original"])
                    if results["original"].get(k) != results[label].get(k)
                }
                print(clone.name, label, json.dumps(diff)[:400])
    print("layout mismatches:", bad)


main()
