#!/usr/bin/env python3

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

from scripts_import import load

gate = load("oss-gate.py")

ISSUE = {"code": "CHK002", "target": "requests"}


def report(issues: list[dict], total: int | None = None, by_code: dict | None = None) -> bytes:
    if by_code is None:
        by_code = {}
        for i in issues:
            by_code[i["code"]] = by_code.get(i["code"], 0) + 1
    summary = {"total": len(issues) if total is None else total, "by_code": by_code}
    return json.dumps({"issues": issues, "summary": summary}).encode()


class NoErrors:
    def iter_errors(self, report):
        return iter(())


class ContractErrorsTest(unittest.TestCase):
    def test_summary_must_agree_with_the_issues(self) -> None:
        self.assertEqual(gate.contract_errors(json.loads(report([ISSUE, ISSUE]))), [])
        self.assertEqual(len(gate.contract_errors(json.loads(report([ISSUE], total=2)))), 1)
        self.assertEqual(
            len(gate.contract_errors(json.loads(report([ISSUE], by_code={"CHK003": 1})))), 1
        )


class JudgeTest(unittest.TestCase):
    def test_identical_valid_outputs_pass(self) -> None:
        out = report([ISSUE])
        res = gate.judge({"cold": (0, out), "warm": (0, out), "nocache": (0, out)}, NoErrors())
        self.assertTrue(res["deterministic"])
        self.assertEqual(res["differs"], [])
        self.assertFalse(res["crash"])
        self.assertEqual(res["schema_errors"], [])
        self.assertEqual(res["issues"], 1)
        self.assertEqual(res["exit"], {"cold": 0, "warm": 0, "nocache": 0})

    def test_a_warm_run_that_differs_is_nondeterministic(self) -> None:
        res = gate.judge(
            {"cold": (0, report([ISSUE])), "warm": (0, report([])), "nocache": (0, report([ISSUE]))},
            NoErrors(),
        )
        self.assertFalse(res["deterministic"])
        self.assertEqual(res["differs"], ["warm"])

    def test_exit_3_or_non_json_is_a_crash(self) -> None:
        out = report([])
        res = gate.judge({"cold": (0, out), "warm": (3, out), "nocache": (0, out)}, NoErrors())
        self.assertTrue(res["crash"])

        res = gate.judge({"cold": (1, b"panicked"), "warm": (1, b"panicked"), "nocache": (1, b"panicked")}, NoErrors())
        self.assertTrue(res["crash"])
        self.assertEqual(len(res["schema_errors"]), 3)
        self.assertNotIn("issues", res)

    def test_exit_2_with_no_output_on_every_run_is_a_rejection(self) -> None:
        res = gate.judge(dict.fromkeys(gate.VARIANTS, (2, b"")), NoErrors())
        self.assertTrue(res["rejected"])
        res = gate.judge({"cold": (2, b""), "warm": (2, b""), "nocache": (0, report([]))}, NoErrors())
        self.assertFalse(res["rejected"])

    def test_schema_and_contract_errors_are_collected_per_variant(self) -> None:
        class OneError:
            def iter_errors(self, report):
                return iter([type("E", (), {"absolute_path": ["issues", 0, "code"], "message": "bad"})()])

        out = report([ISSUE], total=5)
        res = gate.judge({"cold": (0, out), "warm": (0, out), "nocache": (0, out)}, OneError())
        self.assertIn("cold: issues/0/code: bad", res["schema_errors"])
        self.assertEqual(sum(e.startswith("warm: summary.total") for e in res["schema_errors"]), 1)
        self.assertEqual(len(res["schema_errors"]), 6)


class FailingTest(unittest.TestCase):
    def test_each_gate_lists_its_failing_projects(self) -> None:
        ok = {"deterministic": True, "schema_errors": [], "crash": False}
        results = {
            "a": ok,
            "b": {**ok, "deterministic": False},
            "c": {**ok, "schema_errors": ["x"], "crash": True},
        }
        self.assertEqual(
            gate.failing(results), {"determinism": ["b"], "schema": ["c"], "crash": ["c"]}
        )
        self.assertEqual(gate.failing({"a": ok}), {"determinism": [], "schema": [], "crash": []})


class SchemaTest(unittest.TestCase):
    """judge() against the committed schema with the real jsonschema validator."""

    def setUp(self) -> None:
        try:
            import jsonschema
        except ImportError:
            self.skipTest("jsonschema is not installed")
        schema = json.loads(gate.SCHEMA.read_text(encoding="utf-8"))
        self.validator = jsonschema.Draft202012Validator(schema)

    def test_an_empty_report_is_valid_and_a_missing_field_is_not(self) -> None:
        report = {
            "schema_version": "1",
            "version": "0.0.0",
            "project": ".",
            "mode": "auto",
            "production": False,
            "issues": [],
            "summary": {"total": 0, "by_code": {}},
            "suppressed": {"baseline": 0},
        }
        out = json.dumps(report).encode()
        res = gate.judge(dict.fromkeys(gate.VARIANTS, (0, out)), self.validator)
        self.assertEqual(res["schema_errors"], [])

        del report["mode"]
        out = json.dumps(report).encode()
        res = gate.judge({"cold": (0, out)}, self.validator)
        self.assertEqual(res["schema_errors"], ["cold: <root>: 'mode' is a required property"])


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "oss-gate.py"

# Prints a minimal schema-valid report. A project holding REJECT exits 2 with
# no output, like a config error; one holding FLAKY changes under --no-cache;
# one holding CRASH exits 3.
STUB = textwrap.dedent(
    """\
    #!/usr/bin/env python3
    import json, sys
    from pathlib import Path
    if sys.argv[1:] == ["--version"]:
        print("chokkin 0.0.0")
        sys.exit(0)
    proj = Path(sys.argv[-1])
    if (proj / "REJECT").exists():
        print("error: invalid config", file=sys.stderr)
        sys.exit(2)
    if (proj / "CRASH").exists():
        sys.exit(3)
    name = proj.name
    if (proj / "FLAKY").exists() and "--no-cache" in sys.argv:
        name += "-nocache"
    print(json.dumps({
        "schema_version": "1", "version": "0.0.0", "project": name,
        "mode": "app", "production": False, "issues": [],
        "summary": {"total": 0, "by_code": {}, "files": {"runtime": 0, "reachable_runtime": 0}},
        "suppressed": {"baseline": 0}, "diagnostics": [],
    }))
    """
)


@unittest.skipUnless(importlib.util.find_spec("jsonschema"), "jsonschema is not installed")
class OssGateInRepoTest(unittest.TestCase):
    """scripts/oss-gate.py --in-repo, driven by a stub binary (#835)."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.bin = self.root / "chokkin"
        self.bin.write_text(STUB, encoding="utf-8")
        self.bin.chmod(0o755)
        self.fixtures = self.root / "fixtures"
        for name in ("deps/a", "deps/b", "config/broken"):
            (self.fixtures / name).mkdir(parents=True)
        (self.fixtures / "config/broken/REJECT").touch()
        self.expected = self.root / "rejected.txt"
        self.expected.write_text("# invalid config\nconfig/broken\n", encoding="utf-8")

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def gate(self) -> subprocess.CompletedProcess:
        return subprocess.run(
            [
                sys.executable, str(SCRIPT), "--in-repo",
                "--fixtures", str(self.fixtures),
                "--recall", str(self.root / "no-recall.manifest"),
                "--clones", str(self.root / "no-clones"),
                "--bin", str(self.bin),
                "--output", str(self.root / "out"),
                "--expect-rejected", str(self.expected),
            ],
            capture_output=True, text=True, check=False,
            env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
        )

    def summary(self) -> dict:
        return json.loads((self.root / "out/summary.json").read_text(encoding="utf-8"))

    def test_runs_fixtures_without_clones(self) -> None:
        run = self.gate()
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
        summary = self.summary()
        self.assertEqual(sorted(summary["results"]), ["deps/a", "deps/b"])
        self.assertEqual(summary["rejected"], ["config/broken"])
        self.assertEqual(summary["skipped_not_cloned"], [])
        self.assertEqual(summary["corpus"], {})

    def test_fails_on_unexpected_rejection(self) -> None:
        (self.fixtures / "deps/a/REJECT").touch()
        run = self.gate()
        self.assertEqual(run.returncode, 1, run.stdout + run.stderr)
        self.assertEqual(self.summary()["crashed"], ["deps/a"])

    def test_fails_when_an_expected_rejection_runs(self) -> None:
        (self.fixtures / "config/broken/REJECT").unlink()
        run = self.gate()
        self.assertEqual(run.returncode, 1, run.stdout + run.stderr)
        summary = self.summary()
        self.assertFalse(summary["gates"]["rejection"])
        self.assertEqual(summary["not_rejected"], ["config/broken"])

    def test_fails_when_warm_and_nocache_differ(self) -> None:
        (self.fixtures / "deps/b/FLAKY").touch()
        run = self.gate()
        self.assertEqual(run.returncode, 1, run.stdout + run.stderr)
        self.assertEqual(self.summary()["nondeterministic"], ["deps/b"])

    def test_fails_on_crash(self) -> None:
        (self.fixtures / "deps/a/CRASH").touch()
        run = self.gate()
        self.assertEqual(run.returncode, 1, run.stdout + run.stderr)
        self.assertEqual(self.summary()["crashed"], ["deps/a"])

    def test_removes_the_cache_it_wrote(self) -> None:
        (self.fixtures / "deps/a/.chokkin").mkdir()
        self.gate()
        self.assertFalse((self.fixtures / "deps/a/.chokkin").exists())


if __name__ == "__main__":
    unittest.main()
