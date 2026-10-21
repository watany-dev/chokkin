#!/usr/bin/env python3

import json
import tempfile
import unittest
from pathlib import Path

from scripts_import import load

sp = load("sample-precision-labels.py")


def label(slug: str, target: str, code: str = "CHK001") -> str:
    return f"{slug}\t{code}\t{target}\ttp\t-\tchecked"


class PruneTest(unittest.TestCase):
    def test_labelled_hits_over_the_cap_are_dropped_in_hash_order(self) -> None:
        targets = [f"t{i}" for i in range(5)]
        lines = ["# header"] + [label("big", t) for t in targets] + [label("small", "s")]
        hits = {"big": set(targets), "small": {"s"}}
        kept = sp.prune(lines, hits, "CHK001", 2)
        first_two = sorted(targets, key=lambda t: sp.rank("big", "CHK001", t))[:2]
        self.assertEqual(kept, ["# header"] + [label("big", t) for t in targets if t in first_two] + [label("small", "s")])

    def test_other_rules_and_labels_that_are_not_hits_are_kept(self) -> None:
        lines = [label("big", "a"), label("big", "b"), label("big", "info"), label("big", "c", "CHK006")]
        kept = sp.prune(lines, {"big": {"a", "b"}}, "CHK001", 1)
        self.assertEqual(len(kept), 3)
        self.assertIn(label("big", "info"), kept)
        self.assertIn(label("big", "c", "CHK006"), kept)



class MaybePoolTest(unittest.TestCase):
    def test_only_maybe_findings_of_the_rule_are_drawn(self) -> None:
        issues = [
            {"code": "CHK001", "confidence": "maybe", "severity": "warning", "target": "a.py", "message": "m"},
            {"code": "CHK001", "confidence": "likely", "severity": "warning", "target": "b.py", "message": "m"},
            {"code": "CHK006", "confidence": "maybe", "severity": "warning", "target": "c.py", "message": "m"},
        ]
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "lib.json").write_text(json.dumps({"issues": issues}), encoding="utf-8")
            Path(tmp, "broken.json").write_text("not json", encoding="utf-8")
            self.assertEqual(sp.maybe_hits(Path(tmp), "CHK001"), {"lib": {"a.py": "m"}})


if __name__ == "__main__":
    unittest.main()
