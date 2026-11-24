"""Evaluate each case with `packaging`: does the marker hold on every
interpreter `requires-python` allows (versions 2.0.0 to 4.15.3)?

Usage: python3 oracle.py cases.tsv py.out
Writes `true` / `false`, `INVALID` for a marker packaging rejects, `ERR`
when evaluation raises.
"""

import sys

from packaging.markers import InvalidMarker, Marker
from packaging.specifiers import SpecifierSet

GRID = [(a, b, c) for a in (2, 3, 4) for b in range(16) for c in range(4)]


def environment(version: tuple[int, int, int]) -> dict[str, str]:
    return {
        "python_version": f"{version[0]}.{version[1]}",
        "python_full_version": f"{version[0]}.{version[1]}.{version[2]}",
    }


def verdict(supported: list[tuple[int, int, int]], marker: str) -> str:
    try:
        parsed = Marker(marker)
    except InvalidMarker:
        return "INVALID"
    try:
        return str(all(parsed.evaluate(environment(v)) for v in supported)).lower()
    except Exception:  # noqa: BLE001 - any evaluation error is one outcome
        return "ERR"


def main() -> None:
    supported_by: dict[str, list[tuple[int, int, int]]] = {}
    lines = []
    with open(sys.argv[1]) as cases:
        for line in cases:
            requires, marker = line.rstrip("\n").split("\t")
            if requires not in supported_by:
                specifiers = SpecifierSet(requires)
                supported_by[requires] = [
                    v
                    for v in GRID
                    if specifiers.contains(
                        environment(v)["python_full_version"], prereleases=True
                    )
                ]
            lines.append(verdict(supported_by[requires], marker))
    with open(sys.argv[2], "w") as out:
        out.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
