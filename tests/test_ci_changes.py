#!/usr/bin/env python3

import unittest

from scripts_import import load

changes = load("ci-changes.py")


class CodeChangedTest(unittest.TestCase):
    def test_prose_under_docs_or_at_the_root_is_not_code(self) -> None:
        self.assertFalse(changes.code_changed(["CHANGELOG.md", "docs/dev/ci-porting-notes.md"]))
        self.assertFalse(changes.code_changed(["docs/adr/0004-library-is-not-public.md"]))

    def test_prose_a_test_or_the_sdist_reads_is_code(self) -> None:
        for path in ["README.md", "README.ja.md", "docs/dev/spec.ja.md"]:
            self.assertTrue(changes.code_changed([path]), path)

    def test_markdown_outside_docs_and_the_root_is_code(self) -> None:
        self.assertTrue(changes.code_changed(["tests/fixtures/app/README.md"]))
        self.assertTrue(changes.code_changed(["src/notes.md"]))

    def test_non_markdown_under_docs_is_code(self) -> None:
        self.assertTrue(changes.code_changed(["docs/schema/report.schema.json"]))

    def test_one_code_file_among_prose_is_code(self) -> None:
        self.assertTrue(changes.code_changed(["CHANGELOG.md", "src/lib.rs"]))

    def test_an_empty_diff_is_code(self) -> None:
        self.assertTrue(changes.code_changed([]))


if __name__ == "__main__":
    unittest.main()
