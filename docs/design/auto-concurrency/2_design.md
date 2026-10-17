---
title: Auto-concurrency — Design
owner: claude
last_updated: 2026-10-17
last_validated: 2026-10-17
status: Draft
feature: auto-concurrency
doc_role: design
summary: Proposed mechanism for predictive admission — per-task CPU and memory sampling from Linux worker scopes and the macOS process tree, resource profiles per workspace, work class and complexity, a host-wide load forecast with admission reservations, gap-fit admission under the existing throttle, and a shadow-first rollout.
tags: [auto-concurrency, drain, capacity, admission]
paths: ["crates/orbit-core/src/application/job/pipeline/worker/scope.rs", "crates/orbit-core/src/application/job/pipeline/worker/usage.rs", "crates/orbit-core/src/application/job/pipeline/worker/delivery_class.rs", "crates/orbit-core/src/application/admission/auto_admission.rs", "crates/orbit-core/src/application/admission/gap_fit.rs", "crates/orbit-core/src/application/workspace_auto/classify.rs", "crates/orbit-core/src/application/pull_drain/refill/**", "crates/orbit-core/src/runtime/host_resource/**", "crates/orbit-store/src/driver/sqlite/admission_load_store.rs", "crates/orbit-config/src/registry/settings/machine.rs", "crates/orbit-core/src/application/distributed/work_fit.rs", "crates/orbit-store/src/contracts/task/coordination/admission.rs"]
related_features: [auto-concurrency, distributed-drain]
related_artifacts: [ORB-15757, ORB-15774]
---

# Auto-concurrency — Design

> **Status: Draft proposal; rollout steps 1 (Record, §2) and 2 (Profiles
> report, §3) are implemented, and so are the load forecast (§4), the host
> admission lock (§5.2), local-drain steps 3 and 4 (Shadow and Enforce),
> and pull-drain admission (§6). Local drains implement gap fit (§5.1) and
> the starvation guard (§5.3), and record each pass's forecast for the
> accuracy report (§8). macOS records the same minutes from the worker's
> process tree (rollout step 5).** §1 describes today's code and cites it,
> with `mode = off` still the default. §2 describes the recording that ships,
> §3 the profiles `orbit run profiles` reports, §4 and §5 the forecast and
> local admission, §6 pull admission, the `drain_last_pass`, `orbit run show`,
> `orbit run readiness` and `orbit run profiles --accuracy` rows of §7 the
> record and diagnostics a local pass exposes, and §8.1 the accuracy report.
> The per-drain mode controls of §7 are implemented. The dashboard Drain card
> shows that recorded forecast; its curves against measured load remain proposed.

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
task (`pull_drain/refill/mod.rs`, `pull_drain/drain/refill_pass.rs`).

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
  a time series. History is keyed by resource and its high/resume threshold
  pair: workspace throttle overrides affect that workspace's admission,
  while identical thresholds share sustain windows and holds. The schema-1
  envelope remains readable by older binaries, whose resource-only lookups
  start fresh; matching legacy history is carried forward on upgrade.

**Per-task resource accounting.** On Linux, each detached worker runs in its
own systemd user scope, `orbit-worker-<run_id>-<nonce>.scope`
(`application/job/pipeline/worker/scope.rs`). `machine.worker_containment`
defaults to true. Orbit reads the scope's `memory.events` and `pids.events`
to tell a limit breach from other failures. A worker locates its own scope
(`WorkerScopeCgroup::of_current_process`) and records its CPU and memory per
minute (§2). Admission reads none of it yet. macOS has no scopes, so its
workers run uncontained and record the same minutes from the process tree
(§2).

**No-diff work still builds today.** Owner-run pipelines skip validation
for a `no-diff-expected` task with a clean tree: `task_pr_pipeline.yaml`'s
`validate` step is gated on `skipped_no_diff_expected`. Two routes still
build anyway:

- **Claimed route.** It runs the owner's required validation commands on the
  clean base (`task_claimed_pr_pipeline.yaml` `validate_no_diff`, then
  `orbit-engine/src/executor/automation/vcs/claim/no_diff.rs`). The owner
  refuses a handoff without those logs
  (`orbit-store/src/repository/task/coordination/handoff/evidence.rs`). In ws_orbit
  that means a full `cargo check` of the workspace for every claimed review.
- **Instructions.** On every route, the implementer prompt
  (`assets/activities/agent_implement.yaml`) and the repository gates tell
  the agent to run every workspace check before handoff, with no exemption
  for the tag.

[ORB-15764] and [ORB-15765] fix those two. The CPU-light rule also counts
build-heavy QA sweeps as light, because they carry the tag; [ORB-15766]
fixes that.

**History.** Each host keeps one SQLite database, `~/.orbit/orbit.db`, shared
by its workspaces. The `job_runs` table holds each run's start, end and host.
The task id is in `input_json`, and complexity is in the task index. The
`run_resource_minutes` and `host_resource_minutes` tables hold 30 days of
per-minute CPU and memory history (§2).

## 2. Sampling each task

A worker samples **its own** scope, so attribution is exact and no central
sampler is needed.

Implemented in `application/job/pipeline/worker/usage.rs` (minute
aggregator) and `worker/usage_tree.rs` (macOS process tree), with the work
class in `worker/delivery_class.rs`, the host reads in
`runtime/host_resource/usage.rs`, and the macOS walk in
`orbit-common/src/process/ancestry.rs`.

- **What.** Every 5 s, a background thread in a delivery run's worker reads
  its scope's `cpu.stat` `usage_usec` and memory. CPU is measured in cores
  (CPU time over wall time).
- **Memory is non-reclaimable bytes:** `memory.current − file + shmem` from
  `memory.stat`. `memory.current` also counts page cache charged to the
  scope, which the kernel can drop. `file` includes tmpfs and shared memory,
  which it cannot, so `shmem` is added back; box tests use tmpfs. Live
  dk-server-1 scopes on 2026-10-12 had 25–68% of `memory.current` in `file`:
  across 8 scopes `memory.current` summed to 25.7 GB against 15.5 GB of anon
  on a 27 GB host.
- **Granularity.** Samples are aggregated per minute since the worker
  started: mean cores and maximum memory for that minute. Maximum, because a
  rustc or linker peak that lasts a few seconds is what triggers the OOM
  killer. A minute is written as soon as it ends, and the last, partial
  minute when the run ends. CPU counter deltas belong to the interval between
  readings, and are credited before completed minutes are written. An interval
  crossing one or more boundaries is split proportionally by wall time,
  conserving its total CPU time; intra-interval activity is not observable.
  Memory and sample counts remain point readings in the minute containing
  their timestamp, so a minute crossed without a reading has no memory peak
  observation and zero samples.
- **Storage.** Each minute is a row of `run_resource_minutes` in `orbit.db`,
  keyed by workspace, run id and minute index. It also carries the task id,
  the minute's start, the delivery's admission time, work class, complexity
  and a `resumed` marker. A worker that restarts a run continues after the
  minutes already recorded. The newest minute is also how admission learns
  each running task's current use (§4).
- **Work class, once per delivery.** The first submission of a delivery
  pipeline that names one task records its class (§3.1), complexity and
  admission time under the run input's `resource_class` key; every child run
  of the delivery naming the same task inherits it, so a tag edit mid-delivery
  changes nothing. A fresh submission always recaptures it. A claimed leaf is
  created by the pull store, so its worker captures the class at start, from
  the local task and the receipt's complexity.
- **One delivery, several runs.** A local delivery is a chain of runs:
  `task_auto_pipeline`, then `task_gate_pipeline`, then `task_pr_pipeline`. A
  pull delivery is one `task_claimed_pr_pipeline`. Each run samples itself.
  A delivery's series is the per-minute sum over all runs of that task.
  Minutes are counted from the moment the drain admitted it, which is the
  admitted run's `created_at`.
- **Exclusions.** Resumed or retried deliveries restart their clock part way
  through, so they are kept out of profiles. They still count toward the
  current load. Rows are marked `resumed` for a resume or replay submission,
  a retry run, an adopted run, a run resuming checkpoints or a carried
  candidate (`resume_candidate`, `claim_repair`), and a run an earlier worker
  already sampled.
- **Retention.** 30 days for both tables, whatever `retention.runs_days` says.
  `orbit gc runs --apply`, and so the `store-gc` routine, prunes them
  host-wide.
- **macOS process tree.** macOS has no scopes. Every 5 s the worker sums
  its own process tree (`process_tree` and `TreeCpuAccount` in
  `ancestry.rs`). A process's total is its own CPU plus the CPU of the
  children it has waited for (`ri_child_user_time` and
  `ri_child_system_time`, which carry what those children had reaped in
  turn). CPU time is the non-negative growth of the total of each process
  seen in both samples, plus the whole total of a process that appeared
  since the previous sample. A short-lived compiler, linker or test process
  that starts and is reaped between samples is charged through its parent's
  reaped total. One a sample saw alive arrives in its parent's reaped total
  too, so each process that left the tree is looked up again by pid and
  start time. If it is gone, it exited: its last observed total becomes owed
  by the member that listed it, or, when that member exited as well, by the
  nearest ancestor still in the tree, since a reaped process's total carries
  what it had reaped. Growth of the owing member's reaped total pays the
  debt instead of being charged, so the sampled child is counted once and a
  reap that lands samples later (the walk reads a parent before its
  children) is still matched. If it is alive under the same parent, the walk
  missed it and it stays tracked. If it is alive under another parent, it
  was reparented away: what it ran in the tree stays charged and it owes
  nothing. An exited process nobody has waited for stays listed and
  readable as a zombie, so one that left the walk was reaped before the walk
  reached it, and a reap by its owner shows in the owner's reaped total in
  that sample or at the owner's next sighting. A debt settles only at those
  two sightings, and whatever is left is dropped. Each climb restarts the
  window, so a nested reap that lands a sample later one level up is still
  matched. Some debts are never paid, because the descendant was reaped
  outside the tree: its parent exited without waiting for it (a tool timeout
  kills a build, the shell under the agent dies and the agent reaps it, and
  `launchd` reaps the build; or a dropped child handle whose parent exits
  after it), its parent sets `SA_NOCLDWAIT`, or it was reparented and exited
  within one interval. Until the window closes, such a debt absorbs the
  owner's unrelated reaped growth, so the tree undercounts by at most that
  growth over two sightings, and never by more than the debt.
  The first sample is a baseline, so a worker's lifetime before that does
  not land in the first minute. Each process is charged only
  when its start time and parent read the same before and after its usage
  read, and while the parent that listed it still has the same start time.
  A reused or reparented pid is left out of that sample. Memory is the sum of
  `ri_phys_footprint` (`proc_pid_rusage`), the non-reclaimable footprint, not
  resident size: resident size counts file-backed and shared pages and would
  overstate the tree the way `memory.current` does. A tree that cannot be
  read (a sandbox denial, for example) skips that sample and leaves the
  worker's run outcome unchanged.
- **No scope, no samples, on Linux.** A Linux worker without a scope
  records nothing: containment disabled, or no user manager. That host stays
  on today's admission (§5.4) until it has samples.

Workers also keep a host series, `host_resource_minutes`, holding host CPU
and memory per wall-clock minute in the same units, using the shared
`runtime/host_resource/usage.rs` reader for history and forecast baselines.
Linux measures busy cores from `/proc/stat` tick deltas (excluding idle and
iowait, without double-counting guest), and `MemTotal − MemAvailable` bytes.
It does not use `/proc/loadavg`, which the throttle reads: load counts the run queue and runs
far past 100% (load 36 on 15 cores at about 95% utilization on dk-server-1).
Each worker offers every wall-clock minute it observed whole, and the first
insert for that minute wins. Busy and total tick deltas are split across
wall-clock boundaries by the same elapsed-time rule as worker CPU, including
intervals spanning multiple minutes. The initial host minute is omitted unless
a preceding reading covers its beginning; the final partial minute is omitted.
Profiles do not need it. It is the forecast baseline's settled
minute (§4) and the ground truth for checking the forecast (§8).

On macOS the reader uses `host_statistics(HOST_CPU_LOAD_INFO)` for the
host-wide user, system, idle and nice tick counters. Each 32-bit counter's
wrapping delta is computed separately; busy is user + system + nice, and
utilization in cores is `busy_delta / total_delta × online_CPU_count`.
`sysconf(_SC_NPROCESSORS_ONLN)` supplies the actual online CPU count. No tick
advance means unknown CPU, not zero; a failed CPU, VM or capacity read means
an unknown host sample. The admission probe waits at least 1.1 seconds outside
the admission lock for its initial/recent baseline, crossing XNU's one-second
statistics cache. Worker history samples every five seconds and applies the
same tick deltas. A changed CPU count invalidates the interval.

Native memory is `hw.memsize − min(hw.memsize, (free_count + inactive_count)
× sysconf(_SC_PAGESIZE))`, in bytes, from `host_statistics64(HOST_VM_INFO64)`
and `sysctlbyname("hw.memsize")`. This is the throttle's existing estimate of
non-reclaimable physical memory: free includes speculative pages, inactive is
treated as reclaimable, and purgeable overlaps active/inactive so is not
subtracted again. Compressed physical storage remains in used memory; logical
uncompressed pages and swap are not added. This estimate is not Activity
Monitor's Memory Used or the sum of worker footprints. Both native users
release each acquired Mach host send right. See Apple's
[host statistics implementation](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/host.c)
and [VM statistics definition](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/mach/vm_statistics.h).
Explicit injected proc roots still read only their fixture files on every OS;
a missing fixture never falls back to the live host.

## 3. Profiles

### 3.1 Work class

Complexity says how big a task is, not what kind of work it does. A review
or audit task reads and reports. It neither compiles nor runs tests. A code
change does both, and its size scales with complexity. Averaging the two
together would describe neither. Profiles are therefore keyed by **work
class** first:

| Work class | Derived from | Profile key |
|---|---|---|
| `auto-task:<template>` | the task's `auto-task:` tag | `(host, workspace, template)`; a recurring template does the same job every time, so complexity adds nothing |
| `no-diff` | the `no-diff-expected` tag | `(host, workspace, no-diff, complexity)` |
| `change` | everything else | `(host, workspace, change, complexity)` |

The template is the most specific key, so it wins. A template whose tasks
carry `no-diff-expected` falls back to `no-diff`, and any other template
falls back to `change`.

The class is read from the task's tags at admission and stored with the
delivery's samples, so a later tag edit does not move history between
profiles. A template's base class is not stored: it comes from whether the
newest delivery's task in the profile's window carries `no-diff-expected` in
the host's task index.

### 3.2 No-diff work must not build

The work class is a grouping key, not a promise about cost: a `no-diff`
profile is measured like any other. A no-diff task that compiles or runs
tests is a defect in the pipeline or the task's instructions. It is not a
cost to learn. Its profile makes the defect visible.

`orbit run profiles` flags every `no-diff` profile, and every template
profile whose tasks are no-diff, that shows build-like load: more than one
core sustained for two minutes. It names the deliveries behind the flag.
Until [ORB-15764] and [ORB-15765] land, every claimed review will be flagged
(§1). Templates that build by design are exempt from the flag through one
declaration, `auto_task_builds_by_design` (`qa-sweep` and `qa-full-sweep`,
in `orbit-types/src/workflow/auto_task/definition.rs`), which the CPU-light
rule shares. The
existing CPU-light admission rule (`is_cpu_light` in
`admission/cpu_light.rs`) trusts no-diff-expected only together with an
auto-task tag. Its note: "an operator's claim about the diff, not about the
work". Profiles need no such trust, because they record what the work
actually used.

### 3.3 The profile

Worker minutes now carry nullable `cpu_accounting` provenance. New Linux
minutes use `linux-cgroup-v1`; new Mac minutes use `macos-mach-ns-v2`.
The Mac sampler converts each process's own and reaped-children user plus
system Mach absolute ticks using the host's `mach_timebase_info` ratio, with
wide integer arithmetic, and combines them across samples as §2 describes.
An unavailable/zero timebase or a result beyond `u64` invalidates the
sample, preserving the unreadable-tree behavior. Mac minutes stamped
`macos-mach-ns-v1` counted only the own CPU of processes alive at a sample
and missed reaped descendants, under-measuring a building delivery several
times over, so profiles exclude their deliveries on every platform. Every
Mac delivery recorded before this change carries `macos-mach-ns-v1` or no
provenance, so the exclusion empties every Mac profile at upgrade: Mac
admission starts cold, as on a host without profiles (§5.4), and profiles
return only as `macos-mach-ns-v2` deliveries accumulate.
Apple's [fill_task_rusage implementation](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/bsd_kern.c)
copies task power totals into these counters; their units must not be assumed
to be nanoseconds. For example, a 125/3 host needs its actual ratio.

The bounded native calibrations live in the existing `orbit-common` test
binary and are ignored by recurring gates. Run the first explicitly with
`./scripts/build-budget.py -- cargo test -p orbit-common --lib process::tests::ancestry::native_cpu_time_agrees_with_getrusage -- --ignored --exact --nocapture`.
It isolates one process, targets 250 ms of single-thread wall time, and compares
the public tree's own CPU delta with `getrusage(RUSAGE_SELF)`. The documented
tolerance is 5% plus 5 ms for sequential readings, with at least 100 ms of CPU
work required. Its JSON output retains the actual timebase, raw tick delta,
corrected nanoseconds, reference delta and error.
`native_tree_cpu_counts_reaped_descendants_once`, run the same way, checks
reaped descendants: an isolated sampler samples its tree every 250 ms around
a child that runs 24 sequential grandchildren of about 100 ms each, then
around a 1 s child sampled alive at least twice. After each reap, the tree
CPU beyond the sampler's own must be within 10% plus 20 ms of
`getrusage(RUSAGE_CHILDREN)`. Its JSON output records both and the sample
counts.

The additive storage migration leaves every historical counter and its
nullable provenance unchanged. On Mac, profiles exclude the entire delivery
if any minute lacks recognized provenance, including otherwise valid sibling
runs. Unknown and superseded (`macos-mach-ns-v1`) provenance is excluded on
every platform. Legacy unmarked
Linux minutes remain eligible on Linux because their cgroup units did not
change. This compatibility rule supports host-local history; importing
unmarked Mac history into a Linux store is unsupported. Raw minute reads
continue to expose excluded evidence. There is no backfill, historical
relabeling, or universal multiplier: old Mac timebases were not recorded,
so those deliveries age out under the existing retention and corrected
deliveries rebuild the profile window. Already recorded forecasts remain
historical evidence. This changes profile inputs only; admission mode and
the shadow98 rollout remain unchanged.

A **resource profile** for one key is built from that key's most recent
completed deliveries: the last 50, within 30 days.

For each minute `m` since admission, the profile holds:

| Field | Meaning |
|---|---|
| `alive(m)` | fraction of deliveries still running at minute `m` |
| `cpu(m)` | mean cores used at `m`, over the deliveries alive at `m` |
| `mem(m)` | 90th-percentile memory at `m`, over the deliveries alive at `m` |
| `mem_mean(m)`, `mem_sd(m)` | mean and standard deviation of that memory |

CPU uses the mean and memory a high percentile because the costs differ. Too
much CPU only slows everyone down. Too much memory kills a process. The
memory mean and spread let the forecast sum several deliveries' memory as a
quantile of the sum (§4.1) rather than adding their percentiles.

`alive` gives the duration distribution. The candidate's **horizon** `H` is
the minute at which `alive` falls below 0.1, the 90th-percentile duration,
capped at 120 minutes.

Implemented in `runtime/host_resource/profile/` (`build.rs`, `lookup.rs`,
`cache.rs`) and read through `OrbitRuntime::resource_profiles` and
`resource_profile_for`:

- **Delivery.** The runs of one task sharing one `delivery_admitted_at`. Its
  series sums their minutes, each placed at its whole minutes since that
  admission, and it is alive until its last run's `finished_at`. A delivery
  with a resumed run, a live run, or a run whose `job_runs` row is gone is
  left out.
- **Spread.** Each minute also keeps the standard deviation of CPU and memory
  over the deliveries alive then. A profile's spread is the summed deviation
  over the summed mean before the horizon, for CPU and memory: near 0 means
  the deliveries agree, near 1 or above that one minute's mean says little.
- **Heaviest** means the most expected core-minutes before the horizon,
  `Σ cpu(m)·alive(m)`, then the highest memory peak.
- **Caching.** Profiles change only when a delivery finishes, so they are
  cached in memory across passes and runtime clones, not just for the pass.
  Each read runs a cheap freshness probe, the newest `finished_at` and count
  of finished sampled runs, and rebuilds when it or the
  `[machine.auto_concurrency]` bounds change. Pruning changes the count too.
  Only profiles are cached: the forecast's live inputs (running deliveries,
  reservations, baseline) are read fresh every pass.

A key with fewer than 10 deliveries falls back along this chain, stopping at
the first key that has enough. Each lookup reports which step answered:

1. drop complexity: `(host, workspace, class)`;
2. for a template, its base class: `no-diff` or `change` in the same
   workspace, rolling up both template and non-template deliveries of that
   base class;
3. the heaviest profile of the same class on the host (for a template, of
   its base class), including template profiles with that base class;
4. the heaviest profile on the host, so unknown work is assumed expensive;
   when every profile is sparse, the heaviest sparse one;
5. no profile at all, only on a host with no profiles: today's admission.

The chain never crosses from `no-diff` to `change` until step 4. A new review
task borrows another review profile before it is assumed to build.

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
- **Baseline `B`.** The load that isn't from tasks, such as an IDE or a
  manual build, held flat over the horizon. The host and the deliveries are
  measured over the same window: the newest recorded host minute minus what
  the runs used in that minute, floored by the live reading over the
  trailing minute. Only with no recent host minute is it the live reading
  minus the deliveries' measured use now.
- **Host-wide.** The sum runs over live leaves of **every** workspace on the
  host: the `job_runs` table without the workspace filter. Pinned drains,
  explicit `orbit run ship` deliveries and before-landing reviews are all
  included.
- **Reservations.** An admission that has been decided but whose run does not
  exist yet is a reservation (§5.2). It is counted from `t = 0`.
- **Memory sums as a quantile.** For memory, the sum over profiled
  deliveries and reservations is a high quantile of their summed memory, not
  the sum of their 90th percentiles (§4.1).

Implemented in `runtime/host_resource/forecast/` (`build.rs`, `host_load.rs`,
`memory.rs`) and built only under the host admission lock (§5.2):

- **Running set.** Live (`pending`, `running`, `retrying`) runs of the
  delivery jobs (`runtime/host_resource/delivery.rs`) in every workspace,
  grouped per workspace and task. A delivery's class and admission time come
  from its run input's `resource_class`, else its newest minute; a run with
  neither is assumed to be `change` work admitted when the run was created.
  `a_i` is whole minutes since admission. A delivery's current use is the sum
  of its live runs' newest minutes.
- **Flat tail.** A delivery past its profile's last minute, or with no
  profile at all, contributes its current use at every minute.
- **Baseline** (`baseline.rs`). The lock also reads the last
  `BASELINE_HISTORY_MINUTES` (5) of `host_resource_minutes` and of every
  workspace's `run_resource_minutes`, with each run's `finished_at`.
  - *Settled minute.* The newest recorded host minute that ended at most
    3 minutes ago and that every still-recording run covers: each live run's
    newest minute reaches its end, and no live run without a minute had
    started by then. A run is still recording while its newest minute ended,
    or it started without one, within the last 90 s; past that its next
    minute is not waited for. From the minute's CPU mean and memory peak,
    every run minute overlapping it, live or finished, is subtracted in
    proportion to the seconds it covered. A run minute covers from its start
    for its sample count times the 5 s sample interval, at most 60 s, and
    never past its run's `finished_at`: a run at 4 cores that ended 5 s into
    its last minute subtracts about 0.3 cores for that minute, not 3.3. A
    250 ms spike in the live reading does not move this value.
  - *Live floor.* So load that began after the settled minute is not
    invisible, `B` is the larger of the settled value and a live value. CPU
    uses the probe's sustained reading: busy cores since the newest reading
    it keeps that is 60 to 120 s old (one is kept every 5 s or more), minus
    the run minutes over that window, each live run's newest minute extended
    to now. A spike moves it by its length over the window's, 250 ms in 60 s
    at most. It needs a reading that old in the same process, so a drain that
    admits at least every two minutes has one, and a one-off command does
    not: there `B` is the settled value. Memory uses the live reading minus
    the running deliveries' current use. The settled minute ends at most
    about two minutes before now, the bound on how late `B` sees new load
    with no floor.
  - *No settled minute.* No worker recorded one lately (with no delivery
    running, or without worker containment on Linux), or a run blocks every
    recent one: `B` is the live reading over the probe's short window, at
    least 250 ms for Linux/proc fixtures or 1.1 s for macOS, minus the
    running deliveries' current use, as before. The first call waits before
    taking the lock.
  - *Delivery with no minute yet.* Its profile is added in full while the
    live reading already holds its startup load. The live values subtract its
    profile's expected use at its age: over the part of the sustained window
    it was running, else in proportion to the share of its first minute that
    has elapsed, so one created a moment ago subtracts almost nothing. Where
    a settled minute exists it ended before such a delivery started, so the
    subtraction can at most bring `B` down to the settled value: it is capped
    at what the host grew by since. A run that never records (no containment)
    stays in the settled value, as an unmeasured load.

  Memory uses the shared native accounting described in §2. `B` never goes
  below zero. An unreadable host, or no settled minute with a CPU reading
  that saw no ticks since the previous one, leaves that baseline unknown, and
  the series counts it as zero. The forecast reports the host minute `B` was
  measured over as `baseline_minute`.
- **Output.** `HostLoadForecast`: the CPU (cores) and memory (bytes) series
  for `t = 0 … horizon_cap_minutes`, each with its baseline and its peak and
  minute, plus the running deliveries and reservations with the profile step
  that described each, and the profiles a caller looks candidates up in.

### 4.1 Summed memory

Each delivery's `mem(m)` is its own 90th percentile. Adding those for `N`
deliveries forecasts the case where every one of them is at its 90th
percentile at once, which is far rarer than one in ten, so the over-statement
grows with `N`. On dk-server-1 on 2026-10-17, four deliveries measured
4.4 GiB on average and 9.4 GiB at most, against a sum of percentiles of about
15.6 GiB for five or six, and admission was held at `admit_memory_percent`
with 24 GiB available.

At minute `t`, each profiled delivery or reservation `i`, still running with
chance `s_i` (`alive_i(a_i + t) / alive_i(a_i)` for a running delivery,
`alive_r(t)` for a reservation or candidate), contributes its percentile
`p_i = mem_i · s_i`, its mean `μ_i = mem_mean_i · s_i`, and the variance of
"running with chance `s_i`, then drawn from that minute's deliveries":
`v_i = s_i · mem_sd_i² + s_i · (1 − s_i) · mem_mean_i²`. Treating the
deliveries' memory as independent, their sum is about normal, and

```
F_mem(t) = B + held flat + clamp(Σ μ_i + z · sqrt(Σ v_i), max p_i, Σ p_i)
```

with `z = 1.645`, the sum's 95th percentile, in whole bytes. The clamp keeps
one delivery at exactly its 90th-percentile profile value, never puts `N`
deliveries below the largest of them, and never forecasts more than the sum
of percentiles did. The baseline and deliveries held at their measured use
stay exact. A pending pull counts, at each minute, the alternative with the
most `p_i` as one delivery (§5.2).

**Why memory stays more conservative than CPU.** CPU sums each delivery's
mean, `Σ cpu_i · s_i`: an over-forecast only slows everyone down. Memory sums
to a high quantile, at least the largest single delivery's 90th percentile,
because too much memory kills a process. The quantile sits `z` standard
deviations of the sum above its mean, so wider profiles forecast more, and
the term `s_i · (1 − s_i) · mem_mean_i²` adds the chance that a delivery
expected to have finished is still running.

**Evaluation.** Chosen by replaying the recorded forecast/actual pairs of
dk-server-1's store (`orbit run profiles --accuracy`) on 2026-10-17: each
recorded point kept its baseline and other terms, and the sum of the
replayed profiled deliveries' percentiles was swapped for the quantile. The
replay rebuilt profiles from the deliveries finished before each forecast
and reconstructed the recorded forecasts with a median residual of
−0.09 GiB. In the enforce cohort, the only one with two or more profiled
deliveries in a forecast, the +5m mean memory over-forecast fell from
+6.34 GiB to +4.01 GiB, its worst from +18.5 GiB to +9.8 GiB, and
under-forecast minutes stayed at 8.3%. `z = 1.282`, the sum's 90th
percentile, under-forecast 8.7% at +5m but 14.7% at +10m against 10.0%.
The earlier shadow cohort rarely had two profiled deliveries at once and
barely moves: +1.01 GiB and 25.0% under at +5m, before and after.

## 5. Admission

### 5.1 Gap fit

`select_admissions` already walks candidates in priority order and builds the
wave one task at a time. Gap fit adds one more check per candidate. The
candidate, with profile `P_c` and horizon `H_c`, joins the wave only if, for
every `t` in `0 … H_c`:

```
F_cpu(t) + P_c.cpu(t) · alive_c(t) ≤ admit_cpu_percent    × host cores
F_mem(t) with P_c in its summed quantile ≤ admit_memory_percent × host memory
```

The candidate joins the memory quantile of §4.1 as one more delivery with
`s_c = alive_c(t)`; alone, it is `P_c.mem(t) · alive_c(t)`.

When a candidate is admitted, its profile is added to `F` before the next
candidate is checked. One pass can therefore admit several tasks, each
counting the ones before it. When a candidate does not fit, it is recorded
with the minute and resource that failed, for example `cpu 94% at +6m`, and
the walk continues: a smaller task may fit the gap.

**Idle host.** While `F` holds no running delivery and no reservation, and
the pass has admitted nothing yet, a candidate that does not fit is admitted
anyway. Nothing on the host would make room for it by finishing, so a
refusal would last for good: a profile larger than a threshold on its own, as
a pessimistic fallback of §3 often is, or one pushed over only by the
baseline `B`, would never fit. Once admitted it counts in `F` like any other,
so the next candidate in the pass is judged against it.

Every existing gate still applies, and gap fit only removes candidates:

- the drain's maximum concurrency;
- conflicts between tasks admitted together;
- the footprint guard;
- provider limits and crew exclusions.

**The resource throttle stays the backstop.** When it holds, nothing is
admitted, whatever the forecast says. This is what makes it safe to admit
into a host that is busy now but forecast to be quiet when the task's load
arrives. If the forecast is wrong, real pressure stops further admission.

Default thresholds are CPU 90% and memory 75%. The memory threshold must stay
below the throttle's memory high mark (90%), so forecast admission and the
throttle do not fight each other. The CPU threshold is forecast utilization
of the host's cores; the throttle's CPU mark is a Linux load average, which
counts the run queue and can exceed 100%, so the two are not compared.

Implemented for local and pull drains, with local passes in `admission/gap_fit.rs`, over
`HostLoadForecast::gap` (`runtime/host_resource/forecast/fit.rs`):

- **Where.** `select_admissions` asks gap fit about each candidate that
  passed the ceiling, the footprint guard and the conflict checks. A
  candidate it refuses is recorded by gap fit, not as a deferral, and the
  walk continues.
- **Modes.** `off` builds no forecast. `shadow` builds it and selects the wave
  with gap fit under the host lock, records that, writes no reservation, and
  admits the wave `off` selects. `enforce` admits the gap-fit wave and
  reserves each task it admitted in the same lock; a candidate the pull
  admission section refuses afterwards keeps its reservation until it
  expires.
- **No lock without a choice.** A pass with no free slot, because of the
  ceiling or the throttle, or with no candidate, builds no forecast and takes
  no lock.
- **Lookup.** A candidate's profile is looked up by its work class from its
  tags now, its complexity, and `no-diff-expected` for a template's base
  class.
- **Unmeasured host.** A host whose cores or memory cannot be read has no
  threshold to fit under, so every candidate fits, as on a host with no
  profiles.
- **Idle host.** `ForecastGap::fit` applies the idle-host rule for local and
  pull drains alike (§6). An admitted candidate, profiled or not, ends the
  idle state for the rest of the pass.
- **Record.** `drain_last_pass.admission_forecast` holds the mode, the CPU
  and memory peaks of the forecast before the pass admitted, as values and
  percentages of the host, the admitted and skipped candidates with their
  profile source, and each skip's resource, minute, percentage and detail
  (`cpu 94% at +6m`). In `enforce` a skip is also a `gap_fit` deferral in
  `deferred`. A candidate the idle-host rule admitted carries `idle_host`:
  its first miss and whether its profile made it alone or only with the
  baseline, e.g. `cpu 125% at +3m on its own; admitted on an idle host`.
- **Waiting.** A pass whose candidates gap fit refused still has a pending
  backlog, so it sleeps `poll_sleep_seconds`. The forecast is per minute, so
  no gap it shows opens sooner than that poll.

### 5.2 Serializing drains on one host

Two drains on one host must not fill the same gap. Each takes a host lock
(an `IMMEDIATE` transaction on `orbit.db`) for this sequence:

1. Build the forecast.
2. Decide the admissions.
3. Write `admission_reservations` for the admitted capacity, holding the task
   id (or `pending:<request_id>` for a pull), profile keys and time.
4. Release the lock.

A reservation leaves the forecast once its run exists in `job_runs`, or after
five minutes if dispatch failed. The lock covers only computation over local
tables, so it is held for milliseconds.

Implemented as `OrbitRuntime::admit_against_forecast`, over the store's
`admit_under_host_lock`:

- **One entry point.** The caller passes its decision. It gets the forecast
  and returns its own outcome and the tasks it admitted, each with the profile
  lookup key it chose. The decision runs while `orbit.db` is locked for
  writing, so it must not call the store, the network or a subprocess.
- **Fresh reads.** Each call reads the running set, the reservations and the
  host load inside the transaction. Only profiles come from the profile cache,
  read before the lock.
- **Reservations.** Rows of `admission_reservations` (store schema v47) hold
  the workspace, task id, work class, complexity, base class and time. A
  reservation's run is a delivery run in its workspace that names its task and
  was created at or after the reservation. Rows older than five minutes are
  deleted when the next lock is taken. A pending pull holds the alternatives
  in its final admissible set under one `pending:<request_id>` key. Its CPU
  contribution is the maximum at each minute across those alternatives, and
  its memory the alternative with the most 90th-percentile memory at that
  minute, counted as one delivery in the quantile (§4.1), not their sum:
  exactly one task can be granted. This covers different CPU and memory
  peaks without assuming one profile dominates both.
- **Exchange and release.** The store can remove reservation keys, read the
  remaining load and write replacements in one host transaction. A returned
  claim replaces its pending key with the task's precise key before creating
  a leaf; the queued leaf inherits that profile key and clock. Idle, confirmed
  refusal, and abandonment/cancellation remove pending capacity. No host
  write transaction spans an owner call.

### 5.3 Starvation

A gap that small tasks keep filling can block a heavy task forever. When the
first candidate in priority order has failed gap fit for longer than
`head_max_wait_minutes` (30), the drain admits nothing else until that
candidate fits.

The head waits only on running deliveries and reservations: a candidate is
refused only while `F` holds one (§5.1, idle host). Deliveries end and
reservations expire, so a head too large for the thresholds, even one larger
on its own, is admitted on the first pass after the host has neither. Work
behind it is then judged against its reservation, not held by the guard.

Implemented with the head as the first candidate gap fit judges in a pass, so
one a lock or the footprint guard holds back is not the head. Its
`failing_since` is carried in `drain_last_pass.admission_forecast.head` from
pass to pass while it keeps failing, and through passes that judge no head
while it is still pending. A head that fits, or leaves the backlog, ends the
wait. Smarter backfilling is an open question
([3_vision.md §1](./3_vision.md#1-open-questions)).

**Stuck head.** A protected head can still idle a drain for a long time when
its miss never improves: a memory profile near the limit on a host whose
baseline `B` stays high, or a stale `heaviest_*` fallback profile. The drain
reports such a head; it does not bypass it, and nothing about admission
changes.

- **Miss.** Each pass that judges the head records its miss in
  `head.miss`: the profile source and key (`change/medium`), the failing
  resource, minute and percentage, and that resource's baseline `B` as a
  share of the host. Passes that judge no head carry it.
- **Stall.** From the first protected pass, `head.stall` counts consecutive
  protected passes against a reference miss. A pass whose miss improves on
  the reference (another resource fails, it fails at a later minute, or by at
  least one percentage point less, so baseline noise does not count) becomes
  the new reference and the count starts again from one.
- **Stuck.** Once the count reaches `head_stuck_passes` (20, about ten
  minutes at the default 30-second poll, after the 30-minute wait), the stall
  is `stuck`. An `enforce` drain files one friction (tag `automation`) naming
  the drain, the head task, its profile source and key, the miss and the
  baseline, and keeps its id in `stall.friction_id`; a `dedupe-key` line keyed
  on the drain, the head and the stall's start keeps a retried pass from
  filing twice. The drain persists the pass and stall start before filing,
  then saves the returned friction id. If the first write fails, reporting
  waits for a later pass without changing admissions; if the id write fails,
  retry reuses the durable stall start. This also covers `head_stuck_passes = 1`
  and progress resets, while a later distinct stall gets its own friction.
  `shadow` records the stall without filing, since it holds nothing.
- **While it lasts.** `orbit run show` and `orbit run readiness` print a
  `Stuck head:` line from the recorded pass. A head that improves, fits or
  leaves the backlog drops its stall and the line with it; the friction stays
  as the incident's record.

### 5.4 Without profiles

A host with no profiles admits exactly as today, under the throttle. Profile
gaps for individual keys are covered by the fallback chain in §3, which
assumes the worst, so the forecast never treats unknown work as free.

## 6. Pull drains

Implemented in `application/distributed/work_fit.rs` and the pull refill
(`application/pull_drain/refill/mod.rs`); the owner's filter is in the store's admission selection.

A follower does not pick its task; the owner does. So the follower cannot run
gap fit on a specific candidate. Instead, before every request, under the host
admission lock (§5.2):

1. The follower gap-fits (§5.1) every kind of work it could be granted
   against its forecast, each priced by its profile through the fallback chain
   of §3, by the local drain's rule, idle host included: while the forecast
   holds no running delivery or reservation, every kind fits:
   - each work class `no-diff` and `change` with each complexity, unset
     included;
   - each `auto-task:` template this workspace has a profile of on this host.
2. Under `mode = enforce` the request carries the fits as `admissible` on its
   `orbit.task.pull` request: `(work class, complexity)` keys and template
   names. When nothing fits, the drain sends no request, records
   `forecast_no_headroom` as its refusal, and polls again. The request identity
   is allocated before the fit. In the same transaction as the decision,
   pending capacity is reserved for the final admissible set (§5.2), before
   the decision lock is released or the owner is contacted.
3. The owner skips a ready or repair candidate outside the list. A template
   task is inside it when its template is listed; any other task when its
   work class and complexity are. Each skipped task is returned in the
   receipt's `capacity_unavailable`, which the follower records among its
   excluded backlog as `capacity_unavailable`.
4. A claim that returns atomically replaces the placeholder on the follower
   with precise task capacity, keyed by the class of this replica's copy of
   the task. This happens before leaf creation, not after bind or launch.
   The leaf inherits that key and admission clock, and replaces the task
   reservation in the forecast immediately, even before its worker starts.
   Idle and confirmed refusals delete the placeholder. If local allocation
   fails or has no free slot, the unsent placeholder is deleted too.
5. A lost reply or an undecodable/unknown owner outcome retains the pending
   capacity until recovery or the five-minute TTL. Every carried unanswered
   request must reacquire/refresh capacity under the host lock before being
   re-sent with its original identity and admissible set. If the set no longer
   fits, it waits without sending or executing work. If a reply arrives after
   the TTL and another admitter filled the gap, the returned claim remains
   recorded but unlaunched until precise capacity fits. The owner is never
   unclaimed merely because the host has to wait. Confirmed cancellation or
   abandonment releases the placeholder; an unavailable owner leaves it to
   recovery/TTL.

Each request is one candidate against a fresh forecast. The drain records the
last fit as `pull_admission_fit` on its run state: the mode, whether it was
sent, the work that fits, and the minute and resource each unfit kind would
exceed, for example `change/hard: cpu 94% at +6m`.

`mode = shadow` computes and records the same fit, with `sent` false, and
requests as with `off`, writing no pending placeholder. Its returned claims
are reserved before leaf creation, so shadow forecasts count them; shadow
admission remains unchanged. With `off`, on a host with no profiles, or when the host's load cannot
be read, nothing is computed and the request carries no list; the owner then
selects as before.

The new field changes the distributed-drain protocol fingerprint, so an owner
on another binary refuses the pull as before.

A pass bounds its requests by the drain's live ceiling, an operator's
`orbit run concurrency` included. The forecast alone did not make the
submitted-ceiling quirk moot, because a drain with `off` forecasts nothing.

Limitations:

- A template this host has never run in the workspace is not listed, so under
  `enforce` the owner keeps it for another executor until this host has a
  profile of it.
- The owner classifies a task by its own tags, and the follower reserves it
  by its replica's. A replica that lacks the task reserves it as `change`.

## 7. Surfaces and configuration

| Surface | Proposed change |
|---|---|
| Machine config | Host defaults in `[machine.auto_concurrency]` in `crates/orbit-config/src/registry/settings/machine.rs`: `mode` (`off` / `shadow` / `enforce`, default `off`), `admit_cpu_percent` (90, forecast utilization as a percentage of host cores), `admit_memory_percent` (75, forecast non-reclaimable memory as a percentage of host memory), `profile_window` (50 completed deliveries), `min_profile_deliveries` (10), `horizon_cap_minutes` (120 minutes), `head_max_wait_minutes` (30 minutes), `head_stuck_passes` (20 passes, §5.3). `admit_memory_percent` stays below the global `workflow.resource_throttle.memory_high_percent`; CPU utilization is not compared with the throttle's CPU percentage because Linux throttle CPU is load-based and can exceed 100%. |
| `drain_last_pass` | Implemented for local drains (§5.1): an `admission_forecast` block with the peak forecast over the horizon, admitted and skipped candidates with the failing minute and resource and their profile source, and whether the head is being protected, with its miss and, once protected, its stall (§5.3) |
| `orbit run show` | Implemented: an `Admission:` line from the recorded pass, e.g. `forecast — skipped heavy candidate (cpu 94% at +6m)`, with candidate titles, shadow decisions labelled hypothetical, and a protected head named by the starvation guard; a `Stuck head:` line while that head is stuck (§5.3). After an idle pass, it names the last built forecast's timestamp, decisions and peaks, and says the current pass built none. JSON preserves `pipeline_state.drain_last_pass.admission_forecast` and retains built history in `pipeline_state.drain_last_forecast`. |
| `orbit run readiness` | Implemented: the last pass's host forecast peaks, headroom below the current admission thresholds, each judged candidate's profile source and wait, and a stuck head (§5.3). JSON adds `capacity.admission_forecast` and `tasks[].admission_forecast` for the current pass, plus `capacity.last_admission_forecast` for retained built history. Idle passes show that history without changing eligibility; without either recorded block, output is unchanged. See [observing admission](../../CONFIG.md#machineauto_concurrency--host-forecast-admission). |
| `orbit run profiles` | Implemented: the profiles per workspace, work class and complexity: deliveries (sparse below `min_profile_deliveries`), horizon, peak CPU and memory and when they occur, and the CPU and memory spread. Flags no-diff profiles with build-like load and names the deliveries behind each flag (§3.2). `--json` for the full series |
| `orbit run profiles --accuracy` | Implemented (§8.1): composable forecast-time `--since`, exact `--drain` and recorded `--mode` cohorts; mature measured pairs, pending and missing counts per offset, with missing split into unsampled (no delivery live) and gap (delivery live, no host row); shadow decisions and enforce admissions, gap-fit holds and pull holds per drain. `--json` includes the effective selection and raw pairs |
| Per-drain mode | Implemented for local drains: CLI `--auto-concurrency off|shadow|enforce`, registered MCP `orbit.workflow.auto` start `auto_concurrency`, and dashboard HTTP start `auto_concurrency` all use Core submission. At durable admission, explicit or configured mode is captured in `input.auto_concurrency` with `input.auto_concurrency_source`, retained on resume/restart. The loop forwards it to the existing classifier; thresholds, profiles, throttle, ceilings and accuracy lineage remain host policy. Stop/resize and explicit pull overrides are refused before effects. |
| Dashboard | Implemented: an operator launch choice of inherited host mode, off, shadow or enforce. The form reads current inherited host mode from readiness, sends the actual option, and shows the receipt's durable effective mode; the live window shows its captured mode. The Drain card shows the recorded forecast from readiness: its time, CPU and memory peaks against the current admit thresholds, headroom, admitted tasks with their profile source, and a candidate enforce held. A pass that built none says `no forecast this pass` and shows `last_admission_forecast` when readiness has one. Shadow decisions are labelled hypothetical. Forecast curves against measured load with running tasks stacked remain future work. |

`max_active_leaf_runs` keeps its meaning: the operator's ceiling, enforced
first.

## 8. Rollout

1. **Record.** Add per-task sampling and the host minute series. No change to
   admission. Profiles need a few days of deliveries.
2. **Profiles report.** `orbit run profiles`. It shows whether profiles are
   stable enough to forecast with: how spread out each minute is, and how
   spikes line up across deliveries. It also flags no-diff work that builds
   (§3.2), so those defects get fixed before profiles absorb them.
3. **Shadow.** `mode = shadow` computes forecasts and decisions every pass
   and records them, but admits as today. A report compares each forecast at
   +5, +10, +20 and +30 minutes with the measured host series, and the
   shadow decisions with what admission actually did.
4. **Enforce** on dk-server-1 first, a follower with the smallest blast
   radius, then on dk-server-2.
5. **macOS.** Implemented: a sampler sums CPU time and `ri_phys_footprint`
   over the worker's process tree, using
   `orbit-common/src/process/ancestry.rs`, and writes the same
   `run_resource_minutes` rows as the Linux scope sampler. The Mac admits
   as today until those rows exist; recording does not change admission.

### Native macOS verification

Run `scripts/build-budget.py -- cargo test -p orbit-core --lib
runtime::host_resource::tests::usage -- --nocapture` on macOS. The native row
reads real host ticks beyond the cache window, asserts finite core utilization,
a positive CPU count and coherent memory byte totals, and checks failed Mach
calls return unknown. The same matrix covers proc fixtures and independent
Mach counter rollover without generating host load. Existing sampler and pull
forecast/accuracy suites cover shared deltas and unchanged shadow admission.

For operational verification after the candidate is installed through the
normal delivery process, retain public `orbit run profiles --json`,
`orbit run show <pull-drain> --no-reconcile --json`, and
`orbit run profiles --accuracy --json` output from the native Mac workspace.
With eligible profiles carrying recognized CPU accounting provenance and
the existing shadow configuration, an ordinary real pull request should
record `pull_admission_fit` and forecast points; keep existing ceilings and
policy. Let real delivery workers sample
through complete host minutes and the earliest recorded offset (+5 minutes),
then retain a fresh accuracy report with measured pairs. A unit/fixture pass,
a worker-only measurement, or forecasts with no paired host minutes does not
satisfy this operational verification. New native Mac worker minutes carry
`macos-mach-ns-v2` provenance, which counts reaped descendants. Historical
unmarked and `macos-mach-ns-v1` Mac minutes remain stored but their
deliveries are excluded from profiles; this host-reader change does not
rescale, backfill or relabel them.
Linux eligibility and raw history retention remain unchanged.

### 8.1 The accuracy report

Implemented for local and pull drains, with local passes in `workspace_auto/classify.rs` (recording),
`runtime/host_resource/forecast/record.rs` (the recorded points) and
`runtime/host_resource/accuracy.rs` (the report), read through
`OrbitRuntime::forecast_accuracy` and shown by
`orbit run profiles --accuracy`:

- **Record.** Every pass under `shadow` or `enforce` that built a forecast
  writes one `admission_forecasts` row (store schema v48): the drain's
  workspace and run, the forecast time, the mode, the host's cores and
  memory, and the forecast at +5, +10, +20 and +30 minutes for CPU and
  memory. An offset past `horizon_cap_minutes` is not recorded. The row also
  holds the tasks the pass admitted after the pull admission section, and
  the tasks gap fit admitted and skipped; under `shadow` those differ. A
  pass that built no forecast, because no slot or candidate was free, and
  every pass with `mode = off`, records nothing. Recording is best effort,
  like `drain_last_pass`.
- **What is forecast.** The recorded value is `F(t)` plus each task the
  pass actually admitted, counted as a reservation counts: the
  load the forecast expects given what admission did. Under `shadow` that is
  the `off` wave, not the gap-fit wave. A baseline that could not be read
  counts as zero, as it did for admission.
- **Pairs.** A point forecast at `forecast_at` for `+n` pairs with the
  `host_resource_minutes` row of the minute `forecast_at` truncated to the
  minute, plus `n`, only once that target minute has fully ended. One
  `report_at` snapshot bounds the forecasts and determines maturity for
  every point. Until `minute_at + 1 minute <= report_at`, the point is
  **pending**, even if a partial measured row already exists. After that
  boundary it is **missing** if no row exists, otherwise **paired**.
  `missing` is `unsampled` plus `gap`. `unsampled` is an ended minute with
  no host row and no delivery run live: host minutes are written only by
  delivery workers, so an idle drain's forecast has nothing to pair with.
  `gap` is an ended minute with no host row while a delivery worker was
  live, from its `started_at` until its `finished_at`. The drain's own run
  and a run that never started are not deliveries. A run that finishes
  exactly as the minute starts, or starts exactly as the minute ends, does
  not cover it. A live delivery in a paired or still-pending minute does
  not move that point. Per offset, paired + pending + missing equals the
  selected recorded point count; the compatible `unmeasured` field is
  pending + missing. Text keeps `PAIRS`, `PENDING`, `MISSING` and the error
  columns, and adds `UNSAMPLED` and `GAP`. JSON adds `unsampled` and `gap`.
  A completed minute with a row is usable regardless of sample count; this
  report does not certify sampling completeness.
- **Errors**, per offset and for CPU (cores) and memory (bytes): the mean
  and mean absolute error of forecast minus measured, the share of pairs
  under-forecast, and the worst miss with its minute and drain.
- **Shadow decisions**, per drain, over its `shadow` passes: the distinct
  tasks it admitted, those gap fit would have held, and those gap fit would
  have admitted in a pass that did not. Those columns stay on shadow passes.
- **Enforce decisions**, per drain, over its local `enforce` passes: the
  distinct tasks it admitted (`ENFORCE_ADMITTED` / `enforce_admitted`) and
  the distinct tasks gap fit held (`GAP_HELD` / `gap_held`, from
  `fit_skipped`). A task held on one pass and admitted on another counts in
  both. Enforce pull requests held because no work fit are `PULL_HELD` /
  `pull_held`. Existing shadow JSON fields are unchanged.
- **Selection and retention.** Without filters the report reads all drains
  and recorded modes over the last 30 days. Optional `--since` selects
  forecasts at or after an RFC3339 timestamp; `--drain` matches an exact
  drain run id and `--mode shadow|enforce` matches the stored mode. Supplied
  filters intersect, applying equally to forecasts, pairs, errors and local
  or pull decisions. Forecasts after `report_at` are excluded. Text echoes
  both time bounds, drain and mode (`all` when unrestricted). JSON retains
  `since`, `forecasts`, `offsets`, `drains` and `pairs`, adding `report_at`,
  effective `selection` (`since`, `drain`, `mode`; null means unrestricted),
  and per-offset `pending`, `missing`, `unsampled` and `gap`. Empty cohorts have zero counts,
  empty pairs and drains, and null error summaries. Invalid timestamps,
  modes, empty drain ids and a since later than report time are rejected;
  selectors require `--accuracy`. Rows are pruned with the resource history
  by `orbit gc runs --apply` after 30 days, whatever `retention.runs_days`
  says. An earlier explicit since can read older surviving rows, but cannot
  restore pruned history. The report reads evidence without changing it.
- **Attribution limits.** Forecast rows record mode and drain, but do not
  attest source revision, build-budget settings or admission thresholds.
  A time/drain cohort is an operator selection, not proof of a homogeneous
  generation; configuration can change within it. No absent historical
  metadata is inferred or backfilled. Per-task resource profiles have a
  separate delivery population: sparse samples and wide spread remain
  limitations even when a selected host-forecast cohort has many pairs.
  See the [operator runbook](../../runbooks/forecast-accuracy.md) for cohort
  collection. Live post-deploy evaluation and tuning remain separate work.
- **Pull drains** record one row per request forecast in shadow or enforce,
  including forecasts whose request was held. Store schema v50 adds a nullable
  pull decision alongside local task decisions: whether the request was sent
  and whether no kind of work fit (`forecast_no_headroom`). A returned claim
  is added to the original forecast with the same lookup as
  `reserve_pulled_claim`, before the next request is forecast. The accuracy
  report pairs these points identically to local passes. Per drain,
  `PULL_REQUESTS` counts pull forecasts, `PULL_SENT` counts requests sent
  (including idle or failed attempts), and `PULL_WOULD_HOLD` counts shadow
  requests enforce would have held, including ones that returned no claim.
  `PULL_HELD` counts the same `forecast_no_headroom` outcome on enforce
  requests. Pull request counts are separate from the distinct local-task
  `WOULD_HOLD`, `WOULD_ADD` and `GAP_HELD` sets.

## 9. Concerns & Honest Limitations

- **Averaging blurs spikes.** Tasks of one class and complexity differ in
  length, so the validation build lands at different minutes. A per-minute average of
  misaligned spikes is a lower, wider hump. The forecast then underestimates
  peaks and overestimates how long moderate load lasts. The high quantile
  for memory and the throttle backstop absorb part of this. Profiles aligned
  to pipeline steps instead of minutes would fix it
  ([3_vision.md §1](./3_vision.md#1-open-questions), question 1). The
  profiles report in step 2 shows how bad it is before anything enforces.
- **Summed memory assumes independence.** The quantile of §4.1 treats
  deliveries' memory as independent. Deliveries that peak together, such as
  validation builds started in the same minute, can exceed it; the floor at
  the largest single percentile and the throttle backstop bound the miss.
- **Labels are coarse.** Complexity is set at filing or by the task pilot,
  and is sometimes wrong. A code change filed without its real size, or a
  task that writes code while tagged `no-diff-expected`, carries the wrong
  profile until the outlier ages out of the window.
- **The baseline is assumed flat.** Load that isn't from tasks, such as a
  manual build or an IDE indexing, can change during the horizon.
- **Only CPU and memory.** Disk I/O and fsync stalls (the Mac), provider
  rate limits and base-branch conflicts are not forecast. The disk throttle,
  provider-limit exclusions and the conflict and footprint checks still
  apply.
- **Profiles are per host and need warm-up.** A new host, workspace, class
  or complexity is admitted pessimistically, borrowing the heaviest profile
  of its class, until it has 10 deliveries.
- **Linux scopes, macOS process tree.** macOS has no worker scopes and
  samples the worker's process tree instead. A tree the sandbox will not
  let the worker read adds no sample. Without `machine.worker_containment`,
  Linux records nothing either.
- **Throughput is not measured.** The design fills headroom under the
  thresholds. It does not check that more concurrency lands more work. When
  the bottleneck is something else, such as conflicts or review, it fills
  the host anyway, up to the operator's maximum.
- **Starvation guard is crude.** Once the head is protected, a drain admits
  nothing else until the running deliveries and reservations it waits on are
  gone. Other drains on the host are not held by it, so while they keep
  admitting, its wait has no bound. A head whose miss stops improving is
  reported as stuck (§5.3), not admitted.
- **An idle host admits whatever comes first.** The idle-host rule admits a
  candidate larger than the thresholds, and judges one without the baseline
  `B`, which includes memory used outside Orbit. Such a delivery can push the
  host past the thresholds; the resource throttle is the backstop.
- **Thresholds are guesses.** 90% CPU and 75% memory are starting points.
  The shadow report is where they get tuned.

## Task References

- [ORB-15757] — authored this proposal.
- [ORB-15764] — filed to stop claimed `no-diff-expected` deliveries from
  running the owner's required validation.
- [ORB-15765] — filed to stop instructing `no-diff-expected` tasks to run
  workspace gates.
- [ORB-15766] — filed to stop counting build-heavy QA sweeps as CPU-light.
- [ORB-15774] — gap-fit admission for pull drains (§6).
- [ORB-15894] — reports a protected head whose miss stops improving (§5.3).

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
