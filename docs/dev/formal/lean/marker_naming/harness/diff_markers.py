"""O6: PEP 508 marker / PEP 440 "always holds under requires-python".

chokkin skips CHK002 (non-strict) for a dependency whose marker may leave it
uninstalled on a supported interpreter, and reports it when the marker holds
on every one (#919). The Lean spec decides the latter over all interpreters
X.Y.Z (X in 2..4, Y < 17, Z < 7) and OS / platform values.

FP: chokkin reports CHK002 (treats the marker as always true) but the spec
finds a supported interpreter where it is false.
FN: the spec says always, chokkin skips (expected for forms outside its
documented model; listed separately).
"""

import random
import sys

from common import chokkin, issues, oracle, pyproject

OPS = ["<", "<=", "==", "!=", ">=", ">", "~="]
PV = [
    "2",
    "3",
    "3.0",
    "3.8",
    "3.9",
    "3.10",
    "3.12",
    "3.9.0",
    "3.9.1",
    "3.10.2",
    "4",
    "3.*",
    "3.9.*",
    "3.10.*",
    "3.8.0.0",
    "03.09",
]
PFV = [
    "3",
    "3.9",
    "3.9.0",
    "3.9.1",
    "3.10.0",
    "3.10.2",
    "3.12.4",
    "3.9.*",
    "3.10.*",
    "3.9.2.*",
    "4.0",
    "3.8.0.0",
]
RP = [
    ">=3.9",
    ">=3.8,<4",
    ">3.9",
    "~=3.9",
    "==3.10.*",
    ">=3.9.2",
    "<=3.11",
    "!=3.9.*,>=3.8",
    ">=3.10, <3.13",
    ">=3",
    "~=3.9.1",
]


def atom(rng):
    kind = rng.random()
    if kind < 0.45:
        var, vals = "python_version", PV
    elif kind < 0.85:
        var, vals = "python_full_version", PFV
    elif kind < 0.93:
        return f"os_name {rng.choice(['==', '!='])} '{rng.choice(['nt', 'posix'])}'"
    else:
        return f"sys_platform {rng.choice(['==', '!='])} '{rng.choice(['linux', 'win32'])}'"
    op = rng.choice(OPS)
    val = rng.choice(vals)
    quote = rng.choice(["'", '"'])
    sp = rng.choice(["", " "])
    if rng.random() < 0.2 and op in ("<", "<=", ">", ">="):
        return f"{quote}{val}{quote}{sp}{op}{sp}{var}"
    return f"{var}{sp}{op}{sp}{quote}{val}{quote}"


def marker(rng, depth=0):
    if depth < 2 and rng.random() < 0.35:
        joined = f"{marker(rng, depth + 1)} {rng.choice(['and', 'or'])} {marker(rng, depth + 1)}"
        return f"({joined})" if rng.random() < 0.5 else joined
    return atom(rng)


def main(seed, rounds):
    rng = random.Random(seed)
    fps, fns = [], []
    for _ in range(rounds):
        rp = rng.choice(RP)
        markers = [marker(rng) for _ in range(40)]
        answers = oracle(
            [{"kind": "marker", "requires_python": rp, "marker": m} for m in markers]
        )
        deps = [f"zzdep{i}; {m}" for i, m in enumerate(markers)]
        report = chokkin(
            {"pyproject.toml": pyproject(deps, rp), "main.py": "print(1)\n"}
        )
        reported = issues(report, "CHK002")
        for i, (m, a) in enumerate(zip(markers, answers, strict=True)):
            got = f"zzdep{i}" in reported
            if a["verdict"] == "not_always" and got:
                fps.append((rp, m))
            elif a["verdict"] == "always" and not got:
                fns.append((rp, m))
    for rp, m in fps:
        print(f"FP\trequires-python={rp}\t{m}")
    for rp, m in fns:
        print(f"FN\trequires-python={rp}\t{m}")
    print(f"# {rounds * 40} markers, {len(fps)} FP, {len(fns)} FN", file=sys.stderr)


if __name__ == "__main__":
    main(
        int(sys.argv[1]) if len(sys.argv) > 1 else 0,
        int(sys.argv[2]) if len(sys.argv) > 2 else 10,
    )
