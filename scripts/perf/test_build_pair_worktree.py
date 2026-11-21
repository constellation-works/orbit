#!/usr/bin/env python3
"""build-pair.sh snapshots an uncommitted worktree and still refuses a clean one.

The cargo release build is stubbed (`ORBIT_PERF_BUILD_PAIR_STUB`): the check
proves which tree was exported and that provenance names the snapshot. It does
not compile Orbit or take the host perf lane.
"""

from __future__ import annotations

import hashlib
import json
import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BUILD_PAIR = REPO / "scripts" / "perf" / "build-pair.sh"
SCRATCH = REPO / ".orbit" / "tmp"


def _git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        check=check,
        capture_output=True,
        text=True,
        env=env,
    )


def _init_repo(repo: Path) -> None:
    _git(repo, "init", "-b", "main")
    _git(repo, "config", "user.email", "t@example.com")
    _git(repo, "config", "user.name", "Test")
    (repo / ".gitignore").write_text("ignored.txt\n*.ign\n")
    (repo / "tracked.txt").write_text("v1\n")
    (repo / "gone.txt").write_text("here\n")
    (repo / "Cargo.lock").write_text("# fixture lock\n")
    _git(repo, "add", "--", ".gitignore", "tracked.txt", "gone.txt", "Cargo.lock")
    _git(repo, "commit", "-m", "base")


def _freeze_git(repo: Path) -> None:
    """Drop write permission on git metadata, as a managed worktree does."""
    for path in [repo / ".git", * (repo / ".git").rglob("*")]:
        mode = path.stat().st_mode
        path.chmod(mode & ~0o222)


def _thaw_git(repo: Path) -> None:
    for path in [repo / ".git", * (repo / ".git").rglob("*")]:
        mode = path.stat().st_mode
        path.chmod(mode | stat.S_IWUSR)


def _fingerprint(repo: Path) -> dict[str, object]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    head = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "HEAD"],
        check=True, capture_output=True, text=True, env=env,
    ).stdout.strip()
    index_blob = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", ":tracked.txt"],
        check=True, capture_output=True, text=True, env=env,
    ).stdout.strip()
    index_sha = hashlib.sha256((repo / ".git" / "index").read_bytes()).hexdigest()
    files: dict[str, str] = {}
    for path in sorted(repo.rglob("*")):
        if ".git" in path.parts or not path.is_file():
            continue
        files[path.relative_to(repo).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    return {"head": head, "index_blob": index_blob, "index_sha": index_sha, "files": files}


class BuildPairWorktreeTest(unittest.TestCase):
    def setUp(self) -> None:
        SCRATCH.mkdir(parents=True, exist_ok=True)
        self._tmp = tempfile.TemporaryDirectory(dir=SCRATCH)
        self.root = Path(self._tmp.name)
        self.repo = self.root / "wt"
        self.repo.mkdir()
        self.out = self.root / "builds"
        self.lane = self.root / "lane.lock"
        _init_repo(self.repo)

    def tearDown(self) -> None:
        if (self.repo / ".git").exists():
            _thaw_git(self.repo)
        self._tmp.cleanup()

    def env(self) -> dict[str, str]:
        env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("ORBIT_") and not key.startswith("GIT_")
        }
        env["ORBIT_PERF_LANE_LOCK"] = str(self.lane)
        env["ORBIT_PERF_SIMULATE_DISK_PCENT"] = "10"
        env["ORBIT_PERF_LANE_WAIT_SECONDS"] = "5"
        # Safety net: a logic bug must not start a release build of this checkout.
        env["ORBIT_PERF_BUILD_PAIR_STUB"] = "1"
        return env

    def run_pair(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(BUILD_PAIR), *args],
            cwd=REPO,
            env=self.env(),
            capture_output=True,
            text=True,
            timeout=180,
            check=False,
        )

    def test_clean_worktree_refuses_equal_commit_and_leaves_checkout(self) -> None:
        (self.repo / "ignored.txt").write_text("secret\n")
        before = _fingerprint(self.repo)
        _freeze_git(self.repo)
        result = self.run_pair(
            "--base", "HEAD",
            "--candidate-worktree", str(self.repo),
            "--out", str(self.out),
        )
        _thaw_git(self.repo)
        self.assertEqual(result.returncode, 2, result.stderr + result.stdout)
        self.assertIn("same commit", result.stderr)
        self.assertIn(str(before["head"]), result.stderr)
        self.assertFalse(self.out.exists(), result.stderr)
        self.assertEqual(_fingerprint(self.repo), before)
        probe = _git(self.repo, "cat-file", "-t", str(before["head"]), check=False)
        self.assertEqual(probe.returncode, 0)

    def test_nested_candidate_worktree_is_refused_with_root_path(self) -> None:
        (self.repo / "tracked.txt").write_text("v2\n")
        nested = self.repo / "nested"
        nested.mkdir()
        result = self.run_pair(
            "--base", "HEAD",
            "--candidate-worktree", str(nested),
            "--out", str(self.out),
        )
        self.assertEqual(result.returncode, 2, result.stderr + result.stdout)
        self.assertIn(str(self.repo.resolve()), result.stderr)
        self.assertNotIn("same commit", result.stderr)
        self.assertFalse(self.out.exists(), result.stderr)

    def test_same_commit_flags_still_refuse(self) -> None:
        result = self.run_pair("--base", "HEAD", "--candidate", "HEAD", "--out", str(self.out))
        self.assertEqual(result.returncode, 2, result.stderr + result.stdout)
        self.assertIn("same commit", result.stderr)
        self.assertFalse(self.out.exists())

    def test_dirty_worktree_snapshot_is_what_gets_built(self) -> None:
        (self.repo / "tracked.txt").write_text("v3\n")
        _git(self.repo, "add", "--", "tracked.txt")
        (self.repo / "tracked.txt").write_text("v2\n")
        (self.repo / "new.txt").write_text("new\n")
        (self.repo / "ignored.txt").write_text("secret\n")
        (self.repo / "gone.txt").unlink()
        before = _fingerprint(self.repo)
        head = str(before["head"])
        _freeze_git(self.repo)
        result = self.run_pair(
            "--base", "HEAD",
            "--candidate-worktree", str(self.repo),
            "--out", str(self.out),
            "--keep",
        )
        _thaw_git(self.repo)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertEqual(_fingerprint(self.repo), before)

        exported = self.out / ".work" / "src-candidate"
        self.assertEqual((exported / "tracked.txt").read_text(), "v2\n")
        self.assertEqual((exported / "new.txt").read_text(), "new\n")
        self.assertFalse((exported / "ignored.txt").exists())
        self.assertFalse((exported / "gone.txt").exists())
        base_exported = self.out / ".work" / "src-base"
        self.assertEqual((base_exported / "tracked.txt").read_text(), "v1\n")
        self.assertTrue((base_exported / "gone.txt").exists())

        provenance = json.loads((self.out / "provenance.json").read_text())
        candidate = provenance["revisions"]["candidate"]
        base = provenance["revisions"]["base"]
        self.assertIs(candidate["working_tree_snapshot"], True)
        self.assertIs(base["working_tree_snapshot"], False)
        self.assertNotEqual(candidate["commit"], head)
        self.assertNotEqual(candidate["tree"], base["tree"])
        self.assertEqual(base["commit"], head)
        self.assertEqual(provenance["requested"]["candidate_worktree"], str(self.repo.resolve()))
        self.assertIsNone(provenance["requested"]["candidate"])
        self.assertIn("working-tree snapshot", provenance["source_export"])
        self.assertIs(provenance["build_stub"], True)
        binary = (self.out / "candidate" / "orbit").read_bytes()
        self.assertEqual(candidate["binary_sha256"], hashlib.sha256(binary).hexdigest())
        self.assertNotEqual(binary, (self.out / "base" / "orbit").read_bytes())

        leaked = _git(self.repo, "cat-file", "-t", candidate["commit"], check=False)
        self.assertNotEqual(leaked.returncode, 0, leaked.stderr)
        objects = self.out / ".work" / "snapshot-git" / "objects"
        seen = subprocess.run(
            ["git", "-C", str(self.repo), "cat-file", "-t", candidate["commit"]],
            check=False,
            capture_output=True,
            text=True,
            env={
                **{k: v for k, v in os.environ.items() if not k.startswith("GIT_")},
                "GIT_OBJECT_DIRECTORY": str(objects),
                "GIT_ALTERNATE_OBJECT_DIRECTORIES": str(self.repo / ".git" / "objects"),
            },
        )
        self.assertEqual(seen.returncode, 0, seen.stderr)
        self.assertEqual(seen.stdout.strip(), "commit")

    def test_newly_tracked_ignored_file_uses_working_tree_and_excludes_untracked_sibling(self) -> None:
        (self.repo / "tracked.ign").write_text("staged\n")
        _git(self.repo, "add", "-f", "--", "tracked.ign")
        (self.repo / "tracked.ign").write_text("working\n")
        (self.repo / "untracked.ign").write_text("secret\n")
        (self.repo / "ignored.txt").write_text("also-secret\n")
        before = _fingerprint(self.repo)
        head = str(before["head"])
        _freeze_git(self.repo)
        result = self.run_pair(
            "--base", "HEAD",
            "--candidate-worktree", str(self.repo),
            "--out", str(self.out),
            "--keep",
        )
        _thaw_git(self.repo)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertEqual(_fingerprint(self.repo), before)

        exported = self.out / ".work" / "src-candidate"
        self.assertEqual((exported / "tracked.ign").read_text(), "working\n")
        self.assertFalse((exported / "untracked.ign").exists())
        self.assertFalse((exported / "ignored.txt").exists())

        provenance = json.loads((self.out / "provenance.json").read_text())
        candidate = provenance["revisions"]["candidate"]
        base = provenance["revisions"]["base"]
        self.assertIs(candidate["working_tree_snapshot"], True)
        self.assertNotEqual(candidate["commit"], head)
        self.assertNotEqual(candidate["binary_sha256"], base["binary_sha256"])

        leaked = _git(self.repo, "cat-file", "-t", candidate["commit"], check=False)
        self.assertNotEqual(leaked.returncode, 0, leaked.stderr)

    def test_both_candidate_forms_are_refused(self) -> None:
        result = self.run_pair(
            "--base", "HEAD",
            "--candidate", "HEAD",
            "--candidate-worktree", str(self.repo),
            "--out", str(self.out),
        )
        self.assertEqual(result.returncode, 64, result.stderr)
        self.assertIn("--candidate-worktree", result.stderr)


if __name__ == "__main__":
    unittest.main()
