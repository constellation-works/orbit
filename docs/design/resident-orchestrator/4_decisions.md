---
title: Resident Orchestrator — Decisions
owner: codex, grok, claude
last_updated: 2026-09-10
last_validated: 2026-09-10
status: Draft
feature: resident-orchestrator
doc_role: decisions
type: design
summary: Decision log for the drain job, the external clock, the split between leaf ship and workspace auto, and the v2 move to an epic-owned worktree with a windowed workspace drain.
tags: [resident-orchestrator, epic, jobs]
paths: [".orbit/resources/jobs/**", "crates/orbit-core/assets/jobs/**"]
related_features: [resident-orchestrator, activity-job]
related_artifacts: [ORB-10332, ORB-10775, ORB-10776, ORB-10779, ORB-10788, ORB-10815, ORB-10816, ORB-10817, ORB-10818, ORB-10819]
---

# Resident Orchestrator — Decisions

This document preserves the feature's non-obvious decisions and their reasoning.

---

## Epic tag is a supervisor delegation signal, not the job predicate

**Recorded:** 2026-08 · [ORB-10776]

### Context

The removed `task_epic_pipeline` treated `TaskType::Feature` as an epic. A later draft made
the `epic` tag the *job's* pickup selector. The v1 job instead wakes on
`proposed`/`backlog`/`blocked` plus failed/timeout runs, or it would miss ordinary chores
and failed pipelines that are not tagged.

### Decision

`epic` on a root task means "a supervisor owns this outcome." Catalog code must not require
the tag to drain work. Adding a second pickup key (`type: feature`, assignee, folder) for
the *job* is out of contract unless this ADR is superseded.

### Consequences

- Supervisors can still create `epic` roots and children as they do today (ORB-10775).
- Cost: a workspace with leftover `backlog` chores will wake the drain job even when no
  epic exists; isolation is a workspace-layout problem, not a tag filter.
- Leaf-ship exclusion uses the same tag ([Workspace auto is a sequencer, not a leaf ship](#workspace-auto-is-a-sequencer-not-a-leaf-ship)). That does not change this ADR's
  rule: `epic_pipeline` still must not require the tag to drain.

## The supervisor clock is not an Orbit primitive

**Recorded:** 2026-08 · [ORB-10776]

### Context

The first draft specified `resident_orchestrator`, `select_resident_epic`, a JSON comment
protocol, conversation resume, and a seeded `resident-epic-orbit` routine. That rebuilds
an orchestrator inside Orbit next to Cowork / Grok / a knowledgebase cron that already
speak MCP.

### Decision

Orbit v1 ships only `scan_unresolved_work`, `epic_orchestrator`, and `epic_pipeline`.
Do not add an Orbit routine, selector activity, session-resume requirement, or
comment-typed mailbox for this feature. The fire clock lives in a knowledgebase (or
front-door) process that calls `orbit run job epic_pipeline`.

### Consequences

- A future "make it a routine" PR needs to supersede this ADR, not sneak a YAML into
  `.orbit/routines/`.
- Cost: no first-class resident health in `orbit routine list`; operators debug the
  external cron and the job-run log instead.

## Session log is the orchestrator's memory, not a CLI session

**Recorded:** 2026-08 · [ORB-10776]

### Context

Each drain fire is a new CLI process. Conversation resume is out of v1. The orchestrator
still needs to leave itself status, notes, and "check this later," and to see new notes
on the next fire. Task comments are a human thread. A standing `backlog` log task would
wake the scan forever. A file in the knowledgebase cron repo is the wrong workspace.

### Decision

Give the workspace an append-only `orbit.session_log` with kinds `status`, `note`, and
`check_later`. Unresolved `check_later` entries are a `scan_unresolved_work` wake reason.
`status`/`note` are not. Resolve is the only mutation besides append. The session log is an
internal Core capability; the `orbit.session_log.*` agent tools are not part of the public tool
surface. In the v2 epic path, the finisher may edit its owned worktree directly, while child tasks
remain the decomposition path when useful.

### Consequences

- The workspace scan still includes unresolved `check_later` entries; no provider session id is
  required to resume the durable work state.
- Cost: another internal noun and durable store. Unresolved reminders still keep waking the
  drain until the owning operator resolves them; the former public agent tools are withdrawn.

## Drain scan excludes `epic_pipeline` runs

**Recorded:** 2026-08 · [ORB-10779]
**Code anchors:** `crates/orbit-core/src/runtime/v2_host/scan_unresolved.rs::scan_unresolved_work`

### Context

A leftover scan after `max_iterations` fails `epic_pipeline` closed. The next
external fire must see the leftover *tasks* and *child* failed/timeout runs, not
the drain job's own failed row. Including `epic_pipeline` itself would make every
ceiling failure a permanent wake reason and invite the orchestrator to resume
the drain from inside the drain.

### Decision

`scan_unresolved_work` omits job-runs whose `job_id` is `epic_pipeline`. Child
pipeline failures remain wake reasons. The supervisor clock starts a *new*
`epic_pipeline` run; it does not resume the previous drain via the scan set.

### Consequences

- A fail-closed drain can fire again on the next cron tick without first
  cancelling or resolving its own previous run.
- Cost: an operator cannot use the scan to discover a wedged `epic_pipeline`
  run; they use `orbit run history` / `orbit.workflow.run.list` instead.

## Workspace auto is a sequencer, not a leaf ship

**Recorded:** 2026-08 · [ORB-10788]
`orbit run auto`

### Context

Auto `orbit run ship` admitted an `epic`-tagged root (ORB-10775) as a leaf. The
implement pipeline found nothing to commit and parked the tracker in `blocked`.
Folding "start `epic_pipeline`" into `task_auto_pipeline` was the other alternative:
same verb, different child job, different success definition, exclusive
concurrency. That would make empty-backlog and `pipeline_success_guard` lie, and
would race the orchestrator on epic children.

### Decision

Keep `orbit run ship` a leaf implementer. Auto `list_backlog` skips any task that
is `tag: epic` or has such an ancestor (`epic_root` / `epic_child`). Explicit ship
of an epic root is refused before worktree setup; explicit ship of an epic child
stays allowed (the orchestrator path). Logistics live in a new job,
`workspace_auto_pipeline`, invoked as `orbit run auto`: each window iteration re-lists and drains
loose leaves; when no epic run is active, it starts at most one backlog epic via
`epic_pipeline`. Do not seed a routine that directly targets these resident jobs ([The supervisor clock is not an Orbit primitive](#the-supervisor-clock-is-not-an-orbit-primitive) still holds). Do not scope
`scan_unresolved_work` to one epic in this change.

### Consequences

- Two auto verbs. Muscle memory `orbit run ship` no longer starts an epic; operators
  who want logistics use `orbit run auto`.
- An in-progress epic blocks only overlapping loose leaves; conflict-free chores can continue
  during the drain window.
- Cost: a third catalog job and a new CLI verb. Untagged backlog can still race if
  someone fires both `orbit run ship` and `epic_pipeline` on the same workspace.

### Partially superseded

The split itself stands: `orbit run ship` is still the leaf implementer, `orbit run auto`
is still the logistics verb, and the epic family is still excluded from auto leaf ship.
The *tick shape* does not survive — see [Auto drains for a window instead of taking one action](#auto-drains-for-a-window-instead-of-taking-one-action) ([ORB-10819]), which
deletes `hold` and the one-action tick, and [Epic completion is epic-scoped](#epic-completion-is-epic-scoped-not-workspace-scoped) ([ORB-10818]), which reverses
"do not scope `scan_unresolved_work` to one epic" by giving `epic_pipeline` its own
epic-scoped gate instead of scoping the workspace scan.

## An epic owns one worktree and one branch

**Recorded:** 2026-08 · [ORB-10816] · **Implemented** in [ORB-10816]

### Context

`epic_pipeline` had no worktree. Its orchestrator dispatched children through
`task_gate_pipeline`, so each child branched off the workspace base, got its own
worktree, and landed independently. One epic produced N branches, N PRs, and N
`review` items, and nothing ever saw the epic's combined result before it reached
the base branch. The alternative — keep per-child delivery and add a "roll them up"
step afterwards — reconciles work that already landed, which is strictly harder than
never splitting it.

### Decision

An epic run opens one stable worktree and branch, keyed by the epic root's id through
`worktree_setup`'s existing `run_id` / `branch_prefix` inputs. Non-terminal descendants
land into that branch **sequentially** through `task_local_pipeline` with
`base_sync: local`, no push, and `landing_branch` set to the real workspace base. A
child reaches `done` on merge into the epic branch, not `review`. An epic with no
children skips the phase.

Sequential is required, not preferred: `merge_with_rebase_retry` lands a child with
`git merge --ff-only` plus at most two rebases, and parallel children onto one branch
race that fast-forward.

### Consequences

- One PR and one review item per epic.
- Siblings need no lock gate against each other — the epic's ordering is the
  serialization — so epic children bypass the reserve-and-wait loop.
- An epic with ten independent children pays ten serial child runs.
- Cost: a child is `done` while its commits sit only on the epic branch, and
  `worktree_gc` reclaims its worktree at that point. An abandoned epic leaves `done`
  tasks whose work never landed; recasting them is a human act.

## The epic agent works in the worktree instead of dispatching

**Recorded:** 2026-08 · [ORB-10817] · **Implemented**

### Context

`epic_orchestrator` was allowlisted for task/workflow/search/session-log and explicitly
forbidden from git writes, worktree edits, and `agent_implement`. That was coherent when
the epic had no worktree: the agent could not be trusted with a tree it did not own. Once
the epic owns one ([An epic owns one worktree and one branch](#an-epic-owns-one-worktree-and-one-branch)), the prohibition only forces every change through a
child task and a second pipeline.

### Decision

`epic_orchestrator` runs inside the epic worktree with writes scoped to it, on the same
footing as `agent_implement`, and finishes the work itself — delegating to subagents as
useful. Sub-task breakdown becomes optional: a human or a higher-up orchestrator
decomposes an epic when that earns its keep; otherwise the root has no children and the
agent does the whole body of work. It may still author children, and the job loops back
to the drain phase when it does.

### Consequences

- A wedged epic no longer waits on a shipped child for a one-line fix.
- Larger blast radius. The worktree-mismatch guard and the sandbox profile are what keep
  the agent inside its worktree.
- The agent's timeout must be tuned for working, not dispatching.
- Unchanged: no PR merge, no second workspace, no invented approval policy.

## Epic completion is epic-scoped, not workspace-scoped

**Recorded:** 2026-08 · [ORB-10818] · **Implemented** in [ORB-10818]

### Context

`epic_pipeline` gated completion on `scan_unresolved_work` with `fail_if_nonempty`, a
workspace-wide predicate. It failed an epic run because an unrelated chore sat in
`backlog`, and it passed an epic run whose own children were unfinished whenever the
workspace happened to be quiet. [Workspace auto is a sequencer, not a leaf ship](#workspace-auto-is-a-sequencer-not-a-leaf-ship) deferred epic scoping on purpose; the
worktree-owning shape makes the deferral untenable.

### Decision

`epic_pipeline` gates on its own root's non-terminal descendants. A leftover set at the
iteration ceiling fails the run closed, as before. `scan_unresolved_work` keeps its
workspace-wide shape and its `epic_pipeline`-run exclusion — it is simply no longer this
job's gate; it serves the workspace drain. Delivery is inlined against the epic worktree
(`pr_prepare` → `git_rebase` → `git_push` → `pr_open` → `pr_promote`, or `git_merge` for
local mode) rather than delegated to `task_pr_pipeline`, which would build a second
worktree via its own `worktree_setup`.

### Consequences

- An epic's success no longer depends on unrelated workspace state.
- The workspace-wide drain moves to the auto window, where it belongs.
- Cost: two completion predicates now exist. Keep the distinction explicit — epic-scoped
  for `epic_pipeline`, workspace-scoped for the drain.

## Auto drains for a window instead of taking one action

**Recorded:** 2026-08 · [ORB-10819] · **Implemented** (the epic reservation it relies on is live from [ORB-10816])

### Context

`workspace_auto_pipeline` classified once and acted once: ship the leaves, or start one
epic, or hold, or nothing. Backlog membership was sampled at classify time, so work
created a second later waited for the next external fire; `hold` froze conflict-free
chores behind an unrelated epic; and `invoke_and_wait` on `epic_pipeline` would starve
leaves behind a multi-hour epic even without `hold`. Firing the external clock more often
does not fix this — it re-pays startup per tick and still cannot see work that arrives
mid-tick.

### Decision

`orbit run auto --for <duration>` drains for a caller-supplied window: re-list admissible
work each iteration, ship it, sleep when idle, stop starting new work when the deadline
passes. Absent or zero preserves the one-tick behavior. The deadline gates starting work
only; in-flight children finish on their own because detached child runs are not cancelled.
`epic_pipeline` and leaf pipelines are dispatched **detached** and re-observed each iteration.
`decision: hold` is deleted —
an in-progress epic holds one reservation over the union of its descendants'
`context_files`, and loose leaves are admitted by the existing overlap check.

### Consequences

- Work created after the run started ships in the same run.
- Conflict-free leaves and an epic proceed concurrently.
- One `--for 2h` run occupies `max_active_runs: 1` for two hours, and
  `workspace_ship_pipeline` waits on it, so its routine's `overlap: forbid` covers the
  whole window.
- Cost: a clock primitive (`drain_window`) and a non-blocking invoke, neither of which
  existed. Conflict admission stays all-or-nothing; a tolerance threshold and
  `orbit run ship --force` are deliberately out of scope.

## Task References

- **[ORB-10332]** — Remove the unused HTTP epic pipeline.
- **[ORB-10775]** — v1 implementation epic.
- **[ORB-10776]** — Record this split.
- **[ORB-10779]** — Ship the scan, the orchestrator, and `epic_pipeline`.
- **[ORB-10788]** — Sequencer and leaf-ship exclusion.
- **[ORB-10815]** — Epic-owned worktree and continuous workspace drain.
- **[ORB-10816]** — Epic worktree, sequential child drain, epic reservation.
- **[ORB-10817]** — Epic agent works in the worktree.
- **[ORB-10818]** — Epic-scoped completion and inlined delivery.
- **[ORB-10819]** — Drain window, `hold` removal, detached epic dispatch.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
