---
title: Routines — Design
owner: claude
last_updated: 2026-09-07
last_validated: 2026-09-07
status: Accepted
feature: routines
doc_role: design
type: design
summary: Proposed contract for routine definitions, sweep dispatch, host-local state, and OS clock integration.
tags: [routines, scheduler]
paths: ["crates/orbit-cli/src/command/routine/**", "crates/orbit-core/src/application/routines/**", "crates/orbit-cmd/src/registry_routines.rs", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-registry/src/host_identity.rs", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-store/src/sqlite/routine_store/**"]
related_features: [routines, activity-job, host-registry]
related_artifacts: [ORB-10001, ORB-10021, ORB-10207, ORB-10270, ORB-10319, ORB-10800, ORB-10986, ORB-11082, ORB-11315]
---

# Routines — Design

This doc is the v1 contract as shipped in [ORB-10021]: the routine definition schema,
how definitions are discovered, what `orbit sweep` does on each invocation, where state
lives, and how the OS clock drives it. Cross-host coordination, event triggers, and everything else deferred is
in [3_vision.md](./3_vision.md). Decision rationale lives in [4_decisions.md](./4_decisions.md).

## OS sweep clock controls

There are two independent scheduling layers. The per-user OS clock wakes Orbit and
invokes the stateless `orbit sweep` pass; each versioned routine's cron expression then
decides whether that pass fires work. The OS clock is host-local infrastructure, not a
routine definition. Its durable configuration is `~/.orbit/clock.toml`, defaults to a
60-second cadence, and accepts only whole-minute values from 60 through 3600 seconds.

`orbit routine clock status` reports configured cadence, native-manager enabled state,
and whether an enabled Linux timer is active with a finite next trigger. An enabled timer
without that scheduling state is `unhealthy`, has no effective cadence, and reports
`orbit routine clock enable`, which rewrites a stale installed systemd timer if needed,
restarts the timer, and verifies the resulting deadline.
`orbit routine clock pause` disables only launchd/systemd
scheduled invocations (surviving logout/reboot through the native per-user manager);
it preserves routine cursors, fire history, and per-routine pauses, and a deliberate
`orbit sweep` is still available. `enable` resumes with the configured cadence, while
`set --cadence-seconds N` atomically rewrites the host setting and reloads the existing
unit identity. Linux installation, cadence changes, and enablement verify an active timer
with a finite next trigger after native commands complete. A failed verification is an
actionable error rather than a claim that the clock is active. launchd and systemd user managers are
the supported platforms; there is no resident Orbit daemon ([Host-local sweep clock configuration](./4_decisions.md#host-local-sweep-clock-configuration)).

The dashboard Operations view projects the same typed status and control functions
[ORB-10875]. Routine definitions remain workspace-scoped and show their versioned
`enabled` value; the host clock remains one independent host-scoped card.
Next evaluation uses schedule display state (`scheduled`, `disabled`, `paused`,
`waiting`, `never_observed`, `unavailable`) so a disabled or paused routine
does not look armed; a theoretical next slot is labeled hypothetical. The time
itself is `due::next_occurrence` — the cron's next scheduled occurrence,
strictly ahead of now and pinned to its minute — not the sweep's catch-up
eligibility. A routine holding a missed slot under `catch_up_once` is due for
that earlier slot while the row still points forward, which is correct.
Clock last/next tick are wall-clock times; systemd `NextElapseUSecMonotonic`
is used only for schedulability, never as a displayed next tick. Cadence is a
duration. A routine
toggle resolves its file from a freshly loaded `LoadedRoutine`, validates the displayed
workspace, host, target, and expected prior state, changes only the top-level `enabled`
field, reparses the document, and uses an atomic rename. Clock requests are a closed
typed action set (`enable`, `disable`, `set_cadence`) over the existing launchd/systemd
abstraction. The browser never supplies paths, programs, arguments, or shell text.

Both mutation families require an operator capability and write an audit event with the
selected workspace, concrete host, target, typed arguments, and authorization
provenance. Aggregate workspace views are read-only; stale expected state returns a
conflict so delayed or duplicate submissions cannot overwrite newer state.

---

The [shared automation-trigger proposal](../automation-triggers/1_overview.md)
from [ORB-11315] specifies delivery thresholds, preparation/failure eligibility,
immutable batches and separate successful-coverage checkpoints. It is proposed
and unimplemented; existing scheduling and action semantics remain current.

## 1. Routine Definition

A routine is one YAML file under `.orbit/routines/` in a routine-source workspace,
PR-reviewed and versioned like any other shared definition.

```yaml
# .orbit/routines/almanac-auto-commit.yaml
schemaVersion: 1
name: almanac-auto-commit
description: Commit & push almanac changes nightly
enabled: true                  # global kill-switch, versioned
hosts: [dk-mac]                # explicit host pinning; no "any host" in v1
trigger:
  cron: "0 22 * * *"           # standard 5-field cron, evaluated in host-local time
  missed_run: catch_up_once    # catch_up_once | skip (default: skip)
target: job:almanac_commit_pipeline   # job:<name>, resolved via the catalog
policy:
  timeout_minutes: 10
  retries: { max: 2, backoff_minutes: 2 }
  overlap: forbid              # forbid | allow — forbid skips a fire while one is in flight
```

Field semantics:

- **`name`** — unique across all routine sources on a host; collision is a load-time error.
- **`enabled` / `hosts`** — the two *versioned* toggle layers. A routine fires on a host only
  if `enabled: true` and the host's `host_id` appears in `hosts`. Effective state also
  requires no host-local pause (§4).
- **`trigger.cron`** — when the routine is due. `missed_run` governs fires that fall in a
  window when the host was asleep or powered off: `catch_up_once` fires a single make-up run
  on the next sweep (never one per missed slot); `skip` waits for the next natural slot.
- **`target`** — a `job:<name>` reference resolved through the source workspace's job
  catalog at load time. Unresolvable targets are load-time errors. `activity:<name>` is
  reserved and rejected at parse time with wrapping guidance: run dispatch is job-shaped
  (`submit_pipeline_run` resolves jobs by name; nothing dispatches a bare activity), and a
  one-step wrapper job in the same source workspace is the existing composition grammar
  ([Routine targets are catalog references only — no inline command payloads](./4_decisions.md#routine-targets-are-catalog-references-only-no-inline-command-payloads)). There is deliberately no inline command form: the `shell` activity variant
  was removed fail-closed in [ORB-00374] / [The v2 shell activity surface is removed, not sandboxed](../activity-job/4_decisions.md#the-v2-shell-activity-surface-is-removed-not-sandboxed), and reintroducing arbitrary-command
  payloads through the scheduler would reopen that surface on a timer.
- **`policy`** — applied by the dispatcher around the run: timeout, bounded retries with
  fixed backoff, and overlap handling. `overlap: forbid` is the default; `timeout_minutes`
  defaults to 60 and doubles as the staleness horizon (§4), `retries` defaults to
  `{max: 0, backoff_minutes: 2}`. Retries re-dispatch a *failed* fire under the same slot
  (attempt 2..max+1) once the backoff has elapsed, evaluated on later sweep passes. A fire
  that *errored at dispatch* (`submit_pipeline_run` failed synchronously — lock contention, a
  momentary catalog/store hiccup) is retried under the same policy as a run-level failure
  ([ORB-00422]): a transient dispatch failure is the class retries exist to absorb, so it must
  not burn the slot when retry budget remains. With `max: 0` (the default) a dispatch error,
  like any failure, consumes the slot and waits for the next natural one.

Parsing is fail-closed: an invalid routine file is reported and *that routine* is treated
as absent; it never degrades into "fire with defaults".

### Seeded defaults and ownership

`orbit workspace init` seeds `auto_task_scheduler.yaml`, `task_triage.yaml`,
`task_pilot.yaml`, `ship_sweep.yaml`, `worktree_gc.yaml`, and `ci_failure_sweep.yaml`
with a workspace-unique name, the resolved host pin, and
`enabled: false`. The definition's versioned `enabled` field is the opt-in: changing it
to `true` deliberately grants that scheduled capability in the workspace.

Seeded files become workspace-authored immediately. Plain re-init is create-if-missing:
it adds a newly shipped default or recreates a deleted default, but byte-for-byte preserves
existing definitions, including `enabled`, `hosts`, cron, and policy edits. Only destructive
force initialization recreates the workspace and therefore restores template defaults.

After [ORB-10800] / [All five definition-artifact kinds carry managed provenance, and doctor reports it](../activity-job/4_decisions.md#all-five-definition-artifact-kinds-carry-managed-provenance-and-doctor-reports-it), routine seeding is manifest-aware: `.orbit/routines/`
carries a `.orbit-managed-assets.json` recording the digest Orbit last wrote for each
seeded default. The digest is taken over the *rendered* document — after the host-id and
routine-name placeholders resolve — because that is what actually lands on disk. Two
consequences follow. A default dropped from a later release is retired by content
provenance rather than lingering forever in every existing workspace, and re-seeding
unchanged content against the same host is a genuine no-op rather than a rewrite. A
routine an operator has edited is never deleted: it is preserved under
`.retired-managed/routines/`. `orbit doctor` reports routine artifacts as faulty,
deprecated, or stale, and `orbit doctor --fix-stale-artifacts` performs the retirement.

A routines directory carrying no manifest at all predates that provenance, and its routines
are customized by design — flipping `enabled` and keeping the host pin is the lifecycle the
templates invite. Content alone cannot separate such a routine from a file the operator wrote
from scratch, so reconciliation adopts it [ORB-11154]: the binding is parsed from that exact
on-disk instance, and the bytes already on disk are recorded as the ones Orbit owns. Nothing
is rewritten, no warning is emitted, and a later shipped-template change refreshes the routine
onto the adopted binding instead of being reported as a collision forever. Adoption is skipped
for a file that does not parse as a routine or whose binding could not be re-rendered later,
and once the directory has a manifest, a shipped name the manifest does not claim is genuinely
user-authored and is still reported and preserved in place.

The seeded `ci_failure_sweep` targets `job:ci_failure_sweep_pipeline` hourly at
`5 * * * *` — deliberately clear of the other defaults' minutes — with `missed_run: skip`
and `overlap: forbid`. The pipeline runs every GitHub query on the host, files each
current, non-stale failure cluster as a proposed bug task carrying that evidence inline,
dedupes against still-open owners by failure key, and pilots each candidate through the
existing task-pilot job. The all-join lets independently valid pilots apply even when a
sibling is stale or fails. Within a returned partition, deterministic apply also commits
valid tasks independently, then sends only invalid assessments through one targeted repair
attempt at the original pinned revision; stale tasks require fresh preparation. A following
`pipeline_success_guard` fails the parent while any task remains unresolved. The guard is
skipped only for a filer-reported zero-candidate
result, so empty clean/deduped sweeps remain no-ops without hiding failed work.

A release-head failure remains current even when the same workflow is green on the
integration head: freshness is scoped to workflow and ref for both landing branches. If a
pilot proves the repair already landed on integration, admission leaves the deduped task in
proposed quarantine and reports `release_promotion_or_hotfix_needed` with the red release
SHA/run and the pilot's covering repair evidence. The sweep performs neither release
promotion nor hotfix dispatch. A no-diff assessment without concrete covering proof is
reported as `covering_proof_missing` and remains proposed for a later bounded pilot instead
of being treated as already landed. The routine is a scheduling surface only: an
operator-triggered run of the job behaves identically to a scheduled fire.

Publication shares that release boundary. A job can fail because the repository already
records a version, tag, or artifact its operator has not published or promoted yet; the
only correct repair is that release action, which this sweep never performs. The pilot
states that intent explicitly by returning `release_action_required` with the action and
its evidence, and admission then reports
`release_publication_or_operator_action_needed` — carrying the red run, the pilot's
finding, and any selectors it withheld — while the deduped task stays in proposed
quarantine and keeps owning the preserved failure evidence. Admission never infers this
from which files a proposed repair would touch, so a defect the repository does own — a
wrong dependency requirement, a broken manifest, a packaging script bug — remains eligible
for ordinary promotion. The same finding also clears the state automation's readiness flag,
so neither promotion path can turn a pending publication into automatic version edits.

The seeded `ship_sweep` targets `job:workspace_ship_pipeline` with `missed_run: skip` and
`overlap: forbid`. The wrapper resolves the source runtime's ship mode and configured base
branch, invokes and waits for `task_auto_pipeline` without explicit task IDs, and guards
its result. It never calls the legacy cross-workspace CLI sweep or consults
`[workflow] auto_ship`; the child's existing empty-backlog path is a successful no-op.
Waiting keeps the wrapper run active for the whole shipment, so routine overlap protection
covers the child rather than only submission ([Delegate workspace ship routines through a synchronous wrapper job](./4_decisions.md#delegate-workspace-ship-routines-through-a-synchronous-wrapper-job)).

---

## 2. Discovery and Registration

Discovery reuses the global workspace registry (`~/.orbit/workspaces.json`) rather than a
new pointer mechanism — the same shape `orbit run ship-sweep` established for unattended
cross-workspace dispatch.

A workspace becomes a routine source with one versioned config key:

```toml
# <workspace>/.orbit/config.toml
[routines]
role = "source"
```

On each pass, sweep loads the registry, visits every registered, active workspace whose
config declares `role = "source"`, and loads `.orbit/routines/*.yaml` from each. Two
properties fall out:

- **Registration is what already exists.** Registering the workspace with Orbit (which
  polaris needs anyway) plus the config key is the entire setup; the key is versioned, so
  both hosts converge on it through a normal `git pull` with no per-host pointer files.
- **Centralization is convention, not mechanism.** The constellation keeps all routines in
  polaris; the mechanism tolerates additional sources, and `orbit routine list` names each
  routine's source workspace so provenance is never ambiguous.

Host identity is the one genuinely host-local datum: `~/.orbit/host.toml` carries the
versioned `machine_id`, human-facing `host_id`, and immutable `task_prefix`. `orbit init` owns identity
creation and legacy migration; `orbit routine init --install-clock` only installs the OS
clock unit (§5). A malformed `host.toml` is an error, not a fallback; a `[routines] role`
value other than `"source"` is a config error (fail-closed on both).

The implementation boundary is vertical: `crates/orbit-cmd/src/registry_routines.rs` reads
`host.toml` and `workspaces.json` through `orbit-registry`, validates local checkout paths,
and constructs registered runtimes through `registry_runtime`. It projects those inputs
through `RoutinePlacementProvider` and `RoutineWorkspaceProvider` into
`crates/orbit-core/src/application/routines/`. Core owns the registry-neutral scheduler and does not
read either registry file directly. There is no hub snapshot, satellite cache, fleet
health, or remote placement service in the v1 path.

---

## 3. Sweep

`orbit sweep` is the stateless entrypoint the OS clock invokes every minute. Like
`ship-sweep`, it never bootstraps a workspace from the caller's cwd, isolates per-routine
failures, and exits non-zero on infrastructure errors such as malformed host identity,
an unreadable registry, or an unopenable store. A valid empty local registry simply
produces no routines and exits successfully.

Per pass:

1. Take a host-global advisory lock (in the host store, §4). If another sweep holds it,
   exit immediately — overlapping invocations from a slow prior pass must not double-fire.
2. Load local `workspaces.json`, validate checkout paths, persist any resulting status
   updates, and build runtimes for active local checkouts whose `.orbit/` directory exists.
   Collect routines from those whose config declares `role = "source"`, failing closed per
   source or definition without stopping other valid sources.
3. Validate every committed routine pin before scheduler mutation. An exact match for this
   machine's `host_id` is eligible. A name used by another workspace owner's local
   `owner_host_ids` projection reports `host_belongs_elsewhere`; any other name reports
   `host_unresolvable`. Machine-local definitions under `.orbit/routines/local/` are bound
   to this host by their loader and bypass this committed-pin check. No aliases, liveness,
   cache age, or remote registry state participate.
4. Filter to routines where `enabled`, validation says this machine owns the pin, and no
   local pause.
5. Sync unresolved fires against actual run state. A dispatched `running`/`retrying` run
   whose recorded owner is conclusively stopped is failed immediately, including after a
   host restart; an alive or unprobeable owner keeps its overlap slot. Other unresolved
   entries are reclaimed after the routine's `timeout_minutes` staleness horizon, so a
   sweep that crashed between intent and dispatch cannot block `overlap: forbid` forever.
   Fires for a routine that is no longer assigned to this machine are deliberately untouched.
6. For each, compute due-ness from the cron expression and the persisted cursor
   (last slot, else the first-observation baseline — a routine never fires for slots that
   predate its registration on this host; the first sweep records the baseline and fires
   nothing). Due-ness is O(1) via previous-occurrence lookup, never a walk over every
   missed slot; `missed_run` policy decides gaps. A slot is "natural" for two configured
   clock-cadence intervals after its scheduled time: the default 60s clock therefore keeps
   the existing 120s grace, while a configured 300s clock keeps a slot natural for 600s.
   This admits one delayed or missed poll without turning genuine downtime into a `skip`
   fire.
7. For each due routine: check `overlap` against in-flight fires, record the fire intent
   (idempotency key: routine name + scheduled slot + attempt, transactionally with the
   cursor advance), then dispatch the target via `submit_pipeline_run` in the routine's
   source workspace with actor `routine/<name>` as run provenance.
8. Record outcomes and exit.

`orbit routine list`, `orbit routine show`, and `orbit sweep` expose the local registry
source (`local_workspace_registry`) plus stable diagnostic codes and severity in human and
JSON output. Compatibility fields for cache age and staleness remain empty/false. Moving a
committed pin from host A to host B never mutates A's
cursor, fires, or pause. B has no migrated state, so its first sweep records the normal
first-observation baseline; only the next natural slot can fire.

Fires are normal runs: they appear in run history, carry v2 audit envelopes, and are
debuggable with the existing run tooling — there is no separate "scheduled run" ledger.

Naming note: `orbit sweep` is the general scheduler pass. The seeded `ship_sweep` routine
is workspace-local; the legacy `orbit run ship-sweep` cross-workspace entrypoint remains
compatible during routine burn-in and is a separate eventual-removal concern.

---

## 4. Host-Local State and Toggles

All scheduler state lives host-locally (the `routine_*` tables in the host-global store
database `~/.orbit/orbit.db`, module `orbit-store/src/sqlite/routine_store/`), gitignored
and never synced:

- **routine_cursors** — per routine: first-observation baseline + last slot consumed.
- **routine_fires** — one row per fire attempt: `(name, slot, attempt)` idempotency key,
  state (`intent → dispatched → succeeded/failed/timed_out/error`), dispatched run id.
- **routine_pauses** — host-local suppressions written by `orbit routine pause <name>` /
  cleared by `resume`. Durable across reboots; invisible to git.
- **sweep lock** — a `flock(2)` file lock (`~/.orbit/state/routine-sweep.lock`) rather
  than a table: the OS releases it on process death, so a crashed sweep never wedges the
  next pass and no lock-staleness logic is needed.

Toggle resolution, in order: `enabled: false` (versioned, everywhere) → not in `hosts`
(versioned, per host) → local pause (unversioned, this host only). `orbit routine list`
shows all three columns plus computed next-due, so "why didn't this fire?" is one command.

---

## 5. Clock Integration

The OS owns the wake-up; Orbit owns everything else. `orbit routine init --install-clock`
renders and installs the platform unit:

- **macOS** — a launchd agent (`com.orbit.sweep`) with `StartInterval=<cadence>`. launchd also
  fires on wake, which pairs with `missed_run: catch_up_once` for laptop sleep gaps.
- **Linux** — `orbit-sweep.timer` combines `OnActiveSec=<cadence>` with
  `OnUnitActiveSec=<cadence>` plus a oneshot service. Every timer activation (fresh install,
  late reinstall, cadence change, or re-enable) therefore arms a finite first sweep relative
  to that activation; successful service activations schedule subsequent sweeps at the
  configured cadence. `orbit routine clock enable` compares the installed timer with the
  embedded template, rewrites a stale definition (for example pre-fix `OnStartupSec`), and
  daemon-reloads before restart [ORB-11082]. `AccuracySec=5s` bounds manager coalescing
  after each deadline [ORB-10986]. These monotonic
  triggers deliberately do not replay timer events missed while the manager or host was
  down. The first sweep after restart evaluates each routine's cursor, so `catch_up_once`
  collapses missed cron slots to one fire and `skip` waits for the next natural slot.
  Every sweep loads the same `clock.toml` cadence used to render the native timer, so
  changing the clock from 60s to 300s changes its natural-slot grace from 120s to 600s.
  The configured wake-up cost, routine enable/pause state, and host pinning are otherwise
  unchanged.

There is no resident Orbit daemon. Sub-minute triggers and event triggers are explicitly
out of v1 scope for this reason.

---

## 6. Concerns & Honest Limitations

- **No cross-host coordination.** `hosts` pins explicitly; a routine listed on both hosts
  runs on both, independently. "Exactly one of N hosts" requires a lease protocol across a
  tailnet that only exposes 22/443 between these machines — deferred, additive if needed.
- **Definition staleness.** Sweep reads whatever revision of the source workspace is on
  disk; definitions are only as fresh as the last `git pull`. A pull-the-sources routine
  can narrow the window but cannot fix its own staleness (it, too, is a definition). Editing
  routines on the host that runs them has no staleness; the other host lags by one sync.
- **Scheduled execution is a real capability escalation.** A routine source workspace is
  scheduled code execution on every host that trusts it. Targets are catalog-resolved (no
  inline commands) and run under existing activity/job policy, but note the sandbox caveat
  recorded in [External Executor Protocol for dynamic out-of-process executor registration (retired)](../executors/4_decisions.md#external-executor-protocol-for-dynamic-out-of-process-executor-registration-retired): enforcement depends on which runtime path the target takes.
  PR review on the source workspace is part of the security boundary.
- **Minute granularity, host-local time.** Cron is evaluated in host-local time; DST folds
  can skip or double a slot exactly as classic cron does. The idempotency key (name + slot)
  prevents double *fires* for the same slot but cannot invent a skipped slot.
- **`catch_up_once` collapses history.** After a week of laptop sleep, a nightly routine
  fires once, not seven times. This is the intended semantic for every currently envisioned
  routine (commits, reindexing), but it is a lossy default; a count-preserving mode would
  be a new `missed_run` variant.
- **First minute of overlap risk is on the dispatcher.** `overlap: forbid` depends on
  accurate in-flight bookkeeping; a crashed sweep that recorded a fire intent but died
  before dispatch leaves a stale in-flight entry. The outcome-sync step reclaims intents
  and dispatches older than `policy.timeout_minutes` (marking them `error` / `timed_out`);
  the consumed slot is not re-fired — the idempotency key holds. The two failure paths are
  deliberately given different states so retry can tell them apart ([ORB-00422]): a
  *synchronous* `submit_pipeline_run` error is unambiguous (nothing dispatched), so `fire`
  records it as `failed` — retryable under `policy.retries` like a run-level failure; a crashed
  sweep's reclaimed stale intent is *ambiguous* (a worker may have partially started), so the
  outcome sync records it as `error` — terminal, never re-fired, so a make-up fire cannot race
  an orphaned run. A dispatched in-flight run is released before that timeout only when its
  recorded owner is conclusively stopped; live and unprobeable owners remain protected.
- **Routines carry no input payload.** v1 dispatches every target with an empty input
  object; jobs meant for routines must run with defaults. Parameterized fires would be a
  schema addition.

---

## Task References

- [ORB-11315] — proposes shared state-driven triggers and durable coverage semantics.

- [ORB-10001] — authored this design-doc folder (proposal; no implementation).
- [ORB-10021] — implemented routines v1 (types, store, sweep, CLI, clock units).
- [ORB-10270] — historically implemented fleet-aware validation; current local-only
  validation retains stable diagnostics and no-backfill reassignment.
- [ORB-10319] — historical boundary extraction; current placement/workspace composition
  lives in `orbit-cmd` over `orbit-registry` local files.
- [ORB-10207] — added disabled-by-default seeding and workspace-local ship sweep.
- [ORB-00374] — removed the `shell` activity variant and `run_shell` dispatch (fail-closed);
  routines inherit this constraint.
- [ORB-10875] — added dashboard routine and host-clock status and controls.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
