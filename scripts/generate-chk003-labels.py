#!/usr/bin/env python3
"""Generate CHK003 triage labels from oss-metrics findings.tsv.

Heuristic triage for Step 0 volume (see docs/dev/plans/phase-3x-step0-chk003-measurement.md).
Re-run after `make oss-metrics` when the OSS clone set or chokkin version changes.

Usage:
  scripts/generate-chk003-labels.py findings.tsv >> scripts/oss-fixtures.labels.tsv
"""

from __future__ import annotations

import csv
import sys
from pathlib import Path


def _classify(slug: str, target: str, message: str) -> tuple[str, str, str] | None:
    if message.startswith("optional try-import"):
        return (
            "fp",
            "optional-import",
            "auto: optional try-import not treated as hard missing dep",
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

    rows: list[tuple[str, str, str, str, str, str]] = []
    with findings_path.open(newline="") as handle:
        reader = csv.reader(handle, delimiter="\t")
        next(reader, None)
        for row in reader:
            if len(row) < 7 or row[1] != "CHK003":
                continue
            slug, _code, target, _verdict, _bucket, _conf, message = row[:7]
            if slug == "missing_yaml" and target == "src/acme/main.py:yaml":
                continue
            verdict = _classify(slug, target, message)
            if verdict is None:
                print(
                    f"unclassified CHK003: {slug}\t{target}\t{message}",
                    file=sys.stderr,
                )
                return 1
            v, bucket, note = verdict
            rows.append((slug, "CHK003", target, v, bucket, note))

    rows.sort(key=lambda item: (item[0], item[2]))
    for slug, code, target, verdict, bucket, note in rows:
        print(f"{slug}\t{code}\t{target}\t{verdict}\t{bucket}\t{note}")
    print(f"# generated {len(rows)} CHK003 labels", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(_main())
