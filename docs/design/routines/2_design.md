---
title: Routines — Design
owner: claude
last_updated: 2026-10-20
last_validated: 2026-10-19
status: Accepted
feature: routines
doc_role: design
type: design
summary: Contract for routine definitions, clock-tick dispatch, host-local state, and OS clock integration.
tags: [routines, scheduler]
paths: ["crates/orbit-cli/src/command/routine/**", "crates/orbit-core/src/application/routines/**", "crates/orbit-cmd/src/registry/routines.rs", "crates/orbit-cmd/src/registry/runtime/mod.rs", "crates/orbit-registry/src/machine_identity.rs", "crates/orbit-registry/src/workspace_registry/**", "crates/orbit-store/src/driver/sqlite/routine_store/**"]
related_features: [routines, auto-tasks, activity-job, host-registry]
related_artifacts: [ORB-10001, ORB-10021, ORB-10207, ORB-10270, ORB-10319, ORB-10800, ORB-10986, ORB-11082, ORB-11315, ORB-12236, ORB-12237, ORB-12745]
---

# Routines — Design

This doc is the current contract: the routine definition schema, how definitions are
discovered, what `orbit clock tick` does on each invocation, where state
lives, and how the OS clock drives cron, delivery and state triggers. Cross-host
coordination, immediate file-watch/webhook wakeups, and everything else deferred is
in [3_vision.md](./3_vision.md). Decision rationale lives in [4_decisions.md](./4_decisions.md).

## OS sweep clock controls

There are two independent scheduling layers. The per-user OS clock wakes Orbit and invokes
the stateless `orbit clock tick` pass; each versioned routine and auto-task definition then
decides whether that pass fires work. The OS clock is host-local infrastructure, not a
routine definition. Its durable configuration is `~/.orbit/clock.toml`, defaults to a
60-second cadence, and accepts only whole-minute values from 60 through 3600 seconds.

`orbit clock status` reports configured cadence, native-manager enabled state,
and whether an enabled Linux timer is active with a finite next trigger. An enabled timer
without that scheduling state is `unhealthy`, has no effective cadence, and reports
`orbit clock enable`, which rewrites a stale installed systemd timer if needed,
restarts the timer, and verifies the resulting deadline.
A disabled but active systemd timer is also `unhealthy`: status shows its runtime activity
and any future trigger, and `orbit clock pause` stops and verifies it.
An installed unit that still invokes `orbit sweep`, or one whose program path has moved or
been deleted, is stale; `orbit clock repair` rewrites it to this binary invoking
`orbit clock tick` and re-registers it with the native manager. Repair is also the last
`orbit update` convergence step, so an install at a new path repoints the unit in the same
command that moved the binary. Repair never changes enabled/paused state: a paused clock is
corrected on disk and left paused, which is what separates it from `enable`.
`orbit clock pause` disables only launchd/systemd
scheduled invocations (surviving logout/reboot through the native per-user manager);
it preserves routine cursors, fire history, and per-routine pauses, and a deliberate
`orbit clock tick` (or its `orbit sweep` compatibility alias) is still available. `enable`
resumes with the configured cadence, while
`set --cadence-seconds N` atomically rewrites the host setting and reloads the existing
unit identity. Linux installation, cadence changes, and enablement verify an active timer
with a finite next trigger after native commands complete. A failed verification is an
actionable error rather than a claim that the clock is active. launchd and systemd user managers are
the supported platforms; there is no resident Orbit daemon ([Host-local sweep clock configuration](./4_decisions.md#host-local-sweep-clock-configuration)).

The dashboard Operations view projects the same typed status and control functions
[ORB-10875]. Routine definitions remain workspace-scoped and show their versioned
`enabled` value; the host clock remains one independent host-scoped card.
The host-wide `GET /api/routines` projection uses a process-local single-flight
memo with a five-second TTL, shared across workspace selections and tabs. Its
`Age` header reports the cached payload's age. Each request stamps the workspace
registry, global and workspace config, routine and target definitions, and
resolved database files plus their WALs; changes invalidate the next read,
including definition toggles, pauses and recorded runs. Status opens existing
stores observationally, without migrations or run reconciliation. Branch
inspection reads local Git objects, config and refs in process, sharing heads
across routines within a request; a repository libgit2 cannot open (a partial
clone or reftable refs) is read with the same bounded Git commands as before. Preparation retains its local-ahead rule
against the existing remote-tracking ref and never fetches for status.

Automation polling fetches only the selected subtab: routines and clock share
`GET /api/routines`, auto-tasks uses `GET /api/auto-tasks`, and jobs uses routines
plus recent job runs for its catalogue. Both definition lists project compact
automation diagnostics: member pending/fresh/ready/withheld/exhausted counts,
the active batch, and up to 20 withheld or failed samples. Fresh assessments
are those without a pending member or with a matching resulting fingerprint.
Delivery inventories likewise carry totals and bounded unresolved/excluded samples.
The diagnostic disclosure's **Full persisted state** loads
`GET /api/automation/:kind/:name/state?workspace=<id>` on demand (`kind` is
`routine` or `auto-task`). It reads the selected workspace's stored state,
requires a concrete active workspace, and returns 404 for missing state. The
browser caches that full state across ticks; **Reload full state** refreshes it,
and switching workspace clears it. Counts reflect inspection; the full view
is explicitly the persisted snapshot. Neither surface authorizes execution.

`GET /api/routines` still returns those definition rows when native clock
inspection fails (for example a systemd user bus that cannot be reached): the
clock object is `health: unknown` with the bounded diagnostic in `error` /
`health_issue`, `enabled` is JSON null, and clock controls are disabled. That
projection never invents paused or disabled clock authority. `orbit clock status`
continues to fail closed with the same diagnostic.
Core owns one projection of a routine status row, its fire records and the
`retired`, `owner_only` and `load_errors` rows; `orbit routine list` and `show`,
`GET /api/routines` and the MCP `orbit.routine.control` list each select the
fields they render from it. Core also owns one clock-health classification,
which each surface names in its own vocabulary: `orbit clock status` reads any
reported health issue as `unhealthy`, while the dashboard's `health` stays
`healthy` for an enabled clock with a future trigger and reports an enabled
clock without one as `missed`. Dashboard and MCP routine toggles run the same
Core selection (owner-only refusal, name lookup, source workspace, target);
the dashboard keeps its own authorization, audit rows and error codes.
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

Delivery thresholds, preparation eligibility, immutable batches and separate
successful-coverage checkpoints are implemented on the existing clock. The
[automation-trigger operations guide](../automation-triggers/5_operations.md#configuration-and-migration)
documents the supported schema-v1 configuration; its
[state preparation section](../automation-triggers/5_operations.md#state-preparation-orb-11331)
covers state consumers. The broader
[automation-trigger design](../automation-triggers/2_design.md#1-current-implementation-and-gaps)
separately identifies deferred multi-member coordination, automatic policy/host
migration and usage accounting. Its illustrative schema-v2 YAML is not loadable
configuration. Failed-run triage is retired, not a pending trigger implementation.

The dashboard's Automation › Routines rows show the ten most recent fires as
outcome dots, oldest to newest. Each dispatched fire links to its run; a
dispatch error without a run shows its outcome without a link. Consecutive
failures include failed runs, timeouts and dispatch errors, and show their
count and starting slot. A success, skipped slot or in-flight fire resets the
streak. Cron retries count once per scheduled slot, using the latest attempt.
State- and delivery-triggered routines include their automation-admitted runs,
merged with retained cron fires when the trigger has changed.

Three or more consecutive failures produce a warning above the routine rows
and a badge in the Health rail for the selected workspace and host. The badge
refreshes from every tab and clears on a host/workspace switch. History reads
are bounded to 100 records per source; a streak that reaches that boundary is
shown as “at least” the retained count, with a start time that may be earlier.
The HTTP routine payload retains `last_fire` and adds `recent_fires` (newest
first) and `failure_streak` (`count`, `since`, `truncated`).

## 1. Routine Definition

A routine is one YAML file under the registered checkout's gitignored
`.orbit/routines/`. Choose exactly one of `trigger.cron`,
`trigger.deliveries_landed` or `trigger.state`. Routines use `schemaVersion: 1`;
interval (`every_minutes`) scheduling belongs to auto-tasks, not routines.
The cron form is:

```yaml
# .orbit/routines/vault-auto-commit.yaml
schemaVersion: 1
name: vault-auto-commit
description: Commit & push vault changes nightly
enabled: true                  # definition kill-switch
trigger:
  cron: "0 22 * * *"           # standard 5-field cron, evaluated in host-local time
  missed_run: catch_up_once    # catch_up_once | skip (default: skip)
target: job:vault_commit_pipeline    # job:<name>, resolved via the catalog
policy:
  timeout_minutes: 10
  retries: { max: 2, backoff_minutes: 2 }
  overlap: forbid              # forbid | allow — forbid skips a fire while one is in flight
```

Field semantics:

- **`name`** — unique across all routine sources on a host; collision is a load-time error.
- **`enabled`** — the definition toggle. Every registered owner checkout with an
  enabled clock evaluates its cron definitions against its own store;
  delivery/state consumers additionally require their resolved owner machine (§1.1–1.2)
  ([Definitions carry no host pin](./4_decisions.md#definitions-carry-no-host-pin-every-owner-checkout-is-an-independent-schedule)). Effective state also requires no host-local pause (§4).
  A definition written before [ORB-12236] may still carry `hosts:`; it is ignored with a
  load warning naming the file for one release, then rejected as an unknown key.
- **`trigger.cron`** — when the routine is due. `missed_run` governs fires that fall in a
  window when the host was asleep or powered off: `catch_up_once` fires a single make-up run
  on the next sweep (never one per missed slot); `skip` waits for the next natural slot and
  records each missed past-grace slot as a skipped fire whose detail is `missed_run: skip`
  (`orbit routine list --json` shows it on `last_fire.detail`, and `orbit routine show` shows
  it in recent fires). The seeded worktree-gc routine uses `catch_up_once`.
- **`target`** — a `job:<name>` reference resolved through the source workspace's job
  catalog at load time. Unresolvable targets are load-time errors. `activity:<name>` is
  reserved and rejected at parse time with wrapping guidance: run dispatch is job-shaped
  (`submit_pipeline_run` resolves jobs by name; nothing dispatches a bare activity), and a
  one-step wrapper job in the same source workspace is the existing composition grammar
  ([Routine targets are catalog references only — no inline command payloads](./4_decisions.md#routine-targets-are-catalog-references-only--no-inline-command-payloads)). There is deliberately no inline command form: the `shell` activity variant
  was removed fail-closed in [ORB-00374] / [The v2 shell activity surface is removed, not sandboxed](../activity-job/4_decisions.md#the-v2-shell-activity-surface-is-removed-not-sandboxed), and reintroducing arbitrary-command
  payloads through the scheduler would reopen that surface on a timer.
- **`policy`** — for cron, applied by the dispatcher around the run: timeout, bounded retries with
  fixed backoff, and overlap handling. `overlap: forbid` is the default; `timeout_minutes`
  defaults to 60 and doubles as the staleness horizon (§4), `retries` defaults to
  `{max: 0, backoff_minutes: 2}`. Retries re-dispatch a *failed* fire under the same slot
  once the backoff has elapsed, evaluated on later sweep passes. `retries.max` counts
  non-skipped attempts on that slot (the initial fire plus re-dispatches). A skipped
  attempt occupies its number but does not spend budget, so the next re-dispatch uses
  the next free attempt. With no skipped rows that number is still 2..max+1. A fire
  that *errored at dispatch* (`submit_pipeline_run` failed synchronously — lock contention, a
  momentary catalog/store hiccup) is retried under the same policy as a run-level failure
  ([ORB-00422]): a transient dispatch failure is the class retries exist to absorb, so it must
  not burn the slot when retry budget remains. With `max: 0` (the default) a dispatch error,
  like any failure, consumes the slot and waits for the next natural one.

Parsing is fail-closed: an invalid routine file is reported and *that routine* is treated
as absent; it never degrades into "fire with defaults".

### 1.1 State routines and task-pilot configuration

The shipped `task_pilot.yaml` replaces the cron trigger with this complete form
(substitute the registered machine ID and workspace base branch):

```yaml
schemaVersion: 1
name: task-pilot-example
enabled: false
trigger:
  state:
    kind: preparation_eligible
    owner_machine: hm_your_registered_machine
    branch: agent-main
    debounce_minutes: 2
    max_wait_minutes: 10
    max_items: 50
    batch_size: 5
    retries: 1
    deadline_minutes: 90
    eligibility:
      statuses: [proposed, backlog]
      exclude_tags: [no-diff-expected, no-diff-needed]
      require_tags: []
      task_types: []
target: job:task_pilot_pipeline
policy:
  timeout_minutes: 90
  overlap: forbid
  retries: {max: 1, backoff_minutes: 5}
```

`owner_machine` is required and only that machine admits new work. State routines
require `overlap: forbid` and the job fixed by their kind:
`preparation_eligible` targets `job:task_pilot_pipeline`;
`finding_unverified` targets `job:finding_verification_pipeline` and uses the
[finding-verification eligibility contract](../finding-verification/2_design.md#7-state-trigger);
`pilot_warnings_held` targets `job:escalation_triage_pipeline` and uses the
[pilot warning hold contract](../automation-triggers/2_design.md#61-pilot-warning-holds-pilot_warnings_held).
`execution_failed` still parses for compatibility, but its retired target is
skipped and dispatches nothing. These are domain eligibility checks, not arbitrary
status-change subscriptions.

State timing requires positive `debounce_minutes <= max_wait_minutes`,
`max_items` in 1–50, `retries` in 0–5 and `deadline_minutes` in 1–1440.
`max_items` bounds the candidates considered for admission; `batch_size` bounds
members in one attempt, defaults to five capped by `max_items`, and must be
1–min(50, `max_items`). One consumer has at most one active attempt; a batch
shares a pinned source and stored crew. The trigger retry budget is capped by
`policy.retries.max`; retries use the member evaluator's five-minute backoff
and captured absolute deadline, rather than cron slot/backoff semantics.

For preparation, optional `eligibility` defaults to the values above: statuses
are a nonempty subset of `proposed`/`backlog`, excluded tags veto a task,
all required tags must be present, and an empty `task_types` admits every type.
Optional `freshness` resolves each key from the routine, then
`[workflow.task_pilot_freshness]` in `config.toml`, then built-in defaults:
`material_fields: [title, description, criteria, plan, selectors]` and
`source_sensitivity: ignore`. Thus an ordinary branch-head move or retagging
does not by itself re-pilot an assessed task. `context_files` and `any` are the
other source sensitivities. Predicate/freshness edits invalidate affected
assessments; neither block configures finding verification. See
[State preparation](../automation-triggers/5_operations.md#state-preparation-orb-11331)
for all material fields, stale-source checks during apply, limits and rollback.

Inspect `orbit routine list`, `orbit routine show <name> --format json`, and
`orbit clock tick --dry-run --format json` before setting `enabled: true`.
The seed resolves the name, owner and branch, starts disabled, and evaluates as
`disabled` until opted in; an empty eligible inventory is `fresh` (or
`debouncing` while pending material settles). The existing
[routine seed boundary fixture](../../../crates/orbit-cli/tests/workspace/routine_state_seed.rs)
exercises that configuration through init, show and dry-run tick.
Explicit `orbit run task-pilot <id>` remains independent of this scheduled gate.
Preparation records assessments, including unready verdicts; it does not promote
or dispatch the assessed tasks or grant commit/merge authority.

### 1.2 Delivery routines

Use `trigger.deliveries_landed` in place of cron/state, retaining an existing
catalog job that implements the coverage evidence contract:

```yaml
trigger:
  deliveries_landed:
    branch: agent-main
    threshold: 3
    max_wait_minutes: 360
    coverage: landed_code_review_v1
    max_items: 20
    retries: 1
policy:
  overlap: forbid
  retries: {max: 1, backoff_minutes: 5}
```

`owner_machine` is optional: omission resolves to the registered workspace owner;
an explicit stable machine ID overrides it. Only the resolved owner admits work.
An enabled definition reports `owned_elsewhere` or `ownership_unresolved` when
it cannot admit here. This is separate from the retired top-level `hosts:` pin.
Require `1 <= threshold <= max_items <= 50`, positive `max_wait_minutes`,
`retries` in 0–5 and `overlap: forbid`. Defaults are 50 items and zero retries;
the routine policy caps the trigger's retries. Select `landed_code_review_v1`
for new definitions; `integrated_qa_v1` is retired compatibility data.
`missed_run` applies to cron slots, not delivery or state eligibility.

The supported ownership/migration rules are in
[Configuration and migration](../automation-triggers/5_operations.md#configuration-and-migration).
A routine job must return `coverage_evidence` in a persisted step result matching
its frozen `input.automation` and admitted run; ordinary job success alone is
insufficient. See [Evidence submission](../automation-triggers/5_operations.md#evidence-submission)
for the exact receipt fields and validation, and
[Inspection and recovery](../automation-triggers/5_operations.md#inspection-and-recovery)
for persisted debt and failure reasons. Auto-tasks use the same delivery object
under `schedule.deliveries_landed`, creating tasks instead of submitting jobs.

### Seeded defaults and ownership

`orbit workspace init` seeds `ci_failure_sweep.yaml`, `dependabot_alert_sweep.yaml`,
`task_pilot.yaml`, `finding_verification.yaml`, `ship_sweep.yaml`, `worktree_gc.yaml`, and `store_gc.yaml`
with a workspace-unique name and `enabled: false`. The cron defaults resolve nothing else at
seed time, so two hosts initializing the same workspace name write byte-identical cron
definitions [ORB-12236]. `task_pilot.yaml` is a `preparation_eligible` state routine
[ORB-12745]; a state trigger names the one machine that evaluates it, so its
`owner_machine` renders this host's registered machine id and its `branch` the workspace's
registered base branch — the only host-dependent bytes a seed writes. `finding_verification.yaml`
is a `finding_unverified` state routine rendered the same way [ORB-15662]. Which edits re-pilot an
assessed task is `[workflow.task_pilot_freshness]` in `config.toml` (default: title,
description, criteria, plan and selectors; head moves ignored), overridable by the routine's
optional `trigger.state.freshness` block [ORB-13638]. Auto-task definitions
are evaluated by the tick directly; there is no seeded auto-task scheduler routine. The definition's
`enabled` field is the opt-in: changing it to `true` deliberately grants that scheduled
capability in the workspace.

`worktree_gc.yaml` uses `missed_run: catch_up_once`, so a gap in that hourly backstop runs one make-up GC instead of dropping the slots. Other seeded routines keep their own `missed_run` policy.

The files under `crates/orbit-core/assets/routines/retired/` are provenance shapes of retired defaults, not seeds: `orbit workspace sync` compares a workspace definition with them to tell an Orbit-seeded copy from an operator's.

Seeded files become workspace-authored immediately. Plain re-init is create-if-missing:
it adds a newly shipped default or recreates a deleted default, but byte-for-byte preserves
existing definitions, including `enabled`, cron, and policy edits. Only destructive
force initialization recreates the workspace and therefore restores template defaults.

After [ORB-10800] / [All five definition-artifact kinds carry managed provenance, and doctor reports it](../activity-job/4_decisions.md#all-five-definition-artifact-kinds-carry-managed-provenance-and-doctor-reports-it), routine seeding is manifest-aware: `.orbit/routines/`
carries a `.orbit-managed-assets.json` recording the digest Orbit last wrote for each
seeded default. The digest is taken over the *rendered* document — after the routine-name
placeholder resolves — because that is what actually lands on disk. Two
consequences follow. A default dropped from a later release is retired by content
provenance rather than lingering forever in every existing workspace, and re-seeding
unchanged content is a genuine no-op rather than a rewrite. A
routine an operator has edited is never deleted: it is preserved under
`.retired-managed/routines/`. `orbit doctor` reports routine artifacts as faulty,
deprecated, or stale, and `orbit doctor --fix-stale-artifacts` performs the retirement.

The recorded digest alone is too strict across releases [DANI-10392]. A workspace
seeded by an earlier release and then used as documented — opted in with
`enabled: true`, or with the retired `hosts:` key deleted as the loader's warning
instructs — no longer matches that digest, and treating that as a local edit left
the upgrade unable to converge: the retired default kept failing to load on every
tick while `workspace sync` demanded a manual move that changed nothing. So
provenance is byte-exact first and shape-aware second. A file that differs from a
template *this or a prior release shipped for that stem* only in fields the
operator owns — `enabled`, the retired `hosts:` key — is still Orbit's. The
shipped historical shapes are `assets/routines/retired/` (defaults this Orbit no
longer ships) and `assets/routines/superseded/` (earlier shapes of defaults it
still ships); retiring a default or changing a template's fields adds an entry to
one of them in the same change. The consequences:

- A retired default matching such a shape retires without operator action. Orbit
  deletes outright only bytes it can prove it wrote; a lifecycle variant is copied
  to `.retired-managed/routines/` first, so nothing the operator wrote is lost.
- A stale shipped default matching a superseded shape is refreshed onto the
  current template **with its `enabled` setting carried over**, so convergence
  never silently switches off a routine an operator opted into. A cron
  `task_pilot.yaml` refreshed onto the state form takes this host as its owner;
  a recorded binding that already names an owner keeps it, exactly as a
  recorded name is kept [ORB-12745].
- A current-shape default whose only difference is a lifecycle setting is adopted
  in place — recorded as Orbit's without being rewritten.
- A default a prior release wrote without recording it in the manifest is adopted
  the same way when it carries the name this workspace seeds. An operator's own
  routine wearing a bundled filename declares its own name, so it is still
  reported as a collision and preserved.
- A *retired* default the manifest never recorded is judged by content too
  [DANI-10502]. It wears no shipped name, so the adoption rule above cannot
  reach it and the manifest-driven retirement cannot see it; before, it stayed
  in the active catalog forever while every surface advised a sync that
  reported `unchanged`. Reconciliation now scans the directory for definitions
  targeting a retired job: one matching a retired shape leaves the active
  catalog, always with a copy under `.retired-managed/routines/` since no
  recorded digest proves Orbit wrote those exact bytes; anything else is the
  operator's own file, preserved in place and reported with the step that
  clears it.
- Any other difference — cadence, target, policy, description, an added comment —
  is a local edit: preserved, reported, never rewritten.

`orbit doctor` classifies routine provenance through the same helper, so
`--fix-stale-artifacts` and `workspace sync` never disagree about whether a
retired default is safe to remove.

Reconciliation is confined to the catalog. The `routines/` directory, its
manifest, each definition, and the `.retired-managed/routines/` route must be a
real directory or regular file, or absent, judged without following links. Every
creation, refresh (including a lifecycle-variant refresh), retirement, and
preserved-copy move checks this first. Definition writes stage the complete content
in an exclusively created sibling file (`O_NOFOLLOW` on Unix), sync that file,
rename it over the definition, and sync the catalog directory. Readers see a complete
old or new definition; a failed or interrupted staging write leaves the previous
definition intact. A final-component link installed after inspection is replaced by
the rename rather than written through. A symbolic link found during inspection, dangling
or not, is reported as preserved with its path and left untouched. Its manifest
provenance is kept, so a later sync finishes the work once the operator replaces
the link. A linked catalog or manifest refuses the whole routine catalog,
matching the loader, which skips the same links.

A routines directory carrying no manifest at all predates that provenance, and its routines
are customized by design — flipping `enabled` is the lifecycle the templates invite. Content alone cannot separate such a routine from a file the operator wrote
from scratch with the same seeded name, so reconciliation adopts it [ORB-11154]: the binding is parsed from that exact
on-disk instance, and the bytes already on disk are recorded as the ones Orbit owns. Nothing
is rewritten, no warning is emitted, and a later shipped-template change refreshes the routine
onto the adopted binding instead of being reported as a collision forever. Adoption is skipped
for a file that does not parse as a routine, declares a different name from the one this
workspace seeds, or has a binding that could not be re-rendered later. A differently named
routine is reported as a user-authored collision and remains outside managed provenance,
so later template changes cannot replace it. Once the directory has a manifest,
a shipped name the manifest does not claim is genuinely
user-authored and is still reported and preserved in place.

The seeded `ci_failure_sweep` targets `job:ci_failure_sweep_pipeline` hourly at
`5 * * * *` — deliberately clear of the other defaults' minutes — with `missed_run: skip`
and `overlap: forbid`. The pipeline runs every GitHub query on the host, files each
current, non-stale landing failure cluster as a proposed bug task carrying that evidence inline,
using the actual job checkout: a landing-ref push must test its event commit,
while a PR or merge-queue checkout must match an observed landing tip.
Unmerged `orbit/<task>` branch failures are recorded as idempotent evidence artifacts
on the owning task, with no remediation task or pilot candidate. Retention checks
for a protecting execution claim inside the same exclusive commit boundary as
claim settlement: it queues the receipt while protection remains, or commits the
artifact and applied receipt row immediately when the owner is unprotected.
Settlement drains queued receipts in its own commit; replays retain one artifact.
A receipt failure is recorded per observation and does not abort independent
landing-failure filing. Missing task owners
remain retryable, and other non-landing failures are explicitly excluded.
The existing freshness selection still precedes routing; it also sets aside runs on
GitHub's managed `Dependabot Updates` workflow with event `dynamic`, recording
their identities and reason `github_managed_dependabot_update` in collection's
`skipped_runs` and the filing audit before branch probes or investigation. They
are excluded from current, mixed-state and deferred failures; open security alerts
belong to the Dependabot alert sweep. Pull-request CI on `dependabot/*` branches
remains ordinary CI. Freshness selection also sets aside runs on
branches whose pull request closed at the branch's current head with no open pull request,
jobs a workflow concurrency group cancelled, and a run-scoped retryable error repeated on
three distinct consecutive workflow sweeps is reported as persistent instead of failing every
sweep. A retry of the same workflow run counts once. The pipeline
dedupes against still-open owners by failure key, and pilots each candidate through the
existing task-pilot job. The all-join lets independently valid pilots apply even when a
sibling is stale or fails. Within a returned partition, deterministic apply also commits
valid tasks independently, then sends only invalid assessments through one targeted repair
attempt at the original pinned revision; material edits require fresh preparation.
Tasks admitted to a workflow, held by a follower execution claim, or made terminal while
the pilot runs settle as `superseded` without pilot writes. A durable task edit by a human
or another writer also refuses the stale write and settles as `superseded`, including
context edits and an implementer's new plan. Prepare reads each task's history boundary no
later than the snapshot apply compares against, so an admission racing preparation is never
hidden behind it; an automatically selected task moved out of selection before its hydration
settles `superseded` in prepare itself. A task whose selector material the branch
head changed after its routine claim froze the source settles `superseded_by_source`; its
member is claimed afresh at the head rather than retried or retired
([automation triggers §6](../automation-triggers/2_design.md#6-pilot-eligibility-and-freshness)).
A false agent snapshot without a durable
edit, dependency meaning drift, and invalid assessment envelopes remain failures.
Superseded outcomes survive a sibling's targeted repair and count as resolved, so the
pilot succeeds and the sweep's child success guard accepts it. A following
`pipeline_success_guard` fails the parent while any task remains unresolved. The guard is
skipped only for a filer-reported zero-candidate
result, so empty clean/deduped sweeps remain no-ops without hiding failed work.

A release-head failure remains current even when the same workflow is green on the
integration head: freshness is scoped to workflow and ref for both landing branches. If a
pilot proves the repair already landed on integration, admission leaves the deduped task in
proposed quarantine and reports `release_promotion_or_hotfix_needed` with the red release
SHA/run and the pilot's covering repair evidence. The sweep performs neither release
promotion nor hotfix dispatch. A `verified_no_diff` assessment is never promoted: admission
reports it as `pilot_verified_no_diff` with the pilot's evidence and the commits it cites.
After the pilot write the task is archived, with a system comment naming the covering
commits and the assessment, only when every cited commit exists and at least one is an
ancestor of the base branch that relates to the finding. The commits the finding itself
names (any commit in its description, and the landed commit of a `regression_from` target)
never count, since they are on the base branch by construction. When one is known, a
covering commit must descend from all of them, and when the task declared path selectors
it must touch one of those paths. An `--approve-proposed` drain applies the same rule.
Without that proof (no commit cited, an unknown SHA, none on the base branch, or only
commits unrelated to the finding) the task stays proposed with that reason. The routine is a scheduling surface only: an
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
branch, invokes and waits for `workspace_auto_pipeline` without explicit task IDs, and guards
its result. It never calls the legacy cross-workspace CLI sweep or consults
`[workflow] auto_ship`; the child's existing empty-backlog path is a successful no-op.
Waiting keeps the wrapper run active for the whole shipment, so routine overlap protection
covers the child rather than only submission ([Delegate workspace ship routines through a synchronous wrapper job](./4_decisions.md#delegate-workspace-ship-routines-through-a-synchronous-wrapper-job)).

### Delivered worktree cleanup

Successful delivery runs remove their own task worktree after the run has been
durably terminalized. `task_pr_pipeline` does this only after `pr_complete` has
verified the PR's merged state and completed the task; `task_local_pipeline`
uses the same boundary after local delivery. The retired `epic_pipeline` is no
longer a delivery owner, but the shared identity derivation still decodes its
stored `epic_task_id` input to the `epic-epic-<epic_task_id>` worktree, so the
scheduled sweep can still reap what a historical epic run left behind
[ORB-12491]. The
`workspace_auto_pipeline` and `task_gate_pipeline` jobs are coordinators: their
child delivery run owns the worktree and performs the removal, so a drain does
not need to wait for a separate GC fire.

This boundary delegates to the same `collect_worktrees` classifier used by the
`worktree_gc_pipeline` job. It therefore requires a terminal run, a settled task
(`done`, `rejected`, or `archived`), a registered real worktree, and a clean Git
status before removing the directory and branch. Unsettled tasks (including
`review`), non-terminal runs, unresolved tasks, and dirty trees remain on disk as
evidence. The removal report is written into the run pipeline state under
`worktree_cleanup`, with the
same path, task id, action, and `bytes_reclaimed` fields as scheduled GC; it is
visible from `orbit run show` even after the run is terminal.

Delivery cleanup indexes every recorded run before applying its run filter,
just as the scheduled sweep does. A stable worktree token or task fallback
shared with a non-terminal run stays `skipped:ambiguous_run_path`, preserving
both the checkout and its branch. Once all mapped runs are terminal, the path
can be collected only if every mapped run passes the task and Git safety gates
(and the worker-liveness gate for build-output collection). Each shared path is
removed at most once in a sweep.

The earlier unexplained removals were the setup recovery path, not delivery GC:
`ensure_worktree` removes an owned incomplete checkout during path reuse, and
`recover_worktree_add_timeout` removes an owned incomplete checkout left by a
timed-out `git worktree add`. Both use the sanctioned cleanup helper and may
force removal because setup has proved the checkout incomplete and without
retained work. That path explains why leftovers such as `0518-c2`, `0544-c5`,
`0544-c6`, and `0548-c3` could disappear between 06:11 and 06:46 UTC, but it
never considered a successfully delivered, complete checkout. The six later
worktrees therefore remained until manual GC: successful delivery released task
reservations but had no terminal cleanup hook. This change closes that gap while
leaving setup recovery's evidence-preserving gates unchanged.

The embedded GC job keeps the hourly routine as a backstop, with
`older_than_hours: 1`. Immediate delivery cleanup is the normal lifetime; the
one-hour threshold bounds the exceptional lifetime after a cleanup/reporting
failure without making scheduled GC the delivery path.

The same job prunes the owning checkout's `.orbit/tmp` by age through the
`scratch_older_than_hours` input (default 24). That scratch is not tied to a
run's lifetime, so an age window rather than a delivery hook decides when an
entry is disposable.

---

## 2. Discovery and Registration

Discovery reuses the global workspace registry (`~/.orbit/workspaces.json`) rather than a
new pointer mechanism — the same shape `orbit run ship-sweep` established for unattended
cross-workspace dispatch.

Registering an **owner** checkout is the whole opt-in — there is no config key
([Registration is the automation opt-in](./4_decisions.md#registration-is-the-automation-opt-in-there-is-no-routine-source-role)). On each pass, sweep loads the registry and loads
`.orbit/routines/*.yaml` from every registered, active checkout whose `.orbit/`
directory exists. Replica checkouts cannot write the owner's coordination store, so they
contribute one kind of routine only (§2.1). Two properties fall out:

- **Registration is what already exists.** Registering the workspace with Orbit is the
  entire setup; nothing versioned has to agree per host.
- **Centralization is convention, not mechanism.** The constellation keeps all routines in
  a central workspace; the mechanism tolerates additional sources, and `orbit routine list`
  names each routine's source workspace so provenance is never ambiguous.

### 2.1 Replica checkouts: host-local worktree GC and store GC

A replica checkout schedules a cron routine only when it targets one of two
jobs. Owner and replica definitions load together, so a name claimed in both
still fails closed.

`job:worktree_gc_pipeline` reclaims this host's own run worktrees [ORB-14173].
It reads historical tasks carrying this machine's prefix locally and asks the
owner over its tool surface only for the owner's prefix; it writes nothing to
the owner's task store. The owner prefix comes from existing claim admissions,
or a single foreign prefix in this workspace's stored task ids before any pull.
Other prefixes, including ambiguous foreign mirrors without admissions, are
retained as `skipped:task_prefix_unroutable` without an owner call.

`job:store_gc_pipeline` prunes this host's own store [ORB-16354]. The activity
opens the local sqlite store and this workspace's audit directory. It deletes
command-audit rows and resource history past their windows (those tables are
host-wide on this machine) and this workspace's run-audit rows, unreferenced
audit blobs, and the pipeline state of old terminal runs. Run rows and steps
stay. It does not call the owner and does not read or write the owner's
coordination store. A follower-only host otherwise cannot trim that store:
every checkout is a replica, and the owner's store-gc routine runs against the
owner's store, not the follower's.

Every other replica definition — ship sweep, task pilot, CI and Dependabot sweeps, and
any delivery- or state-triggered routine, including one targeting either GC job — is owner
work. The sweep reports it as `skipped` with an `owner_only_in_replica:` reason naming
the owner machine and never dispatches it. `orbit routine list` and `show`, the
dashboard's `owner_only` rows and the MCP `orbit.routine.control` list all project the
same rule from one Core predicate, so a toggle is offered exactly where it succeeds. A
toggle or pause of an owner-only routine is refused with the owner named and writes
nothing. Auto-task evaluation and blocked-task recovery never run in a replica.
Core refuses auto-task definition add, update, toggle, delete, and restore, as
well as manual mint, across the CLI, dashboard, and MCP surfaces. Read-only
definition views and delivery recovery/reset previews remain available; apply
operations stay owner-only. Refusals name the owner machine.

Worktree cleanup therefore has two paths. On the owner, a delivery run removes its own
worktree once it lands (§1, *Delivered worktree cleanup*), with the owner's hourly GC
routine as the backstop. On a replica, a claimed leaf's worktree is reclaimed only by
the replica's own scheduled GC, once its claim handoff was accepted and settled
with the owner or every task's authoritative store reports it settled. A missing
task or an unreachable owner leaves the worktree in place.

A `config.toml` written before [ORB-12236] may still carry `[routines] role = "source"`;
it loads with a warning for one release and selects nothing.

Host identity is the one genuinely host-local datum: the `[machine]` table of the global
`~/.orbit/config.toml` carries the generated `id` (the machine ID), the operator-chosen
`name`, and the immutable `task_prefix`. `orbit init` owns identity creation; a legacy
`host.toml` from before [ORB-12725] is migrated into that table once.
`orbit routine init --install-clock` only installs the OS clock unit (§5). A malformed
`[machine]` table is an error, not a fallback. The stable `id` participates in
owner-only routine admission: a state trigger compares `owner_machine` with the local
machine ID, and replicas classify delivery/state routines owned elsewhere as
owner-only and skip them (§1.1–1.2). The operator-chosen `name` remains the display
label, while `task_prefix` selects this machine's task-ID namespace.

The implementation boundary is vertical: `crates/orbit-cmd/src/registry/routines.rs` reads
the `[machine]` identity from `config.toml` and `workspaces.json` through `orbit-registry`,
validates local checkout paths, and constructs registered runtimes through `registry_runtime`.
It projects those inputs through `RoutineWorkspaceProvider` into
`crates/orbit-core/src/application/routines/`. Core owns the registry-neutral scheduler and does not
read either registry file directly. There is no hub snapshot, satellite cache, fleet
health, or remote placement service in the v1 path.

---

## 3. Clock tick

`orbit clock tick` is the stateless entrypoint the OS clock invokes every minute;
`orbit sweep` invokes the same implementation as a compatibility alias. Like
`ship-sweep`, it never bootstraps a workspace from the caller's cwd, isolates per-routine
failures, and exits non-zero on infrastructure errors such as malformed host identity,
an unreadable registry, or an unopenable store. A valid empty local registry simply
produces no routines and exits successfully.

Per pass:

1. Take a host-global advisory lock (in the host store, §4). If another sweep holds it,
   exit immediately — overlapping invocations from a slow prior pass must not double-fire.
2. Load local `workspaces.json`, validate checkout paths, persist any resulting status
   updates, and build runtimes for active local **owner** checkouts whose `.orbit/`
   directory exists. Collect routines from each, failing closed per source or definition
   without stopping other valid sources.
3. Apply definition enablement and host-local pause controls. A locally paused routine
   skips evaluation. Unpaused delivery/state consumers still reconcile already-admitted
   work when disabled, but admit nothing new.
4. Sync unresolved fires against actual run state. A dispatched `running`/`retrying` run
   whose recorded owner is conclusively stopped is failed immediately, including after a
   host restart; an alive or unprobeable owner keeps its overlap slot. Other unresolved
   entries are reclaimed after the routine's `timeout_minutes` staleness horizon, so a
   sweep that crashed between intent and dispatch cannot block `overlap: forbid` forever.
   Fires recorded for a routine this host no longer loads are deliberately untouched.
5. Route delivery/state definitions to the shared automation evaluator (§3.1).
   For each cron definition, compute due-ness from the cron expression and the persisted cursor
   (last slot, else the first-observation baseline — a routine never fires for slots that
   predate its registration on this host; the first sweep records the baseline and fires
   nothing). Due-ness is O(1) via previous-occurrence lookup, never a walk over every
   missed slot; `missed_run` policy decides gaps. A slot is "natural" for two configured
   clock-cadence intervals after its scheduled time: the default 60s clock therefore keeps
   the existing 120s grace, while a configured 300s clock keeps a slot natural for 600s.
   This admits one delayed or missed poll without recording that slot as missed. Past that
   grace, `skip` records each unconsumed slot (oldest first, at most 64 per tick) as a
   skipped fire with detail `missed_run: skip` and does not dispatch. A longer gap continues
   on the next tick; the cursor does not jump past a slot this pass did not record, so a
   newer natural slot waits until the recorded batch catches up. `catch_up_once` dispatches
   one make-up for the latest slot and writes no skip rows. Skip rows are not recorded while
   any fire is unfinished or a failed fire still has retry budget. `overlap: forbid` holds
   on any unfinished fire (`intent` or `dispatched`), not only the newest row, so a later
   skipped slot cannot hide a run that is still going.
6. For each due cron routine: check `overlap` against in-flight fires, record the fire intent
   (idempotency key: routine name + scheduled slot + attempt, transactionally with the
   cursor advance), then dispatch the target via `submit_pipeline_run` in the routine's
   source workspace with actor `routine/<name>` as run provenance.
   Routine dispatch supplies the run base input with exactly one reserved internal field,
   `__routine_dispatch_orbit_dir`, containing the source workspace's `.orbit` directory.
   This is workspace-routing metadata, not a caller parameter: cron routine target jobs
   accept no caller-supplied parameters, and every step in such a job must declare its own
   `default_input` rather than inheriting the job base input (which would expose the
   reserved field to the activity).
   A due `workspace_ship_pipeline` slot's first attempt is consumed as `skipped` when a
   `workspace_auto_pipeline` is already running or retrying in its source
   workspace. A retry of a failed fire yields to that drain without writing another
   skipped row, so the attempt stays free. A skipped retry already stored on the slot
   does not stick later ticks on `slot_already_claimed`: the sweep dispatches the next
   free attempt, and that skipped row does not spend `retries.max`. The tick reports
   `workspace_drain_live: <run id>` even without
   `--verbose`; `orbit routine show` retains that detail in recent fires when the
   first attempt was recorded. It does not queue a sweep behind a manual drain: the
   later sweep would use its
   own review completion policy after a manual `--complete` window expires.
   Readiness names the running `workspace_auto_pipeline` and its live worker
   ceiling in `capacity.drain_run_id` and `capacity.max_active_leaf_runs`.
   Pending coordinators appear separately in `capacity.queued_drains` with
   their run id, submitted ceiling, completion policy, `allowed_crews` (`null`
   when unrestricted), window (`for_seconds`, `0` for a single pass),
   `submitted_at`, the `execution.env.pass` names their submission did not hold
   (`env_pass_unset`, names only), and `uncredentialed_crews`: the Claude crews
   in their window when that submission recorded every listed Claude credential
   name unset. `orbit run readiness` prints each as a `Queued drain:` line, and
   the dashboard Drain card lists them under the live window with a per-run
   cancel.
7. The detached worker clears `ORBIT_ROOT` unconditionally. With no explicit `--root`, its
   cwd is therefore the workspace selector; when a parent explicitly forwarded `--root`,
   that argument is the selector. Before any step executes, the worker compares the
   declared `__routine_dispatch_orbit_dir` directly with its resolved `orbit_dir`. A
   mismatch is a workspace-identity refusal: the worker persists a diagnostic step with
   error code `routine_dispatch_workspace_mismatch`, cancels the run before step execution,
   and returns the mismatch error. The final state is intentionally `cancelled` (the
   existing cancellation contract is preserved), but it is no longer an unexplained bare
   cancellation. `orbit run show` exposes the persisted error code and declared-versus-
   resolved path message; because no activity output exists, `orbit run logs` falls back to
   the run's worker log, which also contains the diagnostic.
8. The identity check is a direct path comparison, not a normalized workspace-equivalence
   check. A workspace whose `.orbit/config.toml` redirects `root` to a different directory
   is not expected to satisfy the gate unless the worker's resolved `orbit_dir` is exactly
   the declared `.orbit` path. The gate deliberately refuses that redirected-root case so
   routine provenance cannot silently resolve to a different store.
9. Run the auto-task evaluator directly over every discovered runtime, after routine
   evaluation and while still holding the host sweep lock. A due definition mints a task
   without creating a job run. Per-definition rows (`name`, `action`, `slot`, `task_id`,
   `reason`) join routine rows in the report. One definition or workspace error becomes an
   `error` row and does not stop routines or other workspaces. The phase is bounded by the
   finite discovered workspace and definition collections.
10. Record outcomes and exit. In dry-run mode both evaluators report what would happen and
    write no routine fire, auto-task cursor, task, or run state.

The global `--workspace <selector>` narrows one pass to a single registered
workspace: discovery visits only that workspace, so nothing outside it is
evaluated, fired, or recorded, in dry-run and live passes alike. The selector is
resolved against the local registry before the pass touches scheduler state, and
an unknown, unregistered, or inactive selector fails the invocation rather than
silently sweeping the host [ORB-12108].

`orbit routine list`, `orbit routine show`, and `orbit clock tick` name this host and each
routine's source workspace in human and JSON output. Scheduler state never moves between
hosts: a host that starts evaluating a definition has no migrated cursor, so its first
sweep of a cron definition records the normal first-observation baseline and only the
next natural slot can fire. Delivery/state initialization follows §3.1; the other
host's cursors, fires, checkpoints and pauses are untouched.

Fires are normal runs: they appear in run history, carry v2 audit envelopes, and are
debuggable with the existing run tooling — there is no separate "scheduled run" ledger.

Every run records how it was submitted in its `trigger`, which run JSON projects and the
`run.started` audit names as `<kind>:<name>`. `kind` is `cli`, `mcp` (a call over an MCP
session), `dashboard` (`orbit web serve`), `child` (dispatched by a parent run), or
`routine`. A cron fire carries `routine` and `slot`; a run a state-triggered or
delivery-triggered routine admits carries `routine` and the automation `consumer` that
admitted it, with no slot [ORB-12255, ORB-13016].

Naming note: `orbit clock tick` is the general scheduler pass and `orbit sweep` is its
compatibility alias. The seeded `ship_sweep` routine is workspace-local; the legacy
`orbit run ship-sweep` cross-workspace entrypoint is separate.

### 3.1 Delivery and state evaluation

Core supplies authoritative source/task facts and submits jobs; `orbit-automation`
owns due decisions, frozen inputs and receipt validation; Store owns atomic
consumer checkpoints and generation fences. Both variants run within the same
locked clock tick, without cron slots or a second scheduler. Dry-run computes
diagnostics without persisting state or dispatching work.

- **Delivery:** the first enabled observation records the configured branch head
  as an explicit baseline exclusion, with no historical replay or coverage claim.
  Subsequent bounded observations retain verified landing keys, unexamined commits
  and unresolved evidence. A threshold of uncovered eligible deliveries, or the
  maximum age of nonempty pending work, freezes one immutable batch. New arrivals
  remain pending while it runs. Observation and successful coverage have separate
  checkpoints: only an accepted receipt covering the frozen obligations advances
  coverage. Failed/invalid evidence retains debt; bounded retries reuse the batch,
  and exhaustion requires attention. Verified before-PR review certificates may
  exclude review obligations; tags, task completion and no-diff work cannot create
  landings or coverage. See
  [Delivery identity and source gaps](../automation-triggers/5_operations.md#delivery-identity-and-source-gaps).
- **State:** initialization inventories current eligible members in bounded pages,
  persisting scan continuation, material fingerprints, first-seen/change times
  and withheld reasons. Preparation best-effort fetches the configured branch on
  a live tick and retains a local-ahead head or local fallback on fetch failure;
  status and dry-run use existing refs without fetching. A member is due after
  settling for `debounce_minutes` or reaching `max_wait_minutes`. Admission rechecks
  eligibility and freezes member fingerprints/source, retry budget and deadline.
  Deterministic per-member apply evidence settles assessments, including fresh
  unready results; wrapper success is not an assessment. An edit creates new debt,
  and successful apply certifies its resulting fingerprint without a self-loop.
  Exhausted members remain withheld at that fingerprint; other members can proceed.
  Timing edits affect pending work while active budgets stay captured. Kind, owner,
  target or branch edits report `definition_changed`; restore the original
  definition to settle it rather than deleting state. See
  [State preparation](../automation-triggers/5_operations.md#state-preparation-orb-11331)
  for stale-source supersession, per-member reconciliation and capacity limits.

---

## 4. Host-Local State and Toggles

All scheduler state lives host-locally in the existing host SQLite database
`~/.orbit/orbit.db`, gitignored and never synced. Cron scheduling and common
pause controls use `routine_*` tables (`orbit-store/src/driver/sqlite/routine_store/`):

- **routine_cursors** — per routine: first-observation baseline + last slot consumed.
- **routine_fires** — one row per fire attempt: `(name, slot, attempt)` idempotency key,
  state (`intent → dispatched → succeeded/failed/timed_out/error`, or
  `intent → skipped` for a ship slot blocked by a live drain), dispatched run id
  when one exists. A skipped row retains the blocking drain run id in `detail`.
- **routine_pauses** — host-local suppressions written by `orbit routine pause <name>` /
  cleared by `resume`. Durable across reboots; invisible to git.
- **sweep lock** — a `flock(2)` file lock (`~/.orbit/state/routine-sweep.lock`) rather
  than a table: the OS releases it on process death, so a crashed sweep never wedges the
  next pass and no lock-staleness logic is needed.

Delivery/state consumers additionally persist automation state, immutable
batches/attempts and accepted receipts through the automation Store contracts.
Consumer identity includes machine, workspace, definition kind and name; epochs
bind trigger/action identity. Separate observation/coverage boundaries and
member working sets survive reboot and disablement. There is no automatic
cross-host transfer. Run history remains ordinary job history, not cron fire rows;
status merges those automation runs with retained cron history. For source pins,
recovery and rollback limits see the
[operations guide](../automation-triggers/5_operations.md#rollback-and-limits).

Toggle resolution, in order: `enabled: false` (this checkout's definition) → local
pause (this host only). `orbit routine list` shows both columns plus computed
next-due, so "why didn't this fire?" is one command.

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
  configured cadence. `orbit clock enable` compares the installed timer and service with the
  embedded template, rewrites a stale definition (for example pre-fix `OnStartupSec`), and
  rewrites a service still invoking `orbit sweep` to invoke `orbit clock tick`, then
  daemon-reloads before restart [ORB-11082]. `orbit clock repair` writes the same rendered
  units for a drifted program path and daemon-reloads then restarts an enabled timer,
  leaving a disabled one on disk only. `AccuracySec=5s` bounds manager coalescing
  after each deadline [ORB-10986]. These monotonic
  triggers deliberately do not replay timer events missed while the manager or host was
  down. The first sweep after restart evaluates each routine's cursor, so `catch_up_once`
  collapses missed cron slots to one fire and `skip` waits for the next natural slot.
  Every sweep loads the same `clock.toml` cadence used to render the native timer, so
  changing the clock from 60s to 300s changes its natural-slot grace from 120s to 600s.
  The configured wake-up cost and routine enable/pause state are otherwise unchanged.

There is no resident Orbit daemon. Delivery and state eligibility are polled by
this clock; sub-minute triggers and immediate file-watch/webhook wakeups remain
out of scope.

### Existing-host migration

After upgrading, `orbit clock status` reports a native unit that still invokes
`orbit sweep`, or one naming a program that moved or no longer exists, as stale;
`orbit update` converges it automatically and `orbit clock repair` does the same on demand
for a binary another installer placed. Workspace synchronization refreshes managed routine definitions and retires
the former auto-task scheduler routine. `orbit doctor` reports that retired managed file
as deprecated, and `orbit doctor --fix-stale-artifacts` moves an unchanged seeded copy to
`.retired-managed/` while preserving an operator-edited copy there for inspection. A
copy the manifest never recorded is deprecated too, but its remediation is
`orbit workspace sync`: the repair flag deletes only bytes a recorded digest proves
Orbit wrote, and retiring an untracked file keeps a copy instead [DANI-10502].

Until that sync runs, a definition targeting a retired job is *skipped*, not failed: the
loader recognises the retired target (`RETIRED_ROUTINE_JOBS`), so `orbit routine list` shows the routine as retired with the
step that clears it, the dashboard carries it under `retired`, and a clock tick emits
one non-noteworthy `retired` row instead of a load error on every pass [DANI-10392]. A
job the workspace still defines itself resolves through the catalog first, and any other
unresolvable target remains a fail-closed load error.

Discovery cannot tell a definition Orbit seeded from one the operator wrote, so it
states the synchronization step and Core — the layer owning the templates and the
manifest — narrows it: a definition the sync would leave exactly where it is carries
*delete or retarget* instead [DANI-10502]. The advice named on any surface is
therefore always one that changes something.

Legacy `[routines] role` and routine `hosts:` fields warn during their compatibility
window but no longer affect eligibility. Every registered owner checkout with an
enabled host clock evaluates against its own store. Cron schedules are independent
on each owner; delivery/state admission still requires the consumer's owner machine.

---

## 6. Concerns & Honest Limitations

- **No distributed coordination.** Cron runs independently per owner checkout;
  delivery/state consumers admit only on their owner machine. This is a binding,
  not a distributed lease or automatic failover/state-transfer mechanism.
  A cron definition with a repo-global side effect must
  dedupe itself or pause on the extra owners
  ([3_vision.md §1](./3_vision.md#1-open-questions)).
- **Definition locality.** Sweep reads whatever is on this checkout's disk. Definitions
  do not converge via git; each owner edits their own `.orbit/routines/`. A fresh clone
  gets shipped defaults from `orbit workspace init`.
- **Scheduled execution is a real capability escalation.** A routine source workspace is
  scheduled code execution on every host that trusts it. Targets are catalog-resolved (no
  inline commands) and run under existing activity/job policy, but note the sandbox caveat
  recorded in the External Executor Protocol retirement decision (ORB-10395): enforcement depends on which runtime path the target takes.
  Review of this checkout's definitions is part of the security boundary.
- **Minute granularity, host-local time.** Cron is evaluated in host-local time; DST folds
  can skip or double a slot exactly as classic cron does. The idempotency key (name + slot)
  prevents double *fires* for the same slot but cannot invent a skipped slot.
- **`catch_up_once` collapses history.** After a week of laptop sleep, a nightly routine
  fires once, not seven times. This is the intended semantic for make-up work (the seeded
  worktree-gc routine included), but it is a lossy default; a count-preserving mode would
  be a new `missed_run` variant. `skip` does not replay the gap either: it records each
  missed slot with a reason and dispatches nothing until the next natural slot.
- **Cron overlap recovery is on the dispatcher.** `overlap: forbid` depends on
  accurate in-flight bookkeeping and looks for any unfinished fire, not only the newest
  row. A crashed sweep that recorded a fire intent but died
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
- **Cron routines carry no caller input payload.** Cron dispatch injects only the reserved internal
  `__routine_dispatch_orbit_dir` field naming the owning workspace's `.orbit` directory.
  Jobs meant for routines still take no caller-supplied parameters, and each step should
  declare its own `default_input` so that this routing field is never inherited as activity
  input. Parameterized cron fires would be a schema addition. Delivery/state
  routines receive server-issued frozen automation input (§3.1), not arbitrary
  caller parameters.

---

## Task References

- [ORB-11315] — specified shared triggers and durable coverage; delivery/state
  evaluation shipped in [ORB-11330]/[ORB-11331], while broader extensions remain deferred.

- [ORB-10001] — authored this design-doc folder (proposal; no implementation).
- [ORB-10021] — implemented routines v1 (types, store, sweep, CLI, clock units).
- [ORB-12236] — removed the `hosts:` pin, its placement validation, and the
  `[routines] role = "source"` key; eligibility is a registered owner checkout plus the
  definition's own switches.
- [ORB-10270] — historically implemented fleet-aware pin validation, retired with the pin.
- [ORB-10319] — historical boundary extraction; current workspace composition
  lives in `orbit-cmd` over `orbit-registry` local files.
- [ORB-10207] — added disabled-by-default seeding and workspace-local ship sweep.
- [ORB-00374] — removed the `shell` activity variant and `run_shell` dispatch (fail-closed);
  routines inherit this constraint.
- [ORB-10875] — added dashboard routine and host-clock status and controls.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
