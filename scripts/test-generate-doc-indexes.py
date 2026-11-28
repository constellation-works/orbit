#!/usr/bin/env python3
"""Fixture for the feature board's Git-visibility rule.

A Git-ignored local notes directory under docs/design/ must not make
feature-board generation or --check fail or change their output, while a
Git-visible feature with a missing overview or a missing/invalid stage
still fails with a targeted error. A Git query that fails must stop
generation and --check with a Git diagnostic and leave the generated
files untouched. Exercises the public generator script rather than its
shell internals.
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


def commit_all(repo):
    """Commit the fixture, so its files are tracked as in a real checkout."""
    subprocess.run(["git", "add", "-A"], cwd=repo, check=True,
                   capture_output=True, text=True)
    subprocess.run(
        ["git", "-c", "user.name=fixture",
         "-c", "user.email=fixture@example.invalid",
         "-c", "commit.gpgsign=false",
         "commit", "-q", "-m", "fixture"],
        cwd=repo, check=True, capture_output=True, text=True,
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

    def test_committed_missing_overview_fails(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="doc-indexes-committed-", dir=scratch
        ) as directory:
            repo = make_repo(directory)
            broken = repo / "docs/design/broken"
            broken.mkdir()
            (broken / "2_design.md").write_text(
                "# Broken design\n", encoding="utf-8"
            )
            commit_all(repo)
            result = run_generator(repo)
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertIn("1_overview.md", result.stderr)
            self.assertIn("broken", result.stderr)

    def test_git_visibility_failure_stops_both_modes(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="doc-indexes-git-failure-", dir=scratch
        ) as directory:
            repo = make_repo(directory)
            broken = repo / "docs/design/broken"
            broken.mkdir()
            (broken / "2_design.md").write_text(
                "# Broken design\n", encoding="utf-8"
            )
            commit_all(repo)
            board = repo / "docs/design/FEATURES.md"
            index = repo / "docs/INDEX.md"
            board.write_text("SENTINEL BOARD\n", encoding="utf-8")
            index.write_text("SENTINEL INDEX\n", encoding="utf-8")
            # A corrupt index makes `git ls-files` exit 128 (fatal), not "not visible".
            (repo / ".git/index").write_bytes(b"not a git index")

            for args in ((), ("--check",)):
                with self.subTest(mode=args or ("write",)):
                    result = run_generator(repo, *args)
                    self.assertNotEqual(
                        result.returncode, 0, result.stdout + result.stderr
                    )
                    self.assertIn("git ls-files failed", result.stderr)
                    self.assertEqual(
                        board.read_text(encoding="utf-8"), "SENTINEL BOARD\n"
                    )
                    self.assertEqual(
                        index.read_text(encoding="utf-8"), "SENTINEL INDEX\n"
                    )

    def test_non_ascii_feature_stays_listed(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="doc-indexes-unicode-", dir=scratch
        ) as directory:
            repo = make_repo(directory)
            cafe = repo / "docs/design/café"
            cafe.mkdir()
            (cafe / "1_overview.md").write_text(
                "---\ntitle: Café Feature\nstage: building\nnext: Ship it.\n"
                "owner: Fixture\n---\n\n# Café Feature\n",
                encoding="utf-8",
            )
            commit_all(repo)
            result = run_generator(repo)
            self.assertEqual(
                result.returncode, 0, result.stdout + result.stderr
            )
            board = (repo / "docs/design/FEATURES.md").read_text(encoding="utf-8")
            self.assertIn("Café Feature", board)


if __name__ == "__main__":
    unittest.main()
