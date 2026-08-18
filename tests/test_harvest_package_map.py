#!/usr/bin/env python3

import json
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "harvest-package-map.py"


class HarvestPackageMapTest(unittest.TestCase):
    def test_harvests_top_level_and_record_deterministically(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first = root / "first-1.0-py3-none-any.whl"
            second = root / "second-1.0-py3-none-any.whl"
            self.write_wheel(first, "First", "first\nfirst_extra\n", None)
            self.write_wheel(
                second,
                "Second",
                None,
                "second/__init__.py,,\nsecond.py,,\nsecond-1.0.dist-info/METADATA,,\n",
            )

            command = [sys.executable, str(SCRIPT), str(second), str(first)]
            first_run = subprocess.run(command, check=True, capture_output=True, text=True)
            second_run = subprocess.run(command, check=True, capture_output=True, text=True)

            self.assertEqual(first_run.stdout, second_run.stdout)
            payload = json.loads(first_run.stdout)
            self.assertEqual(
                payload["packages"],
                [
                    {"distribution": "First", "imports": ["first", "first_extra"]},
                    {"distribution": "Second", "imports": ["second"]},
                ],
            )
            self.assertEqual(
                [source["file"] for source in payload["sources"]],
                [first.name, second.name],
            )
            self.assertTrue(
                all(len(source["sha256"]) == 64 for source in payload["sources"])
            )

    @staticmethod
    def write_wheel(
        path: Path, distribution: str, top_level: str | None, record: str | None
    ) -> None:
        dist_info = f"{distribution.lower()}-1.0.dist-info/"
        with zipfile.ZipFile(path, "w") as wheel:
            wheel.writestr(
                f"{dist_info}METADATA", f"Name: {distribution}\nVersion: 1.0\n"
            )
            if top_level is not None:
                wheel.writestr(f"{dist_info}top_level.txt", top_level)
            if record is not None:
                wheel.writestr(f"{dist_info}RECORD", record)


if __name__ == "__main__":
    unittest.main()
