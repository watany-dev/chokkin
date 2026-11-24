"""Group mismatches by which lock feature the oracle needed to reach the import.

For each mismatch, recompute reachability with one feature switched off:
  seed-extras : declared p[e] seeds (p, e) and walks what e adds
  edge-extras : a dependency on q[x] turns on q's extra x
  optional    : an extra's packages are installed only through that extra
"""

import json
import sys
from collections import Counter


def reach(sc, seed_extras=True, edge_extras=True):
    pk = {p["name"]: p for p in sc["lock"]}
    seen, stack = set(), []
    for d in sc["declared"]:
        stack.append((d["name"], None))
        if seed_extras:
            stack += [(d["name"], e) for e in d["extras"]]
    while stack:
        n = stack.pop()
        if n in seen:
            continue
        seen.add(n)
        p = pk.get(n[0])
        if not p:
            continue
        deps = p["deps"] if n[1] is None else p["optional"].get(n[1], [])
        for d in deps:
            stack.append((d["name"], None))
            if edge_extras:
                stack += [(d["name"], e) for e in d["extras"]]
    return {n for n, _ in seen}


def main():
    c = Counter()
    examples = {}
    with open(sys.argv[1]) as f:
        lines = f.readlines()
    for line in lines:
        m = json.loads(line)
        sc = m["scenario"]
        d = m["dist"]
        cause = []
        if m["want"] == "CHK004/certain":
            if d not in reach(sc, seed_extras=False):
                cause.append("needs-seed-extras")
            if d not in reach(sc, edge_extras=False):
                cause.append("needs-edge-extras")
        key = (m["format"], m["want"], m["got"], tuple(cause))
        c[key] += 1
        examples.setdefault(key, m)
    for k, v in sorted(c.items()):
        print(v, k)
    with open(sys.argv[1] + ".examples.json", "w") as f:
        json.dump({str(k): v for k, v in examples.items()}, f, indent=1)


if __name__ == "__main__":
    main()
