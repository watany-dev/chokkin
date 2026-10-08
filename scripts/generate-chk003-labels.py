#!/usr/bin/env python3
"""Generate CHK003 triage labels from oss-metrics findings.tsv.

Heuristic triage for Step 0 volume (see docs/dev/plans/phase-3x-step0-chk003-measurement.md).
Re-run after `make oss-metrics` when the OSS clone set or chokkin version changes.

Labels match on (slug, code, target), and one target can carry several
findings (one per import line). A key gets one row; when its findings mix an
optional try-import with a hard import, the hard import's verdict wins because
info-severity findings are not hits (#656).

Findings that already carry a label are skipped, so the output only holds new
rows. A finding no heuristic matches is emitted as `deferred` with bucket `-`
and counted on stderr; triage it by hand (#661).

Usage:
  scripts/generate-chk003-labels.py findings.tsv >> scripts/oss-fixtures.labels.tsv
"""

import csv
import sys
from pathlib import Path

UNCLASSIFIED = (
    "deferred",
    "-",
    "deferred: unclassified by heuristic, needs manual triage",
)


def _classify(target: str, message: str) -> tuple[str, str, str] | None:
    if message.startswith("optional try-import"):
        return (
            "info-expected",
            "optional-import",
            "auto: optional try-import, expected at info severity (#504)",
        )
    low = target.lower()
    if any(
        marker in low
        for marker in (
            "selenium",
            "pytest",
            "docs_src",
            "_typeshed",
            "/tests/",
            "tests/",
        )
    ):
        return (
            "deferred",
            "dev-context",
            "deferred: verify test/docs/typing dependency policy",
        )
    if "no lockfile" in message:
        return (
            "deferred",
            "transitive-policy",
            "deferred: no lockfile — declaration/transitive boundary needs validation",
        )
    return None


def _main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2

    findings_path = Path(sys.argv[1])
    if not findings_path.is_file():
        print(f"findings file not found: {findings_path}", file=sys.stderr)
        return 2

    labels: dict[tuple[str, str], tuple[str, str, str]] = {}
    with findings_path.open(newline="") as handle:
        reader = csv.reader(handle, delimiter="\t")
        next(reader, None)
        for row in reader:
            if len(row) < 7 or row[1] != "CHK003":
                continue
            slug, _code, target, label, _bucket, _conf, message = row[:7]
            if slug == "missing_yaml" and target == "src/acme/main.py:yaml":
                continue
            if label != "unknown":
                continue
            verdict = _classify(target, message)
            if verdict is None:
                verdict = UNCLASSIFIED
            prev = labels.get((slug, target))
            if prev is None or prev[0] == "info-expected":
                labels[(slug, target)] = verdict

    unclassified = 0
    for (slug, target), label in sorted(labels.items()):
        verdict, bucket, note = label
        print(f"{slug}\tCHK003\t{target}\t{verdict}\t{bucket}\t{note}")
        if label is UNCLASSIFIED:
            unclassified += 1
            print(f"unclassified CHK003: {slug}\t{target}", file=sys.stderr)
    print(
        f"# generated {len(labels)} CHK003 labels ({unclassified} unclassified)",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(_main())
