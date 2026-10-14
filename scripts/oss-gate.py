#!/usr/bin/env python3
"""Corpus-wide determinism and schema gate (issue #342, #85 WS5).

Runs chokkin over every cloned project of the pinned corpus plus the in-repo
recall sentinels, three times each. With --in-repo it needs no clones: it runs
over the recall sentinels and every in-repo fixture project (tests/fixtures/*/*)
instead, which is what CI gates on each pull request (#835).

  cold     .chokkin/ cache removed first (the run writes a fresh cache)
  warm     immediately after, reading that cache
  nocache  --no-cache

Gates (each an automatic pass/fail):
  determinism  the three JSON outputs are byte-identical for every project
  schema       every JSON output validates against
               docs/schema/chokkin-report.schema.json, and summary.total /
               summary.by_code agree with the issues array
  crash        no run exits 3 (internal error) or prints non-JSON

  rejection    (--in-repo) the fixtures chokkin rejects on every run (exit 2,
               no output) are exactly those listed in --expect-rejected

A listed fixture is not gated otherwise: the invalid-config fixtures are meant
to be rejected. Any other rejected project fails the crash gate.

chokkin only reads the analyzed project (plus its own .chokkin/ cache, which is
removed again afterwards); nothing from the project is executed, so this is
safe for CI once the clones exist.

Usage:
  scripts/oss-gate.py [OPTIONS]

Options:
  -m, --manifest PATH   Clone list (default: scripts/oss-clones.manifest)
  -R, --recall PATH     Recall sentinels (default: scripts/oss-recall.manifest)
  -c, --clones DIR      Clone root (default: target/oss-clones)
  --in-repo             Skip the clones; run the in-repo fixtures instead
  -F, --fixtures DIR    Fixture root for --in-repo (default: tests/fixtures)
  --expect-rejected PATH
                        Fixtures expected to be rejected, one slug per line
                        (default: scripts/oss-gate-rejected.txt)
  -o, --output DIR      Report directory (default: target/oss-gate)
  -b, --bin PATH        chokkin binary (default: target/release/chokkin)
  --build               cargo build --release before running
  -h, --help            Show help

Requires the `jsonschema` Python package (pip install jsonschema).
Exit status: 0 all gates pass, 1 a gate failed, 2 usage/setup error.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from collections import Counter
from pathlib import Path

import oss_corpus as oc

SCHEMA = oc.ROOT / "docs/schema/chokkin-report.schema.json"
VARIANTS = ("cold", "warm", "nocache")


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(add_help=False)
    p.add_argument("-m", "--manifest", type=Path, default=oc.DEFAULT_MANIFEST)
    p.add_argument("-R", "--recall", type=Path, default=oc.DEFAULT_RECALL)
    p.add_argument("-c", "--clones", type=Path, default=oc.DEFAULT_CLONES)
    p.add_argument("--in-repo", action="store_true")
    p.add_argument("-F", "--fixtures", type=Path, default=oc.ROOT / "tests/fixtures")
    p.add_argument(
        "--expect-rejected", type=Path, default=oc.ROOT / "scripts/oss-gate-rejected.txt"
    )
    p.add_argument("-o", "--output", type=Path, default=oc.ROOT / "target/oss-gate")
    p.add_argument("-b", "--bin", type=Path, default=oc.DEFAULT_BIN)
    p.add_argument("--build", action="store_true")
    p.add_argument("-h", "--help", action="store_true")
    args = p.parse_args()
    if args.help:
        print(__doc__)
        sys.exit(0)
    return args


def contract_errors(report: dict) -> list[str]:
    """Invariants the schema cannot express."""
    issues = report.get("issues", [])
    summary = report.get("summary", {})
    errs = []
    if summary.get("total") != len(issues):
        errs.append(f"summary.total={summary.get('total')} but {len(issues)} issues")
    by_code = dict(Counter(i["code"] for i in issues))
    if summary.get("by_code") != by_code:
        errs.append(f"summary.by_code {summary.get('by_code')} != counted {by_code}")
    return errs


def is_rejected(res: dict, blobs: dict) -> bool:
    """chokkin refused the project the same way on every run (a config error)."""
    return all(res["exit"].get(v) == 2 and not blobs.get(v, b"").strip() for v in VARIANTS)


def fixture_projects(root: Path) -> list[tuple[str, Path]]:
    """Each tests/fixtures/<area>/<case> directory, as the Rust tests use them."""
    return [
        (p.relative_to(root).as_posix(), p)
        for p in sorted(root.glob("*/*"))
        if p.is_dir() and p.name != ".chokkin"
    ]


def judge(outputs: dict[str, tuple[int, bytes]], validator) -> dict:
    """Verdicts for one project from its (exit code, stdout) per variant."""
    res: dict = {"exit": {}, "schema_errors": [], "crash": False}
    for variant, (code, stdout) in outputs.items():
        res["exit"][variant] = code
        if code == 3:
            res["crash"] = True
        try:
            report = json.loads(stdout)
        except json.JSONDecodeError as err:
            res["crash"] = True
            res["schema_errors"].append(f"{variant}: not JSON ({err})")
            continue
        for e in validator.iter_errors(report):
            loc = "/".join(str(x) for x in e.absolute_path) or "<root>"
            res["schema_errors"].append(f"{variant}: {loc}: {e.message}")
        res["schema_errors"] += [f"{variant}: {e}" for e in contract_errors(report)]
        if variant == "cold":
            res["issues"] = len(report.get("issues", []))
    blobs = {v: stdout for v, (_, stdout) in outputs.items()}
    res["differs"] = [v for v in VARIANTS[1:] if blobs.get(v) != blobs.get("cold")]
    res["deterministic"] = not res["differs"]
    res["rejected"] = is_rejected(res, blobs)
    return res


def failing(results: dict[str, dict]) -> dict[str, list[str]]:
    return {
        "determinism": [s for s, r in results.items() if not r["deterministic"]],
        "schema": [s for s, r in results.items() if r["schema_errors"]],
        "crash": [s for s, r in results.items() if r["crash"]],
    }


def measure(slug: str, proj: Path, args, validator) -> dict:
    out_dir = args.output / "json" / slug
    out_dir.mkdir(parents=True, exist_ok=True)
    cache = proj / ".chokkin"
    shutil.rmtree(cache, ignore_errors=True)
    outputs = {}
    try:
        for variant in VARIANTS:
            extra = ("--no-cache",) if variant == "nocache" else ()
            run = oc.chokkin_raw(args.bin, proj, *extra)
            (out_dir / f"{variant}.json").write_bytes(run.stdout)
            outputs[variant] = (run.returncode, run.stdout)
    finally:
        # Leave the clone as clone-oss-fixtures.sh left it.
        shutil.rmtree(cache, ignore_errors=True)
    return judge(outputs, validator)


def main() -> int:
    args = parse_args()
    try:
        import jsonschema
    except ImportError:
        print("the jsonschema package is required: pip install jsonschema", file=sys.stderr)
        return 2
    if args.build:
        oc.build()
    oc.require_bin(args.bin)
    schema = json.loads(SCHEMA.read_text(encoding="utf-8"))
    validator = jsonschema.Draft202012Validator(schema)

    targets: list[tuple[str, Path]] = []
    skipped = []
    if not args.in_repo:
        for row in oc.read_manifest(args.manifest, core_only=True):
            proj = args.clones / row["slug"]
            if proj.is_dir():
                targets.append((row["slug"], proj))
            else:
                skipped.append(row["slug"])
        if not targets:
            print("no clones found — run scripts/clone-oss-fixtures.sh first", file=sys.stderr)
            return 2
    if args.recall.is_file():
        targets += [(s, p) for s, p in oc.read_recall(args.recall) if p.is_dir()]
    expected: set[str] = set()
    if args.in_repo:
        lines = args.expect_rejected.read_text(encoding="utf-8").splitlines()
        expected = {s for line in lines if (s := line.split("#", 1)[0].strip())}
        # A recall sentinel is measured once, under its slug.
        seen = {p.resolve() for _, p in targets}
        targets += [(s, p) for s, p in fixture_projects(args.fixtures) if p.resolve() not in seen]
        if not targets:
            print(f"no fixture projects under {args.fixtures}", file=sys.stderr)
            return 2

    shutil.rmtree(args.output, ignore_errors=True)
    args.output.mkdir(parents=True)
    results = {}
    rejected = []
    for slug, proj in targets:
        r = measure(slug, proj, args, validator)
        if slug in expected and r["rejected"]:
            rejected.append(slug)
            print(f"==> {slug}: rejected (exit 2)", flush=True)
            continue
        results[slug] = r
        flag = "ok" if r["deterministic"] and not r["schema_errors"] and not r["crash"] else "FAIL"
        print(f"==> {slug}: {r.get('issues', '?')} issue(s) {flag}", flush=True)

    fails = failing(results)
    nondet, invalid, crashed = fails["determinism"], fails["schema"], fails["crash"]
    gates = {gate: not slugs for gate, slugs in fails.items()}
    not_rejected = sorted(expected - set(rejected))
    gates["rejection"] = not not_rejected
    summary = {
        "chokkin_version": oc.chokkin_version(args.bin),
        "generated": oc.utc_now(),
        "projects": len(results),
        "outputs_validated": len(results) * len(VARIANTS),
        "gates": gates,
        "nondeterministic": nondet,
        "schema_invalid": invalid,
        "crashed": crashed,
        "skipped_not_cloned": skipped,
        "rejected": rejected,
        "not_rejected": not_rejected,
        "corpus": {} if args.in_repo else oc.read_lock(args.clones),
        "results": results,
    }
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")

    def verdict(ok: bool) -> str:
        return "PASS" if ok else "FAIL"

    lines = [
        "# OSS corpus gate — determinism / schema / crash",
        "",
        f"- chokkin: `{summary['chokkin_version']}`",
        f"- generated: {summary['generated']}",
        f"- projects: {len(results)} ({len(results) * len(VARIANTS)} JSON outputs)"
        + (f"; {len(rejected)} fixture(s) rejected as invalid config" if rejected else ""),
        "",
        *oc.md_table(
            ["Gate", "Criterion", "Measured", "Result"],
            [
                ["determinism", "cold / warm / --no-cache byte-identical",
                 f"{len(results) - len(nondet)}/{len(results)} identical", verdict(gates["determinism"])],
                ["schema", "every output valid + summary consistent",
                 f"{len(results) - len(invalid)}/{len(results)} valid", verdict(gates["schema"])],
                ["crash", "no exit 3 / non-JSON output",
                 f"{len(crashed)} crashed", verdict(gates["crash"])],
                *([["rejection", "listed fixtures exit 2",
                    f"{len(expected) - len(not_rejected)}/{len(expected)} rejected",
                    verdict(gates["rejection"])]] if args.in_repo else []),
            ],
        ),
        "",
        *oc.md_table(
            ["Project", "Issues", "Exit (cold/warm/nocache)", "Identical", "Schema errors"],
            [
                [s, r.get("issues", "?"), "/".join(str(r["exit"].get(v)) for v in VARIANTS),
                 "yes" if r["deterministic"] else "no: " + ",".join(r["differs"]),
                 len(r["schema_errors"])]
                for s, r in results.items()
            ],
            "lrlll",
        ),
    ]
    if not_rejected:
        lines += ["", "Listed in --expect-rejected but not rejected: " + ", ".join(not_rejected)]
    for slug in invalid:
        lines += ["", f"## Schema errors: {slug}", ""]
        lines += [f"- {e}" for e in results[slug]["schema_errors"][:20]]
    (args.output / "report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines[:12]))
    return 0 if all(gates.values()) else 1


if __name__ == "__main__":
    sys.exit(main())
