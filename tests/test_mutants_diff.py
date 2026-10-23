#!/usr/bin/env python3

import tempfile
import unittest
from pathlib import Path

from scripts_import import load

widen_diff = load("mutants-diff.py")

SOURCE = """\
pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

#[cfg(test)]
mod tests {
    #[test]
    fn adds() {
        assert_eq!(super::add(1, 2), 3);
    }
}
"""


def section(path: str, hunk: str, body: str) -> str:
    return f"diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n{hunk}\n{body}"


class WidenTest(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.write("src/graph/build.rs", SOURCE)
        self.write("src/graph/types.rs", "pub struct Node;\n")
        self.write("src/lib.rs", "#[cfg(test)]\nmod walk_reference;\npub fn run() {}\n")
        self.write("src/walk_reference.rs", "pub fn reference() {}\n")
        self.write("tests/graph_build.rs", "#[test]\nfn builds() {}\n")

    def tearDown(self) -> None:
        self.directory.cleanup()

    def write(self, path: str, text: str) -> None:
        (self.root / path).parent.mkdir(parents=True, exist_ok=True)
        (self.root / path).write_text(text, encoding="utf-8")

    def widened(self, diff: str) -> tuple[list[str], list[str]]:
        out, report = widen_diff.widen(self.root, diff)
        files = [part.split()[0] for part in out.split("diff --git a/")[1:]]
        return files, report

    def test_a_removed_line_in_a_test_block_widens_to_the_whole_file(self) -> None:
        diff = section(
            "src/graph/build.rs",
            "@@ -9,2 +9,1 @@ mod tests {",
            "-        assert_eq!(super::add(1, 2), 3);\n-        assert_eq!(super::add(0, 0), 0);\n+        assert_eq!(super::add(1, 2), 3);\n",
        )
        out, _ = widen_diff.widen(self.root, diff)
        self.assertIn("--- /dev/null\n+++ b/src/graph/build.rs\n@@ -0,0 +1,11 @@\n+pub fn add(", out)
        self.assertNotIn("--- a/src/graph/build.rs", out)

    def test_a_test_block_deleted_whole_widens_to_the_file(self) -> None:
        self.write("src/graph/build.rs", "pub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n")
        diff = section(
            "src/graph/build.rs",
            "@@ -4,3 +3,0 @@",
            "-\n-#[cfg(test)]\n-mod tests {}\n",
        )
        files, _ = self.widened(diff)
        self.assertEqual(files, ["src/graph/build.rs"])
        self.assertNotIn("--- a/src/graph/build.rs", widen_diff.widen(self.root, diff)[0])

    def test_an_added_test_or_a_non_test_change_keeps_the_diff(self) -> None:
        added = section("src/graph/build.rs", "@@ -9,0 +10,1 @@ mod tests {", "+        assert!(true);\n")
        code = section("src/graph/build.rs", "@@ -2,1 +2,1 @@", "-    a + b\n+    b + a\n")
        for diff in (added, code):
            out, report = widen_diff.widen(self.root, diff)
            self.assertEqual(out, diff)
            self.assertEqual(report, [])

    def test_an_integration_test_widens_to_its_targets(self) -> None:
        diff = section("tests/graph_build.rs", "@@ -2,1 +1,0 @@", "-fn builds() {}\n")
        files, report = self.widened(diff)
        # The test's own section stays: cargo-mutants never mutates tests.
        self.assertEqual(files, ["tests/graph_build.rs", "src/graph/build.rs", "src/graph/types.rs"])
        self.assertEqual(
            report,
            ["widened: tests/graph_build.rs lost lines, so all of src/graph/build.rs, src/graph/types.rs"],
        )

    def test_a_test_module_file_widens_to_the_files_beside_it(self) -> None:
        diff = section("src/walk_reference.rs", "@@ -1,1 +0,0 @@", "-pub fn reference() {}\n")
        files, _ = self.widened(diff)
        self.assertEqual(files, ["src/walk_reference.rs", "src/lib.rs"])

    def test_a_test_module_file_of_a_non_mod_file_widens_to_its_declarer(self) -> None:
        self.write("src/sources.rs", "#[cfg(test)]\nmod tests;\npub fn walk() {}\n")
        self.write("src/sources/tests.rs", "#[test]\nfn walks() {}\n")
        self.write("src/sources/layout.rs", "pub fn layout() {}\n")
        diff = section("src/sources/tests.rs", "@@ -2,1 +1,0 @@", "-fn walks() {}\n")
        files, _ = self.widened(diff)
        self.assertEqual(files, ["src/sources/tests.rs", "src/sources.rs", "src/sources/layout.rs"])

    def test_an_end_to_end_or_unknown_test_and_a_fixture_are_reported(self) -> None:
        diffs = [
            section("tests/cli_analyze.rs", "@@ -1,1 +0,0 @@", "-x\n"),
            section("tests/brand_new.rs", "@@ -1,1 +0,0 @@", "-x\n"),
            section("tests/fixtures/app/main.py", "@@ -1,1 +0,0 @@", "-import x\n"),
            section("tests/support/mod.rs", "@@ -1,1 +0,0 @@", "-x\n"),
            section("tests/test_bench_gate.py", "@@ -1,1 +0,0 @@", "-x\n"),
        ]
        files, report = self.widened("".join(diffs))
        self.assertEqual(
            files,
            [
                "tests/cli_analyze.rs",
                "tests/brand_new.rs",
                "tests/fixtures/app/main.py",
                "tests/support/mod.rs",
                "tests/test_bench_gate.py",
            ],
        )
        self.assertEqual(
            report,
            [
                "not widened: tests/cli_analyze.rs runs the whole pipeline; the weekly run checks it",
                "not widened: tests/brand_new.rs is not in TEST_TARGETS; the weekly run checks it",
                "not widened: tests/fixtures/app/main.py is a fixture; the weekly run checks it",
                "not widened: tests/support/mod.rs is no test file this maps; the weekly run checks it",
            ],
        )

    def test_a_deleted_test_file_still_widens(self) -> None:
        diff = (
            "diff --git a/tests/graph_build.rs b/tests/graph_build.rs\n"
            "deleted file mode 100644\n--- a/tests/graph_build.rs\n+++ /dev/null\n"
            "@@ -1,2 +0,0 @@\n-#[test]\n-fn builds() {}\n"
        )
        files, _ = self.widened(diff)
        self.assertEqual(files, ["tests/graph_build.rs", "src/graph/build.rs", "src/graph/types.rs"])

    def test_every_test_file_has_a_target_entry(self) -> None:
        root = Path(__file__).resolve().parents[1]
        stems = {path.stem for path in (root / "tests").glob("*.rs")}
        self.assertEqual(stems - widen_diff.TEST_TARGETS.keys(), set())
        for targets in widen_diff.TEST_TARGETS.values():
            for target in targets or ():
                self.assertTrue((root / target).exists(), target)


if __name__ == "__main__":
    unittest.main()
