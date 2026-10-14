#!/usr/bin/env python3

import io
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts_import import load

smoke = load("wheel-smoke.py")


class InstallTest(unittest.TestCase):
    def pip_args(self, dist: str) -> list[str]:
        done = subprocess.CompletedProcess([], 0, "", "")
        with mock.patch.object(smoke.venv, "create"), \
             mock.patch.object(smoke, "run", return_value=done) as run:
            smoke.install(Path(dist), Path("env"))
        cmd = run.call_args.args[0]
        return cmd[cmd.index("install") + 1 :]

    def test_installs_a_wheel_offline(self) -> None:
        args = self.pip_args("chokkin-0.7.5-py3-none-manylinux_2_17_x86_64.whl")
        self.assertIn("--no-index", args)
        self.assertEqual(args[-1], "chokkin-0.7.5-py3-none-manylinux_2_17_x86_64.whl")

    def test_builds_an_sdist_from_source(self) -> None:
        # pip fetches maturin from PyPI for the isolated build, as a user's
        # `pip install chokkin` on a platform without a wheel does (#846).
        args = self.pip_args("chokkin-0.7.5.tar.gz")
        self.assertNotIn("--no-index", args)
        self.assertEqual(args[-1], "chokkin-0.7.5.tar.gz")

    def test_rejects_other_files(self) -> None:
        with self.assertRaises(SystemExit):
            self.pip_args("chokkin-0.7.5.zip")


class SdistFilesTest(unittest.TestCase):
    def check(self, names: list[str]) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            sdist = Path(directory) / "chokkin-0.7.5.tar.gz"
            with tarfile.open(sdist, "w:gz") as tar:
                for name in names:
                    info = tarfile.TarInfo(f"chokkin-0.7.5/{name}")
                    tar.addfile(info, io.BytesIO(b""))
            return smoke.check_sdist_files(sdist)

    def test_only_what_the_build_needs(self) -> None:
        names = ["Cargo.toml", "Cargo.lock", "pyproject.toml", "PKG-INFO", "README.md", "LICENSE",
                 "src/main.rs", "src/resolver/stdlib/modules.txt"]
        self.assertEqual(self.check(names), [])

    def test_reports_files_the_build_does_not_need(self) -> None:
        # #858: the 0.7.5 sdist carried tests/, docs/, .claude/ and so on.
        names = ["Cargo.toml", "src/main.rs", "tests/cli_analyze.rs", ".cargo/config.toml", "Makefile"]
        self.assertEqual(
            self.check(names),
            ["sdist: unexpected files: .cargo/config.toml Makefile tests/cli_analyze.rs"],
        )

    def test_a_wheel_is_not_checked(self) -> None:
        self.assertEqual(smoke.check_sdist_files(Path("chokkin-0.7.5-py3-none-any.whl")), [])


if __name__ == "__main__":
    unittest.main()
