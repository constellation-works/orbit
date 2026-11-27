# Inspect a drain readiness snapshot

## Contents

- [Readiness snapshots](#readiness-snapshots)

## Readiness snapshots

`orbit run readiness` is the diagnostic counterpart to auto-drain. It reads a
bounded snapshot of the explicit workspace and reports each selected backlog
task as ready or waiting, naming unmet dependency IDs/statuses, context-lock
holders, live child-run claims, capacity saturation, and — with `--allow-crew`
— crew exclusion. It never creates a run, reconciles stale runs, reserves
files, or mutates a task.

Explicit task ID selections are treated as a set: repeated valid IDs appear
once and count once against `--limit`. Blank IDs remain invalid, and unknown
IDs are refused.
Without `--concurrency`, the snapshot's ceiling is the live local drain's,
including an `orbit run concurrency` retune (`capacity.limit_source`
`run_control`, or `run_input` when the drain still has its submitted value).

When no local drain is live, a live pull drain supplies that ceiling the same
way (`pull_run_control` or `pull_run_input`), and text adds a `Leaf ceiling:`
line naming the pull run. Otherwise the ceiling is 5 (`default`). An explicit
`--concurrency` is `requested` and wins over both. `capacity.max_active_leaf_runs`
and `capacity.free_slots` use that ceiling, so a follower pull drain submitted
at 3 is not reported against the default of 5.

Its answer can change immediately after the snapshot, so `eligible` means
"would be admitted by this snapshot", never a guarantee that work will start.

`--allow-crew` previews the same restriction as `orbit run auto --allow-crew`:
excluded tasks report `crew_not_allowed` with the crew they would run as,
and permitted tasks keep filling free slots. Without it, the live drain's
own restriction applies (`capacity.allowed_crews` in JSON); with no live
restriction, all configured crews are allowed.

A drain started while another runs waits for it. Readiness lists it as
`Queued drain:` with its completion, ceiling, crews and window. `Unset env:`
names the `execution.env.pass` variables its start did not hold, and the
snapshot names any Claude crew in its window left with no worker credential.
JSON records these queued drains in `capacity.queued_drains`.

It also names a scheduled host shutdown or reboot and its mode. Within the
fixed 180-minute hold, tasks report
`host_shutdown_scheduled` and new unattended admissions hold; running workers
are left alone. A later schedule is reported while admissions continue. Pull
drains report the same hold as `Pull drain requests held:`. Report the time and
configured window, and wait or follow an explicit operator instruction; do not
cancel the schedule or override the hold automatically.

See
[authorization.md](authorization.md#keep-authorized-work-moving).

While sustained host resource pressure throttles admissions
(`[workflow.resource_throttle]`), held tasks report `resource_throttled` and
the output names the resource, its value, threshold and since-when. Unknown
readings never throttle; JSON lists them in
`capacity.resource_telemetry_unknown`. On a replica the throttle is this
host's own and holds its pull drain; the owner's throttle never holds a pull
claim.

When that pull drain's last pass requested no claim for another reason,
`Pull drain requests held:` names it (`capacity.pull_drain_refusal`), for
example `circuit_open` once three of its claims in a row settled as failures,
until its breaker half-opens to admit one probe claim.

When CPU is the only held resource, `no-diff-expected` auto-tasks (marked
`cpu-light`) still start within two reserved slots. Once
those are taken they report `cpu_light_budget_full`; memory, disk or I/O
pressure holds them too. On Linux, sustained disk I/O stall (`io full`, from
`/proc/pressure/io`) is one of the held resources; hosts without it are not gated.

A `Provider limits:` line names each provider usage window at or past its
`workflow.provider_limit_*` threshold, its reset, and the crews admission skips
until then. A task whose every crew is skipped reports `provider_limit` with
that detail. JSON lists every live reading in `provider_limits`: `provider`,
`scope`, `window`, `used_percent`, `exhausted`, `resets_at`, `source`,
`observed_at`, `threshold`, `gated`, `until` and `crews`.

A task whose frozen delivery batch is within two hours of its deadline sorts
ahead of same-priority backlog and names that deadline.

Two of its answers separate contention from capacity:

- `conflict_deferred` means a slot was free and this task did not take it,
  because its files overlap something already spoken for.
  `blocking_task_ids` and `conflicts` name the tasks and selectors, and
  `provenance` says whether the blocker holds the lock (`held_lock`), is a
  task a live child is already carrying (`live_claim`), or was chosen earlier
  in the same admission wave (`same_wave`).

  - When a blocker is held by a
    follower's live execution claim, `blocking_claims` names the claim, its
    machine and run, and the text view prints `claimed-on=<task>@<machine>`: the
    wait clears when that claim settles. The drain's hold reason names it the
    same way, and a candidate overlapping a claim is deferred with
    `execution_claim` provenance. `capacity_saturated`, by contrast,
    means there was no free slot at all.
- `surface_reserved` means a critical or high-priority task ranked ahead of
  this one waits only on context locks, and this task overlaps its surface.
  It is withheld so it cannot take each lock as it frees, and admits once the
  reserving task (`blocking_task_ids`) is admitted or leaves backlog. The
  reserving task reports `context_lock_conflict` with a `detail` saying it
  reserves. At most two tasks reserve per pass; work that does not overlap a
  reserved surface admits normally.
- A `ci-failure-sweep` repair whose `context_lock_conflict` carries a
  `shared_base_repair` object (and a `detail` starting `shared base failure:`)
  waits on a `review` task that waits on it. The holder holds a selector its
  own recovery added, and its current head fails with the repair's exact
  failure signature: either the failure its admitted auto-fix check repair was
  handed (`pr_auto_fix_head_failure`) or a `known_red_base` hold. The repair
  owns the fix.

  - Admission still grants no overlapping write. For each holder,
    head, failure and selector, the owner's admission pass files one human-crew
    task tagged `shared-base-repair-decision` and raises one `agent_request`
    decision on it. The decision asks who owns the repair: free the holder's
    lock without discarding its candidate or pull request, let the holder's PR
    carry the repair, or keep waiting. The `detail` names that task and
    request.

  - Answering records the choice and completes the task; Orbit applies
    nothing to either task. With no human crew configured, nothing is filed and
    the `detail` says so.
- `published_delivery` (text: `resuming published delivery (PR #N)`) marks a
  backlog task whose final-recovery requeue kept its pull request. Its next
  run adopts that branch and pull request before it edits anything, so it is
  admitted past other tasks' context locks rather than waiting on
  `context_lock_conflict`. Before any agent of that run that may edit, the run
  takes the task's own footprint locks, waiting while another task holds them,
  so two writers never share a file.

  - An operator's explicit ship
    (`orbit run ship`) is the exception: the gate un-defers those locks and
    records a `readiness_overridden` event naming the holders, so the run edits
    beside them without waiting.
- `dependency_not_delivered` means a dependency is `done` but its commits exist
  only on other refs than the base the delivery cuts from, typically because its
  pull request has not merged. The task waits in `backlog`, and the drain admits
  it once those commits reach the base; `detail` names the dependency and the
  commits. Land the dependency's pull request, or ship the task with
  `--ignore-dependency-delivery` when its work reached the base under another
  commit message.
- `awaiting_footprint` and `awaiting_exclusive_slot` mean the task declares
  no `context_files`, has no `no-diff-expected` tag and no operator
  `readiness-waived` tag, in a run with more than one slot. It would hold no
  file lock, so it waits for the task pilot, then for a slot with nothing else
  in flight; `detail` names the deadline and the fix.

  - A `conflict_deferred`
    whose `provenance` is `unknown_footprint` is blocked by a running task with
    no footprint, and
    `exclusive_reservation` by such a task waiting to go alone. See
    [Prepare selectors](orchestration-pilot-admission.md#prepare-selectors-before-dispatching-under-traffic).
- `capacity.occupancy` breaks the occupied slots down by what each is doing —
  `lock_waiting`, `implementing`, `post_implementation`, or `unknown` — with
  the wrapper, task, and descendant run IDs behind each. A drain whose slots
  are all `lock_waiting` is queued on itself, which the occupancy total alone
  cannot show. Phase comes from durable run state only; where that evidence is
  missing the phase is `unknown` and names the reason.

```bash
orbit run history -j task_auto_pipeline
orbit run show <run_id>
```
