#!/usr/bin/env python3
"""Differential oracle: chokkin vs other Python dead-code / dependency checkers (#340).

Runs chokkin and pinned versions of vulture, deadcode, deptry, fawltydeps,
ruff (F401) and pyflakes over every cloned project of the pinned 20-project
corpus, normalizes each tool's findings to a comparable key per chokkin rule,
and reports per (rule, tool) how many findings agree, are chokkin-only, or are
other-only.

All tools are static analyzers: nothing from the analyzed projects is executed,
so this is as safe as `make oss-metrics`. The other tools run via `uvx` at the
versions in TOOLS below.

Rule mapping (key compared within one project):
  CHK002 unused dep        deptry DEP002, fawltydeps unused_deps
                           key: distribution name (PEP 503 normalized)
  CHK003 missing dep       deptry DEP001, fawltydeps undeclared_deps
                           key: top-level import name
  CHK004 transitive dep    deptry DEP003                key: top-level import name
  CHK005 misplaced dep     deptry DEP004                key: top-level import name
  CHK006 unused export     vulture / deadcode unused function, class, variable
                           key: (file, name), restricted to public (no leading
                           underscore) names bound at module top level, the
                           same scope CHK006 reports on
  CHK007 unused re-export  ruff F401 (--isolated), pyflakes "imported but unused"
                           key: (file, bound name), __init__.py files only

Other-tool findings located only in tests/, docs/, examples/, scripts/ and
similar non-shipped paths (see in_scope) are dropped before comparing: those
tools scan everything under the root, chokkin's CHK006/CHK007 only report on
first-party package modules, so such hits are a scope difference, not a miss.

Note the claims differ: vulture/deadcode say "unused anywhere", CHK006 says
"not referenced from outside its module", so a module-internal helper is
expected chokkin-only; other-only CHK006 entries are the interesting misses.

Usage:
  scripts/oss-differential.py [OPTIONS]

Options:
  -m, --manifest PATH   Clone list (default: scripts/oss-clones.manifest)
  -c, --clones DIR      Clone root (default: target/oss-clones)
  -o, --output DIR      Output directory (default: target/oss-diff)
  -b, --bin PATH        chokkin binary (default: target/release/chokkin)
  --envs DIR            Per-project venvs (scripts/oss-provision-envs.py); when
                        present, fawltydeps maps imports through <DIR>/<slug>
                        (default: target/oss-envs)
  --projects a,b        Only these slugs
  --build               cargo build --release before running
  -h, --help            Show help

Outputs: findings.tsv (slug, rule, tool, key), summary.json, report.md and the
raw tool output under raw/<slug>/.
"""

from __future__ import annotations

import argparse
import ast
import json
import re
import shutil
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

import oss_corpus as oc

TOOLS = {
    "vulture": ["uvx", "--from", "vulture==2.14", "vulture"],
    "deadcode": ["uvx", "--from", "deadcode==2.4.1", "deadcode"],
    "deptry": ["uvx", "--from", "deptry==0.23.0", "deptry"],
    "fawltydeps": ["uvx", "--from", "fawltydeps==0.20.0", "fawltydeps"],
    "ruff": ["uvx", "ruff@0.12.0"],
    "pyflakes": ["uvx", "--from", "pyflakes==3.4.0", "pyflakes"],
}
RULE_TOOLS = {
    "CHK002": ("deptry", "fawltydeps"),
    "CHK003": ("deptry", "fawltydeps"),
    "CHK004": ("deptry",),
    "CHK005": ("deptry",),
    "CHK006": ("vulture", "deadcode"),
    "CHK007": ("ruff", "pyflakes"),
}
DEPTRY_RULE = {"DEP001": "CHK003", "DEP002": "CHK002", "DEP003": "CHK004", "DEP004": "CHK005"}
VULTURE_RE = re.compile(r"^(.+?):(\d+): unused (function|class|variable) '([^']+)'")
DEADCODE_RE = re.compile(r"^(.+?):(\d+):\d+: (DC0[123]) \w+ `([^`]+)` is never used")
PYFLAKES_RE = re.compile(r"^(.+?):(\d+):\d+:? '([^']+)' imported but unused")
RUFF_NAME_RE = re.compile(r"`([^`]+)` imported but unused")
EXAMPLES = 8  # disagreement examples per cell in report.md


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(add_help=False)
    p.add_argument("-m", "--manifest", type=Path, default=oc.DEFAULT_MANIFEST)
    p.add_argument("-c", "--clones", type=Path, default=oc.DEFAULT_CLONES)
    p.add_argument("-o", "--output", type=Path, default=oc.ROOT / "target/oss-diff")
    p.add_argument("-b", "--bin", type=Path, default=oc.DEFAULT_BIN)
    p.add_argument("--envs", type=Path, default=oc.DEFAULT_ENVS)
    p.add_argument("--projects", default="")
    p.add_argument("--build", action="store_true")
    p.add_argument("-h", "--help", action="store_true")
    args = p.parse_args()
    if args.help:
        print(__doc__)
        sys.exit(0)
    return args


def norm_dist(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def top_import(name: str) -> str:
    return name.split(".", 1)[0].lower()


def bound_name(imported: str) -> str:
    """Name an `imported but unused` message refers to, as bound in the module."""
    if " as " in imported:
        return imported.rsplit(" as ", 1)[1].strip()
    return imported.rstrip(".").rsplit(".", 1)[-1]


class TopLevel:
    """Public names bound at module top level, per file (parsed, never run)."""

    def __init__(self, proj: Path):
        self.proj = proj
        self.cache: dict[str, set[str]] = {}

    def names(self, rel: str) -> set[str]:
        if rel not in self.cache:
            out: set[str] = set()
            try:
                tree = ast.parse((self.proj / rel).read_bytes())
            except (OSError, SyntaxError, ValueError):
                tree = ast.Module(body=[], type_ignores=[])
            stack = list(tree.body)
            while stack:
                node = stack.pop()
                if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                    out.add(node.name)
                elif isinstance(node, (ast.Assign, ast.AnnAssign, ast.AugAssign)):
                    targets = node.targets if isinstance(node, ast.Assign) else [node.target]
                    for t in targets:
                        out.update(n.id for n in ast.walk(t) if isinstance(n, ast.Name))
                elif isinstance(node, (ast.If, ast.Try)):
                    # Conditional top-level definitions (version / import guards).
                    stack += node.body + node.orelse
                    stack += getattr(node, "finalbody", [])
                    for h in getattr(node, "handlers", []):
                        stack += h.body
            self.cache[rel] = {n for n in out if not n.startswith("_")}
        return self.cache[rel]


OUT_OF_SCOPE_DIRS = {
    "tests", "test", "testing", "docs", "doc", "examples", "example", "scripts",
    "benchmarks", "bench", "tools", "tasks", ".github", "docs_src",
    "build", "dist", ".venv", ".tox", ".nox",
}
OUT_OF_SCOPE_FILES = {"conftest.py", "setup.py", "noxfile.py", "tasks.py", "fabfile.py"}


def in_scope(rel: str) -> bool:
    """Shipped-code paths: what the other tools report in tests/docs/examples
    is a scope difference, not a chokkin miss, so it is dropped before comparing."""
    p = Path(rel)
    if any(part in OUT_OF_SCOPE_DIRS for part in p.parts[:-1]):
        return False
    name = p.name
    return not (name in OUT_OF_SCOPE_FILES or name.startswith("test_") or name.endswith("_test.py"))


def run_tool(cmd: list[str], cwd: Path, log: Path) -> str:
    proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, check=False)
    log.write_text(proc.stdout + ("\n--- stderr ---\n" + proc.stderr if proc.stderr else ""),
                   encoding="utf-8")
    return proc.stdout


def rel_path(proj: Path, path: str) -> str:
    p = Path(path)
    if p.is_absolute():
        try:
            p = p.relative_to(proj)
        except ValueError:
            pass
    return p.as_posix().removeprefix("./")


def chokkin_keys(report: dict) -> dict[str, set]:
    keys: dict[str, set] = defaultdict(set)
    for i in report.get("issues", []):
        code = i["code"]
        if code == "CHK002" and i.get("distribution"):
            keys[code].add(norm_dist(i["distribution"]))
        elif code in ("CHK003", "CHK004", "CHK005"):
            name = i.get("symbol") or i.get("distribution")
            if name:
                keys[code].add(top_import(name))
        elif code in ("CHK006", "CHK007") and i.get("file"):
            keys[code].add((i["file"], i["target"].rsplit(":", 1)[1]))
    return keys


def other_keys(slug: str, proj: Path, raw: Path, envs: Path) -> dict[tuple[str, str], set]:
    keys: dict[tuple[str, str], set] = defaultdict(set)
    top = TopLevel(proj)

    for tool, rx, extra in (("vulture", VULTURE_RE, []),
                            ("deadcode", DEADCODE_RE, ["--no-color"])):
        out = run_tool([*TOOLS[tool], ".", *extra], proj, raw / f"{tool}.txt")
        for m in filter(None, map(rx.match, out.splitlines())):
            rel, name = rel_path(proj, m.group(1)), m.group(4)
            if in_scope(rel) and name in top.names(rel):
                keys[("CHK006", tool)].add((rel, name))

    deptry_json = raw / "deptry.json"
    deptry_json.unlink(missing_ok=True)
    run_tool([*TOOLS["deptry"], ".", "--json-output", str(deptry_json)], proj, raw / "deptry.txt")
    if deptry_json.is_file():
        for v in json.loads(deptry_json.read_text(encoding="utf-8")):
            rule = DEPTRY_RULE.get(v["error"]["code"])
            loc = (v.get("location") or {}).get("file") or ""
            if not in_scope(rel_path(proj, loc)):
                continue
            if rule == "CHK002":
                keys[(rule, "deptry")].add(norm_dist(v["module"]))
            elif rule:
                keys[(rule, "deptry")].add(top_import(v["module"]))

    fawlty = [*TOOLS["fawltydeps"], "--check", "--json"]
    if (envs / slug / "bin/python").exists():
        fawlty += ["--pyenv", str((envs / slug).resolve())]
    out = run_tool(fawlty, proj, raw / "fawltydeps.json")
    try:
        data = json.loads(out)
    except json.JSONDecodeError:
        data = {}
    def scoped(d: dict) -> bool:
        return any(in_scope(rel_path(proj, r.get("path", ""))) for r in d.get("references") or [])

    for d in data.get("undeclared_deps") or []:
        if scoped(d):
            keys[("CHK003", "fawltydeps")].add(top_import(d["name"]))
    for d in data.get("unused_deps") or []:
        if scoped(d):
            keys[("CHK002", "fawltydeps")].add(norm_dist(d["name"]))

    out = run_tool([*TOOLS["ruff"], "check", "--isolated", "--select", "F401", "--output-format",
                    "json", "--exit-zero", "--no-cache", "."], proj, raw / "ruff.json")
    try:
        ruff = json.loads(out)
    except json.JSONDecodeError:
        ruff = []
    for v in ruff:
        rel = rel_path(proj, v["filename"])
        m = RUFF_NAME_RE.search(v["message"])
        if m and Path(rel).name == "__init__.py" and in_scope(rel):
            keys[("CHK007", "ruff")].add((rel, bound_name(m.group(1))))

    out = run_tool([*TOOLS["pyflakes"], "."], proj, raw / "pyflakes.txt")
    for line in out.splitlines():
        m = PYFLAKES_RE.match(line)
        if m:
            rel = rel_path(proj, m.group(1))
            if Path(rel).name == "__init__.py" and in_scope(rel):
                keys[("CHK007", "pyflakes")].add((rel, bound_name(m.group(3))))
    return keys


def fmt_key(key) -> str:
    return f"{key[0]}:{key[1]}" if isinstance(key, tuple) else key


def main() -> int:
    args = parse_args()
    if shutil.which("uvx") is None:
        print("uvx is required (https://docs.astral.sh/uv/)", file=sys.stderr)
        return 2
    if args.build:
        oc.build()
    oc.require_bin(args.bin)
    only = {s for s in args.projects.split(",") if s}
    rows = [r for r in oc.read_manifest(args.manifest, core_only=True)
            if (not only or r["slug"] in only) and (args.clones / r["slug"]).is_dir()]
    if not rows:
        print("no clones found — run scripts/clone-oss-fixtures.sh first", file=sys.stderr)
        return 2
    shutil.rmtree(args.output, ignore_errors=True)
    args.output.mkdir(parents=True)

    # cells[(rule, tool)] -> {"agree": [...], "chokkin_only": [...], "other_only": [...]}
    cells: dict[tuple[str, str], dict[str, list]] = defaultdict(lambda: defaultdict(list))
    per_project: dict[str, dict] = {}
    tsv = ["slug\trule\ttool\tkey"]
    for row in rows:
        slug = row["slug"]
        proj = args.clones / slug
        raw = args.output / "raw" / slug
        raw.mkdir(parents=True)
        print(f"==> {slug}", flush=True)
        report = oc.chokkin_report(args.bin, proj, "--no-cache")
        mine = chokkin_keys(report)
        theirs = other_keys(slug, proj, raw, args.envs)
        per_project[slug] = {}
        for rule, tools in RULE_TOOLS.items():
            tsv += [f"{slug}\t{rule}\tchokkin\t{fmt_key(k)}" for k in sorted(mine[rule], key=str)]
            for tool in tools:
                other = theirs[(rule, tool)]
                tsv += [f"{slug}\t{rule}\t{tool}\t{fmt_key(k)}" for k in sorted(other, key=str)]
                c = cells[(rule, tool)]
                agree, c_only, o_only = mine[rule] & other, mine[rule] - other, other - mine[rule]
                c["agree"] += [(slug, k) for k in agree]
                c["chokkin_only"] += [(slug, k) for k in c_only]
                c["other_only"] += [(slug, k) for k in o_only]
                per_project[slug][f"{rule}/{tool}"] = {
                    "chokkin": len(mine[rule]), "other": len(other),
                    "agree": len(agree), "chokkin_only": len(c_only), "other_only": len(o_only),
                }
    (args.output / "findings.tsv").write_text("\n".join(tsv) + "\n", encoding="utf-8")

    matrix = []
    for (rule, tool), c in cells.items():
        a, co, oo = len(c["agree"]), len(c["chokkin_only"]), len(c["other_only"])
        union = a + co + oo
        matrix.append({
            "rule": rule, "tool": tool, "chokkin": a + co, "other": a + oo, "agree": a,
            "chokkin_only": co, "other_only": oo,
            "jaccard_pct": round(100 * a / union, 1) if union else None,
        })
    summary = {
        "chokkin_version": oc.chokkin_version(args.bin),
        "generated": oc.utc_now(),
        "tools": {k: " ".join(v) for k, v in TOOLS.items()},
        "corpus": {s: v for s, v in oc.read_lock(args.clones).items() if s in per_project},
        "matrix": matrix,
        "per_project": per_project,
    }
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")

    lines = [
        "# Differential oracle — chokkin vs other checkers",
        "",
        f"- chokkin: `{summary['chokkin_version']}`",
        f"- generated: {summary['generated']}",
        f"- projects: {len(per_project)}",
        "- tools: " + ", ".join(f"`{' '.join(v[1:])}`" for v in TOOLS.values()),
        "",
        "Counts are distinct keys summed over projects (see the script header for",
        "the key of each rule). Jaccard = agree / (agree + chokkin-only + other-only).",
        "",
        *oc.md_table(
            ["Rule", "Tool", "chokkin", "other", "agree", "chokkin-only", "other-only", "Jaccard"],
            [[m["rule"], m["tool"], m["chokkin"], m["other"], m["agree"], m["chokkin_only"],
              m["other_only"], "n/a" if m["jaccard_pct"] is None else f"{m['jaccard_pct']}%"]
             for m in matrix],
            "llrrrrrr",
        ),
    ]
    for (rule, tool), c in cells.items():
        for side in ("chokkin_only", "other_only"):
            items = sorted(c[side], key=lambda x: (x[0], str(x[1])))
            if not items:
                continue
            lines += ["", f"### {rule} vs {tool}: {side.replace('_', '-')} "
                      f"({len(items)}, first {min(len(items), EXAMPLES)})", ""]
            lines += [f"- {slug}: `{fmt_key(k)}`" for slug, k in items[: EXAMPLES]]
    (args.output / "report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines[: 12 + len(matrix) + 2]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
