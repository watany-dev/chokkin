#!/usr/bin/env python3

import json
import unittest

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
        res = gate.judge({v: (0, out) for v in gate.VARIANTS}, self.validator)
        self.assertEqual(res["schema_errors"], [])

        del report["mode"]
        out = json.dumps(report).encode()
        res = gate.judge({"cold": (0, out)}, self.validator)
        self.assertEqual(res["schema_errors"], ["cold: <root>: 'mode' is a required property"])


if __name__ == "__main__":
    unittest.main()
