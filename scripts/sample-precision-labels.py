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

Usage:
  scripts/sample-precision-labels.py --rule CHK006 [OPTIONS]

Options:
  --rule CODE           Rule to sample (required)
  --total N             Labels the rule should end up with (default: 50)
  --per-project N       Most labels from one project (default: 6)
  -f, --findings PATH   oss-metrics findings.tsv (default: target/oss-metrics/findings.tsv)
  -l, --labels PATH     Labels file (default: scripts/oss-fixtures.labels.tsv)

Output: label rows to fill in, `slug code target TODO - message`, one per line.
Replace TODO with tp or fp (and `-` with a bucket for fp), replace the message
with how the verdict was verified, and append the rows to the labels file.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
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


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--rule", required=True)
    p.add_argument("--total", type=int, default=50)
    p.add_argument("--per-project", type=int, default=6)
    p.add_argument("-f", "--findings", type=Path, default=oc.ROOT / "target/oss-metrics/findings.tsv")
    p.add_argument("-l", "--labels", type=Path, default=oc.ROOT / "scripts/oss-fixtures.labels.tsv")
    args = p.parse_args()
    if not args.findings.is_file():
        print(f"findings file not found: {args.findings} (run make oss-metrics)", file=sys.stderr)
        return 2

    sentinels = {slug for slug, _ in oc.read_recall(oc.DEFAULT_RECALL)}
    labelled = labelled_keys(args.labels, args.rule)
    queues: dict[str, dict[str, str]] = defaultdict(dict)
    with args.findings.open(newline="", encoding="utf-8") as handle:
        for row in csv.DictReader(handle, delimiter="\t", quoting=csv.QUOTE_NONE):
            if row["code"] != args.rule or row["severity"] == "info" or row["slug"] in sentinels:
                continue
            queues[row["slug"]][row["target"]] = row["message"]

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
