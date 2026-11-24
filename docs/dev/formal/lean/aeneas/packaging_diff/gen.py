"""Generate `requires-python<TAB>marker` cases for the packaging differential.

Usage: python3 gen.py cases.tsv
"""

import random
import sys

VALUES = [
    "2",
    "2.7",
    "3",
    "3.0",
    "3.7",
    "3.8",
    "3.8.0",
    "3.8.1",
    "3.8.5",
    "3.9",
    "3.10",
    "3.10.2",
    "3.12",
    "3.13",
    "4",
    "4.0",
    "3.*",
    "3.8.*",
    "3.10.*",
    "3.8.0.0",
    "3.8.1.0",
    "03.8",
    "3.8.0.*",
    "2.*",
    "4.*",
    "3.9.0.0.*",
]
OPERATORS = ["<", "<=", ">", ">=", "==", "!=", "~=", "==="]
VARIABLES = ["python_version", "python_full_version"]
REQUIRES = [
    ">=3.8",
    ">=3.9",
    ">=3.9,<3.13",
    "~=3.10",
    "==3.11.*",
    ">=3.8.1",
    ">3.8",
    "!=3.9.*,>=3.8",
    ">=3,<4",
    "<3.10",
    ">=2.7,!=3.0.*,!=3.1.*",
    "~=3.8.1",
    "==3.10",
    ">=3.10.0.0",
    "<=3.12",
]


def comparison(rng: random.Random) -> str:
    variable = rng.choice(VARIABLES)
    operator = rng.choice(OPERATORS)
    value = rng.choice(VALUES)
    quote = rng.choice(["'", '"'])
    if rng.random() < 0.25:
        return f"{quote}{value}{quote} {operator} {variable}"
    return f"{variable} {operator} {quote}{value}{quote}"


def expression(rng: random.Random, depth: int = 0) -> str:
    roll = rng.random()
    if depth < 2 and roll < 0.3:
        return f"{expression(rng, depth + 1)} and {expression(rng, depth + 1)}"
    if depth < 2 and roll < 0.5:
        return f"{expression(rng, depth + 1)} or {expression(rng, depth + 1)}"
    if depth < 2 and roll < 0.6:
        return f"({expression(rng, depth + 1)})"
    return comparison(rng)


def main() -> None:
    rng = random.Random(1)
    with open(sys.argv[1], "w") as out:
        out.writelines(
            f"{rng.choice(REQUIRES)}\t{expression(rng)}\n" for _ in range(60000)
        )


if __name__ == "__main__":
    main()
