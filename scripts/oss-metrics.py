#!/usr/bin/env python3
"""Measure the Phase 1 §17 exit criteria over the OSS validation set.

Exit criteria (docs/dev/spec.ja.md §17):
  1. unused-dependency (CHK002) false-positive rate < 5%
  2. crashes (chokkin internal error, exit 3) == 0
  3. cold run on a `medium` project <= 2000 ms

Corpus regression gates (#495):
  4. CLI/config errors (exit 2) == 0
  5. every project in --expect stays within its floors (see below)
  6. every rule in PRECISION_FLOORS stays at or above its floor, and every
     `certain` finding of a rule in CERTAIN_FLOORS is labelled and at or
     above that floor (see Precision)

Release gate (with --baseline, #325):
  7. CHK003 hits (not info) not labelled `tp`, outside --expect projects and
     recall sentinels, do not grow over the baseline run
  8. with --with-strict on both runs: no finding labelled `tp` vanishes from
     the --strict output (see Downgrade vs vanish below)
  9. unless --no-slowdown: no medium or large project's median cold run slows
     down past the time baseline's (see Slowdown below)
 10. unless --no-slowdown: the large projects' median cold runs, summed, do
     not drift past the time baseline's (see Drift below)

Usage:
  scripts/oss-metrics.py [OPTIONS]

Options:
  -m, --manifest PATH   Clone list (default: scripts/oss-clones.manifest)
  -l, --labels PATH     Ground-truth labels (default: scripts/oss-fixtures.labels.tsv)
  -R, --recall PATH     Recall sentinels (default: scripts/oss-recall.manifest)
  -e, --expect PATH     Regression floors (default: scripts/oss-expectations.tsv)
  -c, --clones DIR      Clone root (default: target/oss-clones)
  -o, --output DIR      Report directory (default: target/oss-metrics)
  -b, --bin PATH        chokkin binary (default: target/release/chokkin)
  -r, --runs N          Timed repetitions per project, median reported (default: 3)
  --baseline DIR        --output of a baseline run on the same machine; writes
                        compare.md and finding-diff.tsv and adds the CHK003
                        growth and slowdown criteria
  --time-bin PATH       chokkin binary of the previous release: each medium
                        and large project is timed with it and --bin in turn,
                        and criteria 9 and 10 compare with those times instead
                        of --baseline's; needs --baseline
  --no-slowdown         Leave criteria 9 and 10 out
  --strict              Run chokkin with --strict, which also shows `maybe`
                        findings, so a downgrade stays visible; use a separate
                        --output. A --baseline must be --strict too
  --with-strict         Also run every project once with --strict and keep
                        its JSON under strict/; with --baseline (also run with
                        --with-strict) adds criterion 8
  --build               cargo build --release before running
  --clone               Run clone-oss-fixtures.sh first
  --gate                Exit non-zero if any criterion fails
  -h, --help            Show help

Outputs (under --output):
  <slug>.json     raw chokkin JSON report
  findings.tsv    every CHK001–CHK010 finding with ground-truth verdict
                  (columns: slug code target verdict bucket confidence
                  message severity)
  summary.tsv     per-project: size, exit, median_ms, totals, by-code counts
  report.md       human-readable §17 scorecard + per-rule label coverage
  expectations.tsv  this run's values for the --expect projects, in the
                  --expect format (copy over the committed file to refresh)
  compare.md      before/after against --baseline (only with --baseline)
  finding-diff.tsv  finding-level diff against --baseline, keyed on (slug,
                  fingerprint): NEW / GONE / CHANGED (severity or
                  confidence moved); only with --baseline
  strict/<slug>.json  the --strict report (only with --with-strict)
  finding-diff-strict.tsv  the same diff between the two strict/ reports
                  (only with --baseline and --with-strict)

Every timed run is cold: all .chokkin/ caches under the project (workspace
members get their own) are removed before each run and again afterwards, so
clones and fixtures are left as they were. Runs also pass --no-cache, so no
stale result can leak into the findings.

False-positive accounting: each reported CHK002 finding is matched against the
labels file on (slug, code, target). Verdict `fp` counts as a false positive;
`tp` as a true positive; `deferred` and unlabeled findings are unclassified.
The FP-rate gate cannot pass while CHK002 unclassified findings remain.

Severity: a finding reported at severity `info` is not a hit. It is left out of
every reported count, FP rate and precision denominator, and does not satisfy a
`tp` label in the recall gate. Verdict `info-expected` labels a finding that is
correct only at info severity (e.g. a CHK003 for an optional try-import, #504);
reported at any other severity it counts as `fp`.

Precision: every rule's precision tp / (tp + fp) over its labelled hits is
recorded in report.md (#656), next to its per-project mean (macro) and the
largest single-project share of its labelled hits, so a rule whose number one
project drives is visible (#1065); floors use the pooled (micro) precision.
CHK001 / CHK003 / CHK004 / CHK005 / CHK006 / CHK008 / CHK010 have a floor
(#701, #1065, #862), so a precision drop fails the gate even when the counts
hold. CHK002's floor is criterion 1: with no unclassified finding, FP rate < 5%
is precision > 95%. The floors leave the diversity corpus out (#1265): its
precision per rule is recorded in the same row, and gates nothing. CHK007 / CHK009 are recorded only until their labels can
carry one (#862).
CERTAIN_FLOORS gates the `certain` findings alone, at any severity: those are
what --fix acts on (ADR 0003; `--allow-remove-files` deletes a `certain`
CHK001 file whatever its severity). An unknown or deferred one fails it, as
an unclassified CHK002 fails criterion 1, and `info-expected` counts as `fp`:
the finding is right only as a hint, and deleting the file is not.
CHK001 / CHK004 / CHK006 / CHK010 labels are a per-project stratified sample
drawn by scripts/sample-precision-labels.py, so their precision is a sample estimate.
Labels whose finding no longer appears are listed as stale.

Recall accounting: the FP rate alone is satisfied by reporting nothing, so a
separate recall gate measures in-repo sentinel fixtures (--recall manifest)
whose deliberately-unused dependencies are labelled `tp`. Every `tp` label
must appear in the run's findings or the recall gate fails.

Expectations: each --expect row pins a floor on the project's runtime files
reachable from an entry root (`summary.files.reachable_runtime`) and its
per-rule issue counts. The gate fails when the reachable count drops below
the floor (the package root, entry points or workspace were missed) or a
rule's count grows past base + max(5, base/5) (a new false-positive pattern).

Downgrade vs vanish (#700): a confidence or severity change can hide a finding
from the default output (GONE in finding-diff.tsv) while --strict still
reports it. --strict also changes some verdicts (a member's dev declaration
turns CHK003 into CHK005), so the strict reports are compared with each other,
never with a default run (#716). A finding GONE between the two strict reports
vanished. A default-run GONE whose fingerprint HEAD's strict report adds over
its default report was demoted when that strict finding is at `maybe` (hidden
by the confidence floor, #784) and recontexted otherwise (a context change:
examples/ became dev, #709); only confidences and counts are compared, since
--strict raises some severities. A default-run CHANGED to a lower severity, or
to the same severity at a lower confidence, is demoted too, and those from
warning or error to info are counted apart. These columns are informational
only. The gate fails on a vanished `tp`; vanished findings without a label are
counted for triage but do not gate, because a release that fixes false
positives makes hundreds of them vanish.

Slowdown (#1025): criterion 3 covers medium projects only, so a large
project five times slower passed two releases (transformers in v0.7.4/v0.7.5).
With --baseline, a medium or large project whose median_ms is at least
SLOWDOWN_RATIO times the baseline's and SLOWDOWN_MIN_MS over it is re-measured
over CONFIRM_RUNS cold runs, and fails the gate when that median is still
slowed. Small projects and sentinels run in tens of ms, where a ratio is noise.
A project that exited 2 or 3 in either run is left to criteria 2 and 4. Both
runs must be measured on the same machine, and the baseline should be the
previous release: CI's baseline is v0.4.1 for criterion 7, which analyzes far
less than HEAD (llama_index 660 -> 2210 ms), so CI passes the previous
release as --time-bin.

--time-bin times both binaries on each medium and large project, the order
swapped on every repetition, and writes its medians to time-baseline/
summary.tsv. Two runs measured one after the other drift with the machine:
the same main binary summed 1.15x and 1.09x v0.7.7's large projects in two
such runs (see DRIFT_RATIO for the numbers timed in turn).

Drift (#1088): criterion 9 compares each project with the previous run, so
+10% per release passes every time; v0.7.8 made 13 of the large projects
7-24% slower and none failed it. Criterion 10 sums the median_ms of the large
projects that finished in both runs and fails when HEAD's sum is over
DRIFT_RATIO times the time baseline's, in this run and again after the large
projects are re-measured over CONFIRM_RUNS cold runs (both binaries in turn
with --time-bin, HEAD alone otherwise). A medium or large
project at DRIFT_WARN_RATIO times its time baseline and DRIFT_WARN_MIN_MS
over it is listed in compare.md as a warning; it does not fail the gate.
The time baseline is the last release, or the commit whose slowdown the
CHANGELOG accepted; CI's RELEASE_REV moves once a release tag is pushed.

chokkin only reads the analyzed projects; nothing from them is executed.
"""

from __future__ import annotations

import argparse
import json
import shutil
import statistics
import subprocess
import sys
import time
from collections import Counter
from collections.abc import Callable
from pathlib import Path

import oss_corpus as oc

ALL_RULES = [f"CHK{n:03d}" for n in range(1, 11)]
MEDIUM_GATE_MS = 2000
FP_GATE_PCT = 5
# Per-rule precision floors in percent (#701): the measured precision rounded
# down to a multiple of 5. CHK002 has none here: the §17 FP-rate gate is its
# floor, and a stricter one. CHK007 / CHK009 have too few labelled hits yet
# and are recorded only (#862). CHK001 is reported by default in app mode
# only, so its floor waited until 8 app-mode projects carried labels, so that
# no single project's conventions (airflow's DAG files, #1062) decide the
# gate. It rose from 25 to 45 once URLconf include() strings became module
# references (#1094, 30.2% -> 48.1%), to 50 with every hit labelled (53.5%),
# and to 85 once the flat Django layout, Sphinx pages and examples were read
# (#1239, 88.5%). CHK006's 90 was measured on library projects, where a public
# name is info rather than a hit; labelling 8 hits of each app-mode project
# measured 74.8%, so the floor fell to 70 and the gate covers app mode
# (#1238). Settings names, dotted strings and registration decorators took it
# to 96.2%, and the floor is back at 90 rather than 95, which one more false
# positive would fail. CHK005 (89.5%, 19 hits) and CHK008 (85.7%, 14 hits)
# carry a floor since #1241 / #1242 (#862): 85 and 80 each allow one more
# false positive. CHK010 measured 90.0% with no margin; #1242 moved two
# guarded tp hits to info (89.8%), so its floor is 89 rather than the
# multiple of 5 below, which would give up 5 points.
PRECISION_FLOORS = {
    "CHK001": 85, "CHK003": 90, "CHK004": 95, "CHK005": 85,
    "CHK006": 90, "CHK008": 80, "CHK010": 89,
}
# Floors over `certain` findings only (#1094), which --fix acts on. A file
# --fix --allow-remove-files deletes while it is in use breaks the project, so
# CHK001's floor is every one: a `certain` false positive is a bug to fix, not
# a rate to keep under.
CERTAIN_FLOORS = {"CHK001": 100}
# Release-gate slowdown (#1025): two -r 1 runs of one binary on one machine
# differed by at most 1.13x per medium or large project, the slowdowns between
# releases that were not regressions stayed under 1.3x (sqlalchemy v0.7.0 ->
# v0.7.5), and transformers went 5.7x in v0.7.4.
SLOWDOWN_RATIO = 1.5
SLOWDOWN_MIN_MS = 500
SLOWDOWN_SIZES = frozenset({"medium", "large"})
CONFIRM_RUNS = 3
# Cumulative drift (#1088): main after v0.7.8, with airflow and llama_index
# fixed, summed 1.10x-1.14x v0.7.7 over the large projects in five runs timed
# in turn, and a binary against a copy of itself 1.00x-1.02x in three.
DRIFT_RATIO = 1.05
DRIFT_WARN_RATIO = 1.25
DRIFT_WARN_MIN_MS = 100


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(add_help=False)
    p.add_argument("-m", "--manifest", type=Path, default=oc.DEFAULT_MANIFEST)
    p.add_argument("-l", "--labels", type=Path, default=oc.ROOT / "scripts/oss-fixtures.labels.tsv")
    p.add_argument("-R", "--recall", type=Path, default=oc.DEFAULT_RECALL)
    p.add_argument("-e", "--expect", type=Path, default=oc.ROOT / "scripts/oss-expectations.tsv")
    p.add_argument("-c", "--clones", type=Path, default=oc.DEFAULT_CLONES)
    p.add_argument("-o", "--output", type=Path, default=oc.ROOT / "target/oss-metrics")
    p.add_argument("-b", "--bin", type=Path, default=oc.DEFAULT_BIN)
    p.add_argument("-r", "--runs", type=int, default=3)
    p.add_argument("--baseline", type=Path)
    p.add_argument("--strict", action="store_true")
    p.add_argument("--with-strict", action="store_true")
    p.add_argument("--time-bin", type=Path)
    p.add_argument("--no-slowdown", action="store_true")
    p.add_argument("--build", action="store_true")
    p.add_argument("--clone", action="store_true")
    p.add_argument("--gate", action="store_true")
    p.add_argument("-h", "--help", action="store_true")
    args = p.parse_args()
    if args.help:
        print(__doc__)
        sys.exit(0)
    if args.strict and args.with_strict:
        p.error("--with-strict is redundant with --strict")
    if args.time_bin and not args.baseline:
        p.error("--time-bin needs --baseline")
    if args.time_bin and args.no_slowdown:
        p.error("--time-bin has nothing to compare with --no-slowdown")
    return args


def tsv_rows(path: Path) -> list[list[str]]:
    """Data rows of a TSV this script wrote (header dropped)."""
    return [line.split("\t") for line in path.read_text(encoding="utf-8").splitlines()[1:]]


def tsv_field(value: object) -> str:
    # Same escaping as jq's @tsv.
    s = str(value)
    return s.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n").replace("\r", "\\r")


def read_labels(path: Path) -> dict[tuple[str, str, str], tuple[str, str]]:
    """(slug, code, target) -> (verdict, bucket); the first row wins."""
    labels: dict[tuple[str, str, str], tuple[str, str]] = {}
    if not path.is_file():
        return labels
    for line in path.read_text(encoding="utf-8").splitlines():
        cols = line.split("\t")
        if line.startswith("#") or len(cols) < 5:
            continue
        labels.setdefault((cols[0], cols[1], cols[2]), (cols[3], cols[4] or "-"))
    return labels


def read_expect(path: Path) -> dict[str, tuple[int, dict[str, int]]]:
    """slug -> (runtime floor, per-rule base counts)."""
    rows = {}
    if not path.is_file():
        return rows
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("#") or not line.strip():
            continue
        slug, runtime_min, counts = line.split("\t")
        base = {k: int(v) for k, v in (kv.split("=") for kv in counts.split(",") if kv)}
        rows[slug] = (int(runtime_min), base)
    return rows


def expectation_misses(slug: str, report: dict, floor: tuple[int, dict[str, int]]) -> list[str]:
    runtime_min, base = floor
    runtime = report.get("summary", {}).get("files", {}).get("reachable_runtime")
    misses = []
    if runtime is None or runtime < runtime_min:
        misses.append(f"{slug}:runtime={'?' if runtime is None else runtime}<{runtime_min}")
    for code, n in sorted(report.get("summary", {}).get("by_code", {}).items()):
        want = base.get(code, 0)
        slack = max(5, want // 5)
        if n > want + slack:
            misses.append(f"{slug}:{code}={n}>{want}+{slack}")
    return misses


def is_hit(severity: str) -> bool:
    """A finding at info severity is not a hit."""
    return severity != "info"


def effective(verdict: str) -> str:
    """An `info-expected` label on a hit means the finding lost its info
    downgrade, so it counts as `fp`."""
    return "fp" if verdict == "info-expected" else verdict


def count_hits(findings: list[list[str]], code: str, verdict: str | None = None) -> int:
    """findings.tsv rows of `code` that are hits, with the effective `verdict`
    when one is given."""
    return sum(
        1 for f in findings
        if f[1] == code and is_hit(f[7]) and (verdict is None or effective(f[3]) == verdict)
    )


def fp_gate(findings: list[list[str]]) -> tuple[bool, int, int, int]:
    """The §17 CHK002 FP-rate gate: (pass, reported, fp, unclassified). It
    cannot pass while an unknown or deferred CHK002 finding remains."""
    total, fp = count_hits(findings, "CHK002"), count_hits(findings, "CHK002", "fp")
    unclassified = count_hits(findings, "CHK002", "unknown") + count_hits(findings, "CHK002", "deferred")
    ok = unclassified == 0 and (total == 0 or 100 * fp / total < FP_GATE_PCT)
    return ok, total, fp, unclassified


def precision(findings: list[list[str]], code: str) -> tuple[int, int]:
    """(tp, fp) over the labelled hits of `code`; unknown and deferred are left out."""
    return count_hits(findings, code, "tp"), count_hits(findings, code, "fp")


def project_precision(findings: list[list[str]], code: str, skip: frozenset[str] = frozenset()) -> dict[str, tuple[int, int]]:
    """precision() of `code` per project outside `skip`, for the projects with a labelled hit."""
    rows: dict[str, list[list[str]]] = {}
    for f in findings:
        if f[0] not in skip:
            rows.setdefault(f[0], []).append(f)
    per = {slug: precision(fs, code) for slug, fs in rows.items()}
    return {slug: hits for slug, hits in per.items() if sum(hits)}


def ranked_projects(per_project: dict[str, tuple[int, int]]) -> list[tuple[str, tuple[int, int]]]:
    """Projects by labelled hits, most first; a tie goes to the first slug."""
    return sorted(per_project.items(), key=lambda kv: (-sum(kv[1]), kv[0]))


def macro_precision(per_project: dict[str, tuple[int, int]]) -> float | None:
    """Mean of the per-project precisions in percent, each project weighted
    equally (#1065); None when no project has a labelled hit."""
    if not per_project:
        return None
    return 100 * statistics.fmean(tp / (tp + fp) for tp, fp in per_project.values())


def max_share(per_project: dict[str, tuple[int, int]]) -> tuple[str, float] | None:
    """The project holding the most labelled hits and its share of them in
    percent."""
    if not per_project:
        return None
    slug, (tp, fp) = ranked_projects(per_project)[0]
    return slug, 100 * (tp + fp) / sum(tp + fp for tp, fp in per_project.values())


def precision_misses(findings: list[list[str]]) -> list[str]:
    """Rules in PRECISION_FLOORS whose precision is below the floor. A rule
    with no labelled hit is not measured, so it passes; the recall gate is
    what catches a rule that stopped reporting."""
    misses = []
    for code, floor in PRECISION_FLOORS.items():
        tp, fp = precision(findings, code)
        if 100 * tp < floor * (tp + fp):
            misses.append(f"{code}={100 * tp / (tp + fp):.1f}%<{floor}%")
    return misses


def certain_precision(findings: list[list[str]], code: str) -> tuple[int, int, int]:
    """(tp, fp, unclassified) over the `certain` findings of `code` at any
    severity; `info-expected` is `fp`, unknown and deferred are unclassified."""
    verdicts = Counter(f[3] for f in findings if f[1] == code and f[5] == "certain")
    return verdicts["tp"], verdicts["fp"] + verdicts["info-expected"], verdicts["unknown"] + verdicts["deferred"]


def certain_misses(findings: list[list[str]]) -> list[str]:
    """Rules in CERTAIN_FLOORS with an unclassified `certain` finding or a
    `certain` precision below the floor. No `certain` finding passes."""
    misses = []
    for code, floor in CERTAIN_FLOORS.items():
        tp, fp, unclassified = certain_precision(findings, code)
        if unclassified:
            misses.append(f"{code}:certain {unclassified} unclassified")
        if 100 * tp < floor * (tp + fp):
            misses.append(f"{code}:certain={100 * tp / (tp + fp):.1f}%<{floor}%")
    return misses


def recall_missed(
    labels: dict[tuple[str, str, str], tuple[str, str]], reported: set[tuple[str, str, str]]
) -> list[str]:
    return [f"{s}/{c}/{t}" for (s, c, t), (v, _) in labels.items() if v == "tp" and (s, c, t) not in reported]


def too_slow(size: str, median_ms: int) -> bool:
    return size == "medium" and median_ms > MEDIUM_GATE_MS


def slowed(base_ms: int, head_ms: int) -> bool:
    return head_ms >= base_ms * SLOWDOWN_RATIO and head_ms - base_ms >= SLOWDOWN_MIN_MS


def medians(run: Path) -> dict[str, tuple[str, int]]:
    """slug -> (size, median_ms) of the medium and large projects in run's
    summary.tsv whose analysis finished (exit 0 or 1)."""
    return {
        r[0]: (r[2], int(r[4]))
        for r in tsv_rows(run / "summary.tsv")
        if r[2] in SLOWDOWN_SIZES and r[3] in ("0", "1")
    }


def slowdowns(base: Path, head: Path) -> list[list]:
    """[slug, size, base_ms, head_ms] of the projects in both runs' medians
    whose head median slowed down past base's."""
    base_ms = medians(base)
    return [
        [slug, size, base_ms[slug][1], ms]
        for slug, (size, ms) in medians(head).items()
        if slug in base_ms and slowed(base_ms[slug][1], ms)
    ]


def confirm_slowdowns(rows: list[list], rerun: Callable[[str], int]) -> list[list]:
    """Re-measure each slowed project: `rerun(slug)` is its median over
    CONFIRM_RUNS cold runs. Appends that median to each row and returns the
    rows it still finds slowed."""
    for row in rows:
        row.append(rerun(row[0]))
    return [r for r in rows if slowed(r[2], r[4])]


def large_totals(base: Path, head: Path) -> tuple[int, int, list[str]]:
    """(base sum, head sum, slugs) of median_ms over the large projects in both
    runs' medians."""
    base_ms, head_ms = medians(base), medians(head)
    slugs = sorted(s for s, (size, _) in head_ms.items() if size == "large" and s in base_ms)
    return sum(base_ms[s][1] for s in slugs), sum(head_ms[s][1] for s in slugs), slugs


def drifted(base_sum: int, head_sum: int) -> bool:
    return head_sum > base_sum * DRIFT_RATIO


def drift_warnings(base: Path, head: Path) -> list[list]:
    """[slug, size, base_ms, head_ms] of the projects in both runs' medians
    at DRIFT_WARN_RATIO times base's and DRIFT_WARN_MIN_MS over it."""
    base_ms = medians(base)
    return [
        [slug, size, base_ms[slug][1], ms]
        for slug, (size, ms) in medians(head).items()
        if slug in base_ms
        and ms >= base_ms[slug][1] * DRIFT_WARN_RATIO
        and ms - base_ms[slug][1] >= DRIFT_WARN_MIN_MS
    ]


def ratio(base_ms: int, head_ms: int) -> str:
    return f"{head_ms / base_ms:.2f}x" if base_ms else "-"


def slowdown_md(
    base: Path, head: Path, rows: list[list], again: tuple[int, int] | None = None
) -> list[str]:
    """compare.md section of criteria 9 and 10; `rows` as confirm_slowdowns
    left them, `again` the (base, head) large-project sums of a re-measure."""
    base_ms, head_ms = medians(base), medians(head)
    both = base_ms.keys() & head_ms.keys()
    base_sum, head_sum, _ = large_totals(base, head)
    remeasured = "" if again is None else f", re-measured {again[0]} ms -> {again[1]} ms ({ratio(*again)})"
    verdict = "❌ FAIL" if drifted(*(again or (base_sum, head_sum))) else "✅ PASS"
    warnings = drift_warnings(base, head)
    return [
        "## Run time, medium and large projects",
        "",
        f"Median cold run summed over the {len(both)} projects in both runs: "
        f"{sum(base_ms[s][1] for s in both)} ms -> {sum(head_ms[s][1] for s in both)} ms.",
        f"Large projects summed: {base_sum} ms -> {head_sum} ms ({ratio(base_sum, head_sum)}){remeasured}; "
        f"fails over {DRIFT_RATIO:.2f}x (#1088): {verdict}",
        "",
        f"A project fails at >= {SLOWDOWN_RATIO:g}x the baseline and >= {SLOWDOWN_MIN_MS} ms over it, "
        f"in this run and in a re-measure over {CONFIRM_RUNS} runs (#1025).",
        "",
        *(
            oc.md_table(
                ["Project", "Size", "Base ms", "Head ms", "Re-measured ms", "Result"],
                [[*r, "❌ FAIL" if slowed(r[2], r[4]) else "✅ noise"] for r in rows],
                "llrrrl",
            )
            if rows
            else ["_No project slowed down past the criterion._"]
        ),
        "",
        f"Warning (does not gate): a project at >= {DRIFT_WARN_RATIO:g}x the baseline "
        f"and >= {DRIFT_WARN_MIN_MS} ms over it.",
        "",
        *(
            oc.md_table(
                ["Project", "Size", "Base ms", "Head ms", "Ratio"],
                [[*r, ratio(r[2], r[3])] for r in warnings],
                "llrrr",
            )
            if warnings
            else ["_No project reached the warning._"]
        ),
    ]


def parse_report(stdout: bytes) -> dict | None:
    """The JSON report, or None when the output is not a JSON object (crash)."""
    try:
        report = json.loads(stdout)
    except ValueError:
        return None
    return report if isinstance(report, dict) else None


def remove_caches(proj: Path) -> None:
    # Workspace members get their own .chokkin/ next to the project root's.
    for cache in list(proj.rglob(".chokkin")):
        shutil.rmtree(cache, ignore_errors=True)


def timed_run(
    bin_path: Path, proj: Path, runs: int, extra: tuple[str, ...]
) -> tuple[subprocess.CompletedProcess, int]:
    """Cold runs: every .chokkin/ cache is removed before each and after the last."""
    return timed_runs([bin_path], proj, runs, extra)[0]


def timed_runs(
    bins: list[Path], proj: Path, runs: int, extra: tuple[str, ...]
) -> list[tuple[subprocess.CompletedProcess, int]]:
    """timed_run of each binary, taking turns: every repetition runs each once,
    in reverse order on odd ones, so a machine that speeds up or slows down
    over the runs weighs on every binary alike."""
    times: list[list[int]] = [[] for _ in bins]
    last: dict[int, subprocess.CompletedProcess] = {}
    try:
        for rep in range(runs):
            order = range(len(bins)) if rep % 2 == 0 else reversed(range(len(bins)))
            for i in order:
                remove_caches(proj)
                start = time.monotonic()
                last[i] = oc.chokkin_raw(bins[i], proj, "--no-cache", *extra)
                times[i].append(int((time.monotonic() - start) * 1000))
    finally:
        remove_caches(proj)
    return [(last[i], int(statistics.median(t))) for i, t in enumerate(times)]


def chk003_gate_count(out_dir: Path, expect_slugs: set[str]) -> int:
    """CHK003 findings counted by the release gate: hits (not info), not `tp`,
    and outside the --expect projects (gated by their own floors) and recall
    sentinels."""
    skip = expect_slugs | {r[0] for r in tsv_rows(out_dir / "summary.tsv") if r[1] == "recall"}
    return sum(
        1 for r in tsv_rows(out_dir / "findings.tsv")
        if r[1] == "CHK003" and is_hit(r[7]) and r[3] != "tp" and r[0] not in skip
    )


def is_strict(out_dir: Path) -> bool:
    """Whether the run in `out_dir` used --strict, from its report.md."""
    return "- strict: yes" in (out_dir / "report.md").read_text(encoding="utf-8").splitlines()


def report_levels(out_dir: Path, slug: str) -> dict[tuple[str, str], Counter] | None:
    """(code, fingerprint) -> Counter of `severity/confidence` in one project's
    saved JSON report; None when the report is not JSON (crash). Import
    fingerprints carry no line, so one fingerprint can cover several findings."""
    issues = oc.report_issues(out_dir, slug)
    if issues is None:
        return None
    levels: dict[tuple[str, str], Counter] = {}
    for issue in issues:
        level = f"{issue.get('severity') or '?'}/{issue.get('confidence') or '?'}"
        levels.setdefault((issue["code"], issue["fingerprint"]), Counter())[level] += 1
    return levels


def finding_diff(base: Path, head: Path, slugs: list[str]) -> tuple[list[list[str]], list[str]]:
    """NEW / GONE / CHANGED findings of `slugs` between two runs' reports,
    keyed on (slug, fingerprint), and the slugs whose report on either side
    is not JSON. CHANGED means the severity or confidence moved, e.g. a
    library-mode downgrade from warning to info."""
    rows: list[list[str]] = []
    skipped: list[str] = []
    for slug in slugs:
        before, after = report_levels(base, slug), report_levels(head, slug)
        if before is None or after is None:
            skipped.append(slug)
            continue
        for code, fp in sorted(before.keys() | after.keys()):
            b, a = before.get((code, fp), Counter()), after.get((code, fp), Counter())
            # Findings whose level did not move cancel out; what is left pairs
            # up as CHANGED and the surplus on either side is GONE or NEW.
            old, new = sorted((b - a).elements()), sorted((a - b).elements())
            pairs = [("CHANGED", o, n) for o, n in zip(old, new, strict=False)]
            pairs += [("GONE", o, "-") for o in old[len(new):]] + [("NEW", "-", n) for n in new[len(old):]]
            rows += [[status, slug, code, tsv_field(fp), o, n] for status, o, n in pairs]
    return rows, skipped


SEVERITY_RANK = {"info": 0, "warning": 1, "error": 2}
CONFIDENCE_RANK = {"maybe": 0, "likely": 1, "certain": 2}


def demoted(before: str, after: str) -> bool:
    """Whether level `after` (`severity/confidence`) is below `before`: the
    severity dropped, or it held and the confidence dropped."""
    def rank(level: str) -> tuple[int, int]:
        severity, confidence = level.split("/")
        return SEVERITY_RANK.get(severity, 0), CONFIDENCE_RANK.get(confidence, 0)
    return rank(after) < rank(before)


def downgrade_vs_vanish(
    base: Path, head: Path, default_rows: list[list[str]], strict_rows: list[list[str]],
    labels: dict[tuple[str, str, str], tuple[str, str]],
) -> tuple[list[str], bool]:
    """Per-rule split of the default run's GONE into recontexted and demoted
    (HEAD findings only its strict report shows, `maybe` ones demoted), plus the
    default run's CHANGED findings whose level dropped, which are demoted too;
    of the strict-to-strict GONE into vanished by label; and whether no `tp`
    vanished."""
    cache: dict[tuple[Path, str], dict[tuple[str, str], str]] = {}
    conf_cache: dict[tuple[Path, str], dict[tuple[str, str], Counter]] = {}

    def keys(run: Path, slug: str) -> dict[tuple[str, str], str]:
        """(code, escaped fingerprint) -> target in one strict report."""
        if (run, slug) not in cache:
            issues = oc.report_issues(run / "strict", slug) or []
            cache[(run, slug)] = {(i["code"], tsv_field(i["fingerprint"])): i.get("target") or "?" for i in issues}
        return cache[(run, slug)]

    def confidences(run: Path, slug: str) -> dict[tuple[str, str], Counter]:
        """(code, escaped fingerprint) -> confidences of its findings in one report."""
        if (run, slug) not in conf_cache:
            out: dict[tuple[str, str], Counter] = {}
            for i in oc.report_issues(run, slug) or []:
                out.setdefault((i["code"], tsv_field(i["fingerprint"])), Counter())[i.get("confidence") or "?"] += 1
            conf_cache[(run, slug)] = out
        return conf_cache[(run, slug)]

    gone: Counter = Counter()
    recontexted: Counter = Counter()
    demoted_n: Counter = Counter()
    info_n: Counter = Counter()
    hidden: dict[tuple[str, str, str], Counter] = {}
    for status, slug, code, fp, before, after in default_rows:
        if status == "CHANGED" and demoted(before, after):
            demoted_n[code] += 1
            info_n[code] += is_hit(before.split("/")[0]) and not is_hit(after.split("/")[0])
        if status != "GONE":
            continue
        gone[code] += 1
        if (slug, code, fp) not in hidden:
            strict = confidences(head / "strict", slug).get((code, fp), Counter())
            hidden[(slug, code, fp)] = strict - confidences(head, slug).get((code, fp), Counter())
        left = hidden[(slug, code, fp)]
        if left["maybe"] > 0:
            left["maybe"] -= 1
            demoted_n[code] += 1
        elif +left:
            left[next(iter(+left))] -= 1
            recontexted[code] += 1
    vanished: Counter = Counter()
    lost: list[str] = []
    for _, slug, code, fp, _, _ in (r for r in strict_rows if r[0] == "GONE"):
        target = keys(base, slug).get((code, fp), "?")
        verdict = labels.get((slug, code, target), ("unknown", "-"))[0]
        vanished[(code, verdict if verdict in ("tp", "fp") else "other")] += 1
        # One fingerprint can cover several findings; the labelled target is
        # lost only when HEAD reports none of them.
        if verdict == "tp" and (code, fp) not in keys(head, slug):
            lost.append(f"{slug}/{code}/{target}")
    lines = [
        "## Downgrade vs vanish (--strict)",
        "",
        "GONE = default-run GONE; recontexted = of those, reported by HEAD's --strict run",
        "but not its default run, above `maybe` (a context change, e.g. examples/ became dev).",
        "Demoted = default-run CHANGED to a lower severity, or the same severity and a lower",
        "confidence, plus GONE that HEAD's --strict run reports at `maybe`; to info = of the",
        "CHANGED, warning or error that became info.",
        "Vanished = GONE between the two --strict reports (finding-diff-strict.tsv),",
        "by label; a vanished `tp` fails the gate, unlabelled ones need triage.",
        "",
    ]
    codes = sorted(gone.keys() | demoted_n.keys() | {c for c, _ in vanished})
    if not codes:
        return [*lines, "_No finding is GONE or demoted._"], True
    return [
        *lines,
        *oc.md_table(
            ["Rule", "GONE", "Recontexted", "Demoted", "To info", "Vanished tp", "Vanished fp", "Vanished other"],
            [[c, gone[c], recontexted[c], demoted_n[c], info_n[c],
              vanished[(c, "tp")], vanished[(c, "fp")], vanished[(c, "other")]]
             for c in codes],
            "lrrrrrrr",
        ),
        *(["", f"Vanished `tp`: {' '.join(lost)}"] if lost else []),
    ], not lost


def finding_diff_md(rows: list[list[str]], skipped: list[str]) -> list[str]:
    """Per-rule and per-project NEW / GONE / CHANGED counts; the findings
    themselves go to finding-diff.tsv."""
    statuses = ["NEW", "GONE", "CHANGED"]

    def counts(col: int) -> list[list]:
        n = Counter((r[col], r[0]) for r in rows)
        return [[k, *(n[(k, s)] for s in statuses)] for k in sorted({k for k, _ in n})]

    lines = ["## Finding-level diff", ""]
    if skipped:
        lines += [f"Not diffed (non-JSON report): {' '.join(skipped)}", ""]
    if not rows:
        return [*lines, "_No finding moved between baseline and HEAD._"]
    return [
        *lines,
        "Keyed on (slug, fingerprint). CHANGED = severity or confidence moved.",
        "Every finding is listed in finding-diff.tsv.",
        "",
        *oc.md_table(["Rule", *statuses], counts(2), "lrrr"),
        "",
        *oc.md_table(["Project", *statuses], counts(1), "lrrr"),
    ]


def compare_md(
    base: Path, head: Path, report: list[str], diff: tuple[list[list[str]], list[str]], vanish: list[str],
    timing: list[str],
) -> list[str]:
    """Before/after summary for the CI job page; the full scorecard lists every
    finding and outgrows the job summary limit."""
    base_sum = {r[0]: r for r in tsv_rows(base / "summary.tsv")}
    head_sum = {r[0]: r for r in tsv_rows(head / "summary.tsv")}
    base_find = tsv_rows(base / "findings.tsv")
    head_find = tsv_rows(head / "findings.tsv")

    def chk003_pairs(rows):
        return {(r[0], r[2]) for r in rows if r[1] == "CHK003"}

    def chk002(verdicts):
        return ["\t".join(r) for r in head_find if r[1] == "CHK002" and r[3] in verdicts]

    new_chk003 = sorted(chk003_pairs(head_find) - chk003_pairs(base_find))
    unclassified_chk003 = sum(
        1 for r in head_find if r[1] == "CHK003" and r[3] in ("unknown", "deferred")
    )
    start = report.index("## Exit criteria")
    end = report.index("## Per-rule label coverage and precision")
    return [
        *oc.md_table(
            ["Project", "CHK002 base", "CHK002 head", "CHK003 base", "CHK003 head"],
            [
                [s, base_sum[s][6], head_sum[s][6], base_sum[s][7], head_sum[s][7]]
                for s in sorted(base_sum.keys() & head_sum.keys())
            ],
            "lrrrr",
        ),
        "",
        "## CHK003 on HEAD only (slug, target)",
        "",
        "```tsv",
        *(f"{s}\t{t}" for s, t in new_chk003),
        "```",
        "",
        "## Unclassified CHK002 on HEAD",
        "",
        "```tsv",
        *chk002(("unknown", "deferred")),
        "```",
        "",
        "## CHK002 fp on HEAD",
        "",
        "```tsv",
        *chk002(("fp",)),
        "```",
        "",
        f"Unclassified CHK003 on HEAD: {unclassified_chk003}",
        "",
        *finding_diff_md(*diff),
        "",
        *vanish,
        *([""] if vanish else []),
        *timing,
        *([""] if timing else []),
        *report[start:end],
    ]


def main() -> int:
    args = parse_args()
    if args.build:
        oc.build()
    if args.clone:
        clone = subprocess.run(
            [str(oc.ROOT / "scripts/clone-oss-fixtures.sh"), "-m", str(args.manifest), "-o", str(args.clones)],
            check=False,
        )
        if clone.returncode != 0:
            print("warning: some clones failed; continuing with what is present", file=sys.stderr)
    oc.require_bin(args.bin)
    if not args.manifest.is_file():
        print(f"manifest not found: {args.manifest}", file=sys.stderr)
        return 2
    if args.baseline:
        # --strict changes verdicts as well as the confidence floor, so a
        # strict run is only comparable with another strict run (#716).
        if is_strict(args.baseline) != args.strict:
            print(f"{args.baseline} and this run differ in --strict; compare like with like", file=sys.stderr)
            return 2
        if args.with_strict and not (args.baseline / "strict").is_dir():
            print(f"--with-strict needs a baseline run with --with-strict: {args.baseline}", file=sys.stderr)
            return 2
    if args.time_bin:
        oc.require_bin(args.time_bin)

    extra = ("--strict",) if args.strict else ()
    targets: list[tuple[str, str, str, Path]] = []
    for row in oc.read_manifest(args.manifest):
        proj = args.clones / row["slug"]
        if proj.is_dir():
            targets.append((row["slug"], row["category"], row["size"], proj))
        else:
            print(f"skip (not cloned): {row['slug']}", file=sys.stderr)
    if args.recall.is_file():
        for slug, path in oc.read_recall(args.recall):
            if path.is_dir():
                targets.append((slug, "recall", "sentinel", path))
            else:
                print(f"skip (missing recall fixture): {slug}", file=sys.stderr)
    if not targets:
        print("no projects measured — run clone-oss-fixtures.sh first", file=sys.stderr)
        return 2

    labels = read_labels(args.labels)
    expect = read_expect(args.expect)
    out = args.output
    out.mkdir(parents=True, exist_ok=True)
    summary_rows: list[list] = []
    findings: list[list[str]] = []
    reported: set[tuple[str, str, str]] = set()  # hits only
    seen: set[tuple[str, str, str]] = set()  # every finding, incl. info
    expect_rows: list[list] = []
    crashes = config_errors = 0
    expect_misses: list[str] = []
    medium_slow: list[str] = []
    time_rows: list[list] = []

    for slug, category, size, proj in targets:
        print(f"==> {slug} ({category}/{size})", flush=True)
        if args.time_bin and size in SLOWDOWN_SIZES:
            (run, median_ms), (base_run, base_ms) = timed_runs([args.bin, args.time_bin], proj, args.runs, extra)
            time_rows.append([slug, category, size, base_run.returncode, base_ms])
        else:
            run, median_ms = timed_run(args.bin, proj, args.runs, extra)
        (out / f"{slug}.json").write_bytes(run.stdout)
        (out / f"{slug}.stderr").write_bytes(run.stderr)
        report = parse_report(run.stdout)
        if report is None:
            print(f"  non-JSON output (see {out / f'{slug}.stderr'})", file=sys.stderr)

        issues = report.get("issues", []) if report else []
        for issue in issues:
            target = issue.get("target")
            target = "?" if target is None else target
            severity = issue.get("severity") or "?"
            seen.add((slug, issue["code"], target))
            if is_hit(severity):
                reported.add((slug, issue["code"], target))
            verdict, bucket = labels.get((slug, issue["code"], target), ("unknown", "-"))
            findings.append([
                slug, issue["code"], tsv_field(target), verdict, bucket,
                tsv_field(issue.get("confidence") or "?"), tsv_field(issue.get("message") or ""),
                tsv_field(severity),
            ])
        by_code = {c: sum(1 for i in issues if i["code"] == c) for c in ("CHK002", "CHK003")}
        total = report.get("summary", {}).get("total", 0) if report else 0
        summary_rows.append([
            slug, category, size, run.returncode, median_ms, total, by_code["CHK002"], by_code["CHK003"],
        ])

        crashes += run.returncode == 3
        config_errors += run.returncode == 2
        if args.with_strict:
            strict_run, _ = timed_run(args.bin, proj, 1, ("--strict",))
            (out / "strict").mkdir(exist_ok=True)
            (out / "strict" / f"{slug}.json").write_bytes(strict_run.stdout)
            (out / "strict" / f"{slug}.stderr").write_bytes(strict_run.stderr)
            crashes += strict_run.returncode == 3
            config_errors += strict_run.returncode == 2
        if slug in expect:
            summary = (report or {}).get("summary", {})
            counts = ",".join(f"{k}={v}" for k, v in sorted(summary.get("by_code", {}).items()))
            runtime = summary.get("files", {}).get("reachable_runtime") or 0
            expect_rows.append([slug, runtime * 9 // 10, counts])
            expect_misses += expectation_misses(slug, report or {}, expect[slug])
        if too_slow(size, median_ms):
            medium_slow.append(f"{slug}={median_ms}ms")

    def write_tsv(name: str, header: list[str], rows: list[list]) -> Path:
        path = out / name
        lines = ["\t".join(header)] + ["\t".join(str(c) for c in r) for r in rows]
        path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        return path

    summary_path = write_tsv(
        "summary.tsv", ["slug", "category", "size", "exit", "median_ms", "total", "CHK002", "CHK003"], summary_rows
    )
    findings_path = write_tsv(
        "findings.tsv", ["slug", "code", "target", "verdict", "bucket", "confidence", "message", "severity"],
        findings,
    )
    write_tsv("expectations.tsv", ["# slug", "runtime_min", "counts"], expect_rows)
    if args.time_bin:
        (out / "time-baseline").mkdir(exist_ok=True)
        write_tsv("time-baseline/summary.tsv", ["slug", "category", "size", "exit", "median_ms"], time_rows)

    def count(code: str, verdict: str | None = None) -> int:
        return count_hits(findings, code, verdict)

    def info_count(code: str) -> int:
        return sum(1 for f in findings if f[1] == code and f[7] == "info")

    def precision_pct(code: str) -> str:
        tp, fp = precision(findings, code)
        return f"{100 * tp / (tp + fp):.1f}" if tp + fp else "n/a"

    # Recall sentinels are one-label fixtures, not projects: each would count
    # as a 100% project in the macro mean, so they stay in the pooled number only.
    sentinels = frozenset(slug for slug, category, _, _ in targets if category == "recall")
    per_project = {c: project_precision(findings, c, sentinels) for c in ALL_RULES}

    def macro_pct(code: str) -> str:
        macro = macro_precision(per_project[code])
        return "n/a" if macro is None else f"{macro:.1f} ({len(per_project[code])})"

    def share(code: str) -> str:
        top = max_share(per_project[code])
        return "n/a" if top is None else f"{top[0]} {top[1]:.0f}%"

    def by_project(code: str) -> str:
        return ", ".join(f"{slug} {tp}/{tp + fp}" for slug, (tp, fp) in ranked_projects(per_project[code]))

    def stale(code: str) -> int:
        # Labels of this rule whose finding is absent from the run: it was
        # fixed, or a clone revision bump moved its target.
        return sum(1 for (s, c, t), (v, _) in labels.items() if c == code and v != "deferred" and (s, c, t) not in seen)

    # The app-mode CHK001 corpus is recorded only for CHK002 (#1065): most of
    # its false positives are backends named by settings strings and CLIs run
    # from docker or shell, forms chokkin does not read yet, so the gate would
    # measure those known gaps. The orchestration plugin corpus (#1057) is too:
    # its false positives are a dbt adapter named by profiles.yml and the
    # webserver `dg dev` spawns, forms chokkin does not read either. The
    # diversity corpus is too: sphinx's runtime dependencies are the
    # extensions it loads by module-name strings, which chokkin does not map
    # to distributions yet.
    recorded_only = {
        "app-mode": oc.read_section(args.manifest, oc.APP_CORPUS_SECTION),
        "orchestration": oc.read_section(args.manifest, oc.ORCHESTRATION_CORPUS_SECTION),
        "diversity": oc.read_section(args.manifest, oc.DIVERSITY_CORPUS_SECTION),
    }
    skipped = frozenset().union(*recorded_only.values())
    fp_ok, y002_total, y002_fp, y002_unclassified = fp_gate([f for f in findings if f[0] not in skipped])
    fp_rate = f"{100 * y002_fp / y002_total:.1f}" if y002_total else "n/a"

    def recorded_rate(slugs: set[str]) -> str:
        _, total, fp, unclassified = fp_gate([f for f in findings if f[0] in slugs])
        return f"{100 * fp / total:.1f}% ({fp} FP / {total}, {unclassified} unclassified)" if total else "n/a"

    recorded_rates = "; ".join(f"{name} corpus, recorded only: {recorded_rate(slugs)}" for name, slugs in recorded_only.items())

    tp_labels = [k for k, (v, _) in labels.items() if v == "tp"]
    missed = recall_missed(labels, reported)
    # The diversity corpus (#1265) measures domains and build systems the rest
    # lacks, so its precision is recorded next to the floors rather than gated
    # by them: the floors guard the corpus they were measured on against
    # regressions, and the diversity numbers show how far that corpus is from
    # the rest of Python.
    diversity = recorded_only["diversity"]
    precision_low = precision_misses([f for f in findings if f[0] not in diversity])

    def diversity_precision() -> str:
        rows = [f for f in findings if f[0] in diversity]
        parts = []
        for code in PRECISION_FLOORS:
            tp, fp = precision(rows, code)
            if tp + fp:
                parts.append(f"{code} {100 * tp / (tp + fp):.1f}% ({tp}/{tp + fp})")
        return "; diversity corpus, recorded only: " + (", ".join(parts) or "n/a")
    certain_low = certain_misses(findings)

    def certain_measured(code: str) -> str:
        tp, fp, unclassified = certain_precision(findings, code)
        pct = f"{100 * tp / (tp + fp):.1f}%" if tp + fp else "n/a"
        return f"{code} {pct} ({tp} tp / {fp} fp, {unclassified} unclassified)"

    passes = {
        "fp": fp_ok,
        "precision": not precision_low,
        "certain": not certain_low,
        "recall": not missed,
        "crash": crashes == 0,
        "config": config_errors == 0,
        "expect": not expect_misses,
        "speed": not medium_slow,
    }

    def verdict(ok: bool) -> str:
        return "✅ PASS" if ok else "❌ FAIL"

    def coverage(code: str) -> str:
        rep = count(code)
        return f"{100 * (count(code, 'tp') + count(code, 'fp')) / rep:.1f}" if rep else "n/a"

    buckets: dict[str, list] = {}
    for f in findings:
        if f[1] == "CHK003" and f[4] not in ("-", ""):
            b = buckets.setdefault(f[4], [0, ""])
            b[0] += 1
            if len(b[1]) < 120:
                b[1] += f" {f[0]}/{f[2]}"
    bucket_rows = sorted(buckets.items(), key=lambda kv: (-kv[1][0], kv[0]))

    tp_detected = len(tp_labels) - len(missed)
    lines = [
        "# OSS validation — Phase 1 §17 scorecard",
        "",
        f"- chokkin version: `{(oc.chokkin_version(args.bin).split() + ['', ''])[1]}`",
        f"- projects measured: {len(targets)}",
        f"- generated: {oc.utc_now()}",
        f"- timed runs per project (median): {args.runs}",
        f"- strict: {'yes' if args.strict else 'no'}",
        "",
        "## Exit criteria",
        "",
        *oc.md_table(
            ["Criterion", "Target", "Measured", "Result"],
            [
                ["Unused-dep FP rate (CHK002)", f"< {FP_GATE_PCT}%",
                 f"{fp_rate}% ({y002_fp} FP / {y002_total} reported, {y002_unclassified} unclassified;"
                 f" {recorded_rates})",
                 verdict(passes["fp"])],
                ["Precision floor (labelled hits)",
                 ", ".join(f"{c} >= {f}%" for c, f in PRECISION_FLOORS.items()),
                 (" ".join(precision_low) or "all at or above") + diversity_precision(),
                 verdict(passes["precision"])],
                ["Precision floor, `certain` only (what --fix acts on)",
                 ", ".join(f"{c} >= {f}%, all labelled" for c, f in CERTAIN_FLOORS.items()),
                 "; ".join([*certain_low, *(certain_measured(c) for c in CERTAIN_FLOORS)]),
                 verdict(passes["certain"])],
                ["Recall (`tp` labels)", "all detected",
                 f"{tp_detected}/{len(tp_labels)} detected" + (f" (missed: {' '.join(missed)})" if missed else ""),
                 verdict(passes["recall"])],
                ["Crashes (exit 3)", "0", crashes, verdict(passes["crash"])],
                ["CLI/config errors (exit 2)", "0", config_errors, verdict(passes["config"])],
                ["Expectations (runtime floor, rule growth)", "within",
                 " ".join(expect_misses) or "all within", verdict(passes["expect"])],
                ["Cold run, medium project", f"<= {MEDIUM_GATE_MS} ms",
                 f"over: {' '.join(medium_slow)}" if medium_slow else "all within budget",
                 verdict(passes["speed"])],
            ],
        ),
        "",
        "## Per-rule label coverage and precision",
        "",
        "Reported counts hits (severity above info); Info is the info-severity",
        "findings left out of every other column. Coverage % = (tp + fp) / reported.",
        "Precision % = tp / (tp + fp); a rule in the precision-floor criterion fails",
        "the gate below its floor, CHK002 the FP-rate gate, the others are recorded only. For",
        "CHK001/CHK004/CHK006/CHK010 the labels are a stratified sample",
        "(scripts/sample-precision-labels.py). Deferred triage is not ground truth.",
        "Stale = labels whose finding is absent from this run. Reported=0 means the",
        "rule emitted nothing on this corpus — precision and recall are both unverified.",
        "Precision % pools every labelled hit (micro); Macro % is the mean of the",
        "per-project precisions over the (n) corpus projects with a labelled hit (recall",
        "sentinels excluded), and Max share the project holding the most of them (#1065).",
        "A large gap or share means one project drives the number. Floors apply to",
        "Precision % without the diversity corpus, whose own precision the exit",
        "criteria table records.",
        "",
        *oc.md_table(
            ["Rule", "Reported", "Info", "tp", "fp", "deferred", "unknown", "Coverage %", "Precision %",
             "Macro % (n)", "Max share", "Stale"],
            [
                [
                    c, count(c), info_count(c), count(c, "tp"), count(c, "fp"), count(c, "deferred"),
                    count(c, "unknown"), coverage(c), precision_pct(c), macro_pct(c), share(c), stale(c),
                ]
                for c in ALL_RULES
            ],
            "lrrrrrrrrrlr",
        ),
        "",
        "### Labelled hits by project",
        "",
        "tp / (tp + fp) per corpus project (recall sentinels excluded), most labelled first.",
        "",
        *(
            oc.md_table(
                ["Rule", "By project"],
                [[c, by_project(c)] for c in ALL_RULES if per_project[c]],
            )
            if any(per_project.values())
            else ["_No labelled hits on this run._"]
        ),
        "",
        "## CHK003 root-cause buckets",
        "",
        *(
            oc.md_table(
                ["Bucket", "Count", "Examples (slug/target)"],
                [[b, n, ex.lstrip(" ")] for b, (n, ex) in bucket_rows],
                "lrl",
            )
            if bucket_rows
            else ["_No CHK003 findings with a classified bucket on this run._"]
        ),
        "",
        "## Per-project results",
        "",
        *oc.md_table(
            ["Project", "Category", "Size", "Exit", "Median ms", "Issues", "CHK002", "CHK003"],
            summary_rows,
        ),
        "",
        "## Findings (CHK001–CHK010)",
        "",
        *(
            oc.md_table(
                ["Project", "Code", "Target", "Verdict", "Bucket", "Confidence", "Severity", "Message"],
                [[*f[:6], f[7], f[6]] for f in findings],
            )
            if findings
            else ["_No findings across the set._"]
        ),
        "",
        "## Notes",
        "",
        "- FP rate denominator is reported CHK002 findings (user-facing precision).",
        "- CHK002 unclassified = unknown + deferred; both block the §17 FP gate.",
        "- Recall gate counts every `tp` label (all rules, incl. sentinels); an info-severity finding does not satisfy it.",
        f"- CHK003 (missing dependency): {count('CHK003')} reported ({count('CHK003', 'fp')} FP, "
        f"{count('CHK003', 'tp')} tp, {count('CHK003', 'deferred')} deferred, "
        f"{count('CHK003', 'unknown')} unknown) plus {info_count('CHK003')} at info — the count is not a §17 gate;",
        "  its precision has a floor.",
        "- Large-size projects are reported but excluded from the medium cold-run gate.",
    ]
    report_path = out / "report.md"
    report_path.write_text("\n".join(lines) + "\n", encoding="utf-8")

    print(f"\nSummary : {summary_path}\nFindings: {findings_path}\nReport  : {report_path}\n")
    print("\n".join(lines[lines.index("## Exit criteria"):lines.index("## Per-project results")]))

    if args.baseline:
        diff_header = ["status", "slug", "code", "fingerprint", "before", "after"]
        slugs = sorted({r[0] for r in tsv_rows(args.baseline / "summary.tsv")} & {r[0] for r in summary_rows})
        diff = finding_diff(args.baseline, out, slugs)
        write_tsv("finding-diff.tsv", diff_header, diff[0])
        vanish: list[str] = []
        if args.with_strict:
            strict_diff = finding_diff(args.baseline / "strict", out / "strict", slugs)
            write_tsv("finding-diff-strict.tsv", diff_header, strict_diff[0])
            vanish, passes["vanish"] = downgrade_vs_vanish(args.baseline, out, diff[0], strict_diff[0], labels)
            if strict_diff[1]:
                vanish[1:1] = ["", f"Not diffed under --strict (non-JSON report): {' '.join(strict_diff[1])}"]
            print("\n".join(vanish))
            if not passes["vanish"]:
                print("findings vanished from the --strict output; see compare.md", file=sys.stderr)
        timing = []
        if not args.no_slowdown:
            time_base = out / "time-baseline" if args.time_bin else args.baseline
            projects = {slug: proj for slug, _, _, proj in targets}

            def rerun(slug: str) -> int:
                return timed_run(args.bin, projects[slug], CONFIRM_RUNS, extra)[1]

            slow = slowdowns(time_base, out)
            for slug, *_ in slow:
                print(f"==> {slug}: re-measuring a slowdown over {CONFIRM_RUNS} runs", flush=True)
            still_slow = confirm_slowdowns(slow, rerun)
            base_sum, head_sum, large = large_totals(time_base, out)
            again = None
            if drifted(base_sum, head_sum):
                print(f"==> re-measuring {len(large)} large projects over {CONFIRM_RUNS} runs", flush=True)
                if args.time_bin:
                    pairs = [
                        timed_runs([args.time_bin, args.bin], projects[slug], CONFIRM_RUNS, extra) for slug in large
                    ]
                    again = (sum(b[1] for b, _ in pairs), sum(h[1] for _, h in pairs))
                else:
                    again = (base_sum, sum(rerun(slug) for slug in large))
            timing = slowdown_md(time_base, out, slow, again)
            print("\n".join(timing))
            passes["slowdown"] = not still_slow
            if still_slow:
                print(f"slower than {time_base}: {' '.join(r[0] for r in still_slow)}", file=sys.stderr)
            passes["drift"] = not drifted(*(again or (base_sum, head_sum)))
            if not passes["drift"]:
                print(f"large projects drifted past {DRIFT_RATIO:.2f}x {time_base}", file=sys.stderr)
        compare = compare_md(args.baseline, out, lines, diff, vanish, timing)
        (out / "compare.md").write_text("\n".join(compare) + "\n", encoding="utf-8")
        base_n = chk003_gate_count(args.baseline, set(expect))
        head_n = chk003_gate_count(out, set(expect))
        print(f"CHK003 hits (excluding tp): baseline={base_n} head={head_n}")
        passes["chk003"] = head_n <= base_n
        if not passes["chk003"]:
            print(f"CHK003 grew over {args.baseline}", file=sys.stderr)

    if args.gate and not all(passes.values()):
        print("§17 gate FAILED", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
