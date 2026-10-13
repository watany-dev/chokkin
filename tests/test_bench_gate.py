#!/usr/bin/env python3

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "bench-gate.py"

sys.path.insert(0, str(SCRIPT.parent))
spec = importlib.util.spec_from_file_location("bench_gate", SCRIPT)
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class RunBenchTest(unittest.TestCase):
    def run_bench(self, only=None) -> list[str]:
        done = subprocess.CompletedProcess([], 0)
        with mock.patch.object(gate.subprocess, "run", return_value=done) as run:
            self.assertTrue(gate.run_bench("base", only))
        return run.call_args.args[0]

    def test_skips_benchmarks_the_baseline_lacks(self) -> None:
        # A PR that adds a benchmark has no baseline for it on the base commit;
        # criterion's strict --baseline panics on that and the gate exits 2.
        cmd = self.run_bench()
        self.assertEqual(cmd[cmd.index("--") + 1 :], ["--baseline-lenient", "base"])

    def test_reruns_only_the_named_benchmarks(self) -> None:
        cmd = self.run_bench(["pipeline/analyze_cold/1000"])
        self.assertEqual(cmd[-1], "^(pipeline/analyze_cold/1000)$")


class CollectTest(unittest.TestCase):
    def test_names_benchmarks_by_their_criterion_id(self) -> None:
        # criterion turns "/" in a function name into "_" in the directory
        # name, and a re-run filter built from the directory matches nothing.
        with tempfile.TemporaryDirectory() as tmp:
            bench = Path(tmp) / "extract_manifest_setup_py_300deps"
            (bench / "change").mkdir(parents=True)
            (bench / "new").mkdir()
            ci = {"lower_bound": 0.09, "upper_bound": 0.2}
            mean = {"point_estimate": 0.15, "confidence_interval": ci}
            (bench / "change/estimates.json").write_text(json.dumps({"mean": mean}))
            full_id = "extract_manifest/setup_py_300deps"
            (bench / "new/benchmark.json").write_text(json.dumps({"full_id": full_id}))
            with mock.patch.object(gate, "CRITERION", Path(tmp)):
                rows = gate.collect(0)
        self.assertEqual([r["bench"] for r in rows], [full_id])


if __name__ == "__main__":
    unittest.main()
