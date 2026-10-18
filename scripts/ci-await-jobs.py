#!/usr/bin/env python3
"""Wait for other jobs of this workflow run and fail unless each succeeds.

usage: ci-await-jobs.py --job NAME [--job NAME ...]
                        [--poll-seconds S] [--timeout-seconds S]

The Check / Clippy / Test job runs this after its own checks, naming the test
build and every nextest shard job. The Check job therefore completes only when
the whole split test gate has, so its elapsed time stays the gate's wall time
and its conclusion stays the gate's result. It reads the run's jobs through
`gh api` (GH_TOKEN, GITHUB_REPOSITORY and GITHUB_RUN_ID from the runner), using
the latest attempt of each job. A job that completes with any conclusion but
success fails the wait at once. API errors are retried until the timeout.
"""

import argparse
from datetime import datetime
import os
import subprocess
import sys
import time

FIELDS = ".jobs[] | [.name, .status, .conclusion // \"\", .started_at // \"\", .completed_at // \"\", .html_url // \"\"] | @tsv"


def list_jobs(repository, run_id):
    """Return {name: (status, conclusion, started, completed, url)}, or None on an API error."""
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
        name, *rest = line.split("\t")
        jobs[name] = tuple(rest)
    return jobs


def elapsed(started, completed):
    try:
        start = datetime.fromisoformat(started.replace("Z", "+00:00"))
        end = datetime.fromisoformat(completed.replace("Z", "+00:00"))
    except ValueError:
        return "unknown"
    return str(int((end - start).total_seconds()))


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--job", action="append", required=True, dest="jobs")
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
                status, conclusion, started, completed, url = jobs.get(name, ("missing",) + ("",) * 4)
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
                print(f"ci-await: all {len(args.jobs)} awaited jobs succeeded")
                return 0
        if time.monotonic() >= deadline:
            waiting = ", ".join(pending) if jobs is not None else "every job (listing unavailable)"
            print(f"ci-await: timed out after {args.timeout_seconds:g} s waiting for {waiting}",
                  file=sys.stderr)
            return 1
        time.sleep(args.poll_seconds)


if __name__ == "__main__":
    sys.exit(main())
