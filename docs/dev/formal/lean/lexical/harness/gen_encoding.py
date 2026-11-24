"""PEP 263 differential: which files chokkin reads, and as what text.

For every declared name (every alias and codec module of CPython's
`encodings`, plus spelling variants) and several placements, a `main.py`
holds `import m<letter>` with a non-ASCII letter the declared codec encodes.
The Lean oracle says how CPython decodes the file (`Decision`); CPython's
`compile` confirms it; chokkin's CHK010 symbol shows the name it read, or its
`skipped` warning says it read nothing.

Expected of chokkin: a file CPython reads with UTF-8 or with a codec chokkin
supports (spec §6: latin-1, cp1252 and the CJK codecs and their Python
aliases) is read, and the import has the name CPython gives it (NFKC of the
decoded text). A codec chokkin does not support is skipped with the warning
(a documented gap, not counted).

    python3 -I harness/gen_encoding.py --out encoding-mismatches.jsonl
"""

import argparse
import codecs
import collections
import encodings.aliases
import json
import os
import sys
import unicodedata

sys.path.insert(0, os.path.dirname(__file__))
from common import chokkin, oracle, project

SUPPORTED = {
    "latin_1",
    "cp1252",
    "shift_jis",
    "cp932",
    "euc_jp",
    "gbk",
    "gb2312",
    "gb18030",
    "big5",
    "cp950",
    "big5hkscs",
    "euc_kr",
    "cp949",
    "utf_8",
}
LETTERS = ["é", "ü", "ж", "α", "ア", "日", "한", "中", "ş", "ő", "ą", "ř", "ğ", "ā"]


def names():
    out = set(encodings.aliases.aliases) | set(encodings.aliases.aliases.values())
    for base in [
        "latin-1",
        "utf-8",
        "shift_jis",
        "cp1252",
        "euc-kr",
        "iso-8859-1",
        "gb2312",
        "big5",
    ]:
        out |= {
            base.upper(),
            base.replace("-", "_"),
            base.replace("_", "-"),
            base.replace("-", ""),
            base + "-unix",
            base + "-dos",
            "x-" + base,
            base.replace("-", "--"),
            base.replace("-", "."),
        }
    out |= {
        "latin-1-unix",
        "iso-latin-1",
        "ISO_8859-1",
        "utf8",
        "UTF8",
        "u8",
        "utf-8-sig",
        "windows-1252",
        "Windows_1252",
        "sjis",
        "SJIS",
        "ms_kanji",
        "mskanji",
        "cp-1252",
        "latin1",
        "l1",
        "iso8859-15",
        "koi8-r",
        "cp1251",
    }
    return sorted(out)


def letter_for(module):
    for ch in LETTERS:
        try:
            b = ch.encode(module)
        except (UnicodeError, LookupError):
            continue
        if all(x < 0x80 for x in b):
            continue
        try:
            back = b.decode(module)
        except (UnicodeError, LookupError):
            continue
        if ("m" + back).isidentifier():
            return ch, b
    return None, None


def py_reads(data):
    try:
        compile(data, "main.py", "exec", dont_inherit=True)
        return True
    except (SyntaxError, ValueError):
        return False


PLACEMENTS = {
    "line1": lambda decl: [decl],
    "emacs": lambda decl: [decl.replace("# coding: ", "# -*- coding: ") + " -*-"],
    "vim": lambda decl: [decl.replace("# coding: ", "# vim: set fileencoding=") + " :"],
    "line2-shebang": lambda decl: ["#!/usr/bin/env python", decl],
    "line2-blank": lambda decl: ["", decl],
    "line2-after-code": lambda decl: ["import os", decl],
    "line3": lambda decl: ["#!/usr/bin/env python", "#", decl],
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="encoding-mismatches.jsonl")
    args = ap.parse_args()
    stats, buckets = collections.Counter(), collections.defaultdict(list)
    jobs = []
    for name in names():
        try:
            module = codecs.lookup(name).name.replace("-", "_")
        except LookupError:
            module = None
        ch, payload = letter_for(name) if module else ("é", "é".encode("latin-1"))
        if payload is None:
            ch, payload = "é", "é".encode("latin-1")
        # The second payload is valid UTF-8 ("ê" = C3 AA): CPython still decodes
        # it with the declared codec, so it tells whether the declaration wins.
        for pay in (payload, "ê".encode()):
            for place, mk in PLACEMENTS.items():
                for bom in (False, True):
                    if pay != payload and (
                        bom or place not in ("line1", "line2-shebang")
                    ):
                        continue
                    head = "\n".join(mk(f"# coding: {name}")).encode("ascii") + b"\n"
                    data = (
                        (b"\xef\xbb\xbf" if bom else b"")
                        + head
                        + b"import m"
                        + pay
                        + b"\n"
                    )
                    jobs.append((name, place, bom, ch, data))
    for name, place, bom, _letter, data in jobs:
        root = project(data)
        [exp] = oracle([os.path.join(root, "main.py")])
        d = exp["decision"]
        py = py_reads(data)
        # Lean's decision must agree with CPython on whether a codec applies.
        if d.startswith(("unknown:", "bom-mismatch:")) and py:
            buckets["spec-vs-cpython"].append(
                {"name": name, "place": place, "bom": bom, "decision": d}
            )
            continue
        got = chokkin(root)
        sym = [
            i["symbol"]
            for i in got["issues"]
            if i["code"] == "CHK010" and i["file"] == "main.py"
        ]
        stats["cases"] += 1
        if not py:
            if not got["skipped"]:
                buckets["reads-what-cpython-rejects"].append(
                    {
                        "name": name,
                        "place": place,
                        "bom": bom,
                        "decision": d,
                        "symbol": sym,
                    }
                )
            continue
        module = d.replace("utf-8-bom", "utf_8").replace("utf-8", "utf_8")
        body = data[3:] if data.startswith(b"\xef\xbb\xbf") else data
        try:
            want = "m" + unicodedata.normalize(
                "NFKC", body.split(b"import m")[1][:-1].decode(module)
            )
        except (UnicodeError, LookupError):
            want = None
        if module not in SUPPORTED:
            if not got["skipped"]:
                buckets["reads-unsupported"].append(
                    {"name": name, "place": place, "decision": d, "symbol": sym}
                )
            else:
                stats["documented-gap"] += 1
            continue
        if got["skipped"]:
            buckets["skips-supported"].append(
                {"name": name, "place": place, "bom": bom, "decision": d}
            )
        elif sym != [want]:
            buckets["wrong-text"].append(
                {
                    "name": name,
                    "place": place,
                    "bom": bom,
                    "decision": d,
                    "want": want,
                    "got": sym,
                }
            )
    with open(args.out, "w") as f:
        for k, v in buckets.items():
            f.writelines(
                json.dumps({"bucket": k, **case}, ensure_ascii=False) + "\n"
                for case in v
            )
    print(dict(stats), {k: len(v) for k, v in buckets.items()})


if __name__ == "__main__":
    main()
