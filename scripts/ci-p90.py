#!/usr/bin/env python3
"""Measure the Check / Clippy / Test p90 over the latest completed PR CI runs.

Usage: ci-p90.py [--limit N] [--budget-seconds S] [--from-json FILE]

By default it reads GitHub through the authenticated `gh` CLI in this
checkout. `--from-json` instead reads a saved list of runs, each
`{"databaseId" or "run_id", "headSha" or "reported_head_sha", "jobs":
[{"name", "startedAt"/"started_at", "completedAt"/"completed_at",
"conclusion"}]}`, so a measurement can be replayed offline.

An executed run is one whose Check / Clippy / Test job started and was
not skipped. Skipped and cancelled runs are reported as gaps, never
counted as fast runs. The p90 is nearest-rank over the executed Check
durations (job start to completion): rank ceil(0.9 * n) of the ascending
durations. Exit status is 0 when the p90 is at most the budget, 1 when
it exceeds it, and 2 when no executed run is available. This measures a
rolling cohort; it does not by itself attribute a change's effect.
"""

import argparse
import json
import math
import subprocess
import sys
from datetime import datetime

CHECK = "Check / Clippy / Test"
AWAITED = ("Test build", "Test shard 1/3", "Test shard 2/3", "Test shard 3/3")


def parse_time(value):
    if not value:
        return None
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def job_field(job, *names):
    for name in names:
        if job.get(name) is not None:
            return job[name]
    return None


def job_duration(job):
    started = parse_time(job_field(job, "startedAt", "started_at"))
    completed = parse_time(job_field(job, "completedAt", "completed_at"))
    if started is None or completed is None:
        return None
    return (completed - started).total_seconds()


def nearest_rank_p90(durations):
    """Return (rank, value) for the nearest-rank p90 of ascending durations."""
    ordered = sorted(durations)
    rank = math.ceil(0.9 * len(ordered))
    return rank, ordered[rank - 1]


def summarise_run(run):
    jobs = {job["name"]: job for job in run.get("jobs", [])}
    check = jobs.get(CHECK, {})
    conclusion = check.get("conclusion")
    duration = job_duration(check) if check else None
    executed = conclusion not in (None, "skipped", "cancelled") and duration is not None and duration > 0
    last = None
    last_completed = None
    for name in AWAITED:
        completed = parse_time(job_field(jobs.get(name, {}), "completedAt", "completed_at"))
        if completed is not None and (last_completed is None or completed > last_completed):
            last, last_completed = name, completed
    return {
        "run_id": run.get("databaseId", run.get("run_id")),
        "head_sha": str(run.get("headSha", run.get("reported_head_sha", "")))[:9],
        "check_conclusion": conclusion,
        "check_seconds": duration if executed else None,
        "executed": executed,
        "last_awaited_job": last if executed else None,
        "job_seconds": {name: job_duration(jobs[name]) for name in AWAITED if name in jobs},
    }


def load_runs(args):
    if args.from_json:
        with open(args.from_json, encoding="utf-8") as handle:
            return json.load(handle)
    listing = json.loads(subprocess.run(
        ["gh", "run", "list", "--workflow", "ci.yml", "--event", "pull_request",
         "--status", "completed", "--limit", str(args.limit),
         "--json", "databaseId,headSha,conclusion,createdAt"],
        capture_output=True, text=True, check=True).stdout)
    runs = []
    for entry in listing:
        view = json.loads(subprocess.run(
            ["gh", "run", "view", str(entry["databaseId"]), "--json", "jobs,headSha"],
            capture_output=True, text=True, check=True).stdout)
        view.setdefault("databaseId", entry["databaseId"])
        view.setdefault("headSha", entry.get("headSha", ""))
        runs.append(view)
    return runs


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--limit", type=int, default=20,
                        help="latest completed PR CI runs to read (default 20)")
    parser.add_argument("--budget-seconds", type=float, default=600)
    parser.add_argument("--from-json", help="replay a saved run list instead of calling gh")
    args = parser.parse_args()

    summaries = [summarise_run(run) for run in load_runs(args)]
    for summary in summaries:
        print(json.dumps(summary, sort_keys=True))
    executed = [s for s in summaries if s["executed"]]
    skipped = [s for s in summaries if s["check_conclusion"] == "skipped"]
    cancelled = [s for s in summaries if s["check_conclusion"] == "cancelled"]
    other = [s for s in summaries if not s["executed"]
             and s["check_conclusion"] not in ("skipped", "cancelled")]
    print(f"ci-p90: runs={len(summaries)} executed={len(executed)} "
          f"skipped={len(skipped)} cancelled={len(cancelled)} other_gaps={len(other)}")
    if not executed:
        print("ci-p90: no executed Check job in this cohort", file=sys.stderr)
        return 2
    durations = [s["check_seconds"] for s in executed]
    rank, p90 = nearest_rank_p90(durations)
    ordered = sorted(durations)
    median = ordered[len(ordered) // 2] if len(ordered) % 2 else (
        ordered[len(ordered) // 2 - 1] + ordered[len(ordered) // 2]) / 2
    print(f"ci-p90: executed durations ascending (s): "
          f"{' '.join(f'{value:.0f}' for value in ordered)}")
    print(f"ci-p90: nearest-rank p90 rank={rank}/{len(ordered)} value={p90:.0f}s "
          f"median={median:.0f}s max={ordered[-1]:.0f}s budget={args.budget_seconds:.0f}s "
          f"verdict={'within' if p90 <= args.budget_seconds else 'OVER'} budget")
    return 0 if p90 <= args.budget_seconds else 1


if __name__ == "__main__":
    sys.exit(main())
