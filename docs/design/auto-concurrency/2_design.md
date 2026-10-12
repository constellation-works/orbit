---
title: Auto-concurrency — Design
owner: claude
last_updated: 2026-10-11
last_validated: 2026-10-11
status: Draft
feature: auto-concurrency
doc_role: design
type: design
summary: Proposed mechanism for automatic drain concurrency — a goodput objective, an offline estimator over run history, a per-host budget split across drains, congestion signals, an asymmetric controller acting through the existing worker-limit path, and a shadow-first rollout.
tags: [auto-concurrency, drain, capacity, admission]
paths: ["crates/orbit-core/src/adapter/engine_host/v2_host/workspace_auto/classify.rs", "crates/orbit-core/src/adapter/engine_host/v2_host/pull/refill.rs", "crates/orbit-core/src/application/job/run/worker_limit.rs", "crates/orbit-store/src/driver/sqlite/job_run_store/pull.rs", "crates/orbit-core/src/runtime/host_resource/**", "crates/orbit-config/src/registry/settings/machine.rs"]
related_features: [auto-concurrency, distributed-drain, automation-triggers, routines]
related_artifacts: [ORB-15757]
---

# Auto-concurrency — Design

> **Status: Draft proposal, not implemented.** Sections that describe today's
> code say so and cite it; everything else is the proposed design.

This doc covers the objective (§1), the data it can draw on (§2), where the
limit lives (§3), the signals (§4), the controller (§5), the offline estimator
(§6), surfaces and configuration (§7), and rollout (§8). Open directions are in
[3_vision.md](./3_vision.md) and the standing rules in
[4_decisions.md](./4_decisions.md).

## 1. The objective is goodput

By Little's law, a drain holding `c` leaves that each take `T` to finish
completes `c / T` leaves per unit time. Only some of them are worth having,
so the objective multiplies by the fraction `p` that landed and stayed landed:

```
G(c) = c · p(c) / T(c)
```

- **`T(c)`** is measured as **slowdown**: a leaf's duration divided by the
  baseline duration of its complexity class in its workspace. The baseline
  is measured at low occupancy. Without this normalization, the task mix
  dominates the curve: a backlog of low-complexity chores looks like a
  throughput gain at any concurrency.
- **`p(c)`** counts a leaf as good when its task reached `done` through a
  landed commit, and was not reverted or named as the source of a repair
  task within the attribution window
  ([3_vision.md §1](./3_vision.md#1-open-questions), question 2). Failed
  leaves, released claims, no-landing outcomes and landing conflicts are not
  good.
- **Work units.** A good leaf contributes its class baseline in hours, so
  `G` reads as baseline work-hours landed per wall-clock hour.

Taking logarithms, `d ln G / d ln c = 1 + ε_p − ε_T`, where `ε` is the
elasticity of each term with respect to `c`. Adding a slot raises goodput
while `ε_T − ε_p < 1`. Put plainly: one more slot pays as long as it slows
every leaf down by less than `1/c`, after allowing for any drop in landing
rate. This is the marginal rule ("stop where the marginal completion rate
stops improving") restated so it can be measured from per-leaf data.

A level is only informative when demand saturated it. In an interval where
the ready backlog was shorter than the limit, throughput says nothing about
what a higher limit would do. Both the estimator and the controller discard
or discount such intervals.

## 2. What history already records

This section describes today's store. Each host keeps one SQLite database,
`~/.orbit/orbit.db`, shared by every workspace on it
(`crates/orbit-config/src/persistence.rs`). Hosts do not share databases: a
follower holds its own leaf runs, and the owner never sees their durations.

| Need | Recorded today | Where |
|---|---|---|
| Leaf start, end, duration, host | yes: `started_at`, `finished_at`, `duration_ms`, `executed_on_json` (host id and name), `resolved_crew`, `crew_model` | `job_runs` (`orbit-store/src/driver/sqlite/migration/baseline.rs`, `migration/job_runs.rs`). Legacy `task_pr_pipeline` rows have no `executed_on`. |
| Leaf → task | inside `input_json` only (`task_id` or `task_ids`) | `job_runs.input_json` |
| Leaf → admitting drain | parent side only: the drain's `child_dispatches` pipeline state and `pipeline.child_dispatch` audit rows; for pull leaves, `local_pull_admissions` | `orbit gc runs` drops pipeline state |
| Step durations | per step for successful runs; failed runs keep one job-level diagnostic step. Every run's `step.started` and `step.finished` events are in the v2 audit. | `job_run_steps`, `v2_audit_events` |
| Validation command durations | no: `RequiredCommandRun` keeps only the timeout. The `validate` step's whole duration is the finest grain. | `orbit-engine/src/executor/automation/vcs/required_command.rs` |
| Failure class | pull leaves only, in the claim settlement (`ClaimFailureClass`). Local leaves have an error code, not a class. | `contracts/task/coordination/settlement.rs` |
| Task lifecycle | every status change with a timestamp. The `done` note names the PR and merged SHA. Landed commits are recorded per review attempt. | task `events.jsonl` (`orbit-types/src/task/artifacts.rs`), `review_landings` |
| Revert or repair | `regression_from` relations, without a timestamp; no revert marker | `task_bundle_relations` |
| Complexity | yes | `task_bundle_index` |
| Configured concurrency | the submitted value in the drain's input; the latest resize on its state; earlier resizes only in `pipeline.run.workers.*` audit rows | `job_runs.input_json`, `job_run_states`, `audit_events` |
| Backlog depth per pass | local drains: no, `drain_last_pass` is overwritten. Pull: the owner keeps every admission receipt, including idle polls, with `queue_depth`, and never compacts them. | `task_coordination_rows` (`contracts/task/coordination/admission.rs`) |
| Host CPU, memory, disk history | no: only the current pressure state | `<global>/cache/host-resource-pressure.json` |
| Tokens and cost | sparse: per-invocation rows cover a fraction of runs | `invocations`; `reliability_store.rs` notes a token-accounting mismatch with the run store |
| Provider limits | latest reading only, plus `provider_limit` flags on settlements | `provider_limit_observations` |

**Retention.** `orbit gc audit` and `orbit gc runs` default to 60 days. The
first deletes audit rows; the second drops terminal runs' pipeline state but
keeps run rows and steps (`crates/orbit-core/src/application/gc/store.rs`). The
`store_gc` routine ships disabled, so in practice history reaches back to
each host's first run: 2026-08-22 on the Mac, for example. The validation-step
timings and resize audit rows the estimator wants are the prunable ones.

What this means for the estimator (§6):

- **Occupancy is reconstructible.** Leaf start and end times per host are
  enough. The missing leaf → drain link does not matter, because the
  estimator counts all leaves on the host.
- **Saturation is approximate.** Pull hosts have the owner's queue-depth
  series. Local drains need the backlog reconstructed from task status
  history: a task was waiting at time `t` if it was in `backlog` with its
  dependencies done.
- **Outcomes need a join across hosts.** A follower's leaf durations are on
  the follower and its landing is on the owner. The estimator reads both
  through the federated task tools, keyed by task id.
- **Reverts need git.** With no revert marker, "stayed landed" comes from
  `regression_from` relations plus a scan of the base branch for revert
  commits naming the landed SHA or PR.

Phase 1 (§8) therefore also starts recording what is missing:

- a bounded per-pass sample table in `<global>/orbit.db` holding host
  occupancy, each drain's `queued` and `free_slots`, and the throttle state;
- elapsed time on `RequiredCommandRun`;
- a failure class for local leaves.

None of these changes admission.

## 3. Where the limit lives

### 3.1 Today

Each drain carries its own ceiling, `max_active_leaf_runs`:

- The ceiling defaults to 5, has a minimum of 1 and no maximum
  (`crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml`,
  `crates/orbit-core/src/application/job/pipeline/worker/command.rs`).
- An operator changes it live through `orbit run concurrency <run> --set N`,
  `orbit run extend --concurrency` or the `orbit_workflow_auto` resize
  action. All three go through `set_drain_worker_limit`
  (`crates/orbit-core/src/application/job/run/worker_limit.rs`). It writes
  `DrainWorkerLimit { max_active_leaf_runs, previous_max_active_leaf_runs,
  revision, actor, reason, updated_at }` to the run's pipeline state
  (`crates/orbit-types/src/workflow/run/state/drain.rs`) under a revision
  compare-and-set. It also emits `pipeline.run.workers.requested` and
  `pipeline.run.workers.completed` audit events. Only the latest change is
  kept on state; the history lives in the audit events.
- Lowering the ceiling only stops new admission. Running leaves are never
  cancelled.

Occupancy is shared by the workspace, not by the drain. `occupancy()`
(`crates/orbit-store/src/driver/sqlite/job_run_store/pull.rs`) counts every
live leaf of the workspace, including explicit `orbit run ship` deliveries and
the leaves of a stopped coordinator (`inherited_leaf_runs`). Every occupancy
query filters on `workspace_id`. **No host-wide limit exists**: N workspaces
each draining at ceiling `c` can run N × c leaves on one host. The couplings
across workspaces are:

- the resource throttle's shared pressure history
  (`<global>/cache/host-resource-pressure.json`);
- the repository's own cooperative build-slot script, which only covers
  Make targets in checkouts that ship it
  ([build-budget.md](../../runbooks/build-budget.md));
- provider-limit readings, which remove crews but never change a ceiling.

The resource throttle (`crates/orbit-core/src/runtime/host_resource/`) holds
admission when CPU, memory or disk stays above its high mark for 10 s. It
releases below the resume mark, and fails open on unknown readings. Under
CPU-only pressure a local drain may still start up to `cpu_light_leaves`
CPU-light tasks (`crates/orbit-core/src/adapter/engine_host/v2_host/admission/cpu_light.rs`);
a pull drain stops requesting entirely. The throttle is an on/off gate. It
never changes a ceiling.

### 3.2 Proposed: a host budget and workspace caps

The controller tunes one number per host: the **host budget** `B`, a count of
live leaves across every drain on the host. Each workspace may carry a
**workspace cap** `K_w` that bounds its leaves across all hosts.

- **Host budget.** `B` lives in the host's store (`<global>/orbit.db`, which
  every workspace on the host already shares) with its bounds
  `[min, max]` from machine-level configuration (§7). A host-wide occupancy
  query, `occupancy()` without the workspace filter, gives the host's live
  leaf count.
- **Split across drains.** Every pass already records `queued`, the
  admissible tasks it could not admit (`DrainAdmissionPass` in
  `orbit-types/src/workflow/run/state/drain.rs`). Each pass, the budget is split across the host's live drains
  by max-min fairness over demand: a drain's demand is its occupied slots
  plus its admissible waiting tasks, bounded by its workspace cap. A drain
  that wants less than an equal share gets what it wants, and the remainder
  is split among the rest. A drain's effective ceiling is its share. The
  allocation is recomputed per pass, so it moves as backlogs drain.
- **Workspace cap.** `K_w` comes from contention on the workspace's base
  branch, not from host resources. It is set by the operator at first. A
  learned cap is phase 3 work (§8). For pull drains, it is enforced where the
  workspace's admissions are ordered: the owner's `orbit.task.pull`, which
  today has no per-workspace or per-follower cap.
- **Admission check.** Local admission (`classify_workspace_auto_tasks`) and
  pull allocation (`allocate` in `job_run_store/pull.rs`) both take
  `min(drain share − drain occupied, B − host occupied)` free slots. The pull
  path already re-checks its ceiling inside an `IMMEDIATE` transaction. The
  host check joins it there, so two drains cannot both take the last slot.
- **Floor of two.** At a ceiling of 1, admission switches off the footprint
  guard (`FootprintGuard.enabled = max_active_leaf_runs > 1` in
  `workspace_auto/classify.rs`) and admits tasks without a footprint. Auto
  mode never crosses between 1 and 2; its minimum bound is at least 2. An
  operator can still set 1 by hand.

## 4. Signals

The controller reads two kinds of signal. **Congestion signals** are fast,
available within a pass or two, and only ever cause a decrease. **Goodput
signals** are slow, hours behind, and set the soft ceiling (§5.3).

| Signal | Source today | Counts as congestion when |
|---|---|---|
| Resource throttle | `ResourceAdmission` from `runtime/host_resource/` | any resource is held, including CPU-only `LightOnly` |
| Provider limit | crew window `provider_limit` exclusions (`application/distributed/settlement/runtime.rs`) | a crew is newly excluded, or the refusal is `no_runnable_crew` |
| Environmental failure | `FailureClass` (`orbit-types/src/workflow/run/state/failure.rs`) | an `environment`, `transient` or `provider` settlement or leaf failure, or `host_suppressed` |
| Step latency gradient | `validate` step durations in `job_run_steps` and the v2 audit (§2) | the short-window median of a heavy validation step exceeds its long-window baseline by more than the tolerance (default 40%) |
| Landing conflict rate | `base_conflict` releases and rebase failures | the rate over the last window exceeds its baseline (feeds the workspace cap, §8) |

The **latency gradient** is the closest thing a drain has to round-trip time.
It is the ratio of a long-window baseline to a short-window median for the
same step in the same workspace, per complexity class. It is the gradient
limiter of Netflix's concurrency-limits library
([3_vision.md §2](./3_vision.md#2-prior-work)), and it rises before CPU or
memory cross their high marks. Only steps that do heavy local work, such as
validation gates, are measured. Agent think time varies too much with the
task to be a congestion signal.

**Saturation evidence**, which permits an increase, is the conjunction of
three facts for every pass in the interval:

- `free_slots` was 0;
- admissible tasks were still waiting;
- no congestion signal fired.

`classify_workspace_auto_tasks` already outputs `free_slots`, and every pass
writes `queued` to `drain_last_pass`; a pull pass takes it from the owner's
queue depth. `drain_last_pass` is overwritten each pass, which is fine for a
controller that runs inside the pass, but leaves no history for the
estimator (§2).

An unknown reading is not a signal. The resource throttle fails open on
unknown telemetry, and the controller does the same: an interval with
unknown readings neither decreases nor counts as clean for an increase.

## 5. Controller

### 5.1 Where it runs

There is no new daemon. The controller is a deterministic step evaluated
during drain passes. A pass that finds the host's control interval elapsed
takes a host-level lock in `<global>/orbit.db`, evaluates the interval's
signals and writes the new budget, then releases the lock. Whichever drain
passes first does the work. The resource monitor already keeps host-wide
state the same way: one pressure history shared by every drain, updated
under a lock by whichever process samples. A host with no
live drain has no controller and needs none.

### 5.2 The fast loop

Each control interval (proposed default 10 minutes):

1. **Decrease.** If any congestion signal fired and the previous decrease
   has taken effect (host occupancy ≤ `B`), set
   `B ← max(min, ⌊B · β⌋)` with `β = 0.75`, and start a cooldown. Because a
   decrease cancels nothing, the congestion that triggered it persists until
   enough leaves finish. Without the "has taken effect" condition, one
   sustained pressure episode would cut `B` to the minimum.
2. **Increase.** Otherwise, if the interval showed saturation evidence (§4)
   and no cooldown is running, set `B ← min(max, S, B + 1)`. `S` is the
   soft ceiling (§5.3). Two consecutive clean intervals are needed after any
   decrease.
3. **Hold** in every other case, including a supply-limited interval.

The cooldown is half the host's median leaf duration over the last day, and
at least one interval. A decrease is evaluated against conditions that
existed before it, and an increase needs time to show its cost.

### 5.3 The slow loop

Every window of at least 12 settled leaves, and at most 6 hours, the
controller updates its per-host goodput model: the offline estimator (§6)
seeds it, and each window adds points. It sets the soft ceiling `S` to the
smallest level whose estimated goodput is within the confidence band of the
maximum. When two levels cannot be told apart, it prefers the lower one.
`S` rises by one per day without contrary evidence, so a ceiling learned
under worse conditions is retested.

### 5.4 Actuation and pins

The controller writes each drain's share through `set_drain_worker_limit`
with actor `auto-concurrency` and a reason naming the signals behind it.
This reuses the revision check, the `Workers:` line in `orbit run show`, and
the `pipeline.run.workers.*` audit events. A `set_drain_worker_limit` from
any other actor marks that drain **pinned**: the controller stops writing it
for the rest of its window. The pinned drain's occupancy still counts
against the host budget. `orbit run concurrency <run> --auto` hands it back.

Every host-budget change is also appended to a host-level history table in
`<global>/orbit.db`: time, old and new `B`, the rule that fired (decrease,
increase, ceiling), and the signal snapshot. The drain state keeps only the
latest change, and the evaluation in §8 needs the full series.

## 6. Offline estimator

Phase 1 is a read-only report over one host's history. It changes nothing.

1. **Occupancy.** Reconstruct `c(t)`, the number of live leaf runs on the
   host across all workspaces, from leaf start and end times. Use observed
   occupancy, not the configured ceiling: it captures resizes, inherited
   leaves and several drains at once without needing a resize history.
2. **Per leaf.** Record `c̄`, the time-weighted mean occupancy over the
   leaf's life, plus its slowdown, its outcome under §1, and its workspace,
   class and Orbit version.
3. **Saturation filter.** Keep leaves admitted while ready backlog was
   waiting. For pull hosts, that comes from the owner's admission receipts
   (`queue_depth`). For local drains, the backlog is reconstructed from task
   status history (§2). Once the per-pass sample table exists, it replaces
   both.
4. **Fit.** Bin by `c̄`. Per bin, report the leaf count, median slowdown,
   landed fraction and goodput, with bootstrap confidence intervals. Fit
   the Universal Scalability Law
   `G(N) = λN / (1 + σ(N−1) + κN(N−1))` per host. Its peak
   `N* = √((1−σ)/κ)` is reported alongside the empirical argmax. The
   contention term `σ` maps to serialized resources; the coherency term `κ`
   maps to pairwise conflicts between concurrent leaves.
5. **Recommend.** Report the smallest level within the confidence band of
   the best goodput, and the evidence behind it. The estimator never
   recommends a level above the highest well-sampled one. When goodput is
   still rising at that level, it says **ceiling binding**: the data cannot
   locate the optimum and the operator's maximum is the constraint.

It stratifies by host and Orbit version, and prints sample counts beside
every number. A bin with too few leaves (proposed: 20) is shown but not used.

## 7. Surfaces and configuration

| Surface | Proposed change |
|---|---|
| Machine config | `[workflow.auto_concurrency]` beside `[workflow.resource_throttle]` in `crates/orbit-config/src/registry/settings/machine.rs`: `enabled` (false), `min` (2), `max` (required), `decrease_factor` (0.75), `interval_seconds` (600), `latency_tolerance_percent` (40), `shadow` (true) |
| Workspace config | optional `cap`, the workspace cap `K_w` |
| `orbit run auto` | `--concurrency auto` uses the host bounds; `--concurrency auto:MIN-MAX` narrows them for this drain |
| `orbit run concurrency` | `--auto` hands a pinned drain back to the controller; `--set N` pins, as today |
| `orbit run show` | the `Workers:` line shows `auto (MIN..MAX)` and the last change's rule and signals; `drain_summary` gains an `auto_concurrency` block |
| `orbit run readiness` | shows the host budget, each drain's share, and the estimator's recommendation |
| Report | `orbit run concurrency --report [--since]` prints the §6 estimate as a table, or as JSON with `--json` |
| Dashboard | later: host budget, occupancy and signals over time |

## 8. Rollout

1. **Estimator report** (§6), plus the recording gaps from §2. The report
   is read-only. It answers whether the history shows a knee at all, and
   where. If goodput is still rising at today's ceilings
   (dk-server-2 15, dk-server-1 5, Mac 4 at the time of writing), most of the
   controller's value is in backing off under pressure, and that is the part
   to build first.
2. **Shadow mode.** The controller runs and records every decision in the
   host history table, but does not actuate (`shadow = true`). After a week,
   compare its decisions with the operator's resizes and with the
   congestion that followed.
3. **Actuation** on one host. dk-server-1 goes first: it is a follower with
   the smallest blast radius. dk-server-2, the owner, follows once the
   shadow record looks sound, and the Mac may stay advisory
   ([3_vision.md §1](./3_vision.md#1-open-questions), question 8).
4. **Learned workspace caps** from the landing-conflict rate, enforced at the
   owner's `orbit.task.pull`.

## 9. Concerns & Honest Limitations

- **Slow, noisy feedback.** Leaves run for tens of minutes, so a host settles
  only a few dozen a day. The fast loop steers on proxies. If they don't
  track goodput, the controller is confidently wrong in a stable way. Shadow
  mode exists to find out before it matters.
- **Observational history.** Past concurrency was chosen by the operator,
  usually during pushes with deep and often easy backlogs, and across binary
  and settings changes. The saturation filter and the stratification reduce
  this confounding but do not remove it. The estimator's output is a starting
  point and a ceiling, never a setting.
- **Non-stationarity.** The optimum moves with the binary, the task mix,
  provider state and the other host's load. The slow loop chases it, but
  never faster than one window.
- **Provider quota is cross-host.** A host-local loop sees the other host's
  provider use only as `provider_limit` exclusions, after the fact
  ([3_vision.md §1](./3_vision.md#1-open-questions), question 4).
- **Leaves are not equal.** The budget counts leaves, but a Markdown task
  and a full Rust validation load a host very differently. The congestion
  signals absorb this indirectly; the budget does not model it.
- **Reviewing leaves.** A pull drain does not count claimed-PR leaves in
  before-landing review against its ceiling, so a follower can hold up to
  twice its ceiling in live leaves (`job_run_store/pull.rs`;
  [distributed-drain runbook](../../runbooks/distributed-drain.md)). The host
  budget must decide whether those count. They load the provider but rarely
  the CPU.
- **Pull fills slowly after an increase.** A pull pass requests at most its
  *submitted* ceiling per pass, while allocation honours the *live* one
  (`pull/drain/refill_pass.rs`). An increase above the submitted value takes
  several passes to fill. The controller must not read that lag as missing
  demand.
- **Shared pressure history across differing thresholds.** The pressure
  evaluator resets a resource's state when the evaluating workspace's
  thresholds differ from the recorded ones
  (`runtime/host_resource/pressure.rs`). Workspaces on one host with
  different throttle settings may keep resetting each other's sustain
  window, so the throttle never engages, and the controller loses its main
  signal. This is inferred from the code, not observed. Uniform
  machine-level thresholds avoid it.
- **Inherited leaves.** Explicit `orbit run ship` deliveries occupy budget
  that no drain admitted. The controller counts them, but cannot schedule
  around them.
- **Throughput is not the only goal.** More concurrency also means more
  spend, more after-landing review debt and more agent-main churn. The
  objective ignores the first two
  ([3_vision.md §1](./3_vision.md#1-open-questions), question 3). The
  operator's maximum is the lever for them.

## Task References

- [ORB-15757] — authored this proposal.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
