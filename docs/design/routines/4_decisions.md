---
title: Routines — Decisions
owner: claude
last_updated: 2026-08-29
last_validated: 2026-08-29
status: Accepted
feature: routines
doc_role: decisions
type: design
summary: Decision log for the routines scheduler, including default seeding and workspace-local shipment.
tags: [routines, scheduler]
paths: ["crates/orbit-core/src/application/routines/**", "crates/orbit-cmd/src/registry_routines.rs", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-registry/src/**"]
related_features: [routines, activity-job, host-registry]
related_artifacts: [ORB-10001, ORB-10021, ORB-10207, ORB-10270, ORB-10319, ORB-10739, ORB-10986, ORB-11082]
---

# Routines — Decisions

[ORB-10001] recorded five candidate scheduler decisions; [ORB-10021] implemented them for v1. Their titles and recorded task provenance below now carry that history directly.

---

## The OS owns the clock: stateless orbit sweep under launchd/systemd, no resident daemon

**Recorded:** 2026-07-04 21:14:40.327750Z · [ORB-10021]
**Paths:** `docs/design/routines/**`, `crates/orbit-core/src/application/routines/**`

### Context

Something must wake the scheduler. Alternatives: a resident `orbit schedulerd` owning timers in-process (sub-minute precision, event triggers, but a daemon to supervise on two platforms), or delegating wake-ups to the OS schedulers that already exist (launchd on macOS, systemd timers on Linux) invoking a stateless pass every minute.

### Decision

launchd (`StartInterval` 60s) and a systemd timer (`OnActiveSec` plus `OnUnitActiveSec`) invoke `orbit sweep`; sweep is stateless-in, durable-out. The systemd timer arms itself from every timer activation and recurs after service activation. Missed cron-slot semantics belong to each routine's `missed_run` policy. There is no resident Orbit daemon [ORB-10986].

### Consequences

- No process supervision, crash recovery, or memory-leak surface; a wedged pass affects one minute, not the scheduler.
- launchd wake behavior and systemd's first sweep after timer activation pair with `missed_run: catch_up_once` to cover laptop sleep and host downtime without replaying every missed clock tick.
- Cost: minute granularity is a hard floor and event triggers are structurally impossible in v1; correct behavior depends on two platform-specific unit files that must be kept in parity and tested on both platforms.

## Routine discovery via the workspace registry and a versioned [routines] role=source config key

**Recorded:** 2026-07-04 21:14:40.332406Z · [ORB-10021]
**Paths:** `docs/design/routines/**`, `crates/orbit-core/src/application/routines/**`

### Context

Sweep must find routine definitions without a resident daemon and without bootstrapping from the caller's cwd. Alternatives: a host-level pointer file in `~/.orbit/host.toml` naming one designated control workspace per host (explicit two-way handshake), or reusing the global workspace registry the way `orbit run ship-sweep` / `auto_ship` already does for unattended cross-workspace dispatch.

### Decision

Sweep enumerates `~/.orbit/workspaces.json` and collects `.orbit/routines/*.yaml` from every registered, active workspace whose versioned `.orbit/config.toml` declares `[routines] role = "source"`. Centralizing all routines in polaris is constellation convention, not Orbit mechanism. `~/.orbit/host.toml` survives only to carry `host_id`.

### Consequences

- Setup is what already exists: register the workspace plus one versioned config key; both hosts converge through git with no per-host pointer files.
- `orbit routine list` names each routine's source workspace, so provenance stays unambiguous with multiple sources.
- Cost: any registered workspace's config can make it a routine source — the review boundary widens from one blessed repo to every registered workspace's `config.toml`, and sweep correctness now depends on registry hygiene (stale registered paths must be skipped loudly, not silently).

## Routine targets are catalog references only — no inline command payloads

**Recorded:** 2026-07-04 21:14:40.329169Z · [ORB-10021]
**Paths:** `docs/design/routines/**`, `crates/orbit-core/src/application/routines/**`

### Context

The original sketch allowed a `run: {type: shell, command: ...}` payload for small chores. [The v2 shell activity surface is removed, not sandboxed](../activity-job/4_decisions.md#the-v2-shell-activity-surface-is-removed-not-sandboxed) removed the `shell` activity variant and `run_shell` dispatch fail-closed; reintroducing arbitrary-command payloads through the scheduler would reopen that surface on a timer, unattended.

### Decision

`target:` accepts only catalog references resolved at load time; unresolvable targets are load-time errors. v1 dispatches `job:<name>` — run dispatch is job-shaped (`submit_pipeline_run` resolves jobs by name; there is no standalone activity run entrypoint), so `activity:<name>` is reserved and rejected at parse time with guidance to wrap the activity in a one-step job in the same source workspace. Shell-like chores become `deterministic` activities or jobs in the source workspace.

### Consequences

- Scheduled execution inherits existing activity/job policy, audit envelopes, and the fail-closed posture of [The v2 shell activity surface is removed, not sandboxed](../activity-job/4_decisions.md#the-v2-shell-activity-surface-is-removed-not-sandboxed); the scheduler adds a trigger source, not a new execution surface.
- Load-time validation makes a broken reference visible on the next sweep instead of at fire time.
- Cost: every new chore requires authoring a catalog asset (higher friction than a one-line command), and scheduler capability is permanently coupled to catalog capability — including the job-shaped dispatch constraint that keeps `activity:` targets out of v1.

## Routines pin hosts explicitly; no cross-host coordination in v1

**Recorded:** 2026-07-04 21:14:40.332307Z · [ORB-10021]
**Paths:** `docs/design/routines/**`, `crates/orbit-core/src/application/routines/**`

### Context

Some recurring work should run on exactly one machine. A "run on exactly one of N hosts" mode needs a lease protocol between hosts that only expose SSH to each other; the alternative is explicit pinning, where the definition names every host it fires on.

### Decision

Each routine carries a `hosts:` list matched against the host-local `host_id`; there is no "any host" value in v1. Listing two hosts means two independent fires. Failover stays out of scope until a real routine needs it.

### Consequences

- Due computation stays purely host-local: no lease table, no network dependency, no split-brain modes to test.
- The semantics are trivially predictable from the YAML alone.
- Cost: no routine survives its pinned host being down, and adding leases later introduces a second, coordinated mode whose semantics diverge from everything shipped in v1.

## Routine definitions are git-shared; scheduler state is host-local and never synced

**Recorded:** 2026-07-04 21:14:40.331256Z · [ORB-10021]
**Paths:** `docs/design/routines/**`, `crates/orbit-core/src/application/routines/**`

### Context

Routines run on two hosts (dk-mac, dk-server-1) with different availability profiles. Definitions must converge across hosts; scheduler runtime state (last fires, pauses, locks) could either be synced between hosts or kept local. Syncing state would let either machine answer "did the nightly fire on the other box?" but requires a scheduler network protocol between hosts that only expose 22/443 to each other.

### Decision

Routine YAML definitions live in routine-source workspaces and converge via git like any other versioned definition. All scheduler state — fires (with idempotency keys), host-local pauses, and the sweep advisory lock — lives in a host-local SQLite routine store, gitignored and never synced. No scheduler network protocol exists in v1.

### Consequences

- Two hosts converge on definitions through a normal `git pull`; no new sync mechanism to build, secure, or debug.
- State stays consistent with the run history it references, which is also host-local.
- Cost: cross-host observability requires asking each host — there is no single pane of glass, and a definition edit is only as fresh on the other host as its last `git pull`.

## Default routines seed per-workspace at init with host and name resolved at seed time

**Recorded:** 2026-07-11 21:51:20.761360Z · [ORB-10129], [ORB-10207]
**Paths:** `crates/orbit-core/assets/routines/**`, `crates/orbit-core/src/command/routine.rs`, `crates/orbit-core/src/command/init.rs`

### Context
ORB-10129 ships the triage pipeline as a default, but routines have no global directory: discovery reads `.orbit/routines/*.yaml` from `[routines] role = "source"` workspaces, v1 requires explicit host pinning (no "any host"), and routine names must be unique across all sources on a host — so a static shipped YAML cannot work. The real alternatives were leaving defaults workspace-authored from scratch or adding a global routines directory (a discovery-model change [Routine discovery through workspace registry](#routine-discovery-via-the-workspace-registry-and-a-versioned-routines-rolesource-config-key) deliberately avoided).

### Decision
`orbit init` (workspace branch) seeds `DEFAULT_ROUTINE_FILES` templates into `.orbit/routines/`, resolving `__ORBIT_HOST_ID__` via `resolve_host_id` and `__ORBIT_ROUTINE_NAME__` from the **registered workspace name**, validating each rendered document fail-closed before writing. Every default is disabled. The complete set is `auto_task_scheduler`, `task_triage`, `task_pilot`, `ship_sweep`, and `worktree_gc`. Plain re-init creates missing defaults while preserving existing definitions byte-for-byte; destructive `--force` recreates templates. A routine fires only after the workspace is a routine source and its versioned `enabled` field is set true. [ORB-10739]

### Consequences
- Fresh workspaces get reviewable routine definitions without silently granting scheduled execution.
- Per-workspace names let multiple seeded source workspaces coexist on one host despite the global name-uniqueness rule.
- The seeded file pins the initializing host; sharing the repo to another host needs a hand edit of `hosts:` or recreation during destructive initialization.
- Cost: `orbit init` output depends on the machine it runs on (host id, workspace name), and routine template improvements do not overwrite existing workspace-authored files.

### Correction: the suffix is the workspace name, not the checkout directory [ORB-12107]

The suffix was originally taken from the directory containing `.orbit/`, so `orbit workspace init --name qa-sweep` inside `.../repo` seeded `task-pilot-repo` while `orbit routine list` reported a `qa-sweep` workspace — `orbit routine show task-pilot-qa-sweep` found nothing. Worse, the directory basename is not unique on a host: any two `repo`/`src`/`app` checkouts seeded identical names, and a name defined twice drops *both* definitions at load time.

`RoutineSeedIdentity` now carries the host id and the registered workspace name together, and is the only way to reach default-routine seeding, so neither `orbit workspace init` nor `orbit workspace sync` can render a name without one. A workspace name with no characters usable in a routine name is rejected rather than silently falling back to an unsuffixed, host-wide name.

`orbit workspace init` also refuses a name whose seeded routines a *different* registered checkout on the host already declares (committed or `local/`), naming each conflicting file, instead of writing a duplicate set that can never fire.

Existing workspaces are **left untouched**: the managed-asset manifest records each routine's materialization binding, and reconciliation keeps the recorded name. A workspace seeded before this change therefore keeps its directory-derived routine names and keeps working; `orbit workspace sync --check` reports the difference as `binding_drift`, naming the workspace-derived name it would render now. Adopting the new name is a deliberate operator action (rename in the definition and its manifest entry, or re-initialize the routines directory) — nothing renames a live routine automatically, because a rename loses the routine's run history and state, which key off the name.

## Delegate workspace ship routines through a synchronous wrapper job

**Recorded:** 2026-07-15 22:19:13.834542Z · [ORB-10207]
**Paths:** `crates/orbit-core/assets/routines/**`, `crates/orbit-core/assets/jobs/**`, `crates/orbit-core/src/runtime/v2_host/**`

### Context
A scheduled ship routine must dispatch only its source workspace, resolve that workspace ship mode and base branch, and keep the parent run active until normal backlog shipment finishes. The alternatives were special-casing routine dispatch, spawning the legacy multi-workspace CLI sweep, or composing the existing job catalog.

### Decision
Seed a workspace-local ship-sweep routine targeting a shipped wrapper job. The wrapper deterministically resolves ship input for its active runtime, invokes `task_auto_pipeline` with no explicit task IDs, waits for it, and guards child success; it does not consult `workflow.auto_ship` or the cross-workspace sweep path.

### Consequences
- Backlog discovery, readiness, locking, bundling, crew selection, and gates remain owned by `task_auto_pipeline`.
- `overlap: forbid` covers the child shipment because the wrapper does not finish before the child.
- The legacy global ship-sweep remains compatible during burn-in but is not used by routines.
- Cost: the catalog gains a small wrapper job and deterministic resolver activity whose input contract must stay aligned with the canonical ship workflow.

## Host-local sweep clock configuration

**Recorded:** 2026-08-11 03:29:01.559340Z · [ORB-10720]
**Paths:** `crates/orbit-core/src/application/routines/**`, `crates/orbit-cli/src/command/routine/**`, `docs/design/routines/**`

### Context
The OS sweep clock is shared host infrastructure but previously had a hard-coded minutely cadence and only native-manager controls. The alternatives were a workspace routine setting, which would make one workspace own host infrastructure, or a host-local configuration plus Orbit CLI controls.

### Decision
Store the supported whole-minute cadence in host-local `~/.orbit/clock.toml` and expose it through `orbit routine clock`. Native launchd/systemd user services remain the authority for enabled state; routine pauses and manual `orbit sweep` remain separate.

### Consequences
- Clock status reports configured and effective cadence, and native-manager failures include recovery commands.
- On Linux, enabled state and successful manager command exits are insufficient for health: installation and controls report success only when systemd exposes an active timer with a finite next trigger. An elapsed or unscheduled timer reports `orbit routine clock enable`, which rewrites a stale installed unit (for example a pre-fix `OnStartupSec` timer) when it differs from the embedded template, daemon-reloads, restarts the timer even when already enabled, and verifies the repaired state.
- Linux uses monotonic timer-activation and service-activation triggers. `OnActiveSec` establishes the first deadline after every install, reinstall, cadence change, and re-enable; `OnUnitActiveSec` establishes recurrence after each sweep service activation. `AccuracySec=5s` bounds coalescing after either deadline. Missed timer ticks are not replayed, leaving catch-up versus skip behavior to each routine's persisted cursor and `missed_run` policy.
- Cost: the host-local setting intentionally does not travel with a workspace, so operators configure each host separately.

## Task References

- [ORB-10001] — authored this design-doc folder (proposal).
- [ORB-10021] — implemented routines v1; allocated and accepted [The OS owns the clock: stateless orbit sweep under launchd/systemd, no resident daemon](#the-os-owns-the-clock-stateless-orbit-sweep-under-launchdsystemd-no-resident-daemon)..[Routine definitions are git-shared; scheduler state is host-local and never synced](#routine-definitions-are-git-shared-scheduler-state-is-host-local-and-never-synced).
- [ORB-10129] — shipped the default triage routine; allocated and accepted [Default routines seed per-workspace at init with host and name resolved at seed time](#default-routines-seed-per-workspace-at-init-with-host-and-name-resolved-at-seed-time).
- [ORB-10207] — seeded disabled defaults and allocated/accepted [Delegate workspace ship routines through a synchronous wrapper job](#delegate-workspace-ship-routines-through-a-synchronous-wrapper-job) for workspace ship.
- [ORB-10270] — completed [Committed-routine ownership with host-local cursors](../host-registry/4_decisions.md#committed-routine-ownership-with-host-local-cursors)'s runtime enforcement: committed pins resolve through
  current registry or classified spoke-cache data before scheduler mutation, diagnostics
  remain explicit under degradation, and reassignment starts with a fresh baseline.
- [ORB-10319] — moved the registry-specific providers that source identity, workspace
  bindings, and runtimes into `orbit-cmd` over `orbit-registry`; the accepted routine
  decisions and Core scheduler semantics are unchanged.
- [ORB-10138] — exposed per-routine scheduler health over the dashboard HTTP API
  (`GET /api/routines`), realizing the single-host half of the §7 cross-host-visibility
  vision. Read-only projection of `routine_statuses`; no new ADR (no new architectural
  constraint — mirrors the existing `orbit routine list --json` surface).
- [ORB-11082] — Linux `orbit routine clock enable` rewrites a stale installed timer from
  the embedded template and daemon-reloads before restart, so an `OnStartupSec` upgrade
  leftover is recovered by the advertised command instead of looping on enable.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
