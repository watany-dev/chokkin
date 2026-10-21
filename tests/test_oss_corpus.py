#!/usr/bin/env python3

import tempfile
import unittest
from pathlib import Path

from scripts_import import ROOT, load

oc = load("oss_corpus.py")


class ReadManifestTest(unittest.TestCase):
    def test_core_only_stops_at_the_feature_corpus(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "clones.manifest"
            manifest.write_text(
                "# header comment\n"
                "\n"
                "alpha\tlib\tmedium\tv1\thttps://example.com/alpha  # trailing\n"
                "beta\tapp\n"
                "# --- R-01..R-07 feature corpus ---\n"
                "gamma\tlib\tsmall\tv2\thttps://example.com/gamma\n",
                encoding="utf-8",
            )

            core = oc.read_manifest(manifest, core_only=True)
            every = oc.read_manifest(manifest)

        self.assertEqual([r["slug"] for r in core], ["alpha", "beta"])
        self.assertEqual([r["slug"] for r in every], ["alpha", "beta", "gamma"])
        self.assertEqual(core[0]["url"], "https://example.com/alpha")
        # Missing columns are padded, not an IndexError.
        self.assertEqual(core[1], {"slug": "beta", "category": "app", "size": "", "ref": "", "url": ""})


class ReadSectionTest(unittest.TestCase):
    def test_slugs_run_from_the_header_to_the_next_section(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "clones.manifest"
            manifest.write_text(
                "alpha\tlib\n"
                "# --- app-mode corpus (#1) ---\n"
                "# comment\n"
                "beta\tapp\n"
                "gamma\tapp  # trailing\n"
                "# --- later ---\n"
                "delta\tlib\n",
                encoding="utf-8",
            )

            self.assertEqual(oc.read_section(manifest, "# --- app-mode corpus"), {"beta", "gamma"})
            self.assertEqual(oc.read_section(manifest, "# --- missing"), set())


class ReadRecallTest(unittest.TestCase):
    def test_relative_paths_resolve_against_the_repo_root(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            recall = Path(directory) / "recall.manifest"
            recall.write_text(
                "# comment\nrel\ttests/fixtures/x\nabs\t/opt/y\n", encoding="utf-8"
            )
            rows = oc.read_recall(recall)

        self.assertEqual(rows, [("rel", ROOT / "tests/fixtures/x"), ("abs", Path("/opt/y"))])


class ReadEnvManifestTest(unittest.TestCase):
    def test_dash_means_the_default_test_command(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "env.manifest"
            manifest.write_text(
                "# slug\tpythonpath\tinstall\ttest\n"
                "a\tsrc\t-e .\t-\n"
                "b\t.\t.[test]\tpython -m unittest\n",
                encoding="utf-8",
            )
            envs = oc.read_env_manifest(manifest)

        self.assertEqual(envs["a"]["test"], oc.DEFAULT_TEST_CMD)
        self.assertEqual(envs["b"], {"pythonpath": ".", "install": ".[test]", "test": "python -m unittest"})


class ReadLockTest(unittest.TestCase):
    def test_skips_the_header_and_malformed_rows(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            clones = Path(directory)
            self.assertEqual(oc.read_lock(clones), {})
            (clones / "clones.lock.tsv").write_text(
                "slug\tref\turl\tsha\n"
                "alpha\tv1\thttps://example.com/alpha\tabc123\n"
                "broken\tv1\n",
                encoding="utf-8",
            )
            lock = oc.read_lock(clones)

        self.assertEqual(lock, {"alpha": {"ref": "v1", "url": "https://example.com/alpha", "sha": "abc123"}})


class MdTableTest(unittest.TestCase):
    def test_aligns_columns_and_stringifies_cells(self) -> None:
        self.assertEqual(
            oc.md_table(["Name", "Count"], [["a", 1], ["b", None]], "lr"),
            ["| Name | Count |", "|---|---:|", "| a | 1 |", "| b | None |"],
        )


class CommittedManifestsTest(unittest.TestCase):
    """The committed inputs every oss-* script reads still parse."""

    def test_core_corpus_is_the_pinned_twenty(self) -> None:
        core = oc.read_manifest(oc.DEFAULT_MANIFEST, core_only=True)
        slugs = [r["slug"] for r in core]
        self.assertEqual(len(slugs), 20)
        self.assertEqual(len(set(slugs)), 20)
        for row in core:
            self.assertTrue(row["url"].startswith("https://"), row)
            self.assertIn(row["size"], ("small", "medium", "large"), row)

    def test_recall_and_env_manifests_parse(self) -> None:
        recall = oc.read_recall(oc.DEFAULT_RECALL)
        self.assertTrue(recall)
        for slug, path in recall:
            self.assertTrue(path.is_dir(), f"{slug}: {path}")
        slugs = {r["slug"] for r in oc.read_manifest(oc.DEFAULT_MANIFEST)}
        self.assertLessEqual(set(oc.read_env_manifest(oc.DEFAULT_ENV_MANIFEST)), slugs)

    def test_app_corpus_section_is_present(self) -> None:
        app = oc.read_section(oc.DEFAULT_MANIFEST, oc.APP_CORPUS_SECTION)
        self.assertIn("healthchecks", app)
        self.assertNotIn("airflow", app)


if __name__ == "__main__":
    unittest.main()
