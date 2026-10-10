#!/usr/bin/env python3
"""Exercise the unit-test ratchet on synthetic crate trees."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parent
INVENTORY = SCRIPTS / "unit-test-inventory.py"


class UnitTestRatchetTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit" / "tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.baseline = self.root / "baseline.json"

    def write(self, relative, text):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def run_inventory(self, *args):
        return subprocess.run(
            [sys.executable, str(INVENTORY), "--root", str(self.root), *args],
            capture_output=True, text=True, check=False,
        )

    def git(self, *args, check=True):
        environment = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        for name in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"):
            environment.pop(name, None)
        return subprocess.run(
            ["git", "-C", str(self.root), "-c", "user.name=Ratchet fixture",
             "-c", "user.email=ratchet@example.invalid", "-c", "commit.gpgsign=false",
             "-c", f"core.hooksPath={os.devnull}", "-c", "rerere.enabled=false", *args],
            env=environment, capture_output=True, text=True, check=check,
        )

    def test_counts_inline_and_sibling_tests_per_crate(self):
        self.write("crates/alpha/src/lib.rs", (
            "#[cfg(test)]\nmod tests;\n"
            "#[test]\nfn one() {}\n"
            "#[tokio::test(flavor = \"multi_thread\")]\nasync fn two() {}\n"
            "#[test_case]\nfn not_a_test_attribute() {}\n"
        ))
        self.write("crates/alpha/src/tests/mod.rs", "mod extra;\n")
        self.write("crates/alpha/src/tests/extra.rs", "#[test]\nfn three() {}\n#[test]\nfn four() {}\n")
        self.write("crates/beta/src/lib.rs", "pub fn no_tests() {}\n")

        result = self.run_inventory()

        self.assertEqual(result.returncode, 0, result.stderr)
        inventory = json.loads(result.stdout)
        self.assertEqual(inventory["crates"]["alpha"]["test_functions"], 4)
        self.assertEqual(inventory["crates"]["alpha"]["test_file_lines"], 5)
        self.assertEqual(inventory["crates"]["beta"]["test_functions"], 0)
        self.assertEqual(inventory["crates"]["alpha"]["test_function_paths"], [
            "lib.rs::one", "lib.rs::two", "tests/extra.rs::four", "tests/extra.rs::three",
        ])

    def test_check_rejects_unadmitted_tests_and_passes_once_recorded(self):
        self.write("crates/alpha/src/lib.rs", "#[test]\nfn one() {}\n")
        self.assertEqual(self.run_inventory("--write-baseline", str(self.baseline)).returncode, 0)
        self.assertEqual(self.run_inventory("--check", str(self.baseline)).returncode, 0)

        # A new test in a crate at its baseline is growth the ratchet must catch.
        self.write("crates/alpha/src/more.rs", "#[test]\nfn two() {}\n")
        grown = self.run_inventory("--check", str(self.baseline))
        self.assertEqual(grown.returncode, 1, "ratchet must fail for an unadmitted test")
        self.assertIn("more.rs::two", grown.stderr)

        # Raising the baseline in the same change makes the growth an explicit, reviewed diff.
        self.assertEqual(self.run_inventory("--write-baseline", str(self.baseline)).returncode, 0)
        self.assertEqual(self.run_inventory("--check", str(self.baseline)).returncode, 0)

    def test_names_functions_after_stacked_attributes_and_comments(self):
        self.write("crates/alpha/src/lib.rs", (
            "#[test]\n#[ignore = \"child fixture\"]\n#[cfg(unix)]\n"
            "/// A fixture with several intervening attributes.\n"
            "// Another comment.\n#[allow(\n    dead_code\n)]\n"
            "pub(super) fn one() {}\n"
            "#[tokio::test]\n#[cfg(unix)]\npub async fn two() {}\n"
            "#[test] #[ignore] fn r#type() { let _ = [1]; }\n"
        ))
        result = self.run_inventory()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["crates"]["alpha"]["test_function_paths"], [
            "lib.rs::one", "lib.rs::r#type", "lib.rs::two",
        ])

    def test_removal_passes_but_replacement_cannot_reuse_an_admission(self):
        self.write("crates/alpha/src/lib.rs", "#[test]\nfn one() {}\n#[test]\nfn two() {}\n")
        self.assertEqual(self.run_inventory("--write-baseline", str(self.baseline)).returncode, 0)
        self.write("crates/alpha/src/lib.rs", "#[test]\nfn one() {}\n")
        self.assertEqual(self.run_inventory("--check", str(self.baseline)).returncode, 0)

        # The old count gate would let this replacement use the retired test's slot.
        self.write("crates/alpha/src/lib.rs", "#[test]\nfn one() {}\n#[test]\nfn three() {}\n")
        replaced = self.run_inventory("--check", str(self.baseline))
        self.assertEqual(replaced.returncode, 1)
        self.assertIn("lib.rs::three", replaced.stderr)

        self.write("crates/alpha/src/lib.rs", "#[test]\nfn one() {}\n")
        self.assertEqual(self.run_inventory("--write-baseline", str(self.baseline)).returncode, 0)
        self.assertEqual(json.loads(self.baseline.read_text())["crates"]["alpha"], {
            "test_function_paths": ["lib.rs::one"],
        })

    def test_repeated_inline_names_consume_separate_admissions(self):
        first = "mod a {\n#[test]\nfn same() {}\n}\n"
        self.write("crates/alpha/src/lib.rs", first)
        self.assertEqual(self.run_inventory("--write-baseline", str(self.baseline)).returncode, 0)
        self.write("crates/alpha/src/lib.rs", first + "mod b {\n#[test]\nfn same() {}\n}\n")
        self.assertEqual(self.run_inventory("--check", str(self.baseline)).returncode, 1,
                         "a set comparison would lose the second inline test with the same name")
        self.assertEqual(self.run_inventory("--write-baseline", str(self.baseline)).returncode, 0)
        self.assertEqual(json.loads(self.baseline.read_text())["crates"]["alpha"]["test_function_paths"],
                         ["lib.rs::same", "lib.rs::same"])
        self.assertEqual(self.run_inventory("--check", str(self.baseline)).returncode, 0)

    def test_rejects_legacy_or_malformed_baselines(self):
        self.write("crates/alpha/src/lib.rs", "#[test]\nfn one() {}\n")
        for baseline in (
            {"schema_version": 1, "crates": {"alpha": {"test_functions": 1}}},
            {"schema_version": 2, "crates": {"alpha": {"test_function_paths": 1}}},
            {"schema_version": 2, "crates": {"alpha": {"test_function_paths": [None]}}},
        ):
            with self.subTest(baseline=baseline):
                self.baseline.write_text(json.dumps(baseline), encoding="utf-8")
                self.assertEqual(self.run_inventory("--check", str(self.baseline)).returncode, 2)

    def test_unnameable_test_attribute_fails_instead_of_disappearing(self):
        self.write("crates/alpha/src/lib.rs", "#[test]\nmake_test!();\n")
        self.assertEqual(self.run_inventory("--write-baseline", str(self.baseline)).returncode, 2)
        self.assertFalse(self.baseline.exists())

    def test_independent_admissions_merge_completely_or_conflict(self):
        # The concurrent count bumps in the 2026-10-09 incident must never merge
        # cleanly into a stale baseline (ORB-15108). Exercise real Git merging.
        self.write("crates/alpha/src/lib.rs", "#[test]\nfn original() {}\n")
        self.assertEqual(self.run_inventory("--write-baseline", str(self.baseline)).returncode, 0)
        self.git("init", "--initial-branch=base")
        self.git("add", ".")
        self.git("commit", "-m", "Base admission")

        self.git("checkout", "-b", "first")
        self.write("crates/alpha/src/first.rs", "#[test]\nfn first() {}\n")
        self.assertEqual(self.run_inventory("--write-baseline", str(self.baseline)).returncode, 0)
        first_baseline = self.baseline.read_text(encoding="utf-8")
        self.git("add", ".")
        self.git("commit", "-m", "First independent admission")

        self.git("checkout", "-b", "second", "base")
        self.write("crates/alpha/src/second.rs", "#[test]\nfn second() {}\n")
        self.assertEqual(self.run_inventory("--write-baseline", str(self.baseline)).returncode, 0)
        self.git("add", ".")
        self.git("commit", "-m", "Second independent admission")

        merged = self.git("merge", "--no-edit", "first", check=False)
        expected = ["first.rs::first", "lib.rs::original", "second.rs::second"]
        if merged.returncode == 0:
            checked = self.run_inventory("--check", str(self.baseline))
            self.assertEqual(checked.returncode, 0, checked.stderr)
            self.assertEqual(json.loads(self.baseline.read_text())["crates"]["alpha"]["test_function_paths"],
                             expected, "a clean merge must retain both independent admissions")
        else:
            self.assertEqual(merged.returncode, 1, merged.stdout + merged.stderr)
            self.assertEqual(self.git("diff", "--name-only", "--diff-filter=U").stdout.splitlines(),
                             ["baseline.json"], "concurrent admissions must conflict in their baseline")
            self.assertEqual(self.run_inventory("--check", str(self.baseline)).returncode, 2)

        # Both source additions survived the merge. Recreate the old silent-loss
        # outcome explicitly and prove that a stale admission now fails.
        self.baseline.write_text(first_baseline, encoding="utf-8")
        stale = self.run_inventory("--check", str(self.baseline))
        self.assertEqual(stale.returncode, 1, "a merged tree cannot spend one admission twice")
        self.assertIn("second.rs::second", stale.stderr)
        self.assertEqual(self.run_inventory("--write-baseline", str(self.baseline)).returncode, 0)
        self.assertEqual(json.loads(self.baseline.read_text())["crates"]["alpha"]["test_function_paths"], expected)
        self.assertEqual(self.run_inventory("--check", str(self.baseline)).returncode, 0)

    def test_crate_missing_from_baseline_has_zero_allowance(self):
        self.write("crates/alpha/src/lib.rs", "#[test]\nfn one() {}\n")
        self.write("crates/gamma/src/lib.rs", "pub fn no_tests() {}\n")
        self.assertEqual(self.run_inventory("--write-baseline", str(self.baseline)).returncode, 0)

        self.write("crates/new_crate/src/lib.rs", "#[test]\nfn fresh() {}\n")
        result = self.run_inventory("--check", str(self.baseline))

        self.assertEqual(result.returncode, 1, "a new crate with tests must not slip past the ratchet")
        self.assertIn("new_crate", result.stderr)


if __name__ == "__main__":
    unittest.main()
