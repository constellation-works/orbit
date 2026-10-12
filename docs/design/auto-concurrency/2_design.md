---
title: Auto-concurrency — Design
owner: claude
last_updated: 2026-10-11
last_validated: 2026-10-11
status: Draft
feature: auto-concurrency
doc_role: design
summary: Proposed mechanism for predictive admission — per-task CPU and memory sampling from Linux worker scopes, per-workspace-and-complexity resource profiles, a host-wide load forecast with admission reservations, gap-fit admission under the existing throttle, and a shadow-first rollout.
tags: [auto-concurrency, drain, capacity, admission]
paths: ["crates/orbit-core/src/application/job/pipeline/worker/scope.rs", "crates/orbit-core/src/adapter/engine_host/v2_host/admission/auto_admission.rs", "crates/orbit-core/src/adapter/engine_host/v2_host/workspace_auto/classify.rs", "crates/orbit-core/src/adapter/engine_host/v2_host/pull/refill.rs", "crates/orbit-core/src/runtime/host_resource/**", "crates/orbit-config/src/registry/settings/machine.rs"]
related_features: [auto-concurrency, distributed-drain]
related_artifacts: [ORB-15757]
---

# Auto-concurrency — Design

> **Status: Draft proposal, not implemented.** §1 describes today's code and
> cites it; everything after it is the proposed design.

This doc covers what admission does today (§1) and the proposal:

- sampling each task's resource use (§2);
- profiles (§3) and the load forecast (§4);
- the admission rule (§5) and pull drains (§6);
- surfaces (§7) and rollout (§8).

Open directions are in [3_vision.md](./3_vision.md), and the standing rules in
[4_decisions.md](./4_decisions.md).

## 1. Today

**Ceiling.** Each drain carries `max_active_leaf_runs`. It defaults to 5,
with a minimum of 1 and no maximum
(`crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml`). An operator
changes it live with `orbit run concurrency <run> --set N`,
`orbit run extend --concurrency` or the `orbit_workflow_auto` resize action,
all through `set_drain_worker_limit`
(`crates/orbit-core/src/application/job/run/worker_limit.rs`). Lowering it
only stops new admission; nothing is cancelled.

**Occupancy is per workspace.** `occupancy()`
(`crates/orbit-store/src/driver/sqlite/job_run_store/pull.rs`) counts the
workspace's live leaves, including explicit `orbit run ship` deliveries.
Every occupancy query filters on `workspace_id`. No limit spans workspaces:
two workspaces draining on one host each take their full ceiling.

**Admission.** Each pass of `classify_workspace_auto_tasks`
(`workspace_auto/classify.rs`) does three things:

1. It computes free slots as the ceiling minus occupancy.
2. It applies the resource gate.
3. It walks the candidates in priority and age order through
   `select_admissions` (`admission/auto_admission.rs`), which adds each
   candidate to the wave unless it conflicts with a task already admitted.

A pass that dispatched re-lists at once. An idle pass sleeps 30 s when work is
waiting or the host is throttled, and 60 s otherwise. A pull drain instead
requests one claim per free slot from the owner, and the owner chooses the
task (`pull/refill.rs`, `pull/drain/refill_pass.rs`).

**The reactive guard.** The resource throttle (`runtime/host_resource/`)
samples host CPU, memory and disk every 5 s while admission is being
consulted, and stops 300 s after the last consult.

- **Hold.** It holds all admission once a resource stays at or above its high
  mark (90%) for 10 s, and releases below the resume mark (85%).
- **CPU-only pressure.** A local drain may still admit up to
  `cpu_light_leaves` tasks marked CPU-light (`admission/cpu_light.rs`).
- **Fail open.** Unknown readings never hold.
- **Shared state.** Every drain on the host reads one shared state file,
  `<global>/cache/host-resource-pressure.json`. It holds hysteresis state, not
  a time series.

**Per-task resource accounting.** On Linux, each detached worker runs in its
own systemd user scope, `orbit-worker-<run_id>-<nonce>.scope`
(`application/job/pipeline/worker/scope.rs`). `machine.worker_containment`
defaults to true. Orbit reads the scope's `memory.events` and `pids.events`
only to tell a limit breach from other failures. A worker can locate its own
scope (`WorkerScopeCgroup::of_current_process`). Nothing records CPU or memory
use per task. macOS has no scopes, so its workers run uncontained.

**History.** Each host keeps one SQLite database, `~/.orbit/orbit.db`, shared
by its workspaces. The `job_runs` table holds each run's start, end and host.
The task id is in `input_json`, and complexity is in the task index. No CPU
or memory history is kept, for the host or for any task.

## 2. Sampling each task

A worker samples **its own** scope, so attribution is exact and no central
sampler is needed.

- **What.** Every 5 s, a background thread in the worker reads its scope's
  `cpu.stat` `usage_usec` and `memory.current`. CPU is measured in cores
  (CPU time over wall time), memory in bytes.
- **Granularity.** Samples are aggregated per minute since the worker
  started: mean cores and maximum memory for that minute. Maximum, because a
  rustc or linker peak that lasts a few seconds is what triggers the OOM
  killer.
- **Storage.** Each minute is appended to a new `run_resource_minutes` table
  in `orbit.db`, keyed by workspace, run id and minute index, with the task
  id. The newest minute is also how admission learns each running task's
  current use (§4).
- **One delivery, several runs.** A local delivery is a chain of runs:
  `task_auto_pipeline`, then `task_gate_pipeline`, then `task_pr_pipeline`. A
  pull delivery is one `task_claimed_pr_pipeline`. Each run samples itself.
  A delivery's series is the per-minute sum over all runs of that task.
  Minutes are counted from the moment the drain admitted it, which is the
  admitted run's `created_at`.
- **Exclusions.** Resumed or retried deliveries restart their clock part way
  through, so they are kept out of profiles. They still count toward the
  current load.
- **Retention.** 30 days. The table is pruned with the other stores.
- **No scope, no samples.** A worker without a scope records nothing: macOS,
  containment disabled, or no user manager. That host stays on today's
  admission (§5.4) until it has samples. macOS gets a process-tree sampler
  later (§8).

Workers also keep a host series, `host_resource_minutes`, holding host CPU
and memory per wall-clock minute. Every worker offers a row for the current
minute, and the first insert for that minute wins. Profiles do not need it.
It is the ground truth for checking the forecast (§8).

## 3. Profiles

A **resource profile** describes one key, `(host, workspace, complexity)`,
built from that key's most recent completed deliveries: the last 50, within
30 days.

For each minute `m` since admission, the profile holds:

| Field | Meaning |
|---|---|
| `alive(m)` | fraction of deliveries still running at minute `m` |
| `cpu(m)` | mean cores used at `m`, over the deliveries alive at `m` |
| `mem(m)` | 90th-percentile memory at `m`, over the deliveries alive at `m` |

CPU uses the mean and memory a high percentile because the costs differ. Too
much CPU only slows everyone down. Too much memory kills a process.

`alive` gives the duration distribution. The candidate's **horizon** `H` is
the minute at which `alive` falls below 0.1, the 90th-percentile duration,
capped at 120 minutes.

Profiles are computed from the table when admission needs them and cached for
the pass. A key with fewer than 10 deliveries falls back along this chain:

1. `(host, workspace, any complexity)`;
2. the heaviest profile on the host, so unknown work is assumed expensive;
3. no profile at all, only on a host with no profiles: today's admission.

## 4. The load forecast

The forecast covers minutes `t = 0 … H` from now, for CPU and memory
separately.

```
F(t) = B + Σ over running deliveries i of P_i(a_i + t) · alive_i(a_i + t) / alive_i(a_i)
         + Σ over reservations r of P_r(t) · alive_r(t)
```

- **Running deliveries.** A running delivery `i` has age `a_i` and profile
  `P_i`. Its expected load at `a_i + t` is scaled by the chance it is still
  running then, given it is running now. A delivery older than its profile's
  last minute contributes its current measured use, held flat.
- **Baseline `B`.** The host's measured load now, minus the sum of running
  deliveries' measured use now. That leaves the load that isn't from tasks,
  such as an IDE or a manual build, and it is held flat over the horizon.
- **Host-wide.** The sum runs over live leaves of **every** workspace on the
  host: the `job_runs` table without the workspace filter. Pinned drains,
  explicit `orbit run ship` deliveries and before-landing reviews are all
  included.
- **Reservations.** An admission that has been decided but whose run does not
  exist yet is a reservation (§5.2). It is counted from `t = 0`.

## 5. Admission

### 5.1 Gap fit

`select_admissions` already walks candidates in priority order and builds the
wave one task at a time. Gap fit adds one more check per candidate. The
candidate, with profile `P_c` and horizon `H_c`, joins the wave only if, for
every `t` in `0 … H_c`:

```
F_cpu(t) + P_c.cpu(t) · alive_c(t) ≤ admit_cpu_percent    × host cores
F_mem(t) + P_c.mem(t) · alive_c(t) ≤ admit_memory_percent × host memory
```

When a candidate is admitted, its profile is added to `F` before the next
candidate is checked. One pass can therefore admit several tasks, each
counting the ones before it. When a candidate does not fit, it is recorded
with the minute and resource that failed, for example `cpu 94% at +6m`, and
the walk continues: a smaller task may fit the gap.

Every existing gate still applies, and gap fit only removes candidates:

- the drain's maximum concurrency;
- conflicts between tasks admitted together;
- the footprint guard;
- provider limits and crew exclusions.

**The resource throttle stays the backstop.** When it holds, nothing is
admitted, whatever the forecast says. This is what makes it safe to admit
into a host that is busy now but forecast to be quiet when the task's load
arrives. If the forecast is wrong, real pressure stops further admission.

Default thresholds are CPU 80% and memory 75%. Both sit below the throttle's
90% high and 85% resume marks, so forecast admission and the throttle do not
fight each other.

### 5.2 Serializing drains on one host

Two drains on one host must not fill the same gap. Each takes a host lock
(an `IMMEDIATE` transaction on `orbit.db`) for this sequence:

1. Build the forecast.
2. Decide the admissions.
3. Write one `admission_reservations` row per admitted task, holding the task
   id, profile key and time.
4. Release the lock.

A reservation leaves the forecast once its run exists in `job_runs`, or after
five minutes if dispatch failed. The lock covers only computation over local
tables, so it is held for milliseconds.

### 5.3 Starvation

A gap that small tasks keep filling can block a heavy task forever. When the
first candidate in priority order has failed gap fit for longer than
`head_max_wait_minutes` (30), the drain admits nothing else until that
candidate fits. Smarter backfilling is an open question
([3_vision.md §1](./3_vision.md#1-open-questions)).

### 5.4 Without profiles

A host with no profiles admits exactly as today, under the throttle. Profile
gaps for individual keys are covered by the fallback chain in §3, which
assumes the worst, so the forecast never treats unknown work as free.

## 6. Pull drains

A follower does not pick its task; the owner does. So the follower cannot run
gap fit on a specific candidate. Instead:

1. The follower computes which complexities of that workspace fit under its
   forecast.
2. It sends them as `admissible_complexities` on its `orbit.task.pull`
   request, and only requests when at least one fits.
3. The owner's ready-queue selection skips tasks outside the list.

This changes the pull protocol. That is safe because the owner already
refuses pulls from a different binary version. The follower adds a
reservation for the claimed task as soon as the claim returns.

The follower checks one candidate per request, then re-forecasts. The quirk
where a pass requests up to its submitted ceiling, not its live one
(`pull/drain/refill_pass.rs`), stops mattering once each request depends on
the forecast.

## 7. Surfaces and configuration

| Surface | Proposed change |
|---|---|
| Machine config | `[workflow.auto_concurrency]` beside `[workflow.resource_throttle]` in `crates/orbit-config/src/registry/settings/machine.rs`: `mode` (`off` / `shadow` / `enforce`, default `off`), `admit_cpu_percent` (80), `admit_memory_percent` (75), `profile_window` (50), `min_profile_deliveries` (10), `horizon_cap_minutes` (120), `head_max_wait_minutes` (30) |
| `drain_last_pass` | an `admission_forecast` block: the peak forecast over the horizon, admitted and skipped candidates with the failing minute and resource, and whether the head is being protected |
| `orbit run show` | an `Admission:` line, e.g. `forecast — skipped ORB-123 (cpu 94% at +6m)` |
| `orbit run readiness` | the host's forecast peak, its headroom, and the profile source per waiting task |
| `orbit run profiles` | the profiles per workspace and complexity: deliveries, horizon, peak CPU and memory, and when they occur. `--json` for the full series |
| Dashboard | later: the forecast curve against measured load, with running tasks stacked |

`max_active_leaf_runs` keeps its meaning: the operator's ceiling, enforced
first.

## 8. Rollout

1. **Record.** Add per-task sampling and the host minute series. No change to
   admission. Profiles need a few days of deliveries.
2. **Profiles report.** `orbit run profiles`. It shows whether profiles are
   stable enough to forecast with: how spread out each minute is, and how
   spikes line up across deliveries.
3. **Shadow.** `mode = shadow` computes forecasts and decisions every pass
   and records them, but admits as today. A report compares each forecast at
   +5, +10, +20 and +30 minutes with the measured host series, and the
   shadow decisions with what admission actually did.
4. **Enforce** on dk-server-1 first, a follower with the smallest blast
   radius, then on dk-server-2.
5. **macOS.** A sampler that sums CPU time and resident memory over the
   worker's process tree, using `orbit-common/src/process/ancestry.rs`. Until
   it exists, the Mac admits as today.

## 9. Concerns & Honest Limitations

- **Averaging blurs spikes.** Tasks of one complexity differ in length, so the
  validation build lands at different minutes. A per-minute average of
  misaligned spikes is a lower, wider hump. The forecast then underestimates
  peaks and overestimates how long moderate load lasts. The 90th percentile
  for memory and the throttle backstop absorb part of this. Profiles aligned
  to pipeline steps instead of minutes would fix it
  ([3_vision.md §1](./3_vision.md#1-open-questions), question 1). The
  profiles report in step 2 shows how bad it is before anything enforces.
- **Complexity is a coarse label.** It is set at filing or by the task
  pilot, and is sometimes wrong. A mislabelled task carries the wrong
  profile.
- **The baseline is assumed flat.** Load that isn't from tasks, such as a
  manual build or an IDE indexing, can change during the horizon.
- **Only CPU and memory.** Disk I/O and fsync stalls (the Mac), provider
  rate limits and base-branch conflicts are not forecast. The disk throttle,
  provider-limit exclusions and the conflict and footprint checks still
  apply.
- **Profiles are per host and need warm-up.** A new host, workspace or
  complexity is admitted pessimistically, using the heaviest profile, until
  it has 10 deliveries.
- **Linux only at first.** macOS has no worker scopes. Without
  `machine.worker_containment`, Linux records nothing either.
- **Throughput is not measured.** The design fills headroom under the
  thresholds. It does not check that more concurrency lands more work. When
  the bottleneck is something else, such as conflicts or review, it fills
  the host anyway, up to the operator's maximum.
- **Starvation guard is crude.** The head-of-queue wait can idle a host for
  up to 30 minutes while it waits for one heavy task to fit.
- **Shared pressure state across differing thresholds.** The throttle's
  pressure evaluator resets a resource's state when the evaluating
  workspace's thresholds differ from the recorded ones
  (`runtime/host_resource/pressure.rs`). Workspaces with different throttle
  settings may keep resetting each other, and weaken the backstop. This is
  inferred from the code, not observed. Machine-level thresholds avoid it.
- **Thresholds are guesses.** 80% CPU and 75% memory are starting points.
  The shadow report is where they get tuned.

## Task References

- [ORB-15757] — authored this proposal.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
