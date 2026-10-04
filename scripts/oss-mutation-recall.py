#!/usr/bin/env python3
"""Mutation-injection recall over the pinned OSS corpus (#341).

For every cloned project, a disposable copy is mutated one injection at a time
(`git reset --hard && git clean -fdx` between mutations) and chokkin is re-run.
An injection is *detected* when chokkin reports the expected issue for the
injected name; a trap is *triggered* when chokkin reports the issue the trap is
designed to provoke falsely. Only chokkin runs (static analysis): nothing from
the analyzed projects is executed, and the clones themselves are never touched.

Injections (expected to be reported):
  CHK001  unimported module   new <pkg>/chokkin_mut_orphan.py nobody imports
  CHK002  unused dependency   `chokkin-mut-unused>=1` added to
                              [project].dependencies
  CHK003  undeclared import   `import xmltodict` (a real distribution none of the
                              corpus declares) in <pkg>/__init__.py
  CHK010  unresolved import   `import chokkin_mut_missing` (no such module) in
                              <pkg>/__init__.py
  CHK006  unreferenced func   public `chokkin_mut_unreferenced()` appended to an
                              existing (reachable) module of <pkg>
  CHK007  unused re-export    <pkg>/__init__.py re-exports a new function from a
                              new sibling module; nothing uses it

Traps (must not be reported):
  dynamic-import   new <pkg>/chokkin_mut_plugin.py loaded only through
                   importlib.import_module("<pkg>.chokkin_mut_plugin");
                   triggered by CHK001 on that file
  dist-ne-import   PyYAML and Pillow declared and imported as `yaml` / `PIL`;
                   triggered by CHK002 on pyyaml/pillow or CHK003 on yaml/PIL

CHK004 (transitive-only dependency) needs a lockfile; its recall is covered by
the lock_unused_* sentinels in scripts/oss-recall.manifest (oss-gate.py).

Each mutation is checked at the default confidence (what a user sees) and
with `--confidence maybe` (anything chokkin knows). Projects without a static
[project].dependencies list skip the manifest mutations (reported as `n/a`).

Usage:
  scripts/oss-mutation-recall.py [OPTIONS]

Options:
  -m, --manifest PATH   Clone list (default: scripts/oss-clones.manifest; only
                        the pinned 20-project core set is used)
  -c, --clones DIR      Clone root (default: target/oss-clones)
  -o, --output DIR      Output directory (default: target/oss-mutation)
  -b, --bin PATH        chokkin binary (default: target/release/chokkin)
  --projects a,b        Only these slugs
  --build               cargo build --release before running
  -h, --help            Show help

Outputs: results.tsv, summary.json and report.md under the output directory.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import sys
from collections import defaultdict
from pathlib import Path

import oss_corpus as oc

PREFIX = "chokkin_mut"
SKIP_DIRS = {"tests", "test", "testing", "docs", "doc", "examples", "scripts", "benchmarks"}
CONFIDENCES = ("default", "maybe")


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(add_help=False)
    p.add_argument("-m", "--manifest", type=Path, default=oc.DEFAULT_MANIFEST)
    p.add_argument("-c", "--clones", type=Path, default=oc.DEFAULT_CLONES)
    p.add_argument("-o", "--output", type=Path, default=oc.ROOT / "target/oss-mutation")
    p.add_argument("-b", "--bin", type=Path, default=oc.DEFAULT_BIN)
    p.add_argument("--projects", default="")
    p.add_argument("--build", action="store_true")
    p.add_argument("-h", "--help", action="store_true")
    args = p.parse_args()
    if args.help:
        print(__doc__)
        sys.exit(0)
    return args


def find_package(proj: Path) -> Path | None:
    """The first-party import package: the regular package (root or src/) whose
    name matches [project].name, else the one with the most .py files."""
    cands = [
        d
        for base in (proj, proj / "src")
        if base.is_dir()
        for d in sorted(base.iterdir())
        if (d / "__init__.py").is_file() and d.name not in SKIP_DIRS
    ]
    if not cands:
        return None
    m = re.search(r'(?m)^name\s*=\s*"([^"]+)"', _pyproject(proj))
    wanted = re.sub(r"[-_.]+", "_", m.group(1)).lower() if m else ""
    for d in cands:
        if d.name.lower() == wanted:
            return d
    return max(cands, key=lambda d: sum(1 for _ in d.rglob("*.py")))


def _pyproject(proj: Path) -> str:
    try:
        return (proj / "pyproject.toml").read_text(encoding="utf-8")
    except OSError:
        return ""


def add_dependencies(proj: Path, reqs: list[str]) -> bool:
    """Prepend `reqs` to the static [project].dependencies array. Returns False
    when the project has no [project] table or declares dependencies dynamic."""
    text = _pyproject(proj)
    head = re.search(r"(?m)^\[project\]\s*$", text)
    if not head:
        return False
    nxt = re.search(r"(?m)^\[", text[head.end():])
    end = head.end() + nxt.start() if nxt else len(text)
    table = text[head.end():end]
    if re.search(r'(?m)^dynamic\s*=.*"dependencies"', table):
        return False
    items = "".join(f'"{r}", ' for r in reqs)
    dep = re.search(r"(?m)^dependencies\s*=\s*\[", table)
    if dep:
        at = head.end() + dep.end()
        text = text[:at] + items + text[at:]
    else:
        text = text[:head.end()] + f"\ndependencies = [{items}]" + text[head.end():]
    (proj / "pyproject.toml").write_text(text, encoding="utf-8")
    return True


def append(path: Path, code: str) -> None:
    with path.open("a", encoding="utf-8") as fh:
        fh.write(f"\n\n{code}\n")


def pick_module(proj: Path, pkg: Path, baseline: set) -> Path | None:
    """A non-__init__ module of the package that the baseline does not already
    report as unreachable, so a new function in it is reachable code."""
    unreachable = {k for c, k in baseline if c == "CHK001"}
    mods = [
        p
        for p in sorted(pkg.rglob("*.py"))
        if p.name != "__init__.py" and p.relative_to(proj).as_posix() not in unreachable
    ]
    return max(mods, key=lambda p: p.stat().st_size) if mods else None


def keys(report: dict) -> set[tuple[str, str]]:
    """(code, subject) pairs; subject = file for CHK001, the lowercased
    distribution for CHK002, the top import for CHK003, and the bare name for
    CHK006/CHK007/CHK010."""
    out = set()
    for i in report["issues"]:
        code = i["code"]
        if code == "CHK001":
            out.add((code, i.get("file") or ""))
        elif code == "CHK002":
            out.add((code, re.sub(r"[-_.]+", "-", i.get("distribution") or "").lower()))
        elif code == "CHK003":
            name = i.get("symbol") or i.get("distribution") or ""
            out.add((code, name.split(".", 1)[0].lower()))
        elif code in ("CHK006", "CHK007", "CHK010"):
            out.add((code, i["target"].rsplit(":", 1)[-1].rsplit(".", 1)[-1]))
    return out


def scan(bin_path: Path, proj: Path) -> dict[str, set]:
    return {
        "default": keys(oc.chokkin_report(bin_path, proj, "--no-cache")),
        "maybe": keys(oc.chokkin_report(bin_path, proj, "--no-cache", "--confidence", "maybe")),
    }


# Each mutation: (id, kind, rule, apply(work, pkg, target_module) -> keys or None)
# The returned keys are what chokkin must report (injection) or must not (trap);
# None means the mutation does not apply to this project.


def m_orphan(work: Path, pkg: Path, mod: Path | None):
    f = pkg / f"{PREFIX}_orphan.py"
    f.write_text("VALUE = 1\n", encoding="utf-8")
    return {("CHK001", f.relative_to(work).as_posix())}


def m_unused_dep(work: Path, pkg: Path, mod: Path | None):
    if not add_dependencies(work, ["chokkin-mut-unused>=1"]):
        return None
    return {("CHK002", "chokkin-mut-unused")}


def m_undeclared(work: Path, pkg: Path, mod: Path | None):
    append(pkg / "__init__.py", "import xmltodict  # noqa")
    return {("CHK003", "xmltodict")}


def m_unresolved(work: Path, pkg: Path, mod: Path | None):
    append(pkg / "__init__.py", f"import {PREFIX}_missing  # noqa")
    return {("CHK010", f"{PREFIX}_missing")}


def m_unreferenced(work: Path, pkg: Path, mod: Path | None):
    if mod is None:
        return None
    append(mod, f"def {PREFIX}_unreferenced():\n    return None")
    return {("CHK006", f"{PREFIX}_unreferenced")}


def m_reexport(work: Path, pkg: Path, mod: Path | None):
    (pkg / f"{PREFIX}_src.py").write_text(
        f"def {PREFIX}_reexported():\n    return None\n", encoding="utf-8"
    )
    append(pkg / "__init__.py", f"from .{PREFIX}_src import {PREFIX}_reexported  # noqa")
    return {("CHK007", f"{PREFIX}_reexported")}


def t_dynamic(work: Path, pkg: Path, mod: Path | None):
    f = pkg / f"{PREFIX}_plugin.py"
    f.write_text("VALUE = 1\n", encoding="utf-8")
    append(
        pkg / "__init__.py",
        f"import importlib as _{PREFIX}_importlib\n"
        f'_{PREFIX}_importlib.import_module("{pkg.name}.{PREFIX}_plugin")',
    )
    return {("CHK001", f.relative_to(work).as_posix())}


def t_dist_name(work: Path, pkg: Path, mod: Path | None):
    if not add_dependencies(work, ["PyYAML>=6", "Pillow>=10"]):
        return None
    append(pkg / "__init__.py", "import yaml  # noqa\nimport PIL  # noqa")
    return {("CHK002", "pyyaml"), ("CHK002", "pillow"), ("CHK003", "yaml"), ("CHK003", "pil")}


MUTATIONS = [
    ("unimported-module", "injection", "CHK001", m_orphan),
    ("unused-dependency", "injection", "CHK002", m_unused_dep),
    ("undeclared-import", "injection", "CHK003", m_undeclared),
    ("unresolved-import", "injection", "CHK010", m_unresolved),
    ("unreferenced-function", "injection", "CHK006", m_unreferenced),
    ("unused-reexport", "injection", "CHK007", m_reexport),
    ("dynamic-import", "trap", "CHK001", t_dynamic),
    ("dist-ne-import", "trap", "CHK002/CHK003", t_dist_name),
]


def run_project(slug: str, src: Path, work: Path, bin_path: Path) -> list[dict]:
    oc.fresh_copy(src, work)
    oc.reset_tree(work)
    pkg = find_package(work)
    if pkg is None:
        return [{"slug": slug, "mutation": m[0], "kind": m[1], "rule": m[2], "package": "",
                 "default": "n/a", "maybe": "n/a", "note": "no first-party package found"}
                for m in MUTATIONS]
    base = scan(bin_path, work)
    mod = pick_module(work, pkg, base["default"])
    rows = []
    for mid, kind, rule, apply in MUTATIONS:
        oc.reset_tree(work)
        expected = apply(work, pkg, mod)
        row = {"slug": slug, "mutation": mid, "kind": kind, "rule": rule,
               "package": pkg.relative_to(work).as_posix(), "note": ""}
        if expected is None:
            row.update(default="n/a", maybe="n/a", note="no static [project].dependencies"
                       if "dep" in mid or "dist" in mid else "no candidate module")
            rows.append(row)
            continue
        if mid == "unreferenced-function" and mod is not None:
            row["note"] = f"in {mod.relative_to(work).as_posix()}"
        got = scan(bin_path, work)
        for conf in CONFIDENCES:
            hit = sorted(f"{c}:{k}" for c, k in expected & (got[conf] - base[conf]))
            if kind == "injection":
                row[conf] = "detected" if hit else "missed"
            else:
                row[conf] = "triggered" if hit else "clean"
            if hit and kind == "trap":
                row["note"] = ", ".join(hit)
        rows.append(row)
    oc.reset_tree(work)
    return rows


def summarize(rows: list[dict]) -> dict:
    agg: dict[str, dict] = defaultdict(lambda: {"kind": "", "rule": "", "applied": 0,
                                                "default": 0, "maybe": 0})
    for r in rows:
        a = agg[r["mutation"]]
        a["kind"], a["rule"] = r["kind"], r["rule"]
        if r["default"] == "n/a":
            continue
        a["applied"] += 1
        for conf in CONFIDENCES:
            a[conf] += r[conf] in ("detected", "triggered")
    return dict(agg)


def pct(n: int, d: int) -> str:
    return f"{100 * n / d:.1f}%" if d else "n/a"


def main() -> int:
    args = parse_args()
    if args.build:
        oc.build()
    oc.require_bin(args.bin)
    only = {s for s in args.projects.split(",") if s}
    projects = [r["slug"] for r in oc.read_manifest(args.manifest, core_only=True)
                if not only or r["slug"] in only]
    work_root = args.output / "work"
    rows: list[dict] = []
    for slug in projects:
        src = args.clones / slug
        if not (src / ".git").exists():
            print(f"==> {slug}: clone missing, skipped", file=sys.stderr)
            continue
        print(f"==> {slug}", file=sys.stderr)
        rows += run_project(slug, src, work_root / slug, args.bin)
        shutil.rmtree(work_root / slug, ignore_errors=True)
    shutil.rmtree(work_root, ignore_errors=True)

    agg = summarize(rows)
    args.output.mkdir(parents=True, exist_ok=True)
    cols = ["slug", "mutation", "kind", "rule", "package", "default", "maybe", "note"]
    (args.output / "results.tsv").write_text(
        "\t".join(cols) + "\n" + "".join("\t".join(r[c] for c in cols) + "\n" for r in rows),
        encoding="utf-8",
    )
    (args.output / "summary.json").write_text(
        json.dumps({"chokkin": oc.chokkin_version(args.bin), "generated": oc.utc_now(),
                    "projects": len({r["slug"] for r in rows}), "mutations": agg}, indent=2)
        + "\n",
        encoding="utf-8",
    )

    lines = [
        "# Mutation-injection recall",
        "",
        f"- chokkin: `{oc.chokkin_version(args.bin)}`",
        f"- generated: {oc.utc_now()}",
        f"- projects: {len({r['slug'] for r in rows})}",
        "",
        "Recall = detected / applied for injections; trigger rate = triggered /",
        "applied for traps (lower is better). `maybe` re-runs with `--confidence maybe`.",
        "",
        *oc.md_table(
            ["Mutation", "Kind", "Rule", "Applied", "Default", "Rate", "maybe", "Rate"],
            [[m, a["kind"], a["rule"], a["applied"], a["default"], pct(a["default"], a["applied"]),
              a["maybe"], pct(a["maybe"], a["applied"])] for m, a in agg.items()],
            "llllrrrr",
        ),
        "",
        "## Misses and triggers",
        "",
    ]
    bad = [r for r in rows if "missed" in (r["default"], r["maybe"])
           or "triggered" in (r["default"], r["maybe"])]
    lines += oc.md_table(
        ["Project", "Mutation", "Package", "Default", "maybe", "Note"],
        [[r["slug"], r["mutation"], f"`{r['package']}`", r["default"], r["maybe"], r["note"]]
         for r in bad],
    ) if bad else ["_None._"]
    na = [r for r in rows if r["default"] == "n/a"]
    lines += ["", "## Not applied", ""]
    lines += oc.md_table(
        ["Project", "Mutation", "Reason"], [[r["slug"], r["mutation"], r["note"]] for r in na]
    ) if na else ["_None._"]
    (args.output / "report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    sys.exit(main())
