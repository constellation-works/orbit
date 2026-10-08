#!/usr/bin/env python3
"""Exercise the unit-test ratchet on synthetic crate trees."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parent
INVENTORY = SCRIPTS / "unit-test-inventory.py"


class UnitTestRatchetTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
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

    def test_check_fails_when_a_crate_exceeds_its_baseline_and_passes_once_raised(self):
        self.write("crates/alpha/src/lib.rs", "#[test]\nfn one() {}\n")
        self.assertEqual(self.run_inventory("--write-baseline", str(self.baseline)).returncode, 0)
        self.assertEqual(self.run_inventory("--check", str(self.baseline)).returncode, 0)

        # A new test in a crate at its baseline is growth the ratchet must catch.
        self.write("crates/alpha/src/more.rs", "#[test]\nfn two() {}\n")
        grown = self.run_inventory("--check", str(self.baseline))
        self.assertEqual(grown.returncode, 1, "ratchet must fail when the count exceeds the baseline")
        self.assertIn("alpha", grown.stderr)
        self.assertIn("baseline of 1", grown.stderr)

        # Raising the baseline in the same change makes the growth an explicit, reviewed diff.
        self.assertEqual(self.run_inventory("--write-baseline", str(self.baseline)).returncode, 0)
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
