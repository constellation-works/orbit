#!/usr/bin/env python3
"""CLI regression tests for scripts/perf/perf_common.py `require-absent-or-empty`.

The direct subcommand help must describe the refusal, not only the parent
listing, and must not advertise removing anything. The guard itself stays
read-only: an absent or empty PATH passes untouched, and a non-empty PATH is
refused with its contents kept. The checks run the script as a subprocess, so
they exercise the argparse surface and exit codes an operator sees.
"""

from __future__ import annotations

import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

PERF_DIR = Path(__file__).resolve().parent
ROOT = PERF_DIR.parent.parent
PERF_COMMON = PERF_DIR / "perf_common.py"
SCRATCH = ROOT / ".orbit" / "tmp"

ENTRY = "build-pair.sh"


def _run(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-I", str(PERF_COMMON), *args],
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )


def _without_usage(text: str) -> str:
    # The usage line repeats the subcommand name, which contains "empty" and
    # would satisfy an emptiness check without any description.
    return "\n".join(line for line in text.splitlines() if not line.startswith("usage:"))


class PerfCommonCliTest(unittest.TestCase):
    def setUp(self) -> None:
        SCRATCH.mkdir(parents=True, exist_ok=True)
        self._tmp = tempfile.TemporaryDirectory(dir=SCRATCH)
        self.root = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_parent_listing_describes_the_refusal_without_removal(self) -> None:
        result = _run("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertRegex(result.stdout, r"require-absent-or-empty\s+refuse\b")
        self.assertNotRegex(result.stdout, r"(?i)\b(remov|delet)\w*")

    def test_direct_help_describes_the_refusal_without_removal(self) -> None:
        result = _run("require-absent-or-empty", "--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        body = _without_usage(result.stdout)
        self.assertRegex(body, r"(?i)\brefuse\b")
        self.assertRegex(body, r"(?i)\bempty\b")
        self.assertNotRegex(result.stdout, r"(?i)\b(remov|delet)\w*")

    def test_absent_path_is_accepted_and_not_created(self) -> None:
        target = self.root / "absent"
        result = _run("require-absent-or-empty", "--entry", ENTRY, str(target))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(target.exists())

    def test_empty_directory_is_accepted_and_left_in_place(self) -> None:
        target = self.root / "empty"
        target.mkdir()
        result = _run("require-absent-or-empty", "--entry", ENTRY, str(target))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(target.is_dir())
        self.assertEqual(list(target.iterdir()), [])

    def test_nonempty_directory_is_refused_and_contents_kept(self) -> None:
        target = self.root / "full"
        target.mkdir()
        keep = target / "keep.txt"
        keep.write_text("evidence\n")
        result = _run("require-absent-or-empty", "--entry", ENTRY, str(target))
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertTrue(result.stderr.startswith(f"{ENTRY}: "), result.stderr)
        self.assertIn(str(target), result.stderr)
        self.assertEqual(keep.read_text(), "evidence\n")


if __name__ == "__main__":
    unittest.main()
