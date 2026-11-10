#!/usr/bin/env python3
"""Exercise the Check job's wait for the test jobs against a fixture jobs API."""

import http.server
import hashlib
import json
import os
from pathlib import Path
import self_test_hang
import shutil
import sys
import tempfile
import threading
import time
import unittest

SCRIPTS = Path(__file__).resolve().parent
EXPECTED = ["Test build", "Test shard 1/2", "Test shard 2/2"]


def job(name, status="completed", conclusion="success", steps=()):
    return {"name": name, "status": status, "conclusion": conclusion if status == "completed" else None,
            "started_at": "2026-10-18T10:00:00Z", "completed_at": "2026-10-18T10:04:10Z",
            "html_url": f"https://example.invalid/{name.replace(' ', '-')}",
            "steps": [{"name": step, "status": step_status, "conclusion": step_conclusion}
                      for step, step_status, step_conclusion in steps]}


UPLOAD = "Upload nextest archive"


class AwaitJobsTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="ci-await-jobs-", dir=scratch)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.responses = []
        self.requests = []
        self.request_headers = []
        self.timed_response = None
        self.response_headers = {}
        self.pages = None
        self.artifacts = None
        # Raw HTTP 200 job-listing body, when set, replaces the jobs JSON.
        self.raw_job_body = None
        # Optional (page, elapsed) -> (status, headers, JSON payload or None).
        self.artifact_reply = None
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                fixture.requests.append(self.path)
                fixture.request_headers.append(dict(self.headers))
                if "/artifacts" in self.path:
                    elapsed = float(self.headers.get("X-Fixture-Time", "0"))
                    if fixture.artifact_reply:
                        page = 2 if "&page=2" in self.path else 1
                        status, headers, payload = fixture.artifact_reply(page, elapsed)
                    else:
                        produced = fixture.artifacts
                        if callable(produced):
                            produced = produced(elapsed)
                        status, headers = (503, {}) if produced is None else (200, {})
                        payload = None if produced is None else {"total_count": len(produced),
                                                                 "artifacts": produced}
                    body = b"" if payload is None else json.dumps(payload).encode()
                    self.send_response(status)
                    self.send_header("Content-Type", "application/json")
                    for name, value in headers.items():
                        self.send_header(name, value)
                    self.end_headers()
                    self.wfile.write(body)
                    return
                if fixture.raw_job_body is not None:
                    raw = fixture.raw_job_body
                    body = raw.encode() if isinstance(raw, str) else raw
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.end_headers()
                    self.wfile.write(body)
                    return
                response = (fixture.timed_response(float(self.headers.get("X-Fixture-Time", "0")))
                            if fixture.timed_response else
                            fixture.responses.pop(0) if len(fixture.responses) > 1 else fixture.responses[0])
                second_page = "&page=2" in self.path
                if fixture.pages:
                    response = fixture.pages[1 if second_page else 0]
                body = b"" if response is None else json.dumps({"total_count": len(response),
                                                                 "jobs": response}).encode()
                etag = '"' + hashlib.sha256(body).hexdigest() + '"'
                unchanged = fixture.timed_response and self.headers.get("If-None-Match") == etag
                self.send_response(503 if response is None else 304 if unchanged else 200)
                self.send_header("Content-Type", "application/json")
                if fixture.timed_response:
                    self.send_header("ETag", etag)
                for name, value in fixture.response_headers.items():
                    self.send_header(name, value)
                if fixture.pages and not second_page:
                    self.send_header("Link", f'<http://127.0.0.1:{server.server_port}/jobs?filter=latest&per_page=100&page=2>; rel="next"')
                self.end_headers()
                if not unchanged:
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
import os, signal, sys
blocked_pid_file = os.environ.get("CI_AWAIT_BLOCK_PID_FILE")
if blocked_pid_file:
    # A listing that never answers: record the pid, then wait to be killed.
    with open(blocked_pid_file, "w") as pid_file:
        pid_file.write(str(os.getpid()))
    while True:
        signal.pause()
args = ["http://127.0.0.1:{server.server_port}/" + arg if arg.startswith("repos/") else arg
        for arg in sys.argv[1:]]
if "CI_AWAIT_FIXTURE_TIME" in os.environ:
    args += ["--header", "X-Fixture-Time: " + os.environ["CI_AWAIT_FIXTURE_TIME"]]
os.execv({real_gh!r}, [{real_gh!r}, *args])
''')
        (bin_dir / "gh").chmod(0o755)
        self.env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}", GH_TOKEN="offline-token",
                        GH_CONFIG_DIR=str(self.root / "gh-config"), GITHUB_REPOSITORY="fixture/orbit",
                        GITHUB_RUN_ID="4242")
        # Drive the public CLI on a virtual clock: the observed 15-second
        # polling delay is deterministic, without sleeps or host load.
        self.clock_launcher = self.root / "clock.py"
        self.clock_launcher.write_text('''import os, runpy, sys, time, types
from pathlib import Path
elapsed = 0.0
def sleep(seconds):
    global elapsed
    elapsed += seconds
    os.environ["CI_AWAIT_FIXTURE_TIME"] = str(elapsed)
sleep(0)
sys.argv = sys.argv[1:]
namespace = runpy.run_path(sys.argv[0])
namespace["main"].__globals__["time"] = types.SimpleNamespace(
    monotonic=lambda: elapsed, sleep=sleep, time=time.time)
try:
    raise SystemExit(namespace["main"]())
finally:
    Path(os.environ["CI_AWAIT_CLOCK_OUTPUT"]).write_text(str(elapsed))
''')

    def wait(self, *responses, timeout="10", jobs=EXPECTED, step=None, poll="0.01", virtual=False,
             artifact_names=()):
        self.responses[:] = responses
        arguments = [argument for name in jobs for argument in ("--job", name)]
        if step is not None:
            arguments += ["--step", step]
        for name in artifact_names:
            arguments += ["--artifact", name]
        launcher = [str(self.clock_launcher)] if virtual else []
        self.env["CI_AWAIT_CLOCK_OUTPUT"] = str(self.root / "elapsed")
        return self_test_hang.run(
            [sys.executable, *launcher, str(SCRIPTS / "ci-await-jobs.py"), *arguments,
             "--poll-seconds", poll, "--timeout-seconds", timeout],
            step="ci-await-jobs",
            env=self.env, text=True, capture_output=True)

    def test_dependency_record_is_bound_to_the_actual_failed_dependency(self):
        self.env.update(GITHUB_RUN_ATTEMPT="1", GITHUB_SHA="a" * 40,
                        ORBIT_DEPENDENCY_PRODUCER_SHA="c" * 40,
                        ORBIT_DEPENDENCY_EXECUTION="isolated-source-v1",
                        GITHUB_JOB="check", ORBIT_DEPENDENCY_JOB_NAME="waiter",
                        ORBIT_DEPENDENCY_STEP="Wait", GITHUB_WORKFLOW_REF="fixture/orbit/.github/workflows/ci.yml@refs/pull/7/merge")
        source = job("waiter", "in_progress", steps=[("Wait", "in_progress", None)])
        source.update(id=42, run_id=4242, run_attempt=1, head_sha="b" * 40)
        source["steps"][0]["number"] = 3
        dependency = job(EXPECTED[0], conclusion="failure")
        dependency.update(id=43, run_id=4242, run_attempt=1, head_sha="b" * 40)
        result = self.wait([source, dependency], jobs=[EXPECTED[0]])
        self.assertEqual(result.returncode, 1)
        records = [json.loads(line.split("orbit-dependency-evidence:", 1)[1])
                   for line in result.stdout.splitlines() if line.startswith("orbit-dependency-evidence:")]
        self.assertEqual(len(records), 1, result.stdout + result.stderr)
        record = records[0]
        self.assertEqual(record["schema_version"], 1)
        self.assertEqual(record["kind"], "dependency_failed")
        self.assertEqual(record["source"]["job_id"], 42)
        self.assertEqual(record["source"]["step_number"], 3)
        self.assertEqual(record["dependencies"][0]["job_id"], 43)
        self.assertEqual(record["dependencies"][0]["conclusion"], "failure")

    def test_no_dependency_authority_for_api_timeout_own_or_unknown_failures(self):
        self.env.update(GITHUB_RUN_ATTEMPT="1", GITHUB_SHA="a" * 40,
                        ORBIT_DEPENDENCY_PRODUCER_SHA="c" * 40,
                        ORBIT_DEPENDENCY_EXECUTION="isolated-source-v1",
                        GITHUB_JOB="check", ORBIT_DEPENDENCY_JOB_NAME="waiter",
                        ORBIT_DEPENDENCY_STEP="Wait", GITHUB_WORKFLOW_REF="fixture/orbit/.github/workflows/ci.yml@refs/pull/7/merge")
        for case in ("api", "timeout", "own", "unknown", "skipped", "missing_identity"):
            with self.subTest(case=case):
                source = job("waiter", "in_progress", steps=[("Wait", "in_progress", None)])
                source.update(id=42, run_id=4242, run_attempt=1, head_sha="b" * 40)
                source["steps"][0]["number"] = 3
                dependency = job(EXPECTED[0], conclusion="failure")
                dependency.update(id=43, run_id=4242, run_attempt=1, head_sha="b" * 40)
                if case == "own":
                    source["status"], source["conclusion"] = "completed", "failure"
                if case == "timeout":
                    dependency["status"], dependency["conclusion"] = "in_progress", None
                if case == "unknown":
                    dependency["conclusion"] = None
                if case == "skipped":
                    dependency["conclusion"] = "skipped"
                if case == "missing_identity":
                    del dependency["id"]
                result = self.wait(None if case == "api" else [source, dependency],
                                   jobs=[EXPECTED[0]], timeout="2", poll="1", virtual=True)
                self.assertEqual(result.returncode, 1)
                self.assertNotIn("orbit-dependency-evidence:", result.stdout)

    def test_unchanged_jobs_do_not_add_a_full_poll_interval_to_either_handoff(self):
        for step in (None, UPLOAD):
            with self.subTest(step=step):
                self.requests.clear()
                self.request_headers.clear()
                def response(elapsed):
                    if step:
                        return [job("Test build", "in_progress", steps=[
                            (step, "completed" if elapsed >= 16 else "in_progress",
                             "success" if elapsed >= 16 else None)])]
                    return [job("Test build", "completed" if elapsed >= 16 else "in_progress")]
                self.timed_response = response
                result = self.wait(timeout="60", jobs=["Test build"], step=step,
                                   poll="15", virtual=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertLessEqual(float((self.root / "elapsed").read_text()), 19,
                                     "the observed chained CI handoffs must not each add a 15 s poll")
                self.assertTrue(any("If-None-Match" in headers for headers in self.request_headers),
                                "unchanged polls must use conditional requests, preserving API budget")
                self.assertEqual(result.stdout.count("ci-await: Test build"), 1, result.stdout)

        # A cached in-progress listing must never hide a later negative
        # conclusion at either of the same handoff boundaries.
        for step in (None, UPLOAD):
            for conclusion in ("failure", "skipped", "cancelled"):
                with self.subTest(step=step, conclusion=conclusion):
                    def response(elapsed):
                        if step:
                            return [job("Test build", "in_progress", steps=[
                                (step, "completed" if elapsed >= 16 else "in_progress",
                                 conclusion if elapsed >= 16 else None)])]
                        return [job("Test build", "completed" if elapsed >= 16 else "in_progress",
                                    conclusion=conclusion)]
                    self.timed_response = response
                    result = self.wait(timeout="60", jobs=["Test build"], step=step,
                                       poll="15", virtual=True)
                    self.assertEqual(result.returncode, 1, result.stdout)
                    self.assertIn(f"concluded {conclusion}", result.stderr)

    def test_conditional_polling_honors_server_limits_and_the_absolute_deadline(self):
        self.timed_response = lambda elapsed: [job("Test build", "in_progress")]
        for headers, interval in (({}, 3), ({"X-Poll-Interval": "7"}, 7),
                                  ({"Retry-After": "9"}, 9),
                                  ({"X-RateLimit-Remaining": "99"}, 15)):
            with self.subTest(headers=headers):
                self.response_headers = headers
                self.request_headers.clear()
                result = self.wait(timeout="16", jobs=["Test build"], poll="15", virtual=True)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("timed out after 16 s waiting for Test build (in_progress)", result.stderr)
                self.assertNotIn("job listing failed", result.stderr,
                                 "gh's HTTP 304 exit must reuse the complete cached listing")
                self.assertEqual(float((self.root / "elapsed").read_text()), 16,
                                 "304 responses and server minimums cannot extend the absolute deadline")
                times = [float(headers["X-Fixture-Time"]) for headers in self.request_headers]
                self.assertEqual(times[1], interval, times)
                self.assertEqual(times, list(range(0, 16, interval)),
                                 "unchanged HTTP 304 listings must retain the interval")

    def test_a_first_page_etag_cannot_hide_a_failed_job_on_a_later_page(self):
        self.pages = [[job("Test build")], [job("Test shard 1/2", conclusion="failure")]]
        self.response_headers = {"ETag": '"first-page"'}
        result = self.wait([job("unused")], jobs=["Test build", "Test shard 1/2"])
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("Test shard 1/2 concluded failure", result.stderr)
        self.assertTrue(any("&page=2" in path for path in self.requests), self.requests)
        self.assertIn("paginated=true", result.stdout)

    def test_paginated_listing_honors_server_backoff_without_hiding_later_pages(self):
        building = [job("Test build", "in_progress")]
        self.pages = [building, building]
        self.response_headers = {"Retry-After": "60"}
        result = self.wait([job("unused")], timeout="31", jobs=["Test build"],
                           poll="15", virtual=True)
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("timed out after 31 s waiting for Test build (in_progress)", result.stderr)
        self.assertEqual(float((self.root / "elapsed").read_text()), 31,
                         "the server minimum cannot extend the absolute deadline")
        times = [float(headers["X-Fixture-Time"]) for headers in self.request_headers]
        self.assertEqual(times, [0, 0, 0], times)

        # A backoff header must not hide a later-page negative conclusion.
        self.requests.clear()
        self.request_headers.clear()
        self.pages = [[job("Test build")], [job("Test shard 1/2", conclusion="failure")]]
        self.response_headers = {"Retry-After": "60"}
        result = self.wait([job("unused")], jobs=["Test build", "Test shard 1/2"])
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("Test shard 1/2 concluded failure", result.stderr)
        self.assertTrue(any("&page=2" in path for path in self.requests), self.requests)
        self.assertIn("paginated=true", result.stdout)

    def test_waits_until_every_named_job_succeeds(self):
        building = [job("Test build", "in_progress"), job("Test shard 1/2", "queued"),
                    job("Test shard 2/2", "queued"), job("Check / Clippy / Test", "in_progress")]
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

        # A shard starts once the build's upload step succeeds, while the
        # build's partition guard still runs.
        self.requests.clear()
        uploading = [job("Test build", "in_progress", steps=[("Build", "completed", "success"),
                                                            (UPLOAD, "in_progress", None)])]
        uploaded = [job("Test build", "in_progress", steps=[("Build", "completed", "success"),
                                                           (UPLOAD, "completed", "success")])]
        result = self.wait(uploading, uploaded, jobs=["Test build"], step=UPLOAD)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"ci-await: Test build step '{UPLOAD}' succeeded", result.stdout)
        self.assertEqual(len(self.requests), 2)

    def test_a_failed_or_skipped_job_fails_without_waiting_for_the_rest(self):
        for conclusion in ("failure", "skipped", "cancelled"):
            with self.subTest(conclusion=conclusion):
                self.requests.clear()
                result = self.wait([job("Test build"), job("Test shard 1/2", conclusion=conclusion),
                                    job("Test shard 2/2", "in_progress")])
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn(f"Test shard 1/2 concluded {conclusion}", result.stderr)
                self.assertEqual(len(self.requests), 1)
        # A shard must not run against a build that failed or skipped its
        # upload, nor wait out its timeout for an upload that never comes.
        for build in (job("Test build", "in_progress", steps=[(UPLOAD, "completed", "skipped")]),
                      job("Test build", conclusion="failure", steps=[(UPLOAD, "completed", "skipped")]),
                      job("Test build", conclusion="failure", steps=[(UPLOAD, "queued", None)])):
            with self.subTest(build=build):
                self.requests.clear()
                result = self.wait([build], jobs=["Test build"], step=UPLOAD)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("Test build", result.stderr)
                self.assertIn("concluded", result.stderr)
                self.assertEqual(len(self.requests), 1)

    def test_an_absent_job_fails_without_waiting_out_the_timeout(self):
        # Run 42670994582 attempt 1 listed no Test build job. The old wait
        # treated that omission as pending and sat out the full timeout.
        result = self.wait([job("Test build"), job("Test shard 1/2")], timeout="30", poll="15",
                           virtual=True, artifact_names=["nextest-archive-1"])
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("Test shard 2/2 is absent from the run", result.stderr)
        self.assertNotIn("timed out", result.stderr)
        self.assertEqual(len(self.requests), 1, self.requests)
        self.assertFalse(any("/artifacts" in path for path in self.requests), self.requests)
        self.assertEqual(float((self.root / "elapsed").read_text()), 0)

        self.requests.clear()
        failed = job("Test build", conclusion="failure", steps=[(UPLOAD, "completed", "failure")])
        result = self.wait([failed], jobs=["Test build"], step=UPLOAD, virtual=True, timeout="30",
                           artifact_names=["nextest-archive-1"])
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("concluded failure", result.stderr)
        self.assertFalse(any("/artifacts" in path for path in self.requests), self.requests)
        self.assertEqual(float((self.root / "elapsed").read_text()), 0)

    def test_a_blocked_listing_is_killed_at_the_deadline(self):
        pid_file = self.root / "gh.pid"
        self.env["CI_AWAIT_BLOCK_PID_FILE"] = str(pid_file)
        result = self.wait([job("Test build")], timeout="2")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("timed out after 2 s waiting for every job (job listing still running at the "
                      "deadline, killed)", result.stderr)
        self.assertTrue(pid_file.exists(), "the listing child started before the deadline")
        with self.assertRaises(ProcessLookupError, msg="the timed-out listing child is reaped"):
            os.kill(int(pid_file.read_text()), 0)

    def test_a_listing_after_the_deadline_is_not_a_success(self):
        everything = [job("Test build"), job("Test shard 1/2"), job("Test shard 2/2")]
        uploaded = [job("Test build", "in_progress", steps=[(UPLOAD, "completed", "success")])]
        for responses, kwargs in ((everything, {}), (uploaded, {"jobs": ["Test build"], "step": UPLOAD})):
            with self.subTest(kwargs=kwargs):
                result = self.wait(responses, timeout="0", **kwargs)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("timed out after 0 s", result.stderr)
                self.assertNotIn("awaited jobs succeeded", result.stdout)

    def test_the_poll_sleep_does_not_outlast_the_deadline(self):
        started = time.monotonic()
        result = self.wait([job("Test build", "in_progress")], timeout="3", poll="60",
                           jobs=["Test build"])
        # Loose bound: unclipped, the sleep alone takes the full 60 s poll.
        self.assertLess(time.monotonic() - started, 30)
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("timed out after 3 s waiting for Test build (in_progress)", result.stderr)

    def test_archive_parts_must_be_listed_before_the_wait_returns(self):
        uploaded = [job("Test build", "in_progress", steps=[(UPLOAD, "completed", "success")])]
        names = ["nextest-archive-1", "nextest-archive-2"]

        def not_ready(elapsed):
            if elapsed < 0.01:
                return [{"name": "nextest-archive-1", "expired": False, "size_in_bytes": 0},
                        {"name": "nextest-archive-2", "expired": True, "size_in_bytes": 8}]
            return [{"name": "nextest-archive-1", "expired": False, "size_in_bytes": 8},
                    {"name": "nextest-archive-2", "expired": False, "size_in_bytes": 8}]

        self.artifacts = not_ready
        result = self.wait(uploaded, jobs=["Test build"], step=UPLOAD, virtual=True,
                           poll="0.01", timeout="10", artifact_names=names)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertGreater(float((self.root / "elapsed").read_text()), 0,
                           "a zero-size or expired part must keep the wait open")
        self.assertIn("with archive parts nextest-archive-1, nextest-archive-2", result.stdout)
        self.assertIn("awaited jobs succeeded", result.stdout)

        def never(_elapsed):
            return [{"name": "nextest-archive-1", "expired": False, "size_in_bytes": 8}]

        self.artifacts = never
        self.requests.clear()
        result = self.wait(uploaded, jobs=["Test build"], step=UPLOAD, virtual=True,
                           poll="1", timeout="5", artifact_names=names)
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("timed out after 5 s waiting for archive parts nextest-archive-2", result.stderr)
        self.assertNotIn("awaited jobs succeeded", result.stdout)

        calls = {"n": 0}

        def flaky(_elapsed):
            calls["n"] += 1
            if calls["n"] == 1:
                return None
            return [{"name": name, "expired": False, "size_in_bytes": 8} for name in names]

        self.artifacts = flaky
        result = self.wait(uploaded, jobs=["Test build"], step=UPLOAD, virtual=True,
                           poll="0.01", timeout="10", artifact_names=names)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertGreater(float((self.root / "elapsed").read_text()), 0)
        self.assertIn("artifact listing failed", result.stderr)
        self.assertIn("with archive parts nextest-archive-1, nextest-archive-2", result.stdout)

    def artifact_request_times(self):
        return [float(headers["X-Fixture-Time"]) for path, headers in zip(self.requests, self.request_headers)
                if "/artifacts" in path]

    def test_artifact_listing_backoff_is_honored_before_the_next_poll(self):
        # Completed job with an ETag, then an artifact listing throttled by
        # the server. The next artifact poll must wait out the server minimum;
        # the 7 s absolute deadline still ends the wait.
        self.timed_response = lambda elapsed: [job("Test build")]
        cases = (({"Retry-After": "60", "X-RateLimit-Remaining": "0"}, 429),
                 ({"Retry-After": "60"}, 200),
                 ({"X-Poll-Interval": "30"}, 200),
                 ({}, 403),
                 ({}, 429),
                 ({"X-RateLimit-Remaining": "0", "X-RateLimit-Reset": str(int(time.time()) + 120)}, 403))
        for headers, status in cases:
            with self.subTest(headers=headers, status=status):
                self.requests.clear()
                self.request_headers.clear()
                self.artifact_reply = lambda page, elapsed, headers=headers, status=status: (
                    status, headers, {"total_count": 0, "artifacts": []})
                result = self.wait(timeout="7", jobs=["Test build"], poll="15", virtual=True,
                                   artifact_names=["nextest-archive-1"])
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("timed out after 7 s waiting for archive parts", result.stderr)
                self.assertEqual(float((self.root / "elapsed").read_text()), 7,
                                 "the backoff minimum cannot extend the absolute deadline")
                self.assertEqual(self.artifact_request_times(), [0], self.requests)

    def test_paginated_artifact_listing_folds_backoff_from_every_page(self):
        self.timed_response = lambda elapsed: [job("Test build")]

        def page(names):
            return [{"name": name, "expired": False, "size_in_bytes": 8} for name in names]

        other = [f"other-{number}" for number in range(100)]
        cases = (({"Retry-After": "60"}, 200, {}, 200),
                 ({}, 200, {"Retry-After": "60"}, 429))
        for first_headers, first_status, second_headers, second_status in cases:
            with self.subTest(first=first_headers, second=second_headers, second_status=second_status):
                self.requests.clear()
                self.request_headers.clear()

                def reply(number, elapsed):
                    if number == 1:
                        return first_status, first_headers, {"total_count": 101, "artifacts": page(other)}
                    return second_status, second_headers, {"total_count": 101,
                                                           "artifacts": page(["other-100"])}

                self.artifact_reply = reply
                result = self.wait(timeout="7", jobs=["Test build"], poll="15", virtual=True,
                                   artifact_names=["nextest-archive-1"])
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("timed out after 7 s", result.stderr)
                self.assertTrue(any("&page=2" in path for path in self.requests), self.requests)
                self.assertEqual(self.artifact_request_times(), [0, 0], self.requests)
                self.assertEqual(float((self.root / "elapsed").read_text()), 7)

    def test_artifact_polling_uses_fallback_interval(self):
        # Completed jobs carry an ETag, so the job listing drops to a 3 s
        # fast path. Artifact requests are unconditional primary-quota GETs
        # with no ETag fast path, so an API error, a still-missing part, a
        # paginated listing, or fewer than 100 remaining primary requests must
        # wait out --poll-seconds (15 s) before the next artifact cycle; a
        # shorter deadline still ends the wait there (ORB-23867).
        self.timed_response = lambda elapsed: [job("Test build")]

        def available(names):
            return [{"name": name, "expired": False, "size_in_bytes": 8} for name in names]

        other100 = [f"other-{number}" for number in range(100)]

        def error_reply(_page, _elapsed):
            return 503, {}, None

        def missing_reply(_page, _elapsed):
            return 200, {}, {"total_count": 1, "artifacts": available(["other"])}

        def low_quota_reply(_page, _elapsed):
            return 200, {"X-RateLimit-Remaining": "5"}, {
                "total_count": 1, "artifacts": available(["other"])}

        def paginated_reply(number, _elapsed):
            if number == 1:
                return 200, {}, {"total_count": 101, "artifacts": available(other100)}
            return 200, {}, {"total_count": 101, "artifacts": available(["other-100"])}

        cases = (("error", error_reply, [0, 15]),
                 ("missing", missing_reply, [0, 15]),
                 ("low-quota", low_quota_reply, [0, 15]),
                 ("paginated", paginated_reply, [0, 0, 15, 15]))
        for name, reply, expected in cases:
            with self.subTest(case=name):
                self.requests.clear()
                self.request_headers.clear()
                self.artifact_reply = reply
                result = self.wait(timeout="20", jobs=["Test build"], poll="15", virtual=True,
                                   artifact_names=["nextest-archive-1"])
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("timed out after 20 s waiting for archive parts", result.stderr)
                self.assertEqual(float((self.root / "elapsed").read_text()), 20,
                                 "a shorter overall timeout still exits at its deadline")
                self.assertEqual(self.artifact_request_times(), expected, self.requests)

        # A shorter deadline still ends the wait there instead of waiting out
        # the fallback.
        with self.subTest(case="short-timeout"):
            self.requests.clear()
            self.request_headers.clear()
            self.artifact_reply = error_reply
            result = self.wait(timeout="7", jobs=["Test build"], poll="15", virtual=True,
                               artifact_names=["nextest-archive-1"])
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("timed out after 7 s waiting for archive parts", result.stderr)
            self.assertEqual(float((self.root / "elapsed").read_text()), 7)
            self.assertEqual(self.artifact_request_times(), [0], self.requests)

    def test_malformed_single_page_job_listing_retries_until_the_deadline(self):
        bodies = (
            "<html>not-json",
            "{",
            "[]",
            "null",
            '{"total_count": 1}',
            '{"total_count": 1, "jobs": [{"status": "completed"}]}',
        )
        try:
            for body in bodies:
                with self.subTest(body=body):
                    self.requests.clear()
                    self.raw_job_body = body
                    result = self.wait(timeout="30", poll="15", virtual=True)
                    combined = result.stdout + result.stderr
                    self.assertNotIn("Traceback", combined)
                    self.assertEqual(result.returncode, 1, combined)
                    self.assertIn("timed out after 30 s waiting for every job", result.stderr)
                    self.assertGreaterEqual(len(self.requests), 2, self.requests)
                    self.assertEqual(float((self.root / "elapsed").read_text()), 30)
        finally:
            self.raw_job_body = None


if __name__ == "__main__":
    unittest.main()
