#!/usr/bin/env python3

import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "generate-stdlib-modules.py"

spec = importlib.util.spec_from_file_location("generate_stdlib_modules", SCRIPT)
generate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generate)


class AnnotateTest(unittest.TestCase):
    def test_names_the_minors_that_ship_each_root(self) -> None:
        lines = generate.annotate(
            {
                10: {"os", "asynchat", "binhex"},
                11: {"os", "asynchat", "tomllib"},
                12: {"os", "tomllib"},
            }
        )
        self.assertEqual(
            lines,
            ["asynchat 10-11", "binhex 10-10", "os", "tomllib 11-"],
        )

    def test_rejects_a_root_that_leaves_and_returns(self) -> None:
        # One range per root cannot say a root skipped a minor.
        with self.assertRaises(SystemExit):
            generate.annotate({10: {"gap"}, 11: set(), 12: {"gap"}})


class AddOlderMinorsTest(unittest.TestCase):
    def test_derives_each_older_minor_from_the_next(self) -> None:
        modules = {10: {"os", "graphlib", "_aix_support", "_bootsubprocess", "_zoneinfo", "zoneinfo"}}
        generate.add_older_minors(modules)
        self.assertIn("formatter", modules[9])
        self.assertIn("_peg_parser", modules[9])
        self.assertNotIn("_peg_parser", modules[8])
        self.assertNotIn("zoneinfo", modules[8])
        self.assertIn("dummy_threading", modules[8])
        self.assertEqual(
            generate.annotate(modules),
            [
                "_aix_support 9-",
                "_bootlocale 8-9",
                "_bootsubprocess 9-",
                "_dummy_thread 8-8",
                "_peg_parser 9-9",
                "_zoneinfo 9-",
                "dummy_threading 8-8",
                "formatter 8-9",
                "graphlib 9-",
                "os",
                "parser 8-9",
                "symbol 8-9",
                "zoneinfo 9-",
            ],
        )

    def test_rejects_differences_that_do_not_fit_the_newer_minor(self) -> None:
        # 3.8 drops `zoneinfo`, so a 3.9 without it means the table is stale.
        with self.assertRaises(SystemExit):
            generate.add_older_minors({10: {"os"}})


if __name__ == "__main__":
    unittest.main()
