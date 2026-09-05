---
title: Auto-tasks — Design
owner: claude
last_updated: 2026-09-05
last_validated: 2026-09-05
status: Accepted
feature: auto-tasks
doc_role: design
type: design
summary: Current implementation of the auto-task record, due-math, host-local cursor, generic scheduler, CRUD surfaces, the on-demand manual mint, and the dashboard Operations surface.
tags: [auto-tasks]
paths: ["crates/orbit-core/src/application/auto_tasks/**", "crates/orbit-web/src/api/auto_tasks.rs", "crates/orbit-web/assets/dashboard/operations.js"]
related_features: [auto-tasks]
related_artifacts: [ORB-10149, ORB-10439, ORB-10441, ORB-10446, ORB-10472, ORB-10583, ORB-10800, ORB-10876, ORB-11095, ORB-11315]
---

# Auto-tasks — Design

This doc covers the shipped implementation: the definition record, discovery,
due computation, cursor state, the scheduler pass, and the CRUD surfaces. The
routine machinery it rides on (cron eval, fire records, dashboard health) is
documented under `docs/design/routines/`.

The [shared automation-trigger proposal](../automation-triggers/1_overview.md)
from [ORB-11315] specifies delivery thresholds, preparation/failure eligibility,
immutable batches and separate successful-coverage checkpoints. It is proposed
and unimplemented; existing scheduling and action semantics remain current.

## 1. The definition record

`AutoTaskDefinition` (`crates/orbit-types/src/workflow/auto_task.rs`) is a
`deny_unknown_fields` struct: `schemaVersion`, `name`, `description`, `enabled`,
`schedule`, `template`, `dedupe`, and provenance (`created_by/at`,
`updated_by/at`). `schedule` is an untagged enum — `{ cron: "…" }` or
`{ every_minutes: N }`. `template` carries `title`, `description`,
`acceptance_criteria`, `task_type`, `tags`, `priority`, `crew`, and `status`
(default `backlog`). Minted tasks always receive
`complexity: unassessed` — an explicit non-answer, not a fabricated
`low`/`medium`/`hard` assessment. Definitions do not carry complexity;
the shared template-to-task mapping stamps the value. Per [Run budgets are provider-neutral: wall-clock timeouts, never turn caps](./4_decisions.md#run-budgets-are-provider-neutral-wall-clock-timeouts-never-turn-caps) there are **no turn-based knobs**; `deny_unknown_fields`
makes a stray `max_turns`/`turns` a hard parse error.

Definitions live as `.orbit/auto_tasks/<name>.yaml` in the active checkout.
Discovery (`loader.rs`) scans the directory, parses each file fail-closed, and
rejects any file whose stem ≠ its `name`, so the on-disk identity and the
`auto-task:<name>` provenance tag stay in lockstep. In a linked-worktree
runtime, definition discovery and CRUD use `WorkspacePaths::local_dir`;
host-local cursor state continues to use the shared root. This split makes
definition edits ordinary branch content instead of transient tracked dirt in
the registered primary checkout ([Route tracked auto-task definitions through the active worktree](./4_decisions.md#route-tracked-auto-task-definitions-through-the-active-worktree)).

## 2. Due computation and catch-up collapse

`schedule::decide_due(schedule, baseline, last_slot, now)` returns `NotDue` or
`Fire { slot }`. The effective exclusive floor is `last_slot` when the
definition has fired before, else `baseline` (its first-observed slot). Cron
reuses `routines::due::due_decision` under `MissedRunPolicy::CatchUpOnce`;
interval math jumps straight to the most recent boundary
`baseline + floor((now-baseline)/interval)·interval`. Either way a downtime gap
collapses to **one** fire, never one per missed slot.

## 3. Cursor state

`state.rs` stores one cursor per definition in
`<orbit_dir>/state/auto-tasks.json` (`{ baseline_at, last_slot, last_fired_at,
last_task_id }`), using a file-locked read-modify-write.
This is workspace-local, gitignored runtime state (the scoreboard precedent,
L-0041), so a scheduler fire never rewrites the git-versioned definition and a
definition edit never races the scheduler.

## 4. The scheduler pass

`scheduler::run_auto_task_scheduler_at` loads the workspace's definitions and
cursors, then per enabled definition: on first sight it records a baseline and
fires nothing; otherwise it evaluates due-math. On `Fire`, if `dedupe =
skip_if_open` and a task tagged `auto-task:<name>` is still open, it skips
**without advancing the cursor** — so the pending occurrence fires (once,
collapsed) the moment the queue drains. Otherwise it mints a `system_created`
task from the template (tagged for provenance, complexity `unassessed`)
and advances the cursor. Every
minted title is `[auto-task] ` followed by the template title; the prefix is
applied at the shared template-to-task mapping, so definition YAML titles stay
clean and an already-prefixed template is not double-prefixed.

The pass is the deterministic `run_auto_task_scheduler` action
(`dispatch.rs`), wrapped in `auto_task_scheduler_pipeline` (`max_active_runs:
1`), fired by the seeded `auto_task_scheduler` routine (`overlap: forbid`,
minutely). Because it is a routine, its fires flow to `GET /api/routines`.

## 5. CRUD surfaces

`crud.rs` is the single choke point behind both the CLI (`orbit auto-task
add/list/show/update/toggle`) and the registry tools (`orbit.auto_task.*`). Add
rejects duplicate names; update patches present fields; toggle flips `enabled`
(disabling is preserved, never a delete). Both surfaces validate the schedule
(cron parse / interval > 0) and crew at write time, so a bad definition is never
persisted. Successful writes replace the target atomically; a staging or rename
failure leaves the previous definition bytes intact. In a primary checkout the
local and shared roots are identical, preserving the operator-facing path.

`list` is fail-closed-aware. The loader collects a per-file `AutoTaskLoadError`
for every definition it rejects, and after [ORB-10800] those errors are no longer
discarded: each is logged, and `list` errors outright only when *nothing* loaded,
so one malformed file cannot hide the definitions that still work. A definition
that silently stopped firing is discoverable as a `faulty` row on the
`orbit doctor` artifacts surface rather than only via the one command that
happens to touch it.

### 5a. Managed seeding

Default definitions are seeded manifest-aware after [ORB-10800] / [All five definition-artifact kinds carry managed provenance, and doctor reports it](../activity-job/4_decisions.md#all-five-definition-artifact-kinds-carry-managed-provenance-and-doctor-reports-it):
`.orbit/auto_tasks/` carries a `.orbit-managed-assets.json` recording the digest
Orbit wrote for each shipped default, so a default dropped from a later release
can be retired by content provenance instead of remaining loadable forever.
Seeding still never overwrites an existing definition, and an operator-edited
default is preserved under `.retired-managed/auto_tasks/` rather than deleted.

## 5b. Manual mint — `mint` (ORB-10439, renamed by ORB-10446)

`orbit auto-task mint <name>` mints one task from a definition on demand, so
a new or edited definition can be exercised without waiting for its slot (weekly
definitions otherwise cost a week per typo). It lives on `crud.rs` alongside the
other verbs and delegates to `scheduler::mint_task` — the scheduler's mint path
is already separable from due-math (it needs only the definition), so there is
exactly one template→task mapping and a manually minted task is field-for-field
identical to a fired one: same field mapping, same `[auto-task] ` title
convention, same `auto-task:<name>` tag, same `system_created` marker, same
template-supplied status.

Title provenance is enforced by `OrbitRuntime::add_task_with_identity`, the
shared creation boundary used by CLI, MCP, dashboard, scheduler, and internal
callers. An `auto-task:<name>` tag maps to `[auto-task] ` and takes precedence
for scheduler-minted parent tasks. Otherwise finding tags use this fixed,
input-order-independent precedence: `qa-sweep`, `security-review`,
`code-review`, then `friction-curation`. The selected prefix is idempotent.

The mint is **unconditional**. It ignores schedule due-math, `dedupe`, and
`enabled`, and it neither reads nor writes the host-local cursor — an operator
naming a definition explicitly means it, and a manual mint must not perturb
scheduler state. Unknown names fail loudly (`InvalidInput` naming the
definition), so the CLI exits non-zero rather than silently no-op'ing.

Deliberately rejected:

- **A mint-local mint implementation.** A second template→task mapping
  would drift from the scheduler's, and the provenance parity that makes the
  feature worth having is precisely what drift destroys.
- **Honoring `enabled`/`dedupe`/due-math.** That makes `mint` a "run the
  scheduler early" button, which the existing `run_auto_task_scheduler` action
  already is. The gap being closed is *manual mint*, not *early fire*.
- **Advancing the cursor.** It would consume a real scheduled slot, silently
  cancelling the next automatic fire.
- **`--dry-run` / `--force` flags.** `--force` has nothing to override — the mint
  is already unconditional — and `--dry-run` would only re-print the template
  that `auto-task show` already renders. The surface is `<name>` plus `--json`,
  matching the sibling subcommands.

One consequence follows from parity and is intended: because a manually minted task
carries the provenance tag, an open one is visible to `skip_if_open` on the next
scheduler pass and defers that fire, exactly as an open fired instance does. The
cursor does not advance, so the deferred occurrence fires once when the queue
drains. This is the behavior the hand-copy workaround could not provide.

Advertisement follows who does the work [ORB-10798]. Authoring a Git-versioned
definition (`add`, `show`, `update`, `toggle`) is human/admin work: those tools
are `register_inactive`, reachable through their `orbit auto-task` subcommands
but absent from MCP `tools/list`. Reading the definitions (`list`) and minting
one on demand (`orbit.auto_task.mint`) are what an executing agent needs, so
both are registered at `McpToolScope::WorkspaceRequired`. The MCP tool is a thin
adapter over the same `auto_task_mint`, so the mint stays unconditional and
cursor-neutral on every surface.

## 5c. Dashboard Operations surface [ORB-10876]

The dashboard Operations tab exposes the same CRUD/mint runtime rather than a
second scheduler. `#operations/auto-tasks` lists the selected workspace's
definitions (name, enabled, schedule, template summary, dedupe, last
evaluation/mint, last minted task id, next evaluation when the cursor makes
that derivable). Enable/disable writes `enabled` through `auto_task_toggle`
with `expected_enabled` compare-and-swap, operator authorization
(`auto_task.toggle`), and a dashboard-operations audit row. `Mint now` calls
`auto_task_mint` after the operator acknowledges the unconditional warning
(`acknowledge_unconditional: true`); the request is refused without that
disclosure. All-workspace and inactive/unknown workspace selections stay
read-only. Refresh and hash navigation only GET — they never replay a toggle
or mint.

## 6. Concerns & Honest Limitations

The seeded `qa-sweep` definition is disabled by default. When enabled, its
`50 * * * *` cron mints a backlog task for crew `system` at minute 50 each hour,
dedupes while one remains open, and asks the executor to validate recent changes
hands-on and file real findings through Orbit. Its `no-diff-expected` tag lets workflow handoff succeed when the
validation correctly produces only task-side effects.

The workspace-local `model-price-audit` definition (ORB-10583) is an enabled
weekly report-only consumer: it runs Monday at 06:00 in the host-local timezone,
uses `skip_if_open`, mints a backlog chore for crew `terra`, and carries
`model-price-audit`, `pricing`, and `no-diff-expected`. Its template compares
exact `InvocationRecord.model` strings and every current price-table row against
authoritative provider pricing/model/cache documentation. It records source
URLs, retrieval timestamps, rates, units, tiers, and effective boundaries; it
never edits pricing. A proven material drift may produce at most one deduplicated
proposed remediation task for normal human review, while unavailable,
contradictory, or ambiguous official evidence produces a report without a
remediation task. No routine or portable seeded default is added for this
workspace-local definition.

Operators may inspect it with `orbit auto-task show model-price-audit --json` and
use scheduler dry-run inspection before the weekly slot. The generated task's
`execution_summary` is the audit report and must include no-diff evidence,
observed models, checked sources, and effective periods when the table is
accurate.

- **Definitions are not full-text indexed.** Unlike indexed docs, auto-task
  YAML is not in a SQLite/search index; discovery is a directory scan. Acceptable
  at the expected cardinality (a handful of chores per workspace).
- **Workspace-scoped.** The scheduler processes the definitions of the workspace
  whose routine fired it, not a cross-workspace sweep. Multi-workspace fan-out is
  a future direction (see 3_vision.md).
- **Description secrets are not redacted** in the definition YAML (task creation
  still redacts when minting). Definitions are operator-authored, so this is
  low-risk, but not zero.

## Task References

- [ORB-11315] — proposes shared state-driven triggers and durable coverage semantics.

- ORB-10876 — dashboard Operations inspection, toggle, and manual mint.
- ORB-10149 — Auto-task primitive.
- ORB-10439 — on-demand manual mint (renamed to `orbit auto-task mint <name>` by ORB-10446).
- ORB-10441 — mint-time visible title provenance.
- ORB-10472 — worktree-local, atomic definition refresh.
- ORB-10583 — workspace-local weekly official model-price audit definition.
- ORB-11095 — centralized tag-derived title provenance and the canonical
  `code-review` auto-task name.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
