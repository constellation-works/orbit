#!/usr/bin/env python3
"""Exercise the history generator CLI against real, isolated Git histories."""

import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
ASSETS = Path("crates/orbit-core/assets/auto_tasks")
SOURCES = Path("crates/orbit-cli/tests/fixtures/auto-task-body-history/sources.json")


class BodyHistoryTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch, prefix="body-history-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / "repo"
        (self.root / ASSETS).mkdir(parents=True)
        (self.root / SOURCES).parent.mkdir(parents=True)
        (self.root / "scripts").mkdir()
        shutil.copy2(ROOT / "scripts/update-auto-task-body-history.py", self.root / "scripts")
        self.git("init", "-q")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.asset = self.root / ASSETS / "example.yaml"
        self.history = self.root / ASSETS / "body-history.json"
        self.sources = self.root / SOURCES
        self.asset.write_text("""schemaVersion: 1
name: example
schedule:
  every_minutes: 60
template:
  title: Original body
""")
        self.git("add", ".")
        self.git("commit", "-qm", "Original shipped body")
        self.original = self.git("rev-parse", "HEAD").strip()

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, text=True)

    def run_generator(self, *args, root=None, code=0):
        result = subprocess.run(
            [sys.executable, "scripts/update-auto-task-body-history.py", *args],
            cwd=root or self.root, text=True, capture_output=True, timeout=30,
        )
        self.assertEqual(result.returncode, code, result.stdout + result.stderr)
        return result

    def snapshot(self):
        return self.history.read_bytes(), self.sources.read_bytes()

    def successor(self):
        self.asset.write_text(self.asset.read_text().replace("Original body", "Successor body"))

    def test_missing_superseded_body_fails_read_only_and_regeneration_repairs_it(self):
        # Current bodies may be absent; no landed history has been superseded.
        self.history.write_text('{"example": []}\n')
        self.sources.write_text('{"example": {}}\n')
        # Use the generator's canonical formatting for empty tables.
        for path in (self.history, self.sources):
            path.write_text(json.dumps(json.loads(path.read_text()), indent=2) + "\n")
        self.run_generator("--check")
        self.successor()
        # The uncommitted successor must already require the landed predecessor.
        before = self.snapshot()
        self.assertIn("stale", self.run_generator("--check", code=1).stderr)
        self.assertEqual(self.snapshot(), before, "check mode must not repair its inputs")
        self.git("add", str(ASSETS / "example.yaml"))
        self.git("commit", "-qm", "Supersede shipped body")
        self.run_generator("--check", code=1)
        self.run_generator()
        self.assertIn(self.original, json.loads(self.sources.read_text())["example"])
        generated = self.snapshot()
        self.run_generator()
        self.assertEqual(self.snapshot(), generated, "regeneration must be deterministic")
        self.run_generator("--check")
        # Remove both halves, as happened when an asset changed without a backfill.
        history = json.loads(self.history.read_text())
        history["example"] = [r for r in history["example"] if r["revision"] != self.original]
        sources = json.loads(self.sources.read_text())
        del sources["example"][self.original]
        self.history.write_text(json.dumps(history, indent=2, sort_keys=True) + "\n")
        self.sources.write_text(json.dumps(sources, indent=2, sort_keys=True) + "\n")
        self.run_generator("--check", code=1)

    def test_corrupt_source_fixture_fails_without_writing(self):
        self.run_generator()
        sources = json.loads(self.sources.read_text())
        sources["example"][self.original] += "# Not the shipped source\n"
        self.sources.write_text(json.dumps(sources, indent=2, sort_keys=True) + "\n")
        before = self.snapshot()
        self.run_generator("--check", code=1)
        self.assertEqual(self.snapshot(), before)

    def test_shallow_check_refuses_incomplete_history_but_generation_retains_fixtures(self):
        self.run_generator()
        self.successor()
        self.git("add", ".")
        self.git("commit", "-qm", "Successor with original history")
        self.run_generator()
        self.git("add", ".")
        self.git("commit", "-qm", "Record successor history")
        clone = self.root.parent / "shallow"
        subprocess.run(
            ["git", "clone", "-q", "--depth=1", self.root.as_uri(), str(clone)],
            check=True, capture_output=True, timeout=30,
        )
        self.assertIn("full Git history", self.run_generator("--check", root=clone, code=1).stderr)
        before = (clone / SOURCES).read_bytes()
        self.run_generator(root=clone)
        self.assertEqual((clone / SOURCES).read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
