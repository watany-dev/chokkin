#!/usr/bin/env python3
"""Draw a per-project stratified sample of findings to label by hand (#656).

The §17 corpus reports thousands of CHK001 / CHK004 / CHK006 / CHK010 findings,
concentrated in a few large projects (django, mlflow, airflow, ...). Labelling
them all is not feasible and sampling in proportion to the counts would measure
only those projects, so this script picks round-robin across projects, at most
--per-project findings each, until --total findings of the rule are labelled.

Each finding's place in its project's queue is sha256(slug, code, target), so it
does not depend on the other findings. When a clone revision bump or a fix makes
a labelled finding disappear (it shows up as stale in report.md), the remaining
labels stay in the sample and the next run of this script tops the sample back
up with the next findings in hash order. Delete the stale rows when refilling.

Only hits are sampled: info-severity findings and the recall sentinels are left
out. Labels already in --labels count towards the quota and are not printed.

The cap applies to labels added before it, too: --prune rewrites --labels so
that no project keeps more than --per-project labelled hits of the rule, keeping
the first ones in hash order (#1065). Without it, a project labelled before the
cap existed (airflow held 87% of the CHK001 labels) would still dominate the
pooled precision. Labels that are not hits (info, stale) are left alone.

--maybe draws from the findings the default output hides instead: the rule's
`maybe` findings in the --strict reports (oss-metrics --with-strict writes them
to strict/ next to findings.tsv), labelled in their own file so that they never
count as stale default-run labels. Their tp share is the false-negative rate of
hiding `maybe` by default (#1065); it is recorded only. A strict report that is
not JSON is skipped with a warning.

Usage:
  scripts/sample-precision-labels.py --rule CHK006 [OPTIONS]

Options:
  --rule CODE           Rule to sample (required)
  --total N             Labels the rule should end up with (default: 50)
  --per-project N       Most labels from one project (default: 6)
  -f, --findings PATH   oss-metrics findings.tsv (default: target/oss-metrics/findings.tsv)
  -l, --labels PATH     Labels file (default: scripts/oss-fixtures.labels.tsv,
                        with --maybe scripts/oss-maybe.labels.tsv)
  --maybe               Sample the `maybe` findings of the --strict reports
  --prune               Drop the labelled hits over --per-project instead of sampling

Output: label rows to fill in, `slug code target TODO - message`, one per line.
Replace TODO with tp or fp (and `-` with a bucket for fp), replace the message
with how the verdict was verified, and append the rows to the labels file.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import sys
from collections import defaultdict
from pathlib import Path

import oss_corpus as oc


def rank(slug: str, code: str, target: str) -> str:
    return hashlib.sha256(f"{slug}\0{code}\0{target}".encode()).hexdigest()


def labelled_keys(path: Path, code: str) -> set[tuple[str, str]]:
    keys = set()
    for line in path.read_text(encoding="utf-8").splitlines():
        cols = line.split("\t")
        if line.startswith("#") or len(cols) < 5 or cols[1] != code:
            continue
        keys.add((cols[0], cols[2]))
    return keys


def maybe_hits(strict_dir: Path, code: str) -> dict[str, dict[str, str]]:
    """slug -> target -> message of the `maybe` findings of `code` in each
    `<slug>.json` --strict report in `strict_dir`."""
    hits: dict[str, dict[str, str]] = defaultdict(dict)
    for path in sorted(strict_dir.glob("*.json")):
        try:
            issues = json.loads(path.read_text(encoding="utf-8"))["issues"]
        except (ValueError, KeyError) as error:
            print(f"skipping {path}: {error}", file=sys.stderr)
            continue
        for issue in issues:
            if issue["code"] == code and issue["confidence"] == "maybe":
                hits[path.stem][issue["target"]] = issue["message"]
    return dict(hits)


def prune(lines: list[str], hits: dict[str, set[str]], code: str, per_project: int) -> list[str]:
    """`lines` without the labelled hits of `code` past the first `per_project`
    of each project in hash order."""

    def hit(line: str) -> tuple[str, str] | None:
        cols = line.split("\t")
        if line.startswith("#") or len(cols) < 5 or cols[1] != code or cols[2] not in hits.get(cols[0], ()):
            return None
        return cols[0], cols[2]

    labelled: dict[str, list[str]] = defaultdict(list)
    for key in filter(None, map(hit, lines)):
        labelled[key[0]].append(key[1])
    dropped = {
        (slug, t) for slug, ts in labelled.items() for t in sorted(ts, key=lambda t: rank(slug, code, t))[per_project:]
    }
    return [line for line in lines if hit(line) not in dropped]


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--rule", required=True)
    p.add_argument("--total", type=int, default=50)
    p.add_argument("--per-project", type=int, default=6)
    p.add_argument("-f", "--findings", type=Path, default=oc.ROOT / "target/oss-metrics/findings.tsv")
    p.add_argument("-l", "--labels", type=Path)
    p.add_argument("--prune", action="store_true")
    p.add_argument("--maybe", action="store_true")
    args = p.parse_args()
    if args.labels is None:
        args.labels = oc.ROOT / ("scripts/oss-maybe.labels.tsv" if args.maybe else "scripts/oss-fixtures.labels.tsv")
    if not args.findings.is_file():
        print(f"findings file not found: {args.findings} (run make oss-metrics)", file=sys.stderr)
        return 2

    sentinels = {slug for slug, _ in oc.read_recall(oc.DEFAULT_RECALL)}
    labelled = labelled_keys(args.labels, args.rule)
    queues: dict[str, dict[str, str]] = defaultdict(dict)
    if args.maybe:
        queues.update((s, q) for s, q in maybe_hits(args.findings.parent / "strict", args.rule).items() if s not in sentinels)
    else:
        with args.findings.open(newline="", encoding="utf-8") as handle:
            for row in csv.DictReader(handle, delimiter="\t", quoting=csv.QUOTE_NONE):
                if row["code"] != args.rule or row["severity"] == "info" or row["slug"] in sentinels:
                    continue
                queues[row["slug"]][row["target"]] = row["message"]

    if args.prune:
        lines = args.labels.read_text(encoding="utf-8").splitlines()
        hits = {slug: set(q) for slug, q in queues.items()}
        kept = prune(lines, hits, args.rule, args.per_project)
        args.labels.write_text("".join(f"{line}\n" for line in kept), encoding="utf-8")
        print(f"# {args.rule}: dropped {len(lines) - len(kept)} labels over {args.per_project} a project", file=sys.stderr)
        return 0

    taken = {slug: sum((slug, t) in labelled for t in q) for slug, q in queues.items()}
    have = sum(taken.values())
    order = {
        slug: iter(sorted((t for t in q if (slug, t) not in labelled), key=lambda t: rank(slug, args.rule, t)))
        for slug, q in queues.items()
    }
    picked = []
    while have < args.total:
        progressed = False
        for slug in sorted(order):
            if have >= args.total or taken[slug] >= args.per_project:
                continue
            target = next(order[slug], None)
            if target is None:
                continue
            picked.append((slug, target, queues[slug][target]))
            taken[slug] += 1
            have += 1
            progressed = True
        if not progressed:
            break

    for slug, target, message in picked:
        print(f"{slug}\t{args.rule}\t{target}\tTODO\t-\t{message}")
    print(
        f"# {args.rule}: {have - len(picked)} labelled, {len(picked)} to label, "
        f"{sum(len(q) for q in queues.values())} hits in {len(queues)} projects",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
