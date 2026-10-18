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

FIELDS = (".jobs[] | {name, status, conclusion, started_at, completed_at, html_url,"
          " steps: [.steps[]? | {name, status, conclusion}]}")


def list_jobs(repository, run_id):
    """Return {name: job}, or None on an API error."""
    result = subprocess.run(
        ["gh", "api", "--paginate", "--jq", FIELDS,
         f"repos/{repository}/actions/runs/{run_id}/jobs?filter=latest&per_page=100"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if result.returncode != 0:
        print(f"ci-await: job listing failed (exit {result.returncode}): {result.stderr.strip()}",
              file=sys.stderr)
        return None
    jobs = {}
    for line in result.stdout.splitlines():
        if line.strip():
            job = json.loads(line)
            jobs[job["name"]] = job
    return jobs


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

    deadline = time.monotonic() + args.timeout_seconds
    reported = set()
    while True:
        jobs = list_jobs(repository, run_id)
        if jobs is not None:
            pending = []
            for name in args.jobs:
                job = jobs.get(name, {"status": "missing"})
                status, conclusion, url = job["status"], job.get("conclusion"), job.get("html_url")
                started, completed = job.get("started_at") or "", job.get("completed_at") or ""
                step = next((step for step in job.get("steps") or [] if step.get("name") == args.step), None)
                if status != "completed" and step is not None and step.get("status") == "completed":
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
            if not pending:
                reached = f" or passed step '{args.step}'" if args.step else ""
                print(f"ci-await: all {len(args.jobs)} awaited jobs succeeded{reached}")
                return 0
        if time.monotonic() >= deadline:
            waiting = ", ".join(pending) if jobs is not None else "every job (listing unavailable)"
            print(f"ci-await: timed out after {args.timeout_seconds:g} s waiting for {waiting}",
                  file=sys.stderr)
            return 1
        time.sleep(args.poll_seconds)


if __name__ == "__main__":
    sys.exit(main())
