#!/usr/bin/env python3
"""CLI edges of the dashboard memory soak that must fail before any measurement."""

import subprocess
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SOAK = ROOT / "scripts" / "web-memory-soak.py"


class WebMemorySoakCliTests(unittest.TestCase):
    def run_soak(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, "-B", str(SOAK), "--bin", "/bin/true", *args],
            text=True,
            capture_output=True,
            timeout=30,
        )

    def assert_usage(self, result: subprocess.CompletedProcess[str], needle: str) -> None:
        combined = result.stdout + result.stderr
        self.assertEqual(result.returncode, 2, combined)
        self.assertIn("usage:", combined)
        self.assertIn(needle, combined)
        self.assertNotIn("Traceback", combined)
        self.assertNotIn("ValueError", combined)

    def test_rounds_below_3_are_a_usage_error(self):
        for rounds in ("1", "2"):
            with self.subTest(rounds=rounds):
                result = self.run_soak("--rounds", rounds)
                self.assert_usage(result, "at least 3")

    def test_rounds_3_passes_the_parser(self):
        # A bad --server-env stops argparse, so round 3 never starts a soak.
        result = self.run_soak("--rounds", "3", "--server-env", "NOEQUALS")
        self.assert_usage(result, "KEY=VALUE")
        self.assertNotIn("at least 3", result.stdout + result.stderr)

    def test_server_env_without_equals_is_a_usage_error(self):
        result = self.run_soak("--server-env", "MALLOC_ARENA_MAX")
        self.assert_usage(result, "KEY=VALUE")

    def test_server_env_binding_is_accepted_when_it_has_equals(self):
        result = self.run_soak("--rounds", "2", "--server-env", "MALLOC_ARENA_MAX=2")
        self.assert_usage(result, "at least 3")
        combined = result.stdout + result.stderr
        self.assertNotIn("MALLOC_ARENA_MAX", combined)


if __name__ == "__main__":
    unittest.main()
