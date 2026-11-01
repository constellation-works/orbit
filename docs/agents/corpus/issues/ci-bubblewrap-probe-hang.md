---
type: issue
applies_to: [final_recovery]
status: resolved
links: [ORB-20643, ORB-20615]
---

# CI hangs in the Bubblewrap probe step

## Signature

A Linux test job (`Test shard N/3`, `Test partition`, `Test extra partition`) prints nothing in the step `Prepare and probe Bubblewrap for the Linux boundary tests` until cancelled; `Check / Clippy / Test` then fails with `ci-await: timed out after 2400 s`. Sibling shards pass.

## Mechanism

The step ran `apt-get update`/`install`, `bwrap --help` and a namespace probe with no `timeout-minutes` and no per-command bound, and printed `bubblewrap_setup_seconds` only on exit, so a stall showed nothing about the phase (ORB-20643 description). The stalling phase was not identified in the task's description.

## Resolution

The code under review is not at fault. Rerun the failed jobs on the unchanged head; do not requeue the task or edit its candidate for this failure. Escalate if the rerun hangs again at the same step. After ORB-20643 landed (PR #7384, 80d649b8) the step is bounded and phase-reported; a new hang then names a phase.

## Fix task

ORB-20643 (done, merged 2026-11-01T00:39Z).

## Occurrences

Backfilled 2026-11-01 from task history, 2026-10-02 to 2026-11-01 (ORB-20650).

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-10-31 (after 23:17) | ORB-20615 | CI run 41251891591 | Bubblewrap probe hang on PR #7365 head 1f9642e7 (Test shard 2/3 and Test extra partition cancelled, ci-await timeout) | operator-visible only; the gate fix PR was blocked until the fix landed (ORB-20615 settled 2026-11-01T01:05Z) |
| 2026-11-01 (after 00:03) | ORB-20643 | CI run 41254730860 | Bubblewrap probe hang on agent-main 21e1b59dd (Test partition 2 past the 400 s budget) | fixed by this task's PR #7384, merged 2026-11-01T00:39Z |
