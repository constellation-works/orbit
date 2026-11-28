#!/usr/bin/env python3
"""Fixture for the feature board's Git-visibility rule.

A Git-ignored local notes directory under docs/design/ must not make
feature-board generation or --check fail or change their output, while a
Git-visible feature with a missing overview or a missing/invalid stage
still fails with a targeted error. Exercises the public generator script
rather than its shell internals.
"""
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GENERATOR = ROOT / "scripts/generate-doc-indexes.sh"

VALID_OVERVIEW = """\
---
stage: building
next: Ship it.
owner: Fixture
---

# Valid Feature
"""

MINIMAL_INDEX = """\
---
type: context
summary: Fixture index.
---

# Fixture
"""


def make_repo(directory):
    """Create a minimal git repo shaped like the docs tree."""
    repo = Path(directory)
    (repo / "docs/design/valid-feature").mkdir(parents=True)
    (repo / "docs/runbooks").mkdir(parents=True)
    (repo / "docs/agents/records/rca").mkdir(parents=True)
    (repo / "docs/design/valid-feature/1_overview.md").write_text(
        VALID_OVERVIEW, encoding="utf-8"
    )
    (repo / "docs/INDEX.md").write_text(MINIMAL_INDEX, encoding="utf-8")
    subprocess.run(
        ["git", "init", "-q"], cwd=repo, check=True,
        capture_output=True, text=True,
    )
    return repo


def run_generator(repo, *args):
    return subprocess.run(
        ["bash", str(GENERATOR), *args, str(repo)],
        capture_output=True, text=True, check=False,
    )


class GenerateDocIndexesFixture(unittest.TestCase):
    def test_ignored_notes_change_nothing(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="doc-indexes-ignored-", dir=scratch
        ) as directory:
            repo = make_repo(directory)
            notes = repo / "docs/design/local-notes"
            notes.mkdir()
            (notes / "notes.md").write_text(
                "# Scratch notes\n\nLocal only.\n", encoding="utf-8"
            )
            (repo / ".gitignore").write_text(
                "docs/design/local-notes/\n", encoding="utf-8"
            )
            ignored = subprocess.run(
                ["git", "check-ignore", "-q",
                 "docs/design/local-notes/notes.md"],
                cwd=repo, check=False,
            )
            self.assertEqual(
                ignored.returncode, 0,
                "fixture must actually ignore the notes file",
            )

            result = run_generator(repo)
            self.assertEqual(
                result.returncode, 0, result.stdout + result.stderr
            )
            board = repo / "docs/design/FEATURES.md"
            index = repo / "docs/INDEX.md"
            self.assertIn("Valid Feature", board.read_text(encoding="utf-8"))
            self.assertNotIn("local-notes", board.read_text(encoding="utf-8"))
            with_notes_board = board.read_bytes()
            with_notes_index = index.read_bytes()

            check = run_generator(repo, "--check")
            self.assertEqual(check.returncode, 0, check.stdout + check.stderr)

            for child in sorted(notes.iterdir()):
                child.unlink()
            notes.rmdir()
            rerun = run_generator(repo)
            self.assertEqual(rerun.returncode, 0, rerun.stdout + rerun.stderr)
            self.assertEqual(board.read_bytes(), with_notes_board)
            self.assertEqual(index.read_bytes(), with_notes_index)

    def test_visible_missing_overview_fails(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="doc-indexes-overview-", dir=scratch
        ) as directory:
            repo = make_repo(directory)
            broken = repo / "docs/design/broken"
            broken.mkdir()
            (broken / "2_design.md").write_text(
                "# Broken design\n", encoding="utf-8"
            )
            result = run_generator(repo)
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn("1_overview.md", result.stderr)
            self.assertIn("broken", result.stderr)

    def test_visible_missing_stage_fails(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="doc-indexes-stage-", dir=scratch
        ) as directory:
            repo = make_repo(directory)
            (repo / "docs/design/valid-feature/1_overview.md").write_text(
                "---\nnext: Ship it.\nowner: Fixture\n---\n\n# No Stage\n",
                encoding="utf-8",
            )
            result = run_generator(repo)
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn("needs stage", result.stderr)

    def test_visible_invalid_stage_fails(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="doc-indexes-bad-stage-", dir=scratch
        ) as directory:
            repo = make_repo(directory)
            (repo / "docs/design/valid-feature/1_overview.md").write_text(
                "---\nstage: someday\nnext: Ship it.\nowner: Fixture\n"
                "---\n\n# Bad Stage\n",
                encoding="utf-8",
            )
            result = run_generator(repo)
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn("needs stage", result.stderr)
            self.assertIn("someday", result.stderr)


if __name__ == "__main__":
    unittest.main()
