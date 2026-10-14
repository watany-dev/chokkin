#!/usr/bin/env python3

import json
import tempfile
import unittest
from pathlib import Path

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


class PrecisionMissesTest(unittest.TestCase):
    def test_a_rule_at_its_floor_passes(self) -> None:
        findings = [finding("CHK003", "fp")] + [finding("CHK003", "tp")] * 9  # 90%
        self.assertEqual(om.precision_misses(findings), [])

    def test_a_rule_below_its_floor_fails(self) -> None:
        findings = [finding("CHK003", "info-expected")] + [finding("CHK003", "tp")] * 8  # 88.9%
        self.assertEqual(om.precision_misses(findings), ["CHK003=88.9%<90%"])

    def test_rules_without_a_floor_are_recorded_only(self) -> None:
        self.assertNotIn("CHK008", om.PRECISION_FLOORS)
        self.assertEqual(om.precision_misses([finding("CHK008", "fp")]), [])

    def test_chk002_is_floored_by_the_fp_gate(self) -> None:
        # 95% precision is a 5% FP rate, which the §17 gate already fails.
        findings = [finding("CHK002", "fp")] + [finding("CHK002", "tp")] * 19
        self.assertEqual(om.precision_misses(findings), [])
        self.assertFalse(om.fp_gate(findings)[0])

    def test_a_rule_with_no_labelled_hit_passes(self) -> None:
        findings = [finding("CHK003", "unknown"), finding("CHK003", "fp", "info")]
        self.assertEqual(om.precision_misses(findings), [])


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
