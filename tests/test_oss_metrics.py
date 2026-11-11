#!/usr/bin/env python3

import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts_import import load

om = load("oss-metrics.py")


def finding(code: str, verdict: str, severity: str = "warning", slug: str = "p", target: str = "t") -> list[str]:
    # findings.tsv: slug, code, target, verdict, bucket, confidence, message, severity
    return [slug, code, target, verdict, "-", "likely", "", severity]


class FpGateTest(unittest.TestCase):
    def test_the_rate_must_stay_below_five_percent(self) -> None:
        under = [finding("CHK002", "fp")] + [finding("CHK002", "tp")] * 20  # 4.8%
        self.assertEqual(om.fp_gate(under), (True, 21, 1, 0))
        at = [finding("CHK002", "fp")] + [finding("CHK002", "tp")] * 19  # exactly 5%
        self.assertEqual(om.fp_gate(at), (False, 20, 1, 0))

    def test_unclassified_findings_block_the_gate(self) -> None:
        for verdict in ("unknown", "deferred"):
            self.assertEqual(om.fp_gate([finding("CHK002", verdict)]), (False, 1, 0, 1))

    def test_nothing_reported_passes(self) -> None:
        self.assertEqual(om.fp_gate([]), (True, 0, 0, 0))

    def test_info_findings_and_other_rules_are_not_counted(self) -> None:
        findings = [finding("CHK002", "fp", "info"), finding("CHK003", "fp"), finding("CHK002", "tp")]
        self.assertEqual(om.fp_gate(findings), (True, 1, 0, 0))

    def test_info_expected_on_a_hit_is_a_false_positive(self) -> None:
        self.assertEqual(om.fp_gate([finding("CHK002", "info-expected")]), (False, 1, 1, 0))


class CountHitsTest(unittest.TestCase):
    def test_counts_hits_by_effective_verdict(self) -> None:
        findings = [
            finding("CHK003", "tp"),
            finding("CHK003", "info-expected"),
            finding("CHK003", "info-expected", "info"),
            finding("CHK003", "fp"),
        ]
        self.assertEqual(om.count_hits(findings, "CHK003"), 3)
        self.assertEqual(om.count_hits(findings, "CHK003", "fp"), 2)
        self.assertEqual(om.count_hits(findings, "CHK003", "info-expected"), 0)
        self.assertEqual(om.count_hits(findings, "CHK002"), 0)


class PrecisionTest(unittest.TestCase):
    def test_labelled_hits_only(self) -> None:
        findings = [
            finding("CHK003", "tp"),
            finding("CHK003", "info-expected"),
            finding("CHK003", "tp", "info"),
            finding("CHK003", "unknown"),
            finding("CHK003", "deferred"),
        ]
        self.assertEqual(om.precision(findings, "CHK003"), (1, 1))
        self.assertEqual(om.precision([], "CHK003"), (0, 0))


class ProjectPrecisionTest(unittest.TestCase):
    def test_labelled_hits_per_project(self) -> None:
        findings = [
            finding("CHK001", "tp", slug="a"),
            finding("CHK001", "info-expected", slug="a"),
            finding("CHK001", "tp", "info", slug="b"),
            finding("CHK001", "unknown", slug="c"),
            finding("CHK001", "deferred", slug="c"),
            finding("CHK001", "fp", slug="d"),
            finding("CHK002", "tp", slug="e"),
        ]
        self.assertEqual(om.project_precision(findings, "CHK001"), {"a": (1, 1), "d": (0, 1)})
        self.assertEqual(om.project_precision([], "CHK001"), {})

    def test_skipped_projects_are_left_out(self) -> None:
        findings = [finding("CHK002", "tp", slug="sentinel"), finding("CHK002", "fp", slug="a")]
        self.assertEqual(om.project_precision(findings, "CHK002", frozenset({"sentinel"})), {"a": (0, 1)})

    def test_macro_weighs_each_project_equally(self) -> None:
        # Micro is 28/32 = 87.5%; the big project hides the small one's 50%.
        per = {"big": (27, 3), "small": (1, 1)}
        self.assertAlmostEqual(om.macro_precision(per), 70.0)
        self.assertIsNone(om.macro_precision({}))

    def test_max_share_is_the_project_with_most_labelled_hits(self) -> None:
        slug, pct = om.max_share({"airflow": (26, 3), "fastmcp": (3, 0), "poetry": (1, 0)})
        self.assertEqual(slug, "airflow")
        self.assertAlmostEqual(pct, 100 * 29 / 33)
        self.assertEqual(om.max_share({"b": (1, 0), "a": (0, 1)}), ("a", 50.0))
        self.assertIsNone(om.max_share({}))


class PrecisionMissesTest(unittest.TestCase):
    def test_a_rule_at_its_floor_passes(self) -> None:
        findings = [finding("CHK003", "fp")] + [finding("CHK003", "tp")] * 9  # 90%
        self.assertEqual(om.precision_misses(findings), [])

    def test_a_rule_below_its_floor_fails(self) -> None:
        findings = [finding("CHK003", "info-expected")] + [finding("CHK003", "tp")] * 8  # 88.9%
        self.assertEqual(om.precision_misses(findings), ["CHK003=88.9%<90%"])

    def test_rules_without_a_floor_are_recorded_only(self) -> None:
        self.assertNotIn("CHK007", om.PRECISION_FLOORS)
        self.assertEqual(om.precision_misses([finding("CHK007", "fp")]), [])

    def test_chk001_is_floored_once_app_projects_carry_labels(self) -> None:
        findings = [finding("CHK001", "fp")] + [finding("CHK001", "tp")] * 23  # 95.8%
        self.assertEqual(om.precision_misses(findings), [])
        findings.append(finding("CHK001", "fp"))  # 92.0%
        self.assertEqual(om.precision_misses(findings), ["CHK001=92.0%<95%"])

    def test_chk005_and_chk008_are_floored(self) -> None:
        findings = [finding("CHK005", "fp")] * 2 + [finding("CHK005", "tp")] * 8  # 80%
        findings += [finding("CHK008", "fp")] * 3 + [finding("CHK008", "tp")] * 7  # 70%
        self.assertEqual(om.precision_misses(findings), ["CHK005=80.0%<85%", "CHK008=70.0%<80%"])

    def test_chk002_is_floored_by_the_fp_gate(self) -> None:
        # 95% precision is a 5% FP rate, which the §17 gate already fails.
        findings = [finding("CHK002", "fp")] + [finding("CHK002", "tp")] * 19
        self.assertEqual(om.precision_misses(findings), [])
        self.assertFalse(om.fp_gate(findings)[0])

    def test_a_rule_with_no_labelled_hit_passes(self) -> None:
        findings = [finding("CHK003", "unknown"), finding("CHK003", "fp", "info")]
        self.assertEqual(om.precision_misses(findings), [])


def certain(code: str, verdict: str, severity: str = "warning") -> list[str]:
    row = finding(code, verdict, severity)
    row[5] = "certain"
    return row


class CertainMissesTest(unittest.TestCase):
    def test_no_certain_finding_passes(self) -> None:
        self.assertEqual(om.certain_misses([finding("CHK001", "fp")] * 3), [])

    def test_a_certain_false_positive_fails_at_any_severity(self) -> None:
        findings = [certain("CHK001", "tp")] * 9 + [certain("CHK001", "fp", "info")]
        self.assertEqual(om.certain_precision(findings, "CHK001"), (9, 1, 0))
        self.assertEqual(om.certain_misses(findings), ["CHK001:certain=90.0%<100%"])

    def test_info_expected_is_a_false_positive(self) -> None:
        # Right as a hint; deleting the file is not.
        findings = [certain("CHK001", "info-expected", "info")]
        self.assertEqual(om.certain_misses(findings), ["CHK001:certain=0.0%<100%"])

    def test_an_unclassified_certain_finding_fails(self) -> None:
        findings = [certain("CHK001", "tp"), certain("CHK001", "unknown"), certain("CHK001", "deferred")]
        self.assertEqual(om.certain_misses(findings), ["CHK001:certain 2 unclassified"])

    def test_labelled_true_positives_pass(self) -> None:
        self.assertEqual(om.certain_misses([certain("CHK001", "tp", "info")] * 2), [])


class RecallMissedTest(unittest.TestCase):
    def test_every_tp_label_must_be_reported_as_a_hit(self) -> None:
        labels = {
            ("s", "CHK002", "a"): ("tp", "-"),
            ("s", "CHK002", "b"): ("tp", "-"),
            ("s", "CHK002", "c"): ("fp", "-"),
        }
        self.assertEqual(om.recall_missed(labels, {("s", "CHK002", "a")}), ["s/CHK002/b"])
        self.assertEqual(om.recall_missed(labels, {("s", "CHK002", "a"), ("s", "CHK002", "b")}), [])


class TooSlowTest(unittest.TestCase):
    def test_only_medium_projects_over_the_budget(self) -> None:
        self.assertFalse(om.too_slow("medium", om.MEDIUM_GATE_MS))
        self.assertTrue(om.too_slow("medium", om.MEDIUM_GATE_MS + 1))
        self.assertFalse(om.too_slow("large", 60_000))
        self.assertFalse(om.too_slow("sentinel", 60_000))


def write_summary(run: Path, rows: list[tuple[str, str, int, int]]) -> None:
    """summary.tsv of (slug, size, exit, median_ms) rows."""
    run.mkdir(parents=True, exist_ok=True)
    lines = ["slug\tcategory\tsize\texit\tmedian_ms\ttotal\tCHK002\tCHK003"]
    lines += [f"{slug}\tlib\t{size}\t{code}\t{ms}\t0\t0\t0" for slug, size, code, ms in rows]
    (run / "summary.tsv").write_text("\n".join(lines) + "\n", encoding="utf-8")


# Medium and large rows of the summary.tsv of v0.7.3 and v0.7.4, -r 1 on one
# machine: (slug, size, v0.7.3 median_ms, v0.7.4 median_ms). v0.7.4 made
# transformers 4-6x slower (https://github.com/watany-dev/chokkin/blob/a199e81/docs/dev/v0.7.x-uvx-regression-report.ja.md §5).
V073_V074 = [
    ("airflow", "large", 2343, 2428),
    ("attrs", "medium", 23, 26),
    ("black", "large", 41, 45),
    ("click", "medium", 24, 29),
    ("django", "large", 373, 442),
    ("djangorestframework", "large", 45, 46),
    ("fastapi", "large", 77, 96),
    ("fastmcp", "large", 185, 177),
    ("flask", "medium", 29, 33),
    ("httpx", "medium", 25, 30),
    ("jinja", "medium", 30, 33),
    ("langchain", "large", 313, 357),
    ("llama_index", "large", 1566, 1570),
    ("mcp-python-sdk", "large", 126, 119),
    ("mitmproxy", "large", 72, 78),
    ("mlflow", "large", 586, 709),
    ("openai-python", "large", 233, 276),
    ("pdm", "medium", 47, 51),
    ("poetry", "medium", 74, 76),
    ("pydantic", "medium", 100, 123),
    ("requests", "medium", 19, 23),
    ("sqlalchemy", "large", 278, 320),
    ("starlette", "medium", 25, 24),
    ("streamlit", "large", 207, 210),
    ("structlog", "medium", 19, 26),
    ("transformers", "large", 1131, 4219),
    ("typer", "medium", 33, 35),
    ("urllib3", "medium", 32, 49),
    ("uvicorn", "medium", 20, 22),
    ("virtualenv", "medium", 31, 34),
    ("werkzeug", "medium", 35, 37),
]


class SlowdownTest(unittest.TestCase):
    def test_both_the_ratio_and_the_difference_must_be_reached(self) -> None:
        self.assertTrue(om.slowed(1000, 1500))
        self.assertFalse(om.slowed(1000, 1499))
        self.assertFalse(om.slowed(400, 899))  # 2.2x but 499 ms over
        self.assertTrue(om.slowed(400, 900))
        self.assertFalse(om.slowed(3000, 3600))  # 600 ms over but 1.2x

    def test_medium_and_large_projects_that_ran_in_both(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base, head = Path(directory) / "base", Path(directory) / "head"
            write_summary(base, [
                ("medium", "medium", 1, 1000), ("large", "large", 0, 1000), ("small", "small", 1, 100),
                ("sentinel", "sentinel", 1, 100), ("crashed", "large", 3, 100), ("config", "large", 1, 1000),
                ("fast", "large", 1, 1000),
            ])
            write_summary(head, [
                ("medium", "medium", 1, 2000), ("large", "large", 0, 1600), ("small", "small", 1, 900),
                ("sentinel", "sentinel", 1, 900), ("crashed", "large", 1, 2000), ("config", "large", 2, 2000),
                ("fast", "large", 1, 1100), ("new", "large", 1, 9000),
            ])
            self.assertEqual(
                om.slowdowns(base, head), [["medium", "medium", 1000, 2000], ["large", "large", 1000, 1600]]
            )

    def test_v0_7_4_fails_on_transformers_alone(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base, head = Path(directory) / "base", Path(directory) / "head"
            write_summary(base, [(slug, size, 0, before) for slug, size, before, _ in V073_V074])
            write_summary(head, [(slug, size, 0, after) for slug, size, _, after in V073_V074])
            self.assertEqual(om.slowdowns(base, head), [["transformers", "large", 1131, 4219]])

    def test_a_re_measure_under_the_criterion_clears_the_project(self) -> None:
        rows = [["noisy", "large", 1000, 1600], ["slow", "large", 1000, 8000]]
        medians = {"noisy": 1100, "slow": 7900}
        still = om.confirm_slowdowns(rows, medians.__getitem__)
        self.assertEqual(still, [["slow", "large", 1000, 8000, 7900]])
        self.assertEqual(rows[0], ["noisy", "large", 1000, 1600, 1100])

    def test_the_section_lists_each_flagged_project_and_its_result(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base, head = Path(directory) / "base", Path(directory) / "head"
            write_summary(base, [("a", "large", 1, 1000), ("b", "medium", 1, 500), ("s", "small", 1, 50)])
            write_summary(head, [("a", "large", 1, 8000), ("b", "medium", 1, 600), ("s", "small", 1, 60)])
            lines = om.slowdown_md(base, head, [["a", "large", 1000, 8000, 7900], ["b", "medium", 500, 1000, 600]])
            self.assertIn("Median cold run summed over the 2 projects in both runs: 1500 ms -> 8600 ms.", lines)
            self.assertIn("| a | large | 1000 | 8000 | 7900 | ❌ FAIL |", lines)
            self.assertIn("| b | medium | 500 | 1000 | 600 | ✅ noise |", lines)
            self.assertIn("_No project slowed down past the criterion._", om.slowdown_md(base, head, []))


# Large rows of one --time-bin run of main after airflow and llama_index were
# fixed (2839767) against v0.7.7, -r 3: (slug, v0.7.7 median_ms, main
# median_ms). The closest of five runs to the criterion; no project fails
# criterion 9 (#1088).
V077_MAIN = [
    ("airflow", 1200, 1422),
    ("black", 31, 39),
    ("django", 307, 317),
    ("djangorestframework", 42, 43),
    ("fastapi", 69, 67),
    ("fastmcp", 135, 136),
    ("langchain", 224, 245),
    ("llama_index", 1059, 1041),
    ("mcp-python-sdk", 84, 93),
    ("mitmproxy", 58, 69),
    ("mlflow", 394, 443),
    ("openai-python", 178, 205),
    ("sqlalchemy", 204, 220),
    ("streamlit", 154, 154),
    ("transformers", 776, 895),
]


class DriftTest(unittest.TestCase):
    def test_main_after_v0_7_8_drifted_past_v0_7_7(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base, head = Path(directory) / "base", Path(directory) / "head"
            write_summary(base, [(slug, "large", 0, before) for slug, before, _ in V077_MAIN])
            write_summary(head, [(slug, "large", 0, after) for slug, _, after in V077_MAIN])
            self.assertEqual(om.slowdowns(base, head), [])
            base_sum, head_sum, _ = om.large_totals(base, head)
            self.assertEqual((base_sum, head_sum), (4915, 5389))
            self.assertTrue(om.drifted(base_sum, head_sum))

    def test_the_sum_fails_only_over_the_ratio(self) -> None:
        self.assertFalse(om.drifted(10_000, 10_500))
        self.assertTrue(om.drifted(10_000, 10_501))

    def test_the_sum_counts_large_projects_that_finished_in_both(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base, head = Path(directory) / "base", Path(directory) / "head"
            write_summary(base, [
                ("a", "large", 0, 1000), ("b", "large", 1, 2000), ("m", "medium", 1, 500),
                ("crashed", "large", 3, 100), ("gone", "large", 1, 700),
            ])
            write_summary(head, [
                ("a", "large", 0, 1100), ("b", "large", 1, 2100), ("m", "medium", 1, 900),
                ("crashed", "large", 1, 5000), ("new", "large", 1, 9000),
            ])
            self.assertEqual(om.large_totals(base, head), (3000, 3200, ["a", "b"]))

    def test_a_project_warns_at_both_the_ratio_and_the_difference(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base, head = Path(directory) / "base", Path(directory) / "head"
            write_summary(base, [
                ("warn", "large", 1, 400), ("close", "large", 1, 400), ("tiny", "medium", 1, 100),
                ("small", "small", 1, 100),
            ])
            write_summary(head, [
                ("warn", "large", 1, 500), ("close", "large", 1, 499), ("tiny", "medium", 1, 199),
                ("small", "small", 1, 900),
            ])
            self.assertEqual(om.drift_warnings(base, head), [["warn", "large", 400, 500]])

    def test_the_section_reports_the_sum_and_the_warnings(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base, head = Path(directory) / "base", Path(directory) / "head"
            write_summary(base, [("a", "large", 1, 1000), ("b", "large", 1, 1000)])
            write_summary(head, [("a", "large", 1, 1300), ("b", "large", 1, 1000)])
            lines = om.slowdown_md(base, head, [], (2000, 2050))
            self.assertIn(
                "Large projects summed: 2000 ms -> 2300 ms (1.15x), re-measured 2000 ms -> 2050 ms (1.02x); "
                "fails over 1.05x (#1088): ✅ PASS",
                lines,
            )
            self.assertIn("| a | large | 1000 | 1300 | 1.30x |", lines)
            lines = om.slowdown_md(base, head, [])
            self.assertIn("Large projects summed: 2000 ms -> 2300 ms (1.15x); fails over 1.05x (#1088): ❌ FAIL", lines)


class TimedRunsTest(unittest.TestCase):
    def test_binaries_take_turns_in_swapped_order(self) -> None:
        calls: list[str] = []

        def fake(bin_path, proj, *args):
            calls.append(bin_path.name)
            return f"run of {bin_path.name}"

        with mock.patch.object(om.oc, "chokkin_raw", fake), tempfile.TemporaryDirectory() as directory:
            (head, _), (base, _) = om.timed_runs([Path("head"), Path("base")], Path(directory), 3, ())
        self.assertEqual(calls, ["head", "base", "base", "head", "head", "base"])
        self.assertEqual((head, base), ("run of head", "run of base"))


class ParseReportTest(unittest.TestCase):
    def test_non_object_output_is_none(self) -> None:
        self.assertEqual(om.parse_report(b'{"issues": []}'), {"issues": []})
        self.assertIsNone(om.parse_report(b"thread 'main' panicked"))
        self.assertIsNone(om.parse_report(b"[]"))
        self.assertIsNone(om.parse_report(b""))


class ReadLabelsTest(unittest.TestCase):
    def test_first_row_wins_and_short_rows_are_skipped(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "labels.tsv"
            self.assertEqual(om.read_labels(path), {})
            path.write_text(
                "# slug\tcode\ttarget\tverdict\tbucket\n"
                "s\tCHK002\ta\ttp\t\n"
                "s\tCHK002\ta\tfp\tx\n"
                "s\tCHK003\tb\tfp\tvendored\tnote\n"
                "s\tCHK003\tc\tfp\n",
                encoding="utf-8",
            )
            labels = om.read_labels(path)

        self.assertEqual(labels, {("s", "CHK002", "a"): ("tp", "-"), ("s", "CHK003", "b"): ("fp", "vendored")})


class ExpectationsTest(unittest.TestCase):
    def test_read_expect(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "expect.tsv"
            self.assertEqual(om.read_expect(path), {})
            path.write_text("# slug\truntime_min\tcounts\n\na\t90\tCHK001=3,CHK003=40\nb\t5\t\n", encoding="utf-8")
            self.assertEqual(
                om.read_expect(path), {"a": (90, {"CHK001": 3, "CHK003": 40}), "b": (5, {})}
            )

    def test_runtime_floor_and_rule_growth(self) -> None:
        floor = (90, {"CHK001": 3, "CHK003": 40})

        def misses(runtime, by_code):
            summary = {"by_code": by_code}
            if runtime is not None:
                summary["files"] = {"runtime": runtime, "reachable_runtime": runtime}
            return om.expectation_misses("a", {"summary": summary}, floor)

        self.assertEqual(misses(90, {"CHK001": 8, "CHK003": 48, "CHK006": 5}), [])
        self.assertEqual(misses(89, {}), ["a:runtime=89<90"])
        self.assertEqual(misses(None, {}), ["a:runtime=?<90"])
        # Slack is max(5, base // 5): 3 -> 5, 40 -> 8, absent -> 5.
        self.assertEqual(
            misses(90, {"CHK001": 9, "CHK003": 49, "CHK006": 6}),
            ["a:CHK001=9>3+5", "a:CHK003=49>40+8", "a:CHK006=6>0+5"],
        )


class Chk003GateCountTest(unittest.TestCase):
    def test_counts_untriaged_hits_outside_expect_and_recall_projects(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            (out / "summary.tsv").write_text(
                "slug\tcategory\nlib\tlib\nexp\tapp\nsentinel\trecall\n", encoding="utf-8"
            )
            rows = [
                finding("CHK003", "unknown", slug="lib"),
                finding("CHK003", "fp", slug="lib"),
                finding("CHK003", "tp", slug="lib"),
                finding("CHK003", "unknown", "info", slug="lib"),
                finding("CHK002", "unknown", slug="lib"),
                finding("CHK003", "unknown", slug="exp"),
                finding("CHK003", "unknown", slug="sentinel"),
            ]
            (out / "findings.tsv").write_text(
                "header\n" + "".join("\t".join(r) + "\n" for r in rows), encoding="utf-8"
            )
            self.assertEqual(om.chk003_gate_count(out, {"exp"}), 2)


class IsStrictTest(unittest.TestCase):
    def test_reads_the_report_header(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            (out / "report.md").write_text("# x\n\n- strict: yes\n", encoding="utf-8")
            self.assertTrue(om.is_strict(out))
            (out / "report.md").write_text("# x\n\n- strict: no\n", encoding="utf-8")
            self.assertFalse(om.is_strict(out))


def issue(code: str, fingerprint: str, severity: str = "warning", confidence: str = "likely", target: str = "t") -> dict:
    return {"code": code, "fingerprint": fingerprint, "severity": severity, "confidence": confidence, "target": target}


def write_report(run: Path, slug: str, issues: list[dict]) -> None:
    run.mkdir(parents=True, exist_ok=True)
    (run / f"{slug}.json").write_text(json.dumps({"issues": issues}), encoding="utf-8")


class FindingDiffTest(unittest.TestCase):
    def test_new_gone_changed_and_non_json(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base, head = Path(directory) / "base", Path(directory) / "head"
            write_report(base, "p", [issue("CHK002", "kept"), issue("CHK002", "lost"), issue("CHK003", "moved")])
            write_report(head, "p", [issue("CHK002", "kept"), issue("CHK002", "added"),
                                     issue("CHK003", "moved", "info")])
            write_report(base, "crashed", [])
            (head / "crashed.json").write_text("panicked", encoding="utf-8")

            rows, skipped = om.finding_diff(base, head, ["p", "crashed"])

        self.assertEqual(skipped, ["crashed"])
        self.assertEqual(rows, [
            ["NEW", "p", "CHK002", "added", "-", "warning/likely"],
            ["GONE", "p", "CHK002", "lost", "warning/likely", "-"],
            ["CHANGED", "p", "CHK003", "moved", "warning/likely", "info/likely"],
        ])

    def test_one_fingerprint_covering_several_findings(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base, head = Path(directory) / "base", Path(directory) / "head"
            write_report(base, "p", [issue("CHK003", "imp")] * 3)
            write_report(head, "p", [issue("CHK003", "imp"), issue("CHK003", "imp", "info")])
            rows, _ = om.finding_diff(base, head, ["p"])

        self.assertEqual(rows, [
            ["CHANGED", "p", "CHK003", "imp", "warning/likely", "info/likely"],
            ["GONE", "p", "CHK003", "imp", "warning/likely", "-"],
        ])


class DemotedTest(unittest.TestCase):
    def test_severity_first_then_confidence(self) -> None:
        self.assertTrue(om.demoted("warning/certain", "info/certain"))
        self.assertTrue(om.demoted("warning/certain", "warning/likely"))
        self.assertFalse(om.demoted("warning/maybe", "error/maybe"))
        # A raised severity outranks a lowered confidence.
        self.assertFalse(om.demoted("warning/certain", "error/maybe"))
        self.assertFalse(om.demoted("warning/likely", "warning/likely"))


class DowngradeVsVanishTest(unittest.TestCase):
    def test_a_vanished_tp_fails_and_a_demoted_one_does_not(self) -> None:
        labels = {("p", "CHK002", "gone-tp"): ("tp", "-"), ("p", "CHK002", "hidden-tp"): ("tp", "-")}
        with tempfile.TemporaryDirectory() as directory:
            base, head = Path(directory) / "base", Path(directory) / "head"
            write_report(base, "p", [issue("CHK002", "g", target="gone-tp"), issue("CHK002", "h", target="hidden-tp")])
            write_report(base / "strict", "p", [issue("CHK002", "g", target="gone-tp"),
                                                issue("CHK002", "h", target="hidden-tp")])
            # HEAD: `h` drops to maybe (only --strict shows it), `g` is gone everywhere.
            write_report(head, "p", [])
            write_report(head / "strict", "p", [issue("CHK002", "h", confidence="maybe", target="hidden-tp")])
            default_rows, _ = om.finding_diff(base, head, ["p"])
            strict_rows, _ = om.finding_diff(base / "strict", head / "strict", ["p"])

            lines, ok = om.downgrade_vs_vanish(base, head, default_rows, strict_rows, labels)

        self.assertFalse(ok)
        self.assertIn("Vanished `tp`: p/CHK002/gone-tp", lines)
        # Rule | GONE | Recontexted | Demoted | To info | Vanished tp | fp | other
        self.assertIn("| CHK002 | 2 | 0 | 1 | 0 | 1 | 0 | 0 |", lines)

    def test_nothing_gone_passes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory)
            lines, ok = om.downgrade_vs_vanish(run, run, [], [], {})
        self.assertTrue(ok)
        self.assertEqual(lines[-1], "_No finding is GONE or demoted._")


if __name__ == "__main__":
    unittest.main()
