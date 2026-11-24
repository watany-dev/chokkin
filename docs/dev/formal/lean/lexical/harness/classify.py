"""Group mismatches of gen_ignores.py by the shape of the line they are about,
so each group can be minimized to one reproducer by hand."""

import collections
import json
import re
import sys


def physical_lines(text):
    return text.replace("\r\n", "\n").replace("\r", "\n").split("\n")


def classify(row):
    lines = physical_lines(row["text"])
    if row["bucket"].startswith("chk003"):
        tags = set()
        for _m, l in sorted(set(map(tuple, row["want"])) ^ set(map(tuple, row["got"]))):
            line = lines[l - 1]
            nxt = lines[l] if l < len(lines) else ""
            code_part = line.split("  #")[0] if "  #" in line else line
            if line.rstrip().endswith("#") and nxt.startswith("chokkin:"):
                tags.add("directive-spans-newline")
            elif re.search(r"[ 　]", line):
                tags.add("unicode-whitespace")
            elif "chokkin" in code_part:
                tags.add("directive-in-string")
            elif any("chokkin" in x for x in lines):
                tags.add("directive-in-string-other-line")
            else:
                tags.add("other")
        return row["bucket"] + ":" + ",".join(sorted(tags))
    want = set(map(tuple, row["want"]))
    got = set(map(tuple, row["got"]))
    tags = set()
    for l, kind in want ^ got:
        line = lines[l - 1]
        side = "missing" if (l, kind) in want else "extra"
        ws = "unicode-ws" if re.search(r"[ 　]", line) else ""
        tags.add(f"{side}-{kind}{'-' + ws if ws else ''}")
    return "warnings:" + ",".join(sorted(tags))


with open(sys.argv[1]) as fh:
    rows = [json.loads(line) for line in fh]
groups = collections.defaultdict(list)
for row in rows:
    groups[classify(row)].append(row)
for k, v in sorted(groups.items(), key=lambda kv: -len(kv[1])):
    print(len(v), k)
    if len(sys.argv) > 2:
        shortest = min(v, key=lambda r: len(r["text"]))
        print("   ", repr(shortest["text"]), shortest.get("want"), shortest.get("got"))
