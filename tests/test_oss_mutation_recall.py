#!/usr/bin/env python3

import tempfile
import unittest
from pathlib import Path
from typing import ClassVar

from scripts_import import load

mr = load("oss-mutation-recall.py")


class KeysTest(unittest.TestCase):
    def test_subject_per_rule(self) -> None:
        report = {"issues": [
            {"code": "CHK001", "file": "pkg/orphan.py", "target": "pkg/orphan.py"},
            {"code": "CHK002", "distribution": "Chokkin_Mut.Unused", "target": "x"},
            {"code": "CHK003", "symbol": "PIL.Image", "distribution": None, "target": "x"},
            {"code": "CHK003", "symbol": None, "distribution": "xmltodict", "target": "x"},
            {"code": "CHK006", "target": "pkg/mod.py:pkg.mod.chokkin_mut_unreferenced"},
            {"code": "CHK010", "target": "chokkin_mut_missing"},
            {"code": "CHK009", "target": "ignored"},
        ]}
        self.assertEqual(mr.keys(report), {
            ("CHK001", "pkg/orphan.py"),
            ("CHK002", "chokkin-mut-unused"),
            ("CHK003", "pil"),
            ("CHK003", "xmltodict"),
            ("CHK006", "chokkin_mut_unreferenced"),
            ("CHK010", "chokkin_mut_missing"),
        })


class OutcomeTest(unittest.TestCase):
    EXPECTED: ClassVar[set[tuple[str, str]]] = {("CHK002", "chokkin-mut-unused")}

    def test_an_injection_is_detected_only_when_new(self) -> None:
        self.assertEqual(
            mr.outcome("injection", self.EXPECTED, set(), self.EXPECTED),
            ("detected", ["CHK002:chokkin-mut-unused"]),
        )
        self.assertEqual(mr.outcome("injection", self.EXPECTED, set(), {("CHK002", "other")}), ("missed", []))
        # Already reported before the mutation: not a detection.
        self.assertEqual(mr.outcome("injection", self.EXPECTED, self.EXPECTED, self.EXPECTED), ("missed", []))

    def test_a_trap_is_clean_unless_its_false_report_appears(self) -> None:
        self.assertEqual(mr.outcome("trap", self.EXPECTED, set(), set()), ("clean", []))
        self.assertEqual(mr.outcome("trap", self.EXPECTED, set(), self.EXPECTED)[0], "triggered")


class SummarizeTest(unittest.TestCase):
    def test_not_applied_rows_are_left_out_of_the_rate(self) -> None:
        def row(default, maybe):
            return {"mutation": "unused-dependency", "kind": "injection", "rule": "CHK002",
                    "default": default, "maybe": maybe}

        agg = mr.summarize([row("detected", "detected"), row("missed", "detected"), row("n/a", "n/a")])
        self.assertEqual(
            agg, {"unused-dependency": {"kind": "injection", "rule": "CHK002", "applied": 2, "default": 1, "maybe": 2}}
        )
        self.assertEqual(mr.pct(1, 2), "50.0%")
        self.assertEqual(mr.pct(0, 0), "n/a")


class AddDependenciesTest(unittest.TestCase):
    def check(self, pyproject: str | None, expected: str | None) -> None:
        with tempfile.TemporaryDirectory() as directory:
            proj = Path(directory)
            if pyproject is not None:
                (proj / "pyproject.toml").write_text(pyproject, encoding="utf-8")
            added = mr.add_dependencies(proj, ["new>=1"])
            self.assertEqual(added, expected is not None)
            if expected is not None:
                self.assertEqual((proj / "pyproject.toml").read_text(encoding="utf-8"), expected)

    def test_prepends_to_the_existing_array(self) -> None:
        self.check(
            '[project]\nname = "x"\ndependencies = ["a"]\n\n[tool.x]\ndependencies = []\n',
            '[project]\nname = "x"\ndependencies = ["new>=1", "a"]\n\n[tool.x]\ndependencies = []\n',
        )

    def test_adds_the_array_when_absent(self) -> None:
        self.check('[project]\nname = "x"\n', '[project]\ndependencies = ["new>=1", ]\nname = "x"\n')

    def test_skips_dynamic_or_missing_project_tables(self) -> None:
        self.check('[project]\nname = "x"\ndynamic = ["version", "dependencies"]\n', None)
        self.check('[tool.poetry]\nname = "x"\n', None)
        self.check(None, None)


class PackageTest(unittest.TestCase):
    def test_find_package_prefers_the_project_name(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            proj = Path(directory)
            self.assertIsNone(mr.find_package(proj))
            for pkg, files in (("src/my_lib", 1), ("big", 3), ("tests", 9)):
                for n in range(files):
                    path = proj / pkg / ("__init__.py" if n == 0 else f"m{n}.py")
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text("", encoding="utf-8")
            # Without a name the package with most files wins; tests/ is never a candidate.
            self.assertEqual(mr.find_package(proj), proj / "big")
            (proj / "pyproject.toml").write_text('[project]\nname = "My-Lib"\n', encoding="utf-8")
            self.assertEqual(mr.find_package(proj), proj / "src/my_lib")

    def test_pick_module_skips_init_and_unreachable_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            proj = Path(directory)
            pkg = proj / "pkg"
            pkg.mkdir()
            (pkg / "__init__.py").write_text("x" * 100, encoding="utf-8")
            (pkg / "small.py").write_text("x", encoding="utf-8")
            (pkg / "dead.py").write_text("x" * 50, encoding="utf-8")
            self.assertEqual(mr.pick_module(proj, pkg, {("CHK001", "pkg/dead.py")}), pkg / "small.py")
            self.assertEqual(mr.pick_module(proj, pkg, set()), pkg / "dead.py")


if __name__ == "__main__":
    unittest.main()
