#!/usr/bin/env python3

import tempfile
import unittest
from pathlib import Path

from scripts_import import load

od = load("oss-differential.py")

PROJ = Path("/clones/p")


class NamesTest(unittest.TestCase):
    def test_normalization(self) -> None:
        self.assertEqual(od.norm_dist("Zope.Interface__x"), "zope-interface-x")
        self.assertEqual(od.top_import("PIL.Image"), "pil")
        self.assertEqual(od.bound_name("os.path"), "path")
        self.assertEqual(od.bound_name("pkg.mod as alias"), "alias")
        self.assertEqual(od.bound_name(".sub."), "sub")

    def test_in_scope_drops_tests_docs_and_tooling(self) -> None:
        self.assertTrue(od.in_scope("src/pkg/mod.py"))
        self.assertTrue(od.in_scope("pkg/testing_utils.py"))
        for rel in ("tests/test_x.py", "pkg/tests/helpers.py", "docs/conf.py", "conftest.py",
                    "setup.py", "pkg/test_mod.py", "pkg/mod_test.py"):
            self.assertFalse(od.in_scope(rel), rel)

    def test_rel_path(self) -> None:
        self.assertEqual(od.rel_path(PROJ, "/clones/p/pkg/a.py"), "pkg/a.py")
        self.assertEqual(od.rel_path(PROJ, "./pkg/a.py"), "pkg/a.py")
        self.assertEqual(od.rel_path(PROJ, "/elsewhere/a.py"), "/elsewhere/a.py")


class TopLevelTest(unittest.TestCase):
    def test_public_top_level_names_including_guarded_ones(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            proj = Path(directory)
            (proj / "m.py").write_text(
                "import os\n"
                "def f(): pass\n"
                "class C:\n    def method(self): pass\n"
                "X, (Y, _z) = 1, (2, 3)\n"
                "N: int = 0\n"
                "_private = 1\n"
                "if os.name == 'nt':\n    def g(): pass\nelse:\n    def h(): pass\n"
                "try:\n    import json\nexcept ImportError:\n    def fallback(): pass\n",
                encoding="utf-8",
            )
            (proj / "broken.py").write_text("def (:\n", encoding="utf-8")
            top = od.TopLevel(proj)
            self.assertEqual(top.names("m.py"), {"f", "C", "X", "Y", "N", "g", "h", "fallback"})
            self.assertEqual(top.names("broken.py"), set())
            self.assertEqual(top.names("missing.py"), set())


class ChokkinKeysTest(unittest.TestCase):
    def test_keys_per_rule(self) -> None:
        report = {"issues": [
            {"code": "CHK002", "distribution": "Typing_Extensions", "target": "x"},
            {"code": "CHK003", "symbol": "yaml.loader", "distribution": None, "target": "x"},
            {"code": "CHK004", "symbol": None, "distribution": "idna", "target": "x"},
            {"code": "CHK006", "file": "pkg/a.py", "target": "pkg/a.py:helper"},
            {"code": "CHK007", "file": None, "target": "pkg/__init__.py:x"},
        ]}
        self.assertEqual(dict(od.chokkin_keys(report)), {
            "CHK002": {"typing-extensions"},
            "CHK003": {"yaml"},
            "CHK004": {"idna"},
            "CHK006": {("pkg/a.py", "helper")},
        })


class OtherToolParsersTest(unittest.TestCase):
    def test_vulture_and_deadcode_keep_public_top_level_shipped_names(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            proj = Path(directory)
            (proj / "pkg").mkdir()
            (proj / "pkg/a.py").write_text("def helper(): pass\nclass K:\n    def m(self): pass\n", encoding="utf-8")
            top = od.TopLevel(proj)
            vulture = (
                f"{proj}/pkg/a.py:1: unused function 'helper' (60% confidence)\n"
                "pkg/a.py:3: unused method 'm' (60% confidence)\n"
                "tests/test_a.py:1: unused function 'helper' (60% confidence)\n"
                "pkg/a.py:2: unused class 'K' (60% confidence)\n"
            )
            deadcode = (
                "pkg/a.py:1:0: DC02 Function `helper` is never used\n"
                "pkg/a.py:3:4: DC03 Method `m` is never used\n"
            )
            self.assertEqual(
                od.dead_code_keys(od.VULTURE_RE, vulture, proj, top), {("pkg/a.py", "helper"), ("pkg/a.py", "K")}
            )
            self.assertEqual(od.dead_code_keys(od.DEADCODE_RE, deadcode, proj, top), {("pkg/a.py", "helper")})

    def test_deptry(self) -> None:
        def v(code, module, file):
            return {"error": {"code": code}, "module": module, "location": {"file": file}}

        keys = od.deptry_keys([
            v("DEP002", "Typing_Extensions", "pyproject.toml"),
            v("DEP001", "yaml.loader", "pkg/a.py"),
            v("DEP001", "pytest", "tests/test_a.py"),
            v("DEP003", "idna", "pkg/a.py"),
            v("DEP999", "x", "pkg/a.py"),
        ], PROJ)
        self.assertEqual(dict(keys), {"CHK002": {"typing-extensions"}, "CHK003": {"yaml"}, "CHK004": {"idna"}})

    def test_fawltydeps_needs_a_shipped_reference(self) -> None:
        data = {
            "undeclared_deps": [
                {"name": "yaml", "references": [{"path": f"{PROJ}/pkg/a.py"}]},
                {"name": "pytest", "references": [{"path": f"{PROJ}/tests/test_a.py"}]},
            ],
            "unused_deps": [{"name": "Typing_Extensions", "references": [{"path": "pyproject.toml"}]}],
        }
        self.assertEqual(
            dict(od.fawltydeps_keys(data, PROJ)), {"CHK003": {"yaml"}, "CHK002": {"typing-extensions"}}
        )
        self.assertEqual(dict(od.fawltydeps_keys({}, PROJ)), {})

    def test_ruff_and_pyflakes_only_count_package_inits(self) -> None:
        ruff = [
            {"filename": f"{PROJ}/pkg/__init__.py", "message": "`os.path` imported but unused"},
            {"filename": f"{PROJ}/pkg/__init__.py", "message": "`.sub.thing as alias` imported but unused; ..."},
            {"filename": f"{PROJ}/pkg/a.py", "message": "`os` imported but unused"},
            {"filename": f"{PROJ}/tests/__init__.py", "message": "`os` imported but unused"},
        ]
        self.assertEqual(od.ruff_keys(ruff, PROJ), {("pkg/__init__.py", "path"), ("pkg/__init__.py", "alias")})
        pyflakes = (
            "./pkg/__init__.py:1:1: 'os.path' imported but unused\n"
            "./pkg/__init__.py:2:1 '.sub.thing as alias' imported but unused\n"
            "./pkg/a.py:1:1: 'os' imported but unused\n"
            "./pkg/__init__.py:3:1: undefined name 'x'\n"
        )
        self.assertEqual(od.pyflakes_keys(pyflakes, PROJ), {("pkg/__init__.py", "path"), ("pkg/__init__.py", "alias")})


class MatrixRowsTest(unittest.TestCase):
    def test_jaccard(self) -> None:
        cells = {
            ("CHK002", "deptry"): {"agree": [1, 2], "chokkin_only": [3], "other_only": [4, 5]},
            ("CHK007", "ruff"): {"agree": [], "chokkin_only": [], "other_only": []},
        }
        rows = od.matrix_rows(cells)
        self.assertEqual(rows[0], {
            "rule": "CHK002", "tool": "deptry", "chokkin": 3, "other": 4, "agree": 2,
            "chokkin_only": 1, "other_only": 2, "jaccard_pct": 40.0,
        })
        self.assertIsNone(rows[1]["jaccard_pct"])


if __name__ == "__main__":
    unittest.main()
