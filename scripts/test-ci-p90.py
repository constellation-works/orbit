#!/usr/bin/env python3
"""Replay a saved PR run list through ci-p90.py's --from-json boundary."""

from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/ci-p90.py"
START = datetime(2026, 10, 30, tzinfo=timezone.utc)


def stamp(seconds):
    return (START + timedelta(seconds=seconds)).strftime("%Y-%m-%dT%H:%M:%SZ")


def job(name, ended, started=0):
    return {"name": name, "startedAt": stamp(started), "completedAt": stamp(ended), "conclusion": "success"}


class CiP90ReplayTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=scratch, prefix="ci-p90-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def replay(self, runs):
        saved = self.root / "runs.json"
        saved.write_text(json.dumps(runs), encoding="utf-8")
        result = subprocess.run([sys.executable, str(SCRIPT), "--from-json", str(saved)],
                                capture_output=True, text=True, check=False)
        self.assertIn(result.returncode, (0, 1), result.stderr)
        return json.loads(result.stdout.splitlines()[0])

    def test_guardrails_finishing_last_is_the_awaited_job_and_its_duration_is_counted(self):
        # The Check job awaits CI guardrails beside the test jobs (ci.yml), so
        # when guardrails completes last it gates the run and must be named.
        summary = self.replay([{"databaseId": 1, "headSha": "abcdef1234567", "jobs": [
            job("Check / Clippy / Test", ended=500),
            job("Test build", ended=250),
            job("Test shard 1/3", ended=240),
            job("Test shard 2/3", ended=260),
            job("Test shard 3/3", ended=255),
            job("Test extra partition", ended=270),
            job("CI guardrails", ended=495),
        ]}])

        self.assertEqual(summary["check_seconds"], 500)
        self.assertEqual(summary["last_awaited_job"], "CI guardrails")
        self.assertEqual(summary["job_seconds"]["CI guardrails"], 495)


if __name__ == "__main__":
    unittest.main()
