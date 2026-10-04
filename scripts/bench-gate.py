#!/usr/bin/env python3
"""Pass/fail gate for performance regressions against a criterion baseline (#342).

Compares the current tree with a baseline saved earlier by
`make bench-save BASELINE=<name>` (typically on the base commit) and fails when
any benchmark's mean time got slower by more than --threshold *and* criterion's
95% confidence interval for the change lies entirely above zero (so noise alone
does not fail the gate).

Usage:
  scripts/bench-gate.py [OPTIONS]

Options:
  --baseline NAME     criterion baseline to compare with (default: main)
  --threshold PCT     allowed mean slowdown in percent (default: 10)
  --confirm N         re-run only the regressed benchmarks up to N times and
                      fail only on those that regress every time (default: 1);
                      sequential runs on a shared machine drift by 10-15%
  -o, --output DIR    report directory (default: target/bench-gate)
  -h, --help          show help

Typical CI use:
  git checkout <base> && make bench-save BASELINE=main
  git checkout <head> && scripts/bench-gate.py --baseline main

Exit status: 0 no regression, 1 regression, 2 no comparable estimates.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from pathlib import Path

import oss_corpus as oc

CRITERION = oc.ROOT / "target/criterion"


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(add_help=False)
    p.add_argument("--baseline", default="main")
    p.add_argument("--threshold", type=float, default=10.0)
    p.add_argument("--confirm", type=int, default=1)
    p.add_argument("-o", "--output", type=Path, default=oc.ROOT / "target/bench-gate")
    p.add_argument("-h", "--help", action="store_true")
    args = p.parse_args()
    if args.help:
        print(__doc__)
        sys.exit(0)
    return args


def bench_id(change_dir: Path) -> str:
    # target/criterion/<group>/<function>[/<param>]/change
    return change_dir.parent.relative_to(CRITERION).as_posix()


def collect(since: float) -> list[dict]:
    rows = []
    for est in sorted(CRITERION.glob("**/change/estimates.json")):
        if est.stat().st_mtime < since:
            continue  # left over from an earlier comparison
        mean = json.loads(est.read_text(encoding="utf-8"))["mean"]
        ci = mean["confidence_interval"]
        rows.append(
            {
                "bench": bench_id(est.parent),
                "mean_pct": mean["point_estimate"] * 100,
                "ci_low_pct": ci["lower_bound"] * 100,
                "ci_high_pct": ci["upper_bound"] * 100,
            }
        )
    return rows


def run_bench(baseline: str, only: list[str] | None = None) -> bool:
    cmd = ["cargo", "bench", "--benches", "--locked", "--", "--baseline", baseline]
    if only:
        cmd.append("^(" + "|".join(re.escape(b) for b in only) + ")$")
    return subprocess.run(cmd, cwd=oc.ROOT, check=False).returncode == 0


def main() -> int:
    args = parse_args()
    since = time.time()
    if not run_bench(args.baseline):
        print(f"cargo bench failed (missing baseline {args.baseline!r}?)", file=sys.stderr)
        return 2
    rows = collect(since)
    if not rows:
        print(f"no change estimates under {CRITERION} — run `make bench-cmp`", file=sys.stderr)
        return 2
    for r in rows:
        r["regressed"] = r["mean_pct"] > args.threshold and r["ci_low_pct"] > 0
        r["runs"] = [round(r["mean_pct"], 1)]
    regressed = [r for r in rows if r["regressed"]]
    for _ in range(args.confirm):
        if not regressed:
            break
        since = time.time()
        if not run_bench(args.baseline, [r["bench"] for r in regressed]):
            return 2
        again = {r["bench"]: r for r in collect(since)}
        for r in regressed:
            new = again.get(r["bench"])
            if new is None:
                continue
            r["runs"].append(round(new["mean_pct"], 1))
            r.update({k: new[k] for k in ("mean_pct", "ci_low_pct", "ci_high_pct")})
            r["regressed"] = r["mean_pct"] > args.threshold and r["ci_low_pct"] > 0
        regressed = [r for r in regressed if r["regressed"]]

    args.output.mkdir(parents=True, exist_ok=True)
    summary = {
        "baseline": args.baseline,
        "threshold_pct": args.threshold,
        "generated": oc.utc_now(),
        "pass": not regressed,
        "benchmarks": rows,
    }
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    lines = [
        "# Benchmark regression gate",
        "",
        f"- baseline: `{args.baseline}`",
        f"- criterion: mean slowdown > {args.threshold:g}% with the 95% CI above 0, "
        f"in the first run and in each of up to {args.confirm} confirmation re-run(s)",
        f"- result: **{'PASS' if not regressed else 'FAIL'}** "
        f"({len(regressed)} of {len(rows)} benchmarks regressed)",
        "",
        *oc.md_table(
            ["Benchmark", "Mean change", "95% CI", "Runs", "Regressed"],
            [
                [f"`{r['bench']}`", f"{r['mean_pct']:+.1f}%",
                 f"[{r['ci_low_pct']:+.1f}%, {r['ci_high_pct']:+.1f}%]",
                 ", ".join(f"{x:+.1f}%" for x in r["runs"]),
                 "**yes**" if r["regressed"] else "no"]
                for r in rows
            ],
            "lrlll",
        ),
    ]
    (args.output / "report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))
    return 1 if regressed else 0


if __name__ == "__main__":
    sys.exit(main())
