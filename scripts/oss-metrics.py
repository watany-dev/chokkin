#!/usr/bin/env python3
"""Measure the Phase 1 §17 exit criteria over the OSS validation set.

Exit criteria (docs/dev/spec.ja.md §17):
  1. unused-dependency (CHK002) false-positive rate < 5%
  2. crashes (chokkin internal error, exit 3) == 0
  3. cold run on a `medium` project <= 2000 ms

Corpus regression gates (#495):
  4. CLI/config errors (exit 2) == 0
  5. every project in --expect stays within its floors (see below)

Release gate (with --baseline, #325):
  6. CHK003 findings not labelled `tp`, outside --expect projects and recall
     sentinels, do not grow over the baseline run

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
  --baseline DIR        --output of a baseline run; writes compare.md and adds
                        the CHK003 growth criterion
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
recorded in report.md (record only, no threshold yet; #656). CHK001 / CHK004 /
CHK006 / CHK010 labels are a per-project stratified sample drawn by
scripts/sample-precision-labels.py, so their precision is a sample estimate.
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
from pathlib import Path

import oss_corpus as oc

ALL_RULES = [f"CHK{n:03d}" for n in range(1, 11)]
MEDIUM_GATE_MS = 2000
FP_GATE_PCT = 5


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
    p.add_argument("--build", action="store_true")
    p.add_argument("--clone", action="store_true")
    p.add_argument("--gate", action="store_true")
    p.add_argument("-h", "--help", action="store_true")
    args = p.parse_args()
    if args.help:
        print(__doc__)
        sys.exit(0)
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


def remove_caches(proj: Path) -> None:
    # Workspace members get their own .chokkin/ next to the project root's.
    for cache in list(proj.rglob(".chokkin")):
        shutil.rmtree(cache, ignore_errors=True)


def timed_run(bin_path: Path, proj: Path, runs: int) -> tuple[subprocess.CompletedProcess, int]:
    """Cold runs: every .chokkin/ cache is removed before each and after the last."""
    times = []
    try:
        for _ in range(runs):
            remove_caches(proj)
            start = time.monotonic()
            run = oc.chokkin_raw(bin_path, proj, "--no-cache")
            times.append(int((time.monotonic() - start) * 1000))
    finally:
        remove_caches(proj)
    return run, int(statistics.median(times))


def chk003_gate_count(out_dir: Path, expect_slugs: set[str]) -> int:
    """CHK003 findings counted by the release gate: not `tp`, and outside the
    --expect projects (gated by their own floors) and recall sentinels."""
    skip = expect_slugs | {r[0] for r in tsv_rows(out_dir / "summary.tsv") if r[1] == "recall"}
    return sum(
        1 for r in tsv_rows(out_dir / "findings.tsv")
        if r[1] == "CHK003" and r[3] != "tp" and r[0] not in skip
    )


def compare_md(base: Path, head: Path, report: list[str]) -> list[str]:
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

    for slug, category, size, proj in targets:
        print(f"==> {slug} ({category}/{size})", flush=True)
        run, median_ms = timed_run(args.bin, proj, args.runs)
        (out / f"{slug}.json").write_bytes(run.stdout)
        (out / f"{slug}.stderr").write_bytes(run.stderr)
        try:
            report = json.loads(run.stdout)
        except ValueError:
            report = None
        if not isinstance(report, dict):
            report = None
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
        if slug in expect:
            summary = (report or {}).get("summary", {})
            counts = ",".join(f"{k}={v}" for k, v in sorted(summary.get("by_code", {}).items()))
            runtime = summary.get("files", {}).get("reachable_runtime") or 0
            expect_rows.append([slug, runtime * 9 // 10, counts])
            expect_misses += expectation_misses(slug, report or {}, expect[slug])
        if size == "medium" and median_ms > MEDIUM_GATE_MS:
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

    def count(code: str, verdict: str | None = None) -> int:
        return sum(
            1 for f in findings
            if f[1] == code and is_hit(f[7]) and (verdict is None or effective(f[3]) == verdict)
        )

    def info_count(code: str) -> int:
        return sum(1 for f in findings if f[1] == code and f[7] == "info")

    def precision(code: str) -> str:
        tp, fp = count(code, "tp"), count(code, "fp")
        return f"{100 * tp / (tp + fp):.1f}" if tp + fp else "n/a"

    def stale(code: str) -> int:
        # Labels of this rule whose finding is absent from the run: it was
        # fixed, or a clone revision bump moved its target.
        return sum(1 for (s, c, t), (v, _) in labels.items() if c == code and v != "deferred" and (s, c, t) not in seen)

    y002_total, y002_fp = count("CHK002"), count("CHK002", "fp")
    y002_unclassified = count("CHK002", "unknown") + count("CHK002", "deferred")
    fp_rate = f"{100 * y002_fp / y002_total:.1f}" if y002_total else "n/a"

    tp_labels = [k for k, (v, _) in labels.items() if v == "tp"]
    missed = [f"{s}/{c}/{t}" for s, c, t in tp_labels if (s, c, t) not in reported]

    passes = {
        "fp": y002_unclassified == 0 and (y002_total == 0 or 100 * y002_fp / y002_total < FP_GATE_PCT),
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
        "",
        "## Exit criteria",
        "",
        *oc.md_table(
            ["Criterion", "Target", "Measured", "Result"],
            [
                ["Unused-dep FP rate (CHK002)", f"< {FP_GATE_PCT}%",
                 f"{fp_rate}% ({y002_fp} FP / {y002_total} reported, {y002_unclassified} unclassified)",
                 verdict(passes["fp"])],
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
        "Precision % = tp / (tp + fp), recorded only (no threshold yet). For",
        "CHK001/CHK004/CHK006/CHK010 the labels are a stratified sample",
        "(scripts/sample-precision-labels.py). Deferred triage is not ground truth.",
        "Stale = labels whose finding is absent from this run. Reported=0 means the",
        "rule emitted nothing on this corpus — precision and recall are both unverified.",
        "",
        *oc.md_table(
            ["Rule", "Reported", "Info", "tp", "fp", "deferred", "unknown", "Coverage %", "Precision %", "Stale"],
            [
                [
                    c, count(c), info_count(c), count(c, "tp"), count(c, "fp"), count(c, "deferred"),
                    count(c, "unknown"), coverage(c), precision(c), stale(c),
                ]
                for c in ALL_RULES
            ],
            "lrrrrrrrrr",
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
        f"{count('CHK003', 'unknown')} unknown) plus {info_count('CHK003')} at info — informational, not a §17 gate.",
        "- Large-size projects are reported but excluded from the medium cold-run gate.",
    ]
    report_path = out / "report.md"
    report_path.write_text("\n".join(lines) + "\n", encoding="utf-8")

    print(f"\nSummary : {summary_path}\nFindings: {findings_path}\nReport  : {report_path}\n")
    print("\n".join(lines[lines.index("## Exit criteria"):lines.index("## Per-project results")]))

    if args.baseline:
        compare = compare_md(args.baseline, out, lines)
        (out / "compare.md").write_text("\n".join(compare) + "\n", encoding="utf-8")
        base_n = chk003_gate_count(args.baseline, set(expect))
        head_n = chk003_gate_count(out, set(expect))
        print(f"CHK003 (excluding tp): baseline={base_n} head={head_n}")
        passes["chk003"] = head_n <= base_n
        if not passes["chk003"]:
            print(f"CHK003 grew over {args.baseline}", file=sys.stderr)

    if args.gate and not all(passes.values()):
        print("§17 gate FAILED", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
