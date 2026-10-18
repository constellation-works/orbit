#!/usr/bin/env python3
"""Exercise the Check job's wait for the test jobs against a fixture jobs API."""

import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest

SCRIPTS = Path(__file__).resolve().parent
EXPECTED = ["Test build", "Test shard 1/2", "Test shard 2/2"]


def job(name, status="completed", conclusion="success"):
    return {"name": name, "status": status, "conclusion": conclusion if status == "completed" else None,
            "started_at": "2026-10-18T10:00:00Z", "completed_at": "2026-10-18T10:04:10Z",
            "html_url": f"https://example.invalid/{name.replace(' ', '-')}"}


class AwaitJobsTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="ci-await-jobs-", dir=scratch)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.responses = []
        self.requests = []
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                fixture.requests.append(self.path)
                response = fixture.responses.pop(0) if len(fixture.responses) > 1 else fixture.responses[0]
                body = b"" if response is None else json.dumps({"total_count": len(response),
                                                                 "jobs": response}).encode()
                self.send_response(503 if response is None else 200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.01})
        thread.start()

        def close():
            server.shutdown()
            server.server_close()
            thread.join()

        self.addCleanup(close)
        real_gh = shutil.which("gh")
        self.assertIsNotNone(real_gh, "the fixture drives the GitHub CLI the workflow uses")
        # Redirect only the endpoint; real gh pages, projects with --jq and
        # reports HTTP errors.
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        (bin_dir / "gh").write_text(f'''#!/usr/bin/env python3
import os, sys
args = ["http://127.0.0.1:{server.server_port}/" + arg if arg.startswith("repos/") else arg
        for arg in sys.argv[1:]]
os.execv({real_gh!r}, [{real_gh!r}, *args])
''')
        (bin_dir / "gh").chmod(0o755)
        self.env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}", GH_TOKEN="offline-token",
                        GH_CONFIG_DIR=str(self.root / "gh-config"), GITHUB_REPOSITORY="fixture/orbit",
                        GITHUB_RUN_ID="4242")

    def wait(self, *responses, timeout="10"):
        self.responses[:] = responses
        arguments = [argument for name in EXPECTED for argument in ("--job", name)]
        return subprocess.run([sys.executable, str(SCRIPTS / "ci-await-jobs.py"), *arguments,
                               "--poll-seconds", "0.01", "--timeout-seconds", timeout],
                              env=self.env, text=True, capture_output=True, timeout=60)

    def test_waits_until_every_named_job_succeeds(self):
        building = [job("Test build", "in_progress"), job("Check / Clippy / Test", "in_progress")]
        sharding = [job("Test build"), job("Test shard 1/2"), job("Test shard 2/2", "queued")]
        done = [job("Test build"), job("Test shard 1/2"), job("Test shard 2/2"),
                job("Check / Clippy / Test", "in_progress")]
        result = self.wait(building, None, sharding, done)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("all 3 awaited jobs succeeded", result.stdout)
        for name in EXPECTED:
            self.assertEqual(result.stdout.count(f"ci-await: {name} succeeded"), 1, result.stdout)
        self.assertIn("elapsed_s=250", result.stdout)
        self.assertIn("job listing failed", result.stderr, "a transient API error is reported, then retried")
        self.assertEqual(len(self.requests), 4)
        self.assertTrue(all(path.startswith("/repos/fixture/orbit/actions/runs/4242/jobs?")
                            and "filter=latest" in path for path in self.requests), self.requests)

    def test_a_failed_or_skipped_job_fails_without_waiting_for_the_rest(self):
        for conclusion in ("failure", "skipped", "cancelled"):
            with self.subTest(conclusion=conclusion):
                self.requests.clear()
                result = self.wait([job("Test build"), job("Test shard 1/2", conclusion=conclusion),
                                    job("Test shard 2/2", "in_progress")])
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn(f"Test shard 1/2 concluded {conclusion}", result.stderr)
                self.assertEqual(len(self.requests), 1)

    def test_a_job_that_never_completes_times_out_by_name(self):
        result = self.wait([job("Test build"), job("Test shard 1/2")], timeout="0.3")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("timed out after 0.3 s waiting for Test shard 2/2 (missing)", result.stderr)


if __name__ == "__main__":
    unittest.main()
