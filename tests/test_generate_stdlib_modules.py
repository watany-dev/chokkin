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


if __name__ == "__main__":
    unittest.main()
