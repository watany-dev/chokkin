#!/usr/bin/env python3

import json
import os
import tempfile
import unittest
from pathlib import Path

from scripts_import import load

gate = load("bench-gate.py")


def row(bench: str, mean: float, low: float, high: float | None = None) -> dict:
    return {"bench": bench, "mean_pct": mean, "ci_low_pct": low, "ci_high_pct": mean if high is None else high}


class IsRegressedTest(unittest.TestCase):
    def test_needs_a_slowdown_past_the_threshold_and_a_ci_above_zero(self) -> None:
        self.assertTrue(gate.is_regressed(row("a", 10.1, 0.1), 10))
        # Exactly at the threshold is allowed.
        self.assertFalse(gate.is_regressed(row("a", 10.0, 5.0), 10))
        # Slow on average but the CI reaches 0: noise.
        self.assertFalse(gate.is_regressed(row("a", 25.0, 0.0), 10))
        self.assertFalse(gate.is_regressed(row("a", 25.0, -3.0), 10))
        self.assertFalse(gate.is_regressed(row("a", -20.0, -25.0), 10))


class CollectTest(unittest.TestCase):
    def test_reads_criterion_change_estimates_as_percent(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_estimates(root / "analyze/medium/change", 0.123, 0.051, 0.2)
            self.write_estimates(root / "parse/change", -0.02, -0.04, 0.01)
            stale = self.write_estimates(root / "old/change", 0.5, 0.4, 0.6)
            os.utime(stale, (1_000, 1_000))

            rows = gate.collect(2_000, root)

        self.assertEqual([r["bench"] for r in rows], ["analyze/medium", "parse"])
        self.assertAlmostEqual(rows[0]["mean_pct"], 12.3)
        self.assertAlmostEqual(rows[0]["ci_low_pct"], 5.1)
        self.assertAlmostEqual(rows[0]["ci_high_pct"], 20.0)
        self.assertAlmostEqual(rows[1]["mean_pct"], -2.0)

    @staticmethod
    def write_estimates(change: Path, mean: float, low: float, high: float) -> Path:
        change.mkdir(parents=True)
        path = change / "estimates.json"
        estimate = {
            "confidence_interval": {"confidence_level": 0.95, "lower_bound": low, "upper_bound": high},
            "point_estimate": mean,
            "standard_error": 0.01,
        }
        path.write_text(json.dumps({"mean": estimate, "median": estimate}), encoding="utf-8")
        return path


class ConfirmTest(unittest.TestCase):
    def test_a_regression_must_repeat_on_every_rerun(self) -> None:
        rows = [row("steady", 20, 5), row("flaky", 20, 5)]
        reruns = iter([
            {"steady": row("steady", 18, 4), "flaky": row("flaky", 3, -1)},
            {"steady": row("steady", 15, 2)},
        ])
        asked = []

        def rerun(benches):
            asked.append(benches)
            return next(reruns)

        left = gate.confirm(rows, 10, 2, rerun)

        self.assertEqual([r["bench"] for r in left], ["steady"])
        self.assertEqual(asked, [["steady", "flaky"], ["steady"]])
        self.assertEqual(rows[0]["runs"], [20, 18, 15])
        self.assertFalse(rows[1]["regressed"])

    def test_zero_rounds_keeps_the_first_verdict(self) -> None:
        rows = [row("a", 20, 5)]
        left = gate.confirm(rows, 10, 0, lambda benches: self.fail("no rerun expected"))
        self.assertEqual([r["bench"] for r in left], ["a"])

    def test_a_failed_rerun_is_reported(self) -> None:
        self.assertIsNone(gate.confirm([row("a", 20, 5)], 10, 1, lambda benches: None))

    def test_a_bench_missing_from_the_rerun_stays_regressed(self) -> None:
        left = gate.confirm([row("a", 20, 5)], 10, 1, lambda benches: {})
        self.assertEqual([r["bench"] for r in left], ["a"])


if __name__ == "__main__":
    unittest.main()
