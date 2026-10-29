#!/usr/bin/env python3
"""Wait for other jobs of this workflow run and fail unless each succeeds.

usage: ci-await-jobs.py --job NAME [--job NAME ...] [--step NAME]
                        [--poll-seconds S] [--timeout-seconds S]

The Check / Clippy / Test job runs this after its own checks, naming the test
build and every nextest shard job. The Check job therefore completes only when
the whole split test gate has, so its elapsed time stays the gate's wall time
and its conclusion stays the gate's result. It reads the run's jobs through
`gh api` (GH_TOKEN, GITHUB_REPOSITORY and GITHUB_RUN_ID from the runner), using
the latest attempt of each job. A job that completes with any conclusion but
success fails the wait at once. API errors are retried until the timeout.

Single-page listings use ETag conditional requests at three-second intervals.
Authenticated 304 responses do not consume GitHub's primary rate limit.
Without an ETag, on API errors, with fewer than 100 primary requests left,
or when pagination is needed, --poll-seconds (default 15) remains the interval.
The server's X-Poll-Interval and Retry-After minimums also apply.

--timeout-seconds is one absolute budget for the whole wait. Each `gh api`
listing runs under the remaining budget, and the poll sleep is clipped to it. A
listing still running at expiry is killed and reaped (at most
CLEANUP_SECONDS more), and the wait fails naming the unavailable listing. A
listing that returns after expiry is not accepted as success: the wait fails
with a timeout even when every awaited job succeeded.

With --step, a job also counts as done once its step of that name succeeds,
while its later steps still run. The shard jobs wait this way for the test
build's archive upload, not for the partition guard after it. A step that
concludes without success, or a job that completes without success before the
step succeeded, fails the wait at once.
"""

import argparse
from datetime import datetime
import json
import os
import subprocess
import sys
import time

# Bound on reaping a listing killed at the deadline, added to --timeout-seconds.
CLEANUP_SECONDS = 5

FIELDS = (".jobs[] | {name, status, conclusion, started_at, completed_at, html_url,"
          " steps: [.steps[]? | {name, status, conclusion}]}")


class ListingTimeout(Exception):
    """The job listing was still running when the wait budget ran out."""


def request_job_listing(arguments, remaining):
    """Run a listing under the remaining deadline, killing and reaping it at expiry."""
    child = subprocess.Popen(
        ["gh", "api", *arguments],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        stdout, stderr = child.communicate(timeout=max(remaining, 0))
    except subprocess.TimeoutExpired:
        child.kill()
        try:
            child.communicate(timeout=CLEANUP_SECONDS)
        except subprocess.TimeoutExpired:
            print(f"ci-await: job listing child {child.pid} did not exit within "
                  f"{CLEANUP_SECONDS} s of being killed", file=sys.stderr)
        raise ListingTimeout from None
    finally:
        if child.poll() is None:
            child.kill()
    status = child.returncode
    # gh exits 1 for HTTP 304 even with --include. It is a successful
    # conditional response, not a transport error that should slow polling.
    status_line = stdout.partition("\n")[0].split()
    if ("--include" in arguments and len(status_line) >= 2
            and status_line[0].startswith("HTTP/") and status_line[1] == "304"):
        status = 0
    if status != 0:
        print(f"ci-await: job listing failed (exit {status}): {stderr.strip()}",
              file=sys.stderr)
    return status, stdout


class JobListingPoller:
    """Cache only a complete latest-attempt listing, never a partial page."""

    def __init__(self, repository, run_id, poll_seconds):
        self.endpoint = f"repos/{repository}/actions/runs/{run_id}/jobs?filter=latest&per_page=100"
        self.fallback_seconds = poll_seconds
        self.poll_seconds = poll_seconds
        self.etag = None
        self.jobs = None
        self.paginated = False
        self.requests = 0
        self.not_modified = 0
        self.server_minimum = 0
        self.primary_remaining = 1000

    def read_job_listing(self, remaining):
        started = time.monotonic()
        self.poll_seconds = max(self.server_minimum, self.fallback_seconds)
        self.requests += 1
        if self.paginated:
            status, stdout = request_job_listing(
                ["--paginate", "--jq", FIELDS, self.endpoint], remaining)
            if status:
                return None
            return {job["name"]: job for line in stdout.splitlines() if line.strip()
                    for job in [json.loads(line)]}
        arguments = ["--include", self.endpoint]
        if self.etag:
            arguments += ["--header", f"If-None-Match: {self.etag}"]
        status, stdout = request_job_listing(arguments, remaining)
        head, _, body = stdout.partition("\n\n")
        status_line = head.splitlines()[0].split() if head else []
        http_status = status_line[1] if len(status_line) >= 2 else ""
        headers = {name.lower(): value.strip() for line in head.splitlines()[1:]
                   for name, separator, value in [line.partition(":")] if separator}
        try:
            self.server_minimum = float(headers.get("x-poll-interval", str(self.server_minimum)))
            minimum = max(self.server_minimum, float(headers.get("retry-after", "0")))
            self.primary_remaining = int(headers.get("x-ratelimit-remaining", str(self.primary_remaining)))
            if self.primary_remaining == 0 and "x-ratelimit-reset" in headers:
                minimum = max(minimum, float(headers["x-ratelimit-reset"]) - time.time())
            if http_status in ("403", "429") and "retry-after" not in headers:
                minimum = max(minimum, 60)
        except ValueError:
            minimum, self.primary_remaining = self.fallback_seconds, 0
        self.poll_seconds = max(minimum, self.fallback_seconds)
        if status:
            return None
        if http_status == "304" and self.jobs is not None:
            self.not_modified += 1
        elif http_status == "200":
            # A first-page ETag says nothing about later pages. Retain the
            # original fully paginated listing and cadence for larger runs.
            if 'rel="next"' in headers.get("link", ""):
                self.paginated = True
                remaining -= time.monotonic() - started
                if remaining <= 0:
                    raise ListingTimeout
                return self.read_job_listing(remaining)
            payload = json.loads(body)
            self.jobs = {job["name"]: job for job in payload["jobs"]}
            self.etag = headers.get("etag")
        else:
            print(f"ci-await: job listing returned unexpected HTTP status {http_status}", file=sys.stderr)
            return None
        if self.etag and self.primary_remaining >= 100:
            self.poll_seconds = max(minimum, min(3, self.fallback_seconds))
        return self.jobs


def elapsed(started, completed):
    try:
        start = datetime.fromisoformat(started.replace("Z", "+00:00"))
        end = datetime.fromisoformat(completed.replace("Z", "+00:00"))
    except (AttributeError, ValueError):
        return "unknown"
    return str(int((end - start).total_seconds()))


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--job", action="append", required=True, dest="jobs")
    parser.add_argument("--step")
    parser.add_argument("--poll-seconds", type=float, default=15)
    parser.add_argument("--timeout-seconds", type=float, default=2400)
    args = parser.parse_args()
    repository = os.environ["GITHUB_REPOSITORY"]
    run_id = os.environ["GITHUB_RUN_ID"]
    poller = JobListingPoller(repository, run_id, args.poll_seconds)
    try:
        return await_workflow_jobs(args, poller)
    finally:
        print(f"ci-await-api: listings={poller.requests} not_modified={poller.not_modified} "
              f"paginated={str(poller.paginated).lower()}")


def await_workflow_jobs(args, poller):
    """Keep the whole gate's original success, failure and absolute-deadline rules."""

    deadline = time.monotonic() + args.timeout_seconds
    reported = set()
    pending = []
    while True:
        remaining = deadline - time.monotonic()
        jobs = None
        blocked = False
        listing = "listing unavailable"
        if remaining > 0:
            try:
                jobs = poller.read_job_listing(remaining)
            except ListingTimeout:
                blocked = True
                listing = "job listing still running at the deadline, killed"
        if jobs is not None:
            pending = []
            for name in args.jobs:
                job = jobs.get(name, {"status": "missing"})
                status, conclusion, url = job["status"], job.get("conclusion"), job.get("html_url")
                started, completed = job.get("started_at") or "", job.get("completed_at") or ""
                step = next((step for step in job.get("steps") or [] if step.get("name") == args.step), None)
                if step is not None and step.get("status") == "completed":
                    if step.get("conclusion") != "success":
                        print(f"ci-await: {name} step '{args.step}' concluded "
                              f"{step.get('conclusion') or 'without a conclusion'}: {url}", file=sys.stderr)
                        return 1
                    if name not in reported:
                        reported.add(name)
                        print(f"ci-await: {name} step '{args.step}' succeeded started={started}")
                    continue
                if status != "completed":
                    pending.append(f"{name} ({status})")
                    continue
                if conclusion != "success":
                    print(f"ci-await: {name} concluded {conclusion or 'without a conclusion'}: {url}",
                          file=sys.stderr)
                    return 1
                if name not in reported:
                    reported.add(name)
                    print(f"ci-await: {name} succeeded started={started} completed={completed} "
                          f"elapsed_s={elapsed(started, completed)}")
            if not pending and time.monotonic() < deadline:
                reached = f" or passed step '{args.step}'" if args.step else ""
                print(f"ci-await: all {len(args.jobs)} awaited jobs succeeded{reached}")
                return 0
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            if jobs is None and pending:
                waiting = ", ".join(pending)
                if blocked:
                    waiting += f" (latest {listing})"
            elif jobs is None:
                waiting = f"every job ({listing})"
            elif pending:
                waiting = ", ".join(pending)
            else:
                waiting = (f"the final job listing, which reported all {len(args.jobs)} "
                           "awaited jobs successful only after the deadline")
            print(f"ci-await: timed out after {args.timeout_seconds:g} s waiting for {waiting}",
                  file=sys.stderr)
            return 1
        time.sleep(min(poller.poll_seconds, remaining))


if __name__ == "__main__":
    sys.exit(main())
