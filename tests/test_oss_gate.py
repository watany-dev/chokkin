#!/usr/bin/env python3
"""scripts/oss-gate.py --in-repo, driven by a stub binary (#835)."""

import json
import os
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

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


class OssGateInRepoTest(unittest.TestCase):
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
