"""Differential harness for the lockfile closure oracle (O5).

Generates small lock graphs, renders each one as the lockfile a tool would
write (uv / poetry / pdm / pylock), runs the chokkin binary on it as a black
box (`--reporter json`), and compares the verdict per import with the Lean
oracle's.

    python3 -I gen.py --chokkin target/release/chokkin --oracle .lake/build/bin/oracle \
        --cases 300 --seed 1 --out mismatches.jsonl

Only the rendered files and the JSON report cross the boundary: nothing here
reads chokkin's source.
"""

from __future__ import annotations

import argparse
import json
import random
import subprocess
import sys
import tempfile
from pathlib import Path

PREFIX = "oq"
EXTRAS = ["fast", "pool", "cli"]


def norm(name: str) -> str:
    out, pending = [], False
    for ch in name:
        if ch in "-_.":
            pending = True
            continue
        if pending:
            out.append("-")
            pending = False
        out.append(ch.lower())
    if pending:
        out.append("-")
    return "".join(out)


def spell(rng: random.Random, name: str, vary: bool) -> str:
    """A spelling of `name` that normalizes back to it."""
    if not vary or rng.random() < 0.5:
        return name
    parts = name.split("-")
    seps = [rng.choice("-_.") for _ in parts[1:]]
    out = parts[0]
    for sep, part in zip(seps, parts[1:], strict=True):
        out += sep + part
    return out.title() if rng.random() < 0.5 else out


# --- scenario ---------------------------------------------------------------


def scenario(rng: random.Random, fmt: str) -> dict:
    """Abstract lock: packages with base deps and per-extra deps."""
    n = rng.randint(3, 8)
    names = [f"{PREFIX}-{chr(97 + i)}{rng.randint(0, 9)}" for i in range(n)]
    names = list(dict.fromkeys(names))
    allow_extras = fmt != "pylock"
    pkgs = []
    for name in names:
        others = [m for m in names if m != name]
        base = rng.sample(others, k=min(len(others), rng.randint(0, 2)))
        optional = {}
        if allow_extras and rng.random() < 0.5:
            for extra in rng.sample(EXTRAS, k=rng.randint(1, 2)):
                pool = [m for m in others if m not in base]
                if pool:
                    optional[extra] = [
                        {"name": m, "extras": []}
                        for m in rng.sample(pool, k=min(len(pool), rng.randint(1, 2)))
                    ]
        pkgs.append(
            {
                "name": name,
                "deps": [{"name": m, "extras": []} for m in base],
                "optional": optional,
            }
        )
    by_name = {p["name"]: p for p in pkgs}
    # Edge extras: a dependency on q[x] where q has extra x.
    if allow_extras:
        for pkg in pkgs:
            for dep in pkg["deps"]:
                extras = list(by_name[dep["name"]]["optional"])
                if extras and rng.random() < 0.5:
                    dep["extras"] = [rng.choice(extras)]
    declared = []
    for name in rng.sample(names, k=rng.randint(1, 2)):
        extras = list(by_name[name]["optional"])
        chosen = [rng.choice(extras)] if extras and rng.random() < 0.6 else []
        declared.append({"name": name, "extras": chosen})
    declared_names = {d["name"] for d in declared}
    # A dev-group declaration: not installed with the project (PEP 735), so
    # it must not seed the closure of a runtime import.
    dev = []
    rest = [m for m in names if m not in declared_names]
    if rest and rng.random() < 0.4:
        dev.append({"name": rng.choice(rest), "extras": []})
    dev_names = {d["name"] for d in dev}
    imports = [m for m in names if m not in declared_names | dev_names]
    # Declarations whose own module is not imported: only what their extras
    # add is (CHK002 must still count the declaration as used).
    unimported = [d["name"] for d in declared if d["extras"] and rng.random() < 0.5]
    # One locked package becomes a real tool run from a shell script (CHK008).
    binary = None
    if rng.random() < 0.5:
        target = rng.choice(names)
        binary = "black"
        for pkg in pkgs:
            if pkg["name"] == target:
                pkg["name"] = binary
            for dep in pkg["deps"] + [d for ds in pkg["optional"].values() for d in ds]:
                if dep["name"] == target:
                    dep["name"] = binary
        for d in declared + dev:
            if d["name"] == target:
                d["name"] = binary
        imports = [binary if m == target else m for m in imports]
        unimported = [binary if m == target else m for m in unimported]
        if binary in imports:
            imports.remove(binary)
    return {
        "lock": pkgs,
        "declared": declared,
        "dev": dev,
        "imports": imports,
        "unimported": unimported,
        "binary": binary,
        "extrasKnown": allow_extras,
    }


# --- renderers --------------------------------------------------------------


def toml_str(s: str) -> str:
    return json.dumps(s)


def req(dep: dict, spelled: str, version: str = ">=1.0") -> str:
    extras = f"[{','.join(dep['extras'])}]" if dep["extras"] else ""
    return f"{spelled}{extras}{version}"


def render_uv(sc: dict, rng: random.Random) -> str:
    lines = ["version = 1", "revision = 2", 'requires-python = ">=3.11"', ""]

    def dep_line(dep):
        if dep["extras"]:
            extras = ", ".join(toml_str(e) for e in dep["extras"])
            return f"    {{ name = {toml_str(dep['name'])}, extra = [{extras}] }},"
        return f"    {{ name = {toml_str(dep['name'])} }},"

    lines += [
        "[[package]]",
        'name = "app"',
        'version = "0.1.0"',
        'source = { virtual = "." }',
    ]
    lines.append("dependencies = [")
    lines += [dep_line(d) for d in sorted(sc["declared"], key=lambda d: d["name"])]
    lines += ["]", ""]
    for pkg in sorted(sc["lock"], key=lambda p: p["name"]):
        lines += [
            "[[package]]",
            f"name = {toml_str(pkg['name'])}",
            'version = "1.0.0"',
            'source = { registry = "https://pypi.org/simple" }',
        ]
        if pkg["deps"]:
            lines.append("dependencies = [")
            lines += [dep_line(d) for d in sorted(pkg["deps"], key=lambda d: d["name"])]
            lines.append("]")
        if pkg["optional"]:
            lines += ["", "[package.optional-dependencies]"]
            for extra, deps in sorted(pkg["optional"].items()):
                lines.append(f"{extra} = [")
                lines += [dep_line(d) for d in sorted(deps, key=lambda d: d["name"])]
                lines.append("]")
        lines.append("")
    return "\n".join(lines)


def render_poetry(sc: dict, rng: random.Random) -> str:
    lines = [
        "# This file is automatically @generated by Poetry 2.1.1 and should not be changed by hand.",
        "",
    ]
    for pkg in sorted(sc["lock"], key=lambda p: p["name"]):
        lines += [
            "[[package]]",
            f"name = {toml_str(spell(rng, pkg['name'], True))}",
            'version = "1.0.0"',
            'description = ""',
            "optional = false",
            'python-versions = ">=3.8"',
            'groups = ["main"]',
            "files = []",
            "",
        ]
        optional_deps = {}
        for deps in pkg["optional"].values():
            for dep in deps:
                optional_deps.setdefault(dep["name"], dep)
        if pkg["deps"] or optional_deps:
            lines.append("[package.dependencies]")
            for dep in sorted(pkg["deps"], key=lambda d: d["name"]):
                key = spell(rng, dep["name"], True)
                if dep["extras"]:
                    extras = ", ".join(toml_str(e) for e in dep["extras"])
                    lines.append(
                        f'{toml_str(key)} = {{version = ">=1.0", extras = [{extras}]}}'
                    )
                else:
                    lines.append(f'{toml_str(key)} = ">=1.0"')
            for name in sorted(optional_deps):
                key = spell(rng, name, True)
                lines.append(
                    f'{toml_str(key)} = {{version = ">=1.0", optional = true}}'
                )
            lines.append("")
        if pkg["optional"]:
            lines.append("[package.extras]")
            for extra, deps in sorted(pkg["optional"].items()):
                reqs = ", ".join(toml_str(f"{d['name']} (>=1.0)") for d in deps)
                lines.append(f"{extra} = [{reqs}]")
            lines.append("")
    lines += [
        "[metadata]",
        'lock-version = "2.1"',
        'python-versions = ">=3.11"',
        'content-hash = "0"',
        "",
    ]
    return "\n".join(lines)


def render_pdm(sc: dict, rng: random.Random) -> str:
    lines = [
        "# This file is @generated by PDM.",
        "# It is not intended for manual editing.",
        "",
        "[metadata]",
        'groups = ["default"]',
        'strategy = ["inherit_metadata"]',
        'lock_version = "4.5.0"',
        'content_hash = "sha256:0"',
        "",
        "[[metadata.targets]]",
        'requires_python = ">=3.11"',
        "",
    ]
    requested: dict[str, set[str]] = {}
    for dep in sc["declared"]:
        for extra in dep["extras"]:
            requested.setdefault(dep["name"], set()).add(extra)
    for pkg in sc["lock"]:
        for dep in pkg["deps"]:
            for extra in dep["extras"]:
                requested.setdefault(dep["name"], set()).add(extra)

    def entry(name, extras, deps):
        spelled = spell(rng, name, True)
        out = ["[[package]]", f"name = {toml_str(spelled)}", 'version = "1.0.0"']
        if extras:
            out.append(f"extras = [{', '.join(toml_str(e) for e in extras)}]")
        out += ['requires_python = ">=3.8"', 'summary = ""', 'groups = ["default"]']
        if deps:
            out.append("dependencies = [")
            out += [f"    {toml_str(d)}," for d in deps]
            out.append("]")
        out += ["files = []", ""]
        return out

    for pkg in sorted(sc["lock"], key=lambda p: p["name"]):
        deps = [
            req(d, spell(rng, d["name"], True))
            for d in sorted(pkg["deps"], key=lambda d: d["name"])
        ]
        lines += entry(pkg["name"], [], deps)
        for extra in sorted(requested.get(pkg["name"], ())):
            extra_deps = [f"{spell(rng, pkg['name'], True)}==1.0.0"]
            extra_deps += [
                req(d, spell(rng, d["name"], True))
                for d in pkg["optional"].get(extra, [])
            ]
            lines += entry(pkg["name"], [extra], extra_deps)
    return "\n".join(lines)


def render_pylock(sc: dict, rng: random.Random) -> str:
    lines = [
        'lock-version = "1.0"',
        'requires-python = ">=3.11"',
        'created-by = "harness"',
        "",
    ]
    for pkg in sorted(sc["lock"], key=lambda p: p["name"]):
        lines += [
            "[[packages]]",
            f"name = {toml_str(spell(rng, pkg['name'], True))}",
            'version = "1.0.0"',
        ]
        if pkg["deps"]:
            deps = ", ".join(
                f"{{name = {toml_str(spell(rng, d['name'], True))}}}"
                for d in sorted(pkg["deps"], key=lambda d: d["name"])
            )
            lines.append(f"dependencies = [{deps}]")
        lines += [
            f'wheels = [{{name = "{norm(pkg["name"]).replace("-", "_")}-1.0.0-py3-none-any.whl", url = "https://example.invalid/x.whl", hashes = {{sha256 = "00"}}}}]',
            "",
        ]
    return "\n".join(lines)


RENDER = {
    "uv": ("uv.lock", render_uv),
    "uv-toml": (
        "uv.lock",
        lambda sc, rng: "# forces the TOML parser\n" + render_uv(sc, rng),
    ),
    "poetry": ("poetry.lock", render_poetry),
    "pdm": ("pdm.lock", render_pdm),
    "pylock": ("pylock.toml", render_pylock),
}


def write_project(root: Path, sc: dict, fmt: str, rng: random.Random) -> list[str]:
    declared = ", ".join(
        toml_str(req(d, spell(rng, d["name"], True), "")) for d in sc["declared"]
    )
    (root / "pyproject.toml").write_text(
        "[project]\n"
        'name = "app"\n'
        'version = "0.1.0"\n'
        'requires-python = ">=3.11"\n'
        f"dependencies = [{declared}]\n"
        + (
            "\n[dependency-groups]\ndev = ["
            + ", ".join(toml_str(req(d, d["name"], "")) for d in sc.get("dev", []))
            + "]\n"
            if sc.get("dev")
            else ""
        )
        + "\n[tool.chokkin]\n"
        'mode = "app"\n'
        'entry = ["main.py"]\n'
    )
    imports = [
        norm(d["name"]).replace("-", "_")
        for d in sc["declared"]
        if d["name"] not in sc.get("unimported", []) and d["name"] != sc.get("binary")
    ]
    imports += [norm(name).replace("-", "_") for name in sc["imports"]]
    (root / "main.py").write_text("".join(f"import {m}\n" for m in imports))
    if sc.get("binary"):
        (root / "ci.sh").write_text(f"#!/bin/sh\n{sc['binary']} .\n")
    file_name, render = RENDER[fmt]
    (root / file_name).write_text(render(sc, rng))
    return imports


def chokkin_verdicts(binary: str, root: Path) -> dict[str, str]:
    proc = subprocess.run(
        [binary, "--reporter", "json", "--no-cache", str(root)],
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    report = json.loads(proc.stdout)
    out = {}
    for issue in report["issues"]:
        if issue["code"] == "CHK002" and issue.get("distribution"):
            out[f"CHK002:{norm(issue['distribution'])}"] = "CHK002"
            continue
        if issue["code"] == "CHK008":
            out[f"CHK008:{issue.get('binary')}"] = "CHK008"
            continue
        if issue.get("file") != "main.py" or not issue.get("distribution"):
            continue
        code = issue["code"]
        if code == "CHK004":
            out[norm(issue["distribution"])] = f"CHK004/{issue['confidence']}"
        elif code == "CHK003":
            out[norm(issue["distribution"])] = "CHK003"
    return out


RANK = {
    "CHK003": 0,
    "CHK004/likely": 1,
    "CHK004/certain": 2,
    "none": 3,
    "CHK002": 0,
    "CHK008": 0,
}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--chokkin", required=True)
    ap.add_argument("--oracle", required=True)
    ap.add_argument("--cases", type=int, default=100)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--formats", default="uv,uv-toml,poetry,pdm,pylock")
    ap.add_argument("--out", default="mismatches.jsonl")
    args = ap.parse_args()

    oracle = subprocess.Popen(
        [args.oracle], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True
    )
    stats: dict[str, int] = {}
    with open(args.out, "w") as out:
        for i in range(args.cases):
            for fmt in args.formats.split(","):
                rng = random.Random(f"{args.seed}:{i}:{fmt}")
                sc = scenario(rng, fmt.split("-")[0])
                oracle.stdin.write(json.dumps(sc) + "\n")
                oracle.stdin.flush()
                expected = json.loads(oracle.stdout.readline())
                assert expected.get("closed"), expected
                with tempfile.TemporaryDirectory() as tmp:
                    root = Path(tmp)
                    write_project(root, sc, fmt, rng)
                    lock_text = (root / RENDER[fmt][0]).read_text()
                    actual = chokkin_verdicts(args.chokkin, root)
                checks = dict(expected["verdicts"])
                # CHK002: a declaration is used when it or what its extras add
                # is imported (extras known), or it is imported itself.
                for d in sc["declared"]:
                    if d["name"] in sc["unimported"] and sc["extrasKnown"]:
                        checks[f"CHK002:{d['name']}"] = "none"
                # CHK008: the tool is installed iff the lock closure of every
                # declaration (any group) reaches it.
                if sc["binary"]:
                    q = dict(
                        sc, declared=sc["declared"] + sc["dev"], imports=[sc["binary"]]
                    )
                    oracle.stdin.write(json.dumps(q) + "\n")
                    oracle.stdin.flush()
                    prov = json.loads(oracle.stdout.readline())["verdicts"][
                        sc["binary"]
                    ]
                    provided = prov in ("none", "CHK004/certain")
                    # Without extras data (pylock) only base edges count.
                    checks[f"CHK008:{sc['binary']}"] = "none" if provided else "CHK008"
                for dist, want in checks.items():
                    got = actual.get(dist, "none")
                    key = f"{fmt}:{want}->{got}"
                    stats[key] = stats.get(key, 0) + 1
                    if got != want:
                        direction = (
                            "under" if RANK.get(got, 9) < RANK.get(want, 9) else "over"
                        )
                        out.write(
                            json.dumps(
                                {
                                    "case": i,
                                    "format": fmt,
                                    "dist": dist,
                                    "want": want,
                                    "got": got,
                                    "direction": direction,
                                    "scenario": sc,
                                    "lock": lock_text,
                                }
                            )
                            + "\n"
                        )
    for key, count in sorted(stats.items()):
        want_got = key.split(":", 1)[1]
        mark = (
            ""
            if want_got.split("->")[0] == want_got.split("->")[1]
            else "  <-- mismatch"
        )
        print(f"{key:45} {count}{mark}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
