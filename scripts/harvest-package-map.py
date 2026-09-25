#!/usr/bin/env python3
"""Harvest reviewable distribution-to-import candidates from wheel metadata."""

from __future__ import annotations

import argparse
import csv
import hashlib
import io
import json
import re
import sys
import zipfile
from email.parser import BytesParser
from pathlib import Path

IMPORT_ROOT = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


def wheel_candidate(path: Path) -> tuple[str, set[str], str]:
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    with zipfile.ZipFile(path) as wheel:
        names = wheel.namelist()
        metadata_paths = sorted(
            name for name in names if name.endswith(".dist-info/METADATA")
        )
        if len(metadata_paths) != 1:
            raise ValueError(f"{path}: expected exactly one .dist-info/METADATA")

        metadata_path = metadata_paths[0]
        distribution = BytesParser().parsebytes(wheel.read(metadata_path)).get("Name")
        if not distribution:
            raise ValueError(f"{path}: METADATA has no Name field")

        dist_info = metadata_path.removesuffix("METADATA")
        top_level = f"{dist_info}top_level.txt"
        if top_level in names:
            imports = {
                line.strip()
                for line in wheel.read(top_level).decode("utf-8").splitlines()
                if IMPORT_ROOT.fullmatch(line.strip())
            }
        else:
            imports = imports_from_record(wheel, f"{dist_info}RECORD")

    if not imports:
        raise ValueError(f"{path}: no import roots found in top_level.txt or RECORD")
    return distribution, imports, digest


def imports_from_record(wheel: zipfile.ZipFile, record_path: str) -> set[str]:
    if record_path not in wheel.namelist():
        return set()
    imports = set()
    rows = csv.reader(io.StringIO(wheel.read(record_path).decode("utf-8")))
    for row in rows:
        if not row:
            continue
        top = row[0].split("/", 1)[0]
        if top.endswith(".py"):
            top = top.removesuffix(".py")
        if IMPORT_ROOT.fullmatch(top):
            imports.add(top)
    return imports


def harvest(paths: list[Path]) -> dict[str, object]:
    packages: dict[str, set[str]] = {}
    sources = []
    for path in sorted(paths, key=lambda item: item.name):
        distribution, imports, digest = wheel_candidate(path)
        packages.setdefault(distribution, set()).update(imports)
        sources.append({"file": path.name, "sha256": digest})
    return {
        "schema_version": "1",
        "sources": sources,
        "packages": [
            {"distribution": distribution, "imports": sorted(imports)}
            for distribution, imports in sorted(packages.items())
        ],
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Read pinned wheel archives statically and emit package-map candidates; "
            "does not download packages or modify the seed"
        )
    )
    parser.add_argument("wheels", nargs="+", type=Path)
    parser.add_argument("--output", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        payload = harvest(args.wheels)
    except (OSError, ValueError, zipfile.BadZipFile) as error:
        print(error, file=sys.stderr)
        return 2

    rendered = json.dumps(payload, indent=2, sort_keys=True) + "\n"
    try:
        if args.output:
            args.output.write_text(rendered, encoding="utf-8")
        else:
            sys.stdout.write(rendered)
    except OSError as error:
        print(error, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
