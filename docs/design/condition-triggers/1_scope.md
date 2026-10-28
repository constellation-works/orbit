---
title: "Scope: Condition triggers"
owner: claude
last_updated: 2026-10-27
last_validated: 2026-10-27
status: Draft
stage: designing
next: "Review this scope; then file phase 1 (CEL schema, load-time checking, automation explain)"
feature: condition-triggers
doc_role: scope
type: design
summary: "Scope: a condition trigger kind whose predicate is a type-checked CEL expression over task, workspace, host and file state, fired once per subject edge with a durable fire identity, beside the typed trigger kinds"
tags: [condition-triggers, automation-triggers, routines, auto-tasks, scheduling, cel]
---

# Scope: Condition triggers

Status: proposed. This doc is the design deliverable of [ORB-19288]; no code in this
change. It reverses one recorded non-goal of
[automation triggers](../automation-triggers/1_overview.md#1-motivation), "user
expressions/scripts", for predicates only. The other non-goals stay: a generic event
bus, scripts, arbitrary transition subscriptions, cross-host failover and a second
scheduler.

Bearing: Orbit already has these pieces, all read from source at `d0cfa7a04`.

- **Two trigger carriers.** A routine (`.orbit/routines/*.yaml`) runs one catalog job.
  Its `trigger` takes one of `cron`, `deliveries_landed` or `state: {kind: …}`
  (`RoutineTrigger`, `crates/orbit-types/src/workflow/routine.rs`). An auto-task
  (`.orbit/auto_tasks/*.yaml`) mints one task from a template. Its `schedule` takes
  exactly one of `cron`, `every_minutes` or `deliveries_landed`
  (`AutoTaskSchedule` and `RawSchedule`,
  `crates/orbit-types/src/workflow/auto_task/definition.rs`). Both structs refuse
  unknown fields.
- **Typed state kinds.** `StateTriggerKind`
  (`crates/orbit-types/src/workflow/automation/members.rs`) has these kinds:
  `preparation_eligible`, `finding_unverified`, `pilot_warnings_held`,
  `block_escalated`, `decision_requested`, and the retired `execution_failed`. Each
  kind has one Core observation rule
  (`crates/orbit-core/src/application/automation/members/member_host/observation.rs`).
  The only configurable predicate is `PreparationEligibility`, a fixed struct of
  statuses, required tags, excluded tags and task types.
- **One clock and one store.** `orbit clock tick` runs once per
  `cadence_seconds` (default 60, `crates/orbit-core/src/application/routines/clock/settings.rs`).
  It visits each registered owner checkout on the host
  (`crates/orbit-automation/src/routines/loader.rs`) and evaluates routines and
  auto-tasks under a 5-minute `TICK_DEADLINE`
  (`crates/orbit-automation/src/routines/sweep.rs`).
- **Fenced checkpoints.** Consumer state is committed with a generation
  compare-and-swap. A losing writer defers with `ConcurrentEvaluation`
  (`crates/orbit-automation/src/checkpoint.rs`).
- **Keyed actions.** Members claim attempts with an `action_key`. Before a job is
  submitted, the job store resolves that key to any existing run
  (`automation_job_for_key`,
  `crates/orbit-core/src/application/automation/members/member_host/host.rs`). Task
  creation can reserve a key, its input digest and the task ID before it writes the
  bundle (`add_task_admitted_guarded`, `crates/orbit-core/src/application/task/add.rs`).
  Time-scheduled auto-tasks use a different method. They claim a slot in a cursor
  file before minting and then record the task ID
  (`fire_slot`, `crates/orbit-automation/src/auto_tasks/scheduler.rs`).
- **Host facts.** The host resource sampler measures CPU (load per core, and measured
  utilization), memory percent, Linux I/O pressure, and disk used percent. Disk is
  measured for the filesystem that holds each of three paths: the repository root,
  `state/worktrees` and the global root (`HostResourceSample`,
  `crates/orbit-runtime/src/host_resource/mod.rs`, and `host_resource_paths`,
  `crates/orbit-runtime/src/orbit_runtime/host.rs`). A missing value means
  unavailable, never zero. Admission throttling already uses high and resume
  thresholds as hysteresis (`ResourceThrottleSettings`,
  `crates/orbit-config/src/registry/settings/machine.rs`, and
  `crates/orbit-runtime/src/host_resource/pressure.rs`).
- **Authority.** The governed-operation catalog
  (`crates/orbit-common/src/governance/authorization/catalog.rs`) marks routine and
  auto-task toggles and schedule edits from the dashboard as operator-only. The
  `orbit.auto_task.add` and `orbit.auto_task.update` tools can be granted to agents
  (`grants.rs`). Only the operator, or a registered channel's own plugin callback,
  can answer a decision request (`orbit.decision.submit`,
  [human decision channel](../human-decision-channel/2_design.md#3-delegated-human-authority)).

## Problem

Daniel wants automation that waits for a situation instead of a clock tick or a
hard-coded kind. Four situations he named (2026-10-27):

- **A.** The board has drained, so start release preparation.
- **B.** Release preparation is done, so release and tag.
- **C.** The disk is at 90%, so tell me and run GC.
- **D.** A file arrived in a folder, so run a job on it.

Today each would need a new `StateTriggerKind`: an enum variant, a Core observation
rule, a target job and a release. None of them fits a cron schedule. Polling
with an interval auto-task mints a task on every slot, whether or not anything
happened.

The non-goal had good reasons. The trigger design says a human block or a routine
metadata change must not invoke a model
([automation-triggers §2](../automation-triggers/2_design.md#2-shared-contract-and-ownership)).
Expressions also invite loops, unbounded scans and silent failures.

The current code shapes the fix in four ways:

- **Edge state can live in the existing fenced checkpoint.** Nothing records
  "this subject already fired while true". Delivery coverage and state members are
  the closest models.
- **No workspace-wide change feed exists.** Task events are appended to each task's
  own events file (`TaskEventRowV2`, `crates/orbit-types/src/task/artifacts.rs`).
  The store's task query filters by status and tag
  (`crates/orbit-store/src/contracts/task/query.rs`), but has no "changed since"
  filter.
- **Orbit runs no long-lived watcher.** The clock tick is a stateless one-shot
  started by launchd or systemd. Coordination-state automation runs only in owner
  checkouts, but a pull follower's clock still evaluates its replica-local cron
  worktree GC (`worktree_gc_pipeline`) and store GC (`store_gc_pipeline`); every
  other routine, auto-task and recovery stays owner-only (`runs_in_replica`,
  `crates/orbit-core/src/application/routines/loader.rs`; `collect_host_routines`
  consumed at `crates/orbit-core/src/application/routines/sweep.rs:217`).
- **Any path to tag or publish must keep release authority with Daniel.** RELEASING.md
  and the constellation rules forbid an agent from tagging or publishing unasked.
  npm publish needs Daniel's OTP.

## Goal

A routine or auto-task can fire on a typed, side-effect-free predicate over a
task, the workspace board, the owner host or a file under a declared root. It
fires once each time that predicate becomes true for a subject, and never twice for
the same edge, across crashes, restarts and concurrent evaluators. An operator can
see, at any time, what matches now and why each subject does or doesn't match.

The typed kinds, cron, intervals, delivery coverage and every existing definition
stay unchanged. A condition can't move a task or run a high-authority action
unless an operator grants it explicitly.

## Shape

The task's items map to sections as follows: items 1–5 are §1–§5, item 6 is §6
and §7, item 7 is §8, item 8 is §9, item 9 is §10, item 11 is §11, and item 10 is
Phasing and Open questions.

### Why the non-goal is reversed

The overview ruled out user expressions together with scripts and arbitrary
subscriptions. Its worries were Turing-complete code in the scheduler, a model
invoked on any status flip, and work no one can explain. This design reverses
the non-goal for **predicates only** and answers each worry:

| Worry | Answer here |
|---|---|
| Arbitrary code | A non-Turing-complete, side-effect-free language (CEL, §5), type-checked against an Orbit schema when the definition loads |
| A model invoked on every flip | Edge-triggered fires (§3), holds and re-arm hysteresis, fire caps, cooldowns and storm refusal (§8). Phase 1 actions are mints and catalog jobs only, never task transitions (§6). |
| Unexplainable work | `orbit automation explain`, which evaluates each clause. A dry run at enable time. Every fire and skip is recorded (§9). |
| Unbounded scans | Literal-only cross-reads, required status or tag prefilters, per-pass caps and bounded roots (§4) |

Every new mechanism is still an automation-triggers consumer. It uses the same
sweep, Store checkpoint, action-key admission and ownership rules. No second
clock is added.

### 1. Position: a `condition` kind beside the typed kinds

**Decision.** `condition` is a new trigger form that routines carry as
`trigger.condition` and auto-tasks carry as `schedule.condition`. It sits next to
`cron`, `every_minutes`, `deliveries_landed` and `state`, and replaces none of them.
The definition that carries it supplies the primary action: a job for a routine, a
mint for an auto-task.

**Why.** Typed kinds carry contracts that a predicate over current state can't
express:

- **`deliveries_landed` stays typed.** It owes coverage of exact commit ranges,
  batches frozen at a threshold, and receipts accepted only on validated evidence
  ([automation-triggers §3–5](../automation-triggers/2_design.md#3-delivery-identity-and-captured-coverage)).
  "Some deliveries landed" is easy to write as an expression. "Every landed commit
  was examined exactly once" is not.
- **`preparation_eligible` and `finding_unverified` stay typed.** They key members on
  a material fingerprint and settle each member through an accepted assessment or
  verdict. A boolean can't supply a fingerprint or a receipt.
- **`cron` and `every_minutes` stay typed.** They carry a slot identity and the
  `missed_run` policy. A `now`-based condition fires on an edge, not a slot, so it
  can't say "the 09:00 slot was missed".
- **`pilot_warnings_held` and `block_escalated` are preset candidates.** Each has a
  one-rule membership: the shared promotion predicate, or `triage_candidacy`. Each
  settles from a marker
  ([automation-triggers §6.1–6.2](../automation-triggers/2_design.md#61-pilot-warning-holds-pilot_warnings_held)).
  They could become shipped condition presets such as
  `task.status == 'blocked' && task.block.awaiting_triage`. Three things are needed
  first:
  1. those predicates exposed as computed, read-only task facts;
  2. a condition fire that can batch members into one job by crew (today
     `batch_size`, [ORB-12746]);
  3. marker-based settlement in place of action acknowledgement.

  Until all three exist, they stay typed. `decision_requested` stays typed too,
  because it is keyed by request, not by task state.

**Rejected alternatives.**

- **Replace the typed kinds with expressions.** This loses coverage, freshness and
  receipt semantics, which no predicate carries.
- **Grow a fixed filter struct per kind, as `PreparationEligibility` does.** Each
  new situation would need a new struct, Core code and a release. A, B, C and D
  would need four.
- **A separate `.orbit/conditions/` definition kind with an `actions[]` list.** It
  would add a third loader, CRUD tool set, dashboard toggle and authorization row
  set. Routines and auto-tasks already own their action surfaces.
- **An event bus or webhook subscriptions.** There is no durable state, no edge
  memory and no crash safety. These are still non-goals.

### 2. Subjects and what an expression reads

**Decision.** Each condition declares one `scope`, which fixes its subject set and the
variable bound to each subject:

| `scope` | Subjects | Bound variable | Checked |
|---|---|---|---|
| `task` | Tasks of the owning workspace that pass the prefilter (§4) | `task` | Per task; fires per matching task |
| `workspace` | The owning workspace, one subject | none (reads `board`) | Once per pass |
| `host` | The owner host, one subject | none (reads `host`) | Once per pass |
| `file` | Each regular file under the declared `roots` matching `glob`, or, with `group_by: stem`, each group of same-stem files in one directory | `file` (or `group`) | Per file or group; fires per new one |

Every expression may also read the following. Nothing else is in the schema.

| Read | Type and fields | Source |
|---|---|---|
| `task` (task scope) | `id`, `title`, `status`, `tags` (list), `type`, `priority`, `complexity`, `crew` (optional), `parent_id` (optional), `dependencies` (list of IDs), `created_at`, `updated_at`, `status_since` (time of the latest move into the current status) | Task envelope and history |
| `tasks['<ID>']` | Same shape as `task`, read-only. The index must be a string literal in the same workspace. | Prefetched by ID |
| `board` | `by_status` (map from status to count). `by_tag['<tag>']` (literal tags only). `eligible`, `backlog`, `in_progress`, `blocked`, `review` and `open_runs`, each with `count` and `empty`. `drain_running` (bool). | Task query; `application/admission/backlog_exclusion/snapshot.rs` for `eligible`; job store for open runs and the live `workspace_auto_pipeline` |
| `host` | `machine_id`. `cpu_percent`, `cpu_utilization_percent`, `memory_percent` and `io_full_percent` (optional doubles). `disk.repo`, `disk.worktrees` and `disk.global`, each with `used_percent`. | Host resource sampler |
| `system.path['<root>']` | `files`, `bytes`, `stable_files` (ints) and `newest_mtime` (timestamp), for a declared root only | Root rescan (§4) |
| `task.origin` (any task read) | `trigger` (`routine:<name>` or `auto-task:<name>`), `fire_id`, `depth`. Absent unless a condition fire created the task; test with `has(task.origin)`. | The minted task's `condition_fired` creation event (§11) |
| `file` (file scope) | `path`, `root`, `name`, `stem` (name without its final extension), `ext` (lower case, no dot), `size`, `mtime`, `stable` (bool), `fingerprint` | Root rescan |
| `group` (file scope with `group_by: stem`) | `dir`, `stem`, `files` (list of `file`, at most 16), `stable` | Root rescan |
| `now` | Timestamp of the pass, UTC. CEL's timezone accessors give local time. | Pass clock |

Rules:

- **Data an expression can't read.** Task descriptions, criteria, comments,
  artifacts and run output are not readable. Conditions are about state, not
  content matching. Free text is also how injected instructions travel.
- **Literal cross-reads.** `tasks[...]`, `by_tag[...]` and `system.path[...]` take a
  literal index, so the loader knows every cross-read before the first pass. This
  fixes prefetch cost, lets explain list them, and lets loop detection see what a
  definition watches (§8). A computed index is a load error.
- **Unavailable values.** Absent host values, a cross-read ID that no longer exists,
  and an undeclared path make the evaluation **unknown**, not false. An unknown
  evaluation neither fires nor re-arms, and it is reported (§9). This follows the
  sampler's rule that a missing value is unavailable, never zero.
- **Disk paths.** `host.disk` covers the three paths the sampler already measures,
  not every mount. To read another mount, the path must first be declared in host
  config. That is a phase 3 extension, not a guess about mounts.

**Cross-workspace reads: no.** A condition reads only its owning workspace:

- Another workspace may be owned by another host. Its local copy is a stale
  replica, and the shared constellation rules forbid acting on replica state.
- A fire's identity needs subject revisions under one owner (§3).
- Two owners can't fence one consumer's checkpoint.

A situation that spans workspaces is written as one condition per workspace, or
waits for federated reads (Open questions).

**Rejected alternatives.**

- **Expose the whole task record and free-text fields.** This makes cost unbounded
  and invites prompt-like matching on descriptions.
- **Dynamic `tasks[expr]` lookups.** The cost is unknown at load, and the
  definition's dependencies can't be listed.
- **Treat unknown as false.** A sampler outage would then re-arm every host
  condition, and the first good sample would fire it again.

### 3. Firing semantics

**Decision.** Fires are edge-triggered per subject. Each subject is in one of
three states:

```
armed --(when true, held for `for_minutes`)--> fired --(rearm true)--> armed
```

- **Fire.** An armed subject fires when `when` is true at this pass and has stayed
  true continuously for `for_minutes` (default 0).
- **Continuity.** "Continuously" means every evaluation since `true_since` was true,
  and no observation gap exceeded `max(3 × cadence, 5 minutes)`. A false or unknown
  evaluation, or a longer gap, resets `true_since`. A hold never counts time Orbit
  did not observe.
- **Re-arm.** A fired subject returns to armed only when `rearm` is true. The default
  `rearm` is `!(when)`. An explicit `rearm` gives hysteresis, for example: fire at
  90%, re-arm below 85%. An unknown result changes nothing.
- **Unseen subjects.** A subject that is no longer seen (a task archived, a file
  removed) drops its state, except for the tombstones below. When it reappears it
  starts armed.
- **Enable and epoch change.** These baseline by default. Subjects true at that
  moment are recorded as fired without an action. `on_enable: fire_existing` opts
  into firing them, subject to the match cap (§8). This follows the cold-start rule
  for deliveries
  ([automation-triggers §10](../automation-triggers/2_design.md#10-compatibility-and-migration)).
  Use case B needs it: enabling a release trigger must not release every old `done`
  task.

**Fire identity.** Each edge gets one durable identity:

```
consumer   = (owner machine, workspace id, definition kind, name)    # existing consumer identity
epoch      = digest(scope, when, rearm, for_minutes, roots, glob, action target or template)
revision   = task: the subject's history position (append-only length) at the edge
             file: the content fingerprint (use case D)
             group: the sorted member fingerprints
             workspace / host: the subject's arm sequence
fire_id    = sha256(consumer, epoch, subject key, revision, arm sequence)
action_key = "condition:<fire_id>:<n>"   # n = 0 for the primary action, 1 for notify
```

`arm sequence` counts the subject's re-arms and is stored in the fenced
checkpoint. For a task, the history position records exactly which state the edge
was observed at, and explain cites it. This **observation revision** is immutable:
later history writes never replace it in the fire ID, captured subject or action
key. Per-fire approval uses a separate subject freshness binding (§7); it never
compares the current raw history length with the observation revision.

**Protocol.** Each pass follows the existing three checkpoints
([automation-triggers §4–5](../automation-triggers/2_design.md#5-crash-safety-concurrency-and-minimal-state)):

1. Load the consumer state at generation `g` and evaluate the subjects.
2. Commit `g+1`. Each new edge moves to `fired`, carrying its `fire_id` and action
   keys as **intents**, with the captured subject and observation revision. A
   per-fire approval also freezes its freshness binding from that same snapshot
   (§7), before any fire or decision bookkeeping. A losing evaluator gets
   `ConcurrentEvaluation` and writes nothing.
3. For each intent, look up the key: `automation_job_for_key` for a job, or the keyed
   task reservation for a mint. If an action exists, acknowledge it. Otherwise
   admit the action under that key once its grant and any per-fire decision pass
   §7. An unanswered request keeps the intent pending without admitting a job.
4. Commit the acknowledgement (task or run ID) on the fire record.

| Failure window | Behavior |
|---|---|
| Crash before step 2 | Nothing recorded. The next pass re-evaluates; still true means the same edge. |
| Intent saved, action not created | The next pass reuses the same key and any frozen approval binding; admission still requires §7. |
| Action created, acknowledgement lost | The key resolves to the original task or run. No second mint. |
| Two evaluators | Only one step-2 commit wins the compare-and-swap. Key admission fences any stale actor that already holds an intent. |
| Subject changed between evaluation and action | An action without per-fire approval still runs for the recorded edge, receiving the captured subject and observation revision. With per-fire approval, a change that invalidates the freshness binding supersedes the request and prevents admission (§7); the captured input is never silently replaced. |

Condition auto-tasks mint through the keyed task path, not the slot cursor
(`fire_slot`). That closes the cursor's gap of a mint whose task ID was never
recorded, for this kind.

Fired-subject tombstones keep each fire for `fired_retention_days` (default 30)
or up to the 1,000-record cap (`MEMBER_CAPACITY`, `members.rs`). This stops a file
moved out and back in, or a task archived and restored, from firing again
within the window. Missing or corrupt condition state fails closed as a
diagnostic, never as a fresh baseline
([automation-triggers §5](../automation-triggers/2_design.md#5-crash-safety-concurrency-and-minimal-state)).

**Rejected alternatives.**

- **Level-triggered fires (on every pass while true).** Dedupe could only suppress
  these, and an interval auto-task already gives that behavior.
- **Fire identity from wall-clock time.** Two evaluators and a retry would disagree.
- **No hold, hysteresis only.** Hysteresis stops repeat fires but not the first fire on
  a 2-minute quiet gap between drains (use case A).
- **Event subscriptions ("on status = done").** These miss changes made while Orbit
  was down, and they double-fire on replay. State plus edge memory gives the same
  result and survives both.

### 4. Evaluation

**Decision.** Conditions are evaluated in the existing clock tick, next to routines and
auto-tasks. They share that tick's `SourceCache`
(`crates/orbit-core/src/application/automation/source_cache.rs`) and its deadline.
Change signals are hints that make a pass sooner or cheaper. They are never needed
for a correct result.

- **Phase 2, clock only.** Each tick evaluates each enabled condition:
  - workspace and host scopes: once;
  - task scope: over its prefiltered tasks, in bounded laps;
  - file scope: over a rescan of its roots.

  Latency is one cadence, 60 seconds by default.
- **Phase 6, task change hints.** The task commit path records the IDs of changed
  tasks as dirty-ID hints in the owner store. A task-scope pass evaluates hinted tasks
  first and skips a tick when nothing changed. The design already allows this
  ("Optional dirty-ID hints accelerate this path; their loss must not affect
  correctness", automation-triggers §2).
- **Phase 6, file notifications.** In a long-lived owner process (`orbit web serve`
  or `orbit mcp serve`), inotify or FSEvents through the `notify` crate marks a root
  dirty and evaluates that root's file conditions early, under the same fence. The
  periodic rescan stays the source of truth. Files added while Orbit was down, or
  whose notifications were dropped, are found on the next rescan.

**Cost bounds.**

- **Task prefilter.** A `scope: task` `when` must have a top-level conjunct
  that constrains `task.status` (`==` or `in`) or requires a literal tag
  (`'x' in task.tags`). The loader extracts these clauses into a store query with
  `statuses` and `tags`. It never lists the whole board. A condition without such a
  clause is a load error.
- **Per-pass limits.** At most 200 subjects are evaluated per condition per pass,
  in ID-ordered laps with a persisted continuation (the 200 per workspace and pass
  proposed in automation-triggers §2). A lap that can't finish says so in explain.
  Aggregates, host samples and cross-reads are computed once per pass per workspace.
- **Expression limits.** Each expression is capped at 4 KiB of source and 200 AST
  nodes, with no more than 2 nested comprehensions. Each evaluation gets a
  per-evaluation cost limit. Comprehensions (`all`, `exists`, `map`, `filter`) can
  only range over bounded lists (`task.tags`, `task.dependencies`).
- **File roots.** Each root has `max_depth` (default 2, max 8) and `max_files`
  (default 10,000). Symlinks are not followed. Hidden files and partial-download
  patterns (`*.part`, `*.crdownload`, `.~*`) are skipped by default. A root over
  `max_files` stalls its condition with `root_too_large`. It never truncates
  silently, because a truncated scan would invent removals and new files.
- **Hashing.** A fingerprint is computed only for a stable file that is about to
  fire and whose stat key (device, inode, size, mtime) matches no tombstone. It is
  cached by that stat key, so a rescan never re-reads an unchanged file. A file over
  `hash_full_max_bytes` is fingerprinted from samples (use case D), so a 5 GB
  recording costs about 12 MiB of reads, not 5 GB.
- **Retained state.** Each condition retains at most 1,000 subject records.
  Saturation stops the lap and reports backpressure, as members do.

**Ownership.** Coordination-state automation is owner-only; replica-local cron GC
is the shipped exception:

- Workspace, task and file conditions are evaluated only by the workspace's
  registered owner host, in its owner checkout. This is a design choice for the
  new condition kind, not the shipped clock rule: the routine loader collects
  both owner entries and replica checkouts and keeps only the replica-local cron
  routines targeting worktree GC or store GC (`collect_host_routines` and
  `runs_in_replica`, `crates/orbit-core/src/application/routines/loader.rs`,
  consumed at `crates/orbit-core/src/application/routines/sweep.rs:217`).
  Delivery and state triggers stay owner-only even for those jobs.
- Every coordination-state action write passes `ensure_coordination_task_write_permitted`
  (`crates/orbit-runtime/src/orbit_runtime/identity.rs`), so a replica checkout
  refuses to fire one even if it loads the file.
- A pull follower evaluates only its replica-local cron worktree GC and store GC.
  Its drain otherwise inherits whatever the owner minted.
- In phase 3, `scope: host` reads the owner host. Follower-host conditions need
  host-level definitions (Open questions); that restriction is for the proposed
  condition kind, alongside the existing replica-local cron GC scheduling.

**While the owner is down.** Nothing is evaluated and nothing fires. When the owner
returns, the next pass compares current state with the stored arm states:

- An armed subject that is now true fires. This is a real edge: false at the
  last observation, true now.
- A subject that went true and back to false entirely while the owner was down is
  missed.
- Holds restart, because the gap exceeds the continuity limit.
- File rescans find files that arrived during the outage.

**Rejected alternatives.**

- **A resident daemon as the evaluator.** Orbit has no daemon. Requiring one would make
  correctness depend on a process that launchd or systemd doesn't run.
- **Full-board scans.** Their cost grows with every `done` task, forever.
- **Notifications as the source of truth.** Notifications can be dropped, and the
  watcher's queue can overflow while Orbit is down.

### 5. Language: CEL

**Decision.** Expressions are written in the
[Common Expression Language](https://github.com/google/cel-spec) (CEL). CEL is
side-effect-free and not Turing complete. Its only iteration is over finite
collections, through macros. It has a gradual type system designed for checking
against a declared environment, and it already gates admission in Kubernetes,
Envoy and Firebase rules. The Rust implementation is the cel-rust project,
published as the `cel` crate (`cel-interpreter` in older releases). Neither crate
is an Orbit dependency today, so adding one follows the ARCHITECTURE.md dependency
rule.

Orbit declares one CEL environment per scope: the variables and field types in
§2, with no user-defined functions and no `dyn`. The admitted subset is literals,
comparison and logic operators, `in`, `has()`, `size()`, string functions
(`startsWith`, `endsWith`, `contains`, `lowerAscii`), timestamp and duration
arithmetic, and the bounded comprehension macros. `matches()` (RE2) is
not admitted in v1.

**Type checking at load.** The routine and auto-task loaders already fail closed per
file (`loader.rs`). A condition adds these checks: the expression parses, every
identifier and field exists in its scope's environment, every operator is well
typed, the whole expression is `bool`, cross-read indexes are literals, the
prefilter clause exists (§4), and the size limits hold. Any failure is a load error
that names the file, the clause and its position. The definition then counts as
absent and never fires with defaults.

The Rust implementations evaluate CEL, but they lack cel-go's complete checker.
Phase 1 confirms what the chosen crate provides. If it falls short, Orbit adds a
checker over the parsed AST for the admitted subset. That is feasible because the
subset has no `dyn`, overloads or user functions.

| Candidate | Side effects | Turing complete | Load-time types | Rust | Verdict |
|---|---|---|---|---|---|
| **CEL** | None | No | Yes (declared environment) | `cel` crate | Chosen |
| Rhai | Host-registered only | Yes (loops, functions) | No (dynamic) | Native | Rejected: a scripting language, so no halting bound |
| Lua (`mlua`) | Sandboxable | Yes | No | Binding | Rejected: same, plus a C runtime |
| Starlark (`starlark-rust`) | None | No recursion, bounded loops | Optional annotations | Native | Rejected: a configuration language with statements and functions, more than a predicate needs |
| Rego (`regorus`) | None | No | Partial | Native | Rejected: Datalog-style policy semantics, a poor fit for one boolean over one subject |
| JSONLogic | None | No | No | Crates exist | Rejected: no schema checking, verbose JSON |
| JMESPath | None | No | No | Crate exists | Rejected: a query language, weak typing, no timestamps |
| Typed YAML predicate tree | None | No | Yes | Native | Rejected: grows into a worse CEL, with a new structure per operator |

Daniel's sketch reads as CEL with two spellings changed: `board.eligible.isEmpty`
becomes `board.eligible.empty`, and `system.path["<dir>"].length` becomes
`system.path["<dir>"].files`. The schema offers `count` and `empty`, so the
common checks need no `size()`.

### 6. Actions

**Decision.** Phase 2 actions are the carrier's own:

- **Auto-task mint.** The template, with the subject passed in.
- **Routine job run.** A catalog `job:<name>`, with the subject as input.
- **`notify`** (phase 3), an optional second action on either carrier (§C).

Running a routine from an auto-task, or the reverse, isn't needed: a routine
already runs a job.

- **Passing the subject.** A job receives a `subject` input object: the scope, the
  subject key, the revision at the edge, and the bound variable's fields (for
  example `subject.file.path`). An auto-task template may use `{{subject.<field>}}`
  placeholders in its title and description. Values are substituted as data,
  capped in length, with paths rendered as code. The minted task records a
  `condition_fired` history event that carries the fire ID and subject. A manual
  `orbit auto-task mint` of a condition auto-task stays unconditional and
  cursor-neutral, as it is today. It has no subject, so `{{subject.*}}` renders as
  `(none)`.
- **Multiple actions.** A condition carries one primary action and at most one
  `notify`. Each has its own action key (`:0`, `:1`). Each is admitted and recorded
  independently, so a failed notification never re-mints. Two non-notify actions on
  one edge are two definitions with the same condition, each with its own edge state.
  A shared, named condition is an open question.
- **Task transitions are deferred.** No phase lets a condition move a task (for
  example `blocked` to `backlog`, or `proposed` to approved). The reasons:
  - **Authority.** Re-backlogging a blocked task is "a deliberate human transition"
    (`RETIRED_ROUTINE_JOBS`, `crates/orbit-automation/src/routines/loader.rs`).
    Approval belongs to pilot admission and the operator. A transition would
    let a definition carry authority that the agent who wrote it doesn't hold.
  - **Loops.** A transition changes the very state conditions read. A condition
    whose action makes its own subject false and then true again oscillates.
    A mint or job changes the subject only through a separate run, which the guards
    in §8 can see and bound.

  A later transition action would need an operator grant (§7) and would count as
  watched-state mutation for loop detection.

### 7. Authority for high-authority actions

**Decision.** An action's authority comes from its target, never from the
condition. A job definition can declare `authority: operator` and, independently,
`approval: per_fire`. A definition can't lower either. Release, tag and publish
jobs declare both. A condition whose target needs operator authority requires
two things:

1. **An operator grant on the definition.** `orbit automation grant <definition>`
   records in the owner Store:
   - the operator;
   - a **digest** over the definition body, the target job and its activities;
   - an expiry (default 30 days, at most 90);
   - an optional maximum number of fires.

   The grant tool's catalog row has no Agent capability, so
   `classify_agent_tool_grant` makes it always operator-only (`grants.rs`). Without a
   current grant, the condition still evaluates and explains, but each edge is
   recorded as `skipped: grant_missing` or `grant_stale` and nothing runs.
2. **A per-fire decision, when the target declares `approval: per_fire`.** The fire
   raises a decision request on the subject task with a new gate,
   `trigger_action`:
   - **Answers:** `approve` or `dismiss`.
   - **Binding:** `fire_id`, the grant identity and digest, and
     `subject_freshness` frozen at the edge as defined below. The fire ID retains
     the immutable observation revision (§3).
   - **Superseded when:** the definition, target or grant changes, the grant is
     revoked, expires or is replaced (even by one with the same digest), or
     `subject_freshness` advances. Supersession is terminal; restoring earlier
     content or status never revives the request.

   **Subject freshness.** For a fire `F`, `subject_freshness(F, history)` is the
   position of the latest task-history entry other than trusted bookkeeping for
   `F` and its `trigger_action` request. Positions are append-only, so this value
   can only advance. Freeze it with the observed task at step 2 of §3, and compare
   that frozen value with the current value when raising the request and when
   applying an answer. Do not capture a newer binding when raising or retrying:
   a real change between the edge and the request must also supersede it.

   The exemption is narrow: the owner's `condition_fired` entry for `F`, the
   `decision_requested` entry for its request, and that request's channel-delivery
   and answer bookkeeping. Exempt entries must be typed, owner-validated and
   correlated to the stored fire/request IDs, and must change only those records
   and bookkeeping-derived metadata such as `updated_at`. Event names or a
   caller-supplied fire ID alone cannot claim the exemption. A write that also
   changes task content is never exempt. All other subject writes advance
   freshness, including status, tags, description, criteria, candidate or release
   input changes, even if `when` stays true. An unknown entry, incomplete history,
   or unverifiable bookkeeping provenance fails closed. Comparing `updated_at` or
   raw history length would reintroduce self-invalidation; comparing only current content
   would wrongly allow an edit followed by a revert to revive approval.

   Only a human answers, through `orbit.decision.submit`
   ([decision channel §3](../human-decision-channel/2_design.md#3-delegated-human-authority)).
   On `approve`, the apply step checks that the request is open, the grant is
   current, the definition and target still match, and subject freshness is
   unchanged. That check and admission must be fenced against concurrent subject,
   definition, target or grant writes: a change before admission refuses the
   answer rather than running from a stale check. It admits the job under the
   fire's action key with the captured input and records the approving human as
   the run's authority. Its answer history write is exempt bookkeeping, so it
   cannot invalidate its own admission. An already admitted key resolves to the
   same run on retry. On `dismiss` the fire closes, and the subject stays fired
   until it re-arms. A superseded request admits nothing and also leaves the
   subject fired until re-arm; it never silently refreshes the old fire's input or
   asks again against a newer subject. A condition whose scope has no subject task
   (workspace, host, file) can't use `approval: per_fire` in v1. Its
   operator-authority targets need the grant alone.

**Protection from agent edits.** An agent with `orbit.auto_task.update`, or with
write access to a definition file, can still edit a definition. Editing changes
the digest, which voids the grant (`grant_stale`). The change is visible in
explain, on the dashboard and in `routine list`, and nothing runs until an operator
grants again. Editing the target job voids the grant the same way. An agent can't
answer the decision either. So a definition an agent changed can never run a
release, and disabling one only removes authority. Dashboard toggles for these
definitions keep their operator-only catalog rows.

**Why both.** The grant says "this automation may ask". The per-fire approval says
"this time, yes". A grant alone would make a release unattended, which RELEASING.md
and the constellation rules forbid. Per-fire approval alone would let an
agent-written definition raise release questions under Daniel's name and train
reflexive approval.

**Rejected alternatives.**

- **A trust flag in the YAML (`authority: operator`).** Anyone who can edit the file
  could set it.
- **A grant without a digest.** A later edit would inherit the authority.
- **Approval through an ordinary task comment.** A comment isn't a human-only,
  binding-checked answer.

### 8. Guardrails

| Guard | Default | Behavior |
|---|---|---|
| `cooldown_minutes` | 0 | Minimum time between fires of one subject. An edge inside the cooldown stays armed and pending, and fires when the cooldown ends if `when` is still true. |
| `max_fires_per_hour` and `max_fires_per_day` | 10 and 50 | Per definition. Edges over the budget wait as `deferred: fire_cap`, oldest first, and are re-checked when budget frees. Waiting edges are never dropped or burst. |
| `max_matches` | 20 (at most 1,000) | At enable, an epoch change or `fire_existing`, more current matches than this refuses the change with `too_many_matches` and lists a sample. During operation, more than this many **new** edges in one pass stalls the condition with `match_storm`: nothing fires until an operator runs `orbit automation clear-stall`. |
| Self-exclusion | on | A `task` condition never matches a task minted by its own definition. Minted tasks carry `auto-task:<name>` and the `condition_fired` provenance. |
| Chain depth | 5 | A fire whose subject was created or last changed by another fire's action has depth = parent depth + 1, read from `task.origin.depth` (§11). Past the limit, the fire is refused and recorded with `loop_suspected`, and the definition stalls. This catches A → B → A loops through tasks. |
| Oscillation | 3 fires per subject per hour | Further edges on that subject are refused with `oscillating` until an operator clears the stall. |
| Self-match at load | — | The loader evaluates a `task` condition against its own auto-task template, as a task in the template's status with its tags. A definition that would match its own output is a load error. |

The chain-depth and self-exclusion checks rely on provenance that Orbit already
writes: the `auto-task:<name>` tag (`template_params`,
`crates/orbit-core/src/application/auto_tasks/scheduler.rs`), plus the new
`condition_fired` event.

**Rejected alternative.** Static loop proof across all definitions. Actions run
agents whose effects can't be predicted. Runtime depth and oscillation bounds are
honest; a static proof would not be.

### 9. Explainability and operations

- **`orbit automation explain <definition> [--subject <key>] [--json]`**, and the
  read-only MCP tool `orbit.automation.explain`, available to agents. The output
  includes:
  - the scope, epoch, ownership verdict, grant state and last pass;
  - the current matches (up to 20), each with its arm state, `true_since` and hold
    time remaining;
  - the closest misses (up to 20), meaning subjects that fail exactly one top-level
    clause;
  - for each recent fire, its upstream chain: the subject's `origin` fire, that
    fire's subject and origin, and so on, back to a fire with no origin (§11);
  - for any one subject, a tree of the top-level `&&` and `||` clauses with each
    clause's value and the operand values it read:

    ```
    board.eligible.empty               true   (board.eligible.count = 0)
    && board.in_progress.count == 0    false  (board.in_progress.count = 2)
    → false; armed; hold 30m not started
    ```

  Explain evaluates without committing anything.
- **Dry run at enable.** Enabling a condition definition through `orbit auto-task
  toggle` or the dashboard toggle runs explain first and prints the would-fire set.
  It refuses enable on `too_many_matches`, and `--dry-run` stops there. A routine is
  enabled by editing `enabled:` in its file, so no command can refuse it. Instead,
  its first evaluation after enable runs the same check, and it stalls with
  `too_many_matches` without firing. (`orbit routine resume` only clears a
  host-local pause, so it isn't an enable path.)
- **`orbit automation grant | revoke | clear-stall <definition>`** are operator-only.
- **Where fires are recorded.**
  - Every fire record lives in the owner Store, with its intent, its
    acknowledgement, its captured subject and immutable observation revision.
    A per-fire approval also records the frozen freshness binding and the linked
    decision-request ID (§7); retries reuse them.
  - Each minted task carries a `condition_fired` history event.
  - A task-scope subject gets a `condition_fired` event naming the fire, so task
    history shows why automation acted. This event and the fire's own decision
    channel bookkeeping still append to history (and may update `updated_at`),
    while leaving its subject freshness binding unchanged under §7. They never
    rewrite the observation revision or omit the audit history to keep approval
    current.
  - `orbit routine list` and `orbit auto-task list` show the last fire and the last
    skip with its reason.
  - The dashboard Automation view lists condition definitions with matches, arm
    states, grants and recent fires.
- **Where skips are recorded.** Skips keep stable reason codes, as the existing
  diagnostics do: `unknown`, `holding`, `cooldown`, `fire_cap`, `grant_missing`,
  `grant_stale`, `loop_suspected`, `oscillating`, `match_storm`, `root_too_large`
  and `owned_elsewhere`. Skips are counted, not written to task history, so a noisy
  condition can't flood a task.

### 10. Compatibility

- **Persisted definition shape.** `trigger.condition: {…}` on routines and
  `schedule.condition: {…}` on auto-tasks are new optional forms. So are the
  `when:` and `guard_miss:` guard keys beside the typed kinds (§11). No existing
  byte changes.
- **Older binaries refuse.** `RoutineTrigger` and `RawSchedule` refuse unknown fields.
  An older binary that meets a condition definition, or a guarded typed one,
  reports a per-file load error;
  the routine or auto-task counts as absent and never fires with defaults. Every
  other definition keeps loading. Refusing is chosen over ignoring: an ignored
  condition would look like a disabled definition, which would hide that it isn't
  running.
- **Condition state.** Arm states, fire records, grants and tombstones are kept in
  their own namespaced rows in the owner's SQLite automation store, through a
  forward-only migration, not in `AutomationState`. That struct doesn't refuse
  unknown fields, so an older binary that rewrote it would silently drop condition
  state. The migration declares its compatibility as RELEASING.md requires, and
  deploys stay in lockstep (polaris runbook RB-15, Orbit binary update across
  hosts).
- **Rollback.** An older binary refuses condition definitions and leaves condition
  rows alone. Upgrading again resumes from the stored arm states, and the first
  pass applies the owner-down rule (§4).
- **The 30-day window.** Any compatibility path this feature adds (a renamed key, a
  phase 1 field spelling, an older fire-record shape) follows the
  [30-day, release-gated rule](../state-compatibility/4_decisions.md#compatibility-paths-expire-on-a-30-day-release-gated-window)
  [ORB-15732]. It is removed only when it is 30 days old, the owner runs a release
  that contains it, and no consumer remains.
- **Followers.** Followers evaluate only their replica-local cron worktree GC and
  store GC (`runs_in_replica`), so an older follower binary affects only that
  replica-local GC scheduling.
  The decision-request gate `trigger_action` is new: an older dashboard or plugin
  that meets it must show it as unknown and refuse to answer it.

### 11. Combining and chaining

**Combining: an optional `when:` guard on any kind.** Decision: `cron`,
`every_minutes`, `deliveries_landed` and the typed `state` kinds take an optional
`when:` string written in the condition language. It is checked against the
workspace environment (`board`, `host`, `tasks[...]`, `system.path[...]`, `now`),
with no bound subject. The kind decides when to look, and the guard decides whether
to fire:

```yaml
schedule:
  cron: "0 2 * * *"
  when: "board.eligible.empty"
```

What a false or unknown guard does depends on the kind:

- **`cron` and `every_minutes`.** A due slot whose guard is false stays unconsumed
  and fires on the first later pass where the guard is true. The next slot collapses
  it, as catch-up already does. This is how the shipped preconditions
  `skip_if_unchanged` and `skip_unless_blocked_candidates` already treat a skip
  ("the slot is deliberately left unconsumed", `fire_locked`,
  `crates/orbit-automation/src/auto_tasks/scheduler.rs`). So the example reads "once
  per night, at the first moment after 02:00 that the board is empty".
  `guard_miss: skip` consumes the slot instead, giving "only if empty at 02:00".
- **`deliveries_landed`.** A false guard dispatches nothing. Pending deliveries and
  their coverage debt are kept, and `max_wait_minutes` doesn't override the guard.
- **`state` kinds.** A false guard withholds due members with reason `guard_false`.
  Withheld members are retained.
- **Unknown.** An unknown guard fails closed: no fire, and the reason is
  `guard_unknown`. The shipped preconditions fail open when inconclusive,
  because they only save work. A guard states a requirement, so failing open would
  break it.

The guard is level-checked at each due moment. It has no edge state, because the
kind already supplies identity: a slot, a batch or a member. A `condition` doesn't
take a guard, since its `when` is the expression, and file arrival is a scope of
`condition` (§2), not an event kind. "A file arrived while the board is empty" is
therefore one expression: `file.stable && board.eligible.empty`.

The guard is the only place triggers combine. The two shipped typed preconditions
stay as they are, for compatibility. Once their facts are exposed, they could be
written as guards.

**Chaining: through durable state.** Decision: one trigger follows another only
through the records the first one's action leaves.

- **Origin stamp.** Every fire stamps what it creates with its source. A minted task
  gets a `condition_fired` creation event carrying `trigger`, `fire_id` and `depth`,
  readable as `task.origin`. A job run's trigger record carries the fire ID.
  The origin lives on the history event, not the task envelope, so the envelope's
  persisted shape doesn't change.
- **Sequencing A → B.** A's action mints a task. B's condition watches it, and can
  require `has(task.origin) && task.origin.trigger == 'auto-task:release-prep'` when
  a tag alone is too loose (use case B). The `has()` guard keeps a task with no
  origin false rather than unknown.
- **Depth.** `depth` is the origin fire's depth plus one, and the chain-depth guard in
  §8 refuses fires past 5. Explain shows each fire's upstream chain (§9).
- **Ordered work.** Multi-step work with retries and stops belongs in a job. One
  trigger starts it, and the job's steps carry the order. Triggers are not a
  workflow engine.

**Rejected alternatives.**

- **A nested `all:` / `any:` tree of trigger blocks.** Each block brings its own
  timing model (slots, batches, edges), so the tree's meaning would be the hardest
  part to read. Explain would have to render nested timing as well as clause values.
  The type checker would have to reconcile several environments. One guard
  expression over one environment avoids all three problems.
- **Direct links ("when A fires, run B").** These hide state: nothing durable says B
  is owed. A human can't stop the chain between A and B by acting on a record. A
  link held in memory doesn't survive a restart. The repository rule says
  activities never rely on agent output, only on the durable artifacts it leaves
  (AGENTS.md, "Release and Orbit design rules"). Chaining through state follows that
  rule.
- **Time-correlation rules ("B fires within 1 hour of A").** They need a retained
  event window and its own identity rules. A condition over state already covers
  the real cases. For example,
  `has(task.origin) && task.origin.trigger == 'auto-task:release-prep' && now - task.status_since < duration('1h')`
  needs no event history.

### Configuration

A condition block, shared by both carriers:

```yaml
trigger:            # routine; an auto-task uses `schedule:` instead
  condition:
    scope: task                 # task | workspace | host | file
    when: "<CEL bool>"
    rearm: "<CEL bool>"         # optional; default !(when)
    for_minutes: 0              # hold before firing
    cooldown_minutes: 0
    max_fires_per_hour: 10
    max_fires_per_day: 50
    max_matches: 20
    on_enable: baseline         # baseline | fire_existing
    roots: ["inbox/"]           # file scope only (relative = workspace checkout;
                                #   plugin-seeded: {{config.<key>}} etc.)
    glob: "**/*"                # file scope only
    max_depth: 2                # file scope only
    settle_seconds: 120         # file scope: `file.stable` window
    identity: auto              # file scope: auto | full (fingerprint mode)
    hash_full_max_bytes: 268435456   # file scope: above this, auto samples
    group_by: none              # file scope: none | stem
    group_settle_seconds: 300   # file scope with group_by: stem
    notify:                     # optional second action (phase 3)
      message: "<text with {{subject.*}} placeholders>"
```

| Key | Default | Meaning |
|---|---|---|
| `automation.watch_roots` (host config) | `[]` | Absolute roots outside any workspace that file conditions on this host may watch. Only an operator edits host config. A plugin-seeded definition may also watch inside its plugin's granted `permissions.fs.read` (use case D). |
| `automation.condition_fired_retention_days` (workspace config) | `30` | How long fired-subject tombstones are kept, within the 1,000-record cap |
| `automation.condition_pass_subjects` (workspace config) | `200` | Subjects evaluated per condition per pass |

File roots inside the owner checkout are allowed, except `.git/` and `.orbit/`,
which are refused because watching them causes loops and exposes secrets. A root
outside the checkout must be listed in `automation.watch_roots`, so a workspace
definition can't make the host read an arbitrary directory. The evaluator, running
in the clock process, only stats and hashes files. The trigger grants no sandbox
access. A job that reads the file needs a policy that already allows that root. The
enable dry run reports when it can tell that the target's sandbox can't read the
root, and refuses to enable.

## Use cases, walked through

### A. Board empty → release prep

```yaml
# .orbit/auto_tasks/release-prep.yaml
name: release-prep
schedule:
  condition:
    scope: workspace
    when: "board.eligible.empty && board.in_progress.count == 0 && board.open_runs.count == 0"
    for_minutes: 30
    cooldown_minutes: 1440
template:
  title: "Prepare release"
  tags: [release]
  status: backlog
dedupe: skip_if_open
```

- **Aggregates.** `board.eligible` uses the same admission view the drain uses
  (`backlog_exclusion/snapshot.rs`), so a backlog of only scheduled ([ORB-19214]) or
  held tasks reads as empty. Use `board.backlog.empty` if held work should also
  count as "not drained".
- **The hold.** The hold is needed. Between two drains the board can be empty for a
  minute or two. `for_minutes: 30` requires 30 observed minutes of an empty board
  with no gap over 5 minutes. The default re-arm (`!when`) re-arms as soon as work
  appears.
- **Its own task.** The minted `Prepare release` task makes the board non-empty, so
  the subject re-arms. When that task is done, the board may be empty again.
  `skip_if_open` doesn't help once the task is closed; the 1-day cooldown is what
  stops a second release prep that day. Oscillation (§8) bounds anything worse.
- **What's missing.** Nothing beyond phases 1 and 2.

### B. Release prep done → release and tag

```yaml
# .orbit/routines/release-on-prep-done.yaml
schemaVersion: 1
name: release-on-prep-done
enabled: false
trigger:
  condition:
    scope: task
    when: "'release' in task.tags && task.status == 'done' && has(task.origin) && task.origin.trigger == 'auto-task:release-prep'"
    max_matches: 1
target: job:release_pipeline     # workspace-defined; declares authority: operator, approval: per_fire
```

1. The prefilter is status `done` plus tag `release`. The `origin` clause chains B to
   A through A's minted task (§11), so a hand-made task tagged `release` doesn't
   qualify. Enabling baselines the old `done` release tasks, and the dry run shows
   zero would-fire.
2. Daniel runs `orbit automation grant release-on-prep-done`, which records the
   digest of the definition, `release_pipeline` and its activities.
3. A's task reaches `done` at history position 40. The edge freezes observation
   revision `40`, the captured task fields, and `subject_freshness = 40`.
   Its fire ID is `F = sha256(consumer, epoch, task key, 40, arm sequence)`.
   Because the target declares `approval: per_fire`, the fire raises request `R`
   on the task: "Release from this preparation? approve / dismiss". Mail delivers
   it once the decision-mail transport ships. `R` binds
   `(F, grant identity, grant digest, 40)`.
4. The owner's `condition_fired(F)` is entry 41 and `decision_requested(R)` is
   entry 42. Raw history length is now 42 and `updated_at` may have advanced, but
   both entries are exempt under §7: current subject freshness is still 40, so
   `R` remains open. Reversing those two writes, or retrying their bookkeeping,
   gives the same result; neither recaptures revision 42 or changes `F`.
5. With no other change, Daniel approves `R`. The fenced apply sees freshness 40
   and the same current grant, definition and target. The answer's own history
   entry 43 is exempt too. It admits `release_pipeline` with the captured subject
   under `condition:F:0`, attributed to Daniel. A retry or a second evaluator
   resolves the same key.
6. Alternatively, before answering, Daniel reopens the preparation task into
   `review` at entry 43. Subject freshness advances to 43; `R` is superseded and
   an approval attempt admits no job. Returning it to `done` at entry 44 advances
   freshness again, so `R` stays superseded. Its observation revision is still 40
   and `F` is unchanged; the subject must re-arm and reach a new edge before a new
   fire can ask again. Any other subject edit, such as a description change while
   the task remains `done` and `when` stays true, likewise advances freshness.
7. An agent that edits the routine or the job makes the grant stale and supersedes
   the pending request, even without a task-history write. Revoking, expiring or
   replacing the grant also supersedes it. Daniel must grant again for a future
   fire; no such change refreshes `R`.

What is missing, and why B is phase 5:

- **No release job.** Orbit ships no `release_pipeline`. Version bumps, tags, the
  promotion to `main` and the release workflow are workspace steps from
  RELEASING.md, and they belong in a workspace-defined job, not Orbit core.
- **Publishing stays manual.** The npm publish needs Daniel's OTP and is done by hand
  (RELEASING.md). At most, the job can prepare everything and stop before that step.
- **New mechanisms.** The `authority` and `approval` job fields, the grant record and
  the `trigger_action` gate are all new.

### C. Disk at 90% → notify and GC

```yaml
# .orbit/routines/disk-pressure-gc.yaml
schemaVersion: 1
name: disk-pressure-gc
enabled: false
trigger:
  condition:
    scope: host
    when: "host.disk.worktrees.used_percent >= 90.0"
    rearm: "host.disk.worktrees.used_percent < 85.0"
    for_minutes: 5
    notify:
      message: "{{subject.host.machine_id}}: worktree disk at {{subject.host.disk.worktrees.used_percent}}%; running worktree GC"
target: job:worktree_gc_pipeline
policy: {timeout_minutes: 30, overlap: forbid}
```

- **Facts.** These come from the sampler that admission already uses. An unavailable
  disk reading is unknown: no fire and no re-arm.
- **Hysteresis.** The pair 90 and 85 mirrors `disk_high_percent` and
  `disk_resume_percent` in the throttle settings. If GC doesn't bring the disk below
  85%, the condition stays fired. It doesn't repeat, and the notification says GC ran.
- **Two actions.** The notification (`:1`) and the GC job (`:0`) are admitted and
  recorded independently.
- **Broader GC.** For a wider `orbit gc`, add a second routine with the same
  condition targeting `job:store_gc_pipeline`. Both jobs ship today.

What is missing:

- **Notice delivery.** `notify` needs a no-answer *notice* record in the decision
  channel, and a transport to deliver it. The `orbit-decision-mail` transport is a stub
  today ([human decision channel](../human-decision-channel/1_scope.md)). Until it
  ships, notices appear in the dashboard Automation view, in `orbit automation explain`
  and in the clock log.
- **Other mounts.** Reading a mount other than the three sampled paths needs a declared
  extra path.
- **Follower hosts.** Follower hosts already schedule their own replica-local cron
  worktree GC and store GC today (`runs_in_replica`,
  `crates/orbit-core/src/application/routines/loader.rs`), which is where
  dk-server-1's worktrees fill. The proposed `scope: host` condition still needs
  host-level definitions (Open questions), so in phase 3 this use case covers owner
  hosts only; that is a design choice for conditions, not a limit of the shipped
  cron GC.

### D. File added → job

```yaml
# .orbit/routines/ingest-media.yaml
schemaVersion: 1
name: ingest-media
enabled: false
trigger:
  condition:
    scope: file
    roots: ["/srv/inbox/media"]      # outside the checkout: must be in automation.watch_roots
    glob: "**/*"
    max_depth: 2
    settle_seconds: 120
    when: "file.ext in ['pdf', 'mp4', 'mov'] && file.stable && file.size > 0"
target: job:media_ingest_pipeline    # workspace-defined
```

- **Aggregate or per-file.** Daniel's `system.path["<dir>"].files == 1` is valid in a
  workspace or host condition, and it's useful for "the inbox has a backlog". It is
  lossy for "a file was added": two arrivals in one pass read as 2, and an add plus
  a remove read as no change. `scope: file` makes each file a subject, so
  edge-triggering gives exactly one fire per new file.
- **Facts.** The evaluator keeps a per-root manifest of (path, size, mtime,
  first seen). `file.stable` is true once size and mtime have stayed the same across
  observations at least `settle_seconds` apart. With a 60-second clock and the
  default 120 seconds, a file fires about 2 to 3 minutes after its last write. A
  half-copied `.mp4` never fires early. A long copy or a synced folder needs a
  longer window, so `settle_seconds` is set per trigger (the bound is 10 to 3,600
  seconds).
- **Identity.** The subject key, and so the revision, is the file's
  **fingerprint**:
  - **Small files.** With `identity: auto`, a file up to `hash_full_max_bytes`
    (default 256 MiB) is fingerprinted by the SHA-256 of its content.
  - **Large files.** A larger file is fingerprinted by the SHA-256 of its size and its
    first, middle and last 4 MiB, so a 1–5 GB recording costs about 12 MiB of reads.
    `identity: full` always hashes the whole file.
  - **Stat key first.** Before any read, the evaluator compares the stat key
    (device, inode, size, mtime) with the fired tombstones. A match is the same
    subject, so a rename or a move within one filesystem doesn't read the file and
    doesn't fire again. Only a file with a new stat key is fingerprinted, once, when
    it is stable and the rest of `when` holds.

  This gives these outcomes:
  - Re-saving the same bytes gives a new stat key and the same fingerprint, so it
    doesn't fire again.
  - A move across filesystems behaves the same way.
  - Changed bytes at the same path are a new subject and fire.
  - Two identical files at different paths fire once (Open questions).
  - A sampled fingerprint can collide for two large files that differ only outside
    the samples. That is unlikely for real recordings, and `identity: full` removes
    it.
- **Sibling files as one subject.** `talk.mp4` and `talk.vtt` that arrive together
  must become one source, because an imported VTT means transcription is skipped.
  As separate subjects they fire twice, in either order.
  - **How grouping works.** `group_by: stem` makes the subject the group of files in
    one directory that share a stem (the name without its final extension), bound
    as `group`.
  - **When a group is stable.** `group.stable` is true when every member is stable
    and no member has been added or changed for `group_settle_seconds`.
  - **Group identity.** The key is (root, directory, stem). Its revision is the
    sorted member fingerprints at the edge.
  - **Late siblings.** A sibling that arrives after the group fired doesn't fire it
    again, because the subject is already fired and `when` still holds.
  - **Rejected: a sibling lookup in the expression**
    (`system.path[dir].has(file.stem + '.vtt')`). It needs a computed index, which
    breaks the literal rule (§2). Both arrival orders still race: the `.mp4` can be
    stable and fire before the `.vtt` lands.
  - **Rejected: "the action must tolerate it".** That moves deduplication into every
    plugin, and the work still runs twice.
- **Change detection.** In phase 4, each tick rescans. In phase 6, notifications
  through the `notify` crate in a long-lived process trigger an earlier rescan. The
  rescan stays the source of truth, so files added while Orbit was down are found
  on the next pass.
- **Roots and access.** Roots may sit outside the workspace, under these rules:
  - **Inside the owner checkout.** A root is admitted, except `.git/` and `.orbit/`.
  - **Outside the checkout.** A root needs either an entry in host
    `automation.watch_roots`, which only an operator edits, or, for a plugin-seeded
    definition, a place inside that plugin's granted `permissions.fs.read`. The
    operator consented to that grant at `orbit plugin enable`
    ([plugins](../plugins/1_scope.md)).
  - **Limits.** Depth and file-count limits apply (§4).
  - **No sandbox access.** The trigger grants no sandbox read to the job.
- **Unreadable roots.** On macOS, `~/Downloads`, `~/Documents`, `~/Desktop` and
  removable or network volumes are protected by the privacy permissions (TCC). The
  clock process that launchd starts needs Files and Folders or Full Disk Access to
  list them, and the worker's Seatbelt profile is a separate, second gate.
  - **At load or enable.** A root the evaluator can't list (a TCC denial, `EACCES`,
    an unmounted volume) is a load or enable error, `root_unreadable`, naming the
    root and the OS error.
  - **Later.** If access is lost after enable, the condition becomes unknown and
    stalls with `root_unreadable`, shown in explain and in an `orbit doctor` row.
  - **Never empty.** An unreadable root is never read as empty. An empty read would
    drop every tombstone, and every file would fire again once access returned.
- **Plugin-seeded definitions.** A plugin may seed a routine or auto-task with
  `scope: file`. As with every plugin definition, it is seeded disabled, and
  enabling it is an operator act.
  - **Templated roots.** Its `roots` may use the plugin manifest's template set,
    `{{workspace}}`, `{{plugin_state}}` and `{{config.<key>}}`. These render against
    the effective `[plugins.<ns>]` settings, so the user picks the watched folder in
    the plugin's settings. No other templating is admitted.
  - **Validation on every load.** The rendered roots are validated each time the
    definition loads: the root exists, it is a directory, the evaluator can read
    it, it is inside an allowlist or the plugin's grant, and it is not `.git` or
    `.orbit`.
  - **Changing the folder.** A changed rendered root is a new epoch. It baselines the
    new folder's existing files and reruns the enable dry run. A root that fails
    validation is a load error naming the root, never a zero-subject match.
- **Passing the file.**
  - **Job input.** The job's `subject` input carries the file's `path`, `name`, `stem`,
    `ext`, `size`, `mtime` and `fingerprint`, or a group's `files`.
  - **Tool steps.** Job steps already template their inputs from `{{ input.<key> }}`
    (for example `crates/orbit-core/assets/jobs/blocked_task_recovery_pipeline.yaml`).
    A `plugin.tool_call` step therefore takes
    `path: "{{ input.subject.file.path }}"` or
    `files: "{{ input.subject.group.files }}"`.
  - **Permission check.** When the target's tool steps belong to a plugin whose
    granted `fs.read` doesn't cover the root, enable refuses with
    `root_not_granted_to_target`.
  - **No copying.** Orbit doesn't copy the file into the plugin's store. Copying is
    the plugin's own design choice.
- **The action need not finish the work unattended.** A file fire starts a job, and
  the job's steps decide where it waits. The orbit-office intake ([ORB-19206]) will run
  only the import and the cost preview. Processing waits for the user to confirm
  the cost against the digest, which is an office rule (STD-05 §R21 in that
  repository), not an Orbit one:

  ```yaml
  # seeded disabled by the office plugin (illustrative)
  name: office-kt-inbox
  enabled: false
  trigger:
    condition:
      scope: file
      roots: ["{{config.kt_inbox_dir}}"]   # must lie inside the plugin's granted fs.read
      group_by: stem
      group_settle_seconds: 300
      settle_seconds: 300
      when: "group.files.exists(f, f.ext in ['mp4', 'mov', 'm4a']) && group.stable"
  target: job:office_kt_import   # plugin.tool_call office.capture with {{ input.subject.group.files }}
  ```

- **What's missing.**
  - A media ingest job: workspace-defined, or plugin-shipped for the office.
  - Plugin-seeded file roots and `group_by` (phase 4).
  - Phase 6, for sub-minute latency.

## Non-goals

- **Scripts, user functions, or regular expressions in v1.** Predicates only (§5).
- **Task transitions as actions.** Deferred under §6. Approving, re-backlogging or
  completing a task stays a human or typed-pipeline decision.
- **Cross-workspace and cross-host reads.** Each condition reads its owner's
  workspace and the owner host only (§2).
- **Replacing typed kinds.** Coverage, fingerprints, slots and receipts stay typed
  (§1).
- **Exactly-once execution of a job's side effects.** Fires are exactly-once at
  action admission. As with every routine, a job's own effects are only as
  idempotent as the job.
- **A generic event bus or webhooks.** These stay non-goals of automation triggers.
- **Unattended releases.** No grant makes a release run without a per-fire human
  approval when the target asks for one (§7).
- **A workflow engine.** Triggers start work, and jobs order it (§11). There are no
  trigger-to-trigger links, nested trigger trees or time-correlation rules.

## Concerns and honest limitations

- **State, not history.** A condition sees state at each pass. A subject that is true
  for less than one cadence, or that goes true and false while the owner is down, is
  missed. A situation that has to count every transition needs a typed kind with a
  source continuation. That is why `deliveries_landed` stays typed.
- **Latency is one cadence.** It is 60 seconds by default, plus the hold, plus about
  2 settle windows for files. Phase 6 hints shorten it but don't guarantee it.
- **The Rust CEL checker.** Load-time checking depends on a checker the Rust crates
  may not fully ship. Phase 1 may have to write one for the admitted subset, and
  that checker is Orbit-owned code with its own bugs. Either way, the evaluator's
  per-evaluation cost limit is the backstop.
- **The task prefilter rules out some conditions.** A condition with no status or tag
  constraint is refused. That is deliberate (cost), but "any task whose crew is X"
  must name the statuses it cares about.
- **Filenames are untrusted input.** A file condition passes `file.name` and
  `file.path` to a job, and maybe on to an agent prompt through a template. That is
  an injection channel. Templates render them as quoted data, but a job that hands
  them to an agent inherits the risk.
- **Fingerprint cost and collisions.** A file under 256 MiB is read in full once,
  when it first becomes stable. A larger one is sampled, which can in principle
  collide (`identity: full` removes the risk at full read cost). Many files arriving
  together make a slow pass, bounded by the tick deadline. Unfinished fingerprints
  carry over to the next pass.
- **Late siblings.** A grouped file that arrives after its group fired is not
  processed by that fire. A drop folder fed by a slow sync needs a
  `group_settle_seconds` longer than the gap between siblings.
- **Chain depth sees tasks, not aggregates.** Depth follows `origin` through tasks. A
  loop that runs only through aggregates (A mints, the board changes, A re-arms) has
  no origin link. Cooldowns, fire caps and oscillation bound it instead (§8).
- **Host-scope duplication.** Two workspaces on one owner host with similar disk
  conditions are two definitions. They fire twice and notify twice. Host-level
  definitions would fix this.
- **The doc's size.** This scope doc is near the limit of one sitting. If phase 2
  grows specs (the CEL environment, fire records), the feature graduates to the full
  track (CONVENTIONS.md §1).

## Phasing

1. **Language and explain.** Nothing fires yet. This phase adds:
   - the `cel` dependency, the scope environments for `task` and `workspace`, the
     admitted subset and the load-time checker;
   - `trigger.condition` and `schedule.condition` parsing, with
     `enabled: false` enforced;
   - `orbit automation explain`, with clause trees over live state.

   Operators see what would match before anything can fire.
2. **Firing for task and workspace scopes.** This phase adds:
   - condition state rows (migration), edges, holds, re-arm and fire identity;
   - keyed mint and job actions with the subject passed in;
   - the guardrails in §8, the dry run at enable, `clear-stall`, and recording in
     history, lists and the dashboard;
   - `when:` guards on `cron`, `every_minutes`, `deliveries_landed` and `state`
     kinds, plus `origin` stamping and chain depth (§11).

   Covers use case A.
3. **Host scope and notifications.** Host facts from the sampler, notice records in
   the decision channel, and the `notify` action. Covers use case C on owner hosts.
4. **File scope.** This phase adds:
   - roots, the `watch_roots` allowlist, and the rescan manifest;
   - plugin-seeded templated roots checked against plugin grants;
   - `root_unreadable` reporting, including macOS privacy denials;
   - `stable`, fingerprints, tombstones, `group_by: stem` and `system.path`
     aggregates.

   Covers use case D, including the office intake, with rescan only.
5. **High-authority actions.** The job `authority` and `approval` fields, operator
   grants with digests, and the `trigger_action` decision gate. Covers use case B
   once a workspace defines its release job.
6. **Later.** Each of these is filed only when needed:
   - task dirty-ID hints and OS file notifications;
   - host-level definitions for followers;
   - presets for `pilot_warnings_held` and `block_escalated` (§1);
   - transition actions under a grant.

## Open questions

1. **Task transitions.** Should a grant ever authorize one (for example re-backlog
   within the final-recovery requeue bound), or should transitions stay
   pipeline-only? Leaning: pipeline-only. Revisit only with a concrete use case.
2. **Identical files at different paths.** Should they fire once (fingerprint
   identity, the default here) or once per path? A `path` identity mode costs little,
   but it doubles what fire identity means.
3. **Language-tagged siblings.** Should `group_by: stem` also strip a language tag, so
   that `talk.en.vtt` groups with `talk.mp4`? Removing only the final extension gives
   `talk.en`. A `stem_strip` pattern list would cover it, at the cost of another
   setting.
4. **Multiple actions.** Is it enough to have two definitions with the same
   condition, or should a definition be able to reference a named shared condition
   so that the edge state is shared?
5. **Federated reads.** Could a condition read another owner's workspace through
   federated MCP, with that owner's revisions in the fire identity? Leaning: no,
   until a use case needs it.
6. **Own tasks in aggregates.** Should `board` aggregates leave out a condition's
   own minted tasks, so that A doesn't re-arm itself? It would remove the
   cooldown dance, at the cost of a less obvious "board".
7. **Host-level definitions.** Where should they live? The global root
   (`~/.orbit/`) is the only other root allowed. Followers would evaluate only
   host-local actions there: notices, and GC of their own worktrees.
8. **Re-enable after disable.** Should it baseline (the choice here), or fire subjects
   that went true while disabled? Baselining means an operator who disabled a
   condition is never surprised by a backlog of fires.

## Acceptance (feature level)

- A routine or auto-task with `condition` whose expression names an unknown field,
  is not `bool`, uses a computed cross-read index or lacks a task prefilter fails to
  load. The error names the file and the clause, and every other definition still
  loads.
- An older binary that meets a condition definition reports a per-file load error
  and fires nothing.
- `orbit automation explain` shows, without writing anything, the current matches,
  the closest misses and a clause tree with operand values for any subject.
- Enabling a condition with more current matches than `max_matches` is refused. By
  default, enabling baselines current matches without firing.
- A subject that becomes true fires exactly once: across a crash at every step of the
  protocol, a restart, and two concurrent evaluators. It fires again only after its
  `rearm` holds and `when` becomes true again.
- A `for_minutes` hold doesn't fire on a shorter true spell, or across an
  observation gap longer than the continuity limit.
- A fire passes its subject to the action. The minted task or job input carries the
  fire ID, the subject key, the immutable observation revision at the edge and the
  bound variable's fields. Fire and decision history writes never change that
  identity or recapture its input.
- A pull follower and a replica checkout never fire a workspace, task or file
  condition.
- An operator-authority target doesn't run without a current grant whose digest
  matches the definition and target. With `approval: per_fire`, it doesn't run
  without a human answer, and an agent edit stops both.
- A per-fire request's binding stays current across its own trusted `condition_fired`,
  decision-request, delivery and answer bookkeeping, including their history and
  `updated_at` changes. A real subject change between observation and request or
  between request and admission supersedes it even if `when` stays true; a
  definition, target or grant change does likewise. Reverting a change never
  revives the request. The apply step fences freshness and authority checks with
  keyed admission, and retries resolve the original run. Use case B's
  history-40/41/42 trace admits on unchanged freshness 40 and refuses after a real
  change at 43.
- A file condition fires once per new stable file, even for a re-save of the same
  bytes or a rename within its roots. It finds files added while Orbit was down.
  With `group_by: stem`, a `.mp4` and `.vtt` pair fires once, in either arrival order.
- A root the evaluator can't read is a load or enable error naming the root, or a
  `root_unreadable` stall after enable. It is never a match of zero subjects, and its
  tombstones are kept.
- A `when:` guard on a `cron`, `every_minutes`, `deliveries_landed` or `state` kind
  suppresses the fire while false or unknown. It never drops delivery debt or state
  members.
- Each task a fire mints carries its origin (trigger, fire ID, depth). A fire past
  depth 5 is refused and recorded, and explain shows each fire's upstream chain.

## Task References

- [ORB-19288] — designs condition triggers (this doc).
- [ORB-12746] — added state-trigger batching (`batch_size`), which presets would need.
- [ORB-15732] — recorded the 30-day, release-gated compatibility window.
- [ORB-19214] — added the `not_before` scheduled hold, which `board.eligible` excludes.
- [ORB-19206] — orbit-office orchestration that relayed the watched-folder intake requirements for use case D.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
