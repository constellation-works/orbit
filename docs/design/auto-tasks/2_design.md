---
title: Auto-tasks — Design
owner: claude
last_updated: 2026-09-12
last_validated: 2026-09-12
status: Accepted
feature: auto-tasks
doc_role: design
type: design
summary: Current implementation of the auto-task record, due-math, host-local cursor, generic scheduler, CRUD surfaces, the on-demand manual mint, and the dashboard Operations surface.
tags: [auto-tasks]
paths: ["crates/orbit-core/src/application/auto_tasks/**", "crates/orbit-web/src/api/auto_tasks.rs", "crates/orbit-web/assets/dashboard/operations.js"]
related_features: [auto-tasks, routines]
related_artifacts: [ORB-10149, ORB-10439, ORB-10441, ORB-10446, ORB-10472, ORB-10583, ORB-10800, ORB-10876, ORB-11095, ORB-11315, ORB-11730]
---

# Auto-tasks — Design

This doc covers the shipped implementation: the definition record, discovery,
due computation, cursor state, the scheduler pass, and the CRUD surfaces. The
routine machinery it rides on (cron eval, fire records, dashboard health) is
documented under `docs/design/routines/`.

> **Pending change — clock consolidation (decided 2026-09-12, unimplemented).** §4's
> routine → job → activity wrapping is replaced by a direct call from the host clock tick;
> the `auto_task_scheduler` routine, `auto_task_scheduler_pipeline` job, and
> `run_auto_task_scheduler` activity are retired. Definitions gain no host field: every
> registered owner checkout with an enabled clock evaluates every enabled definition
> against its own store. See
> [Auto-task definitions are evaluated by the host tick, not fired by a routine](./4_decisions.md#auto-task-definitions-are-evaluated-by-the-host-tick-not-fired-by-a-routine)
> and [routines/3_vision.md §0](../routines/3_vision.md#0-graduating-clock-consolidation).

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

## 2b. Catch-up eligibility is not the next scheduled occurrence

`decide_due` answers **catch-up eligibility**: is there an unconsumed slot this
pass may fire? After downtime that answer is a slot *already in the past* — a
fire the scheduler still owes. `schedule::next_scheduled_slot(schedule,
baseline, now)` answers a different question: when does the schedule **next
come around**? It is always strictly after `now`, and it is what operator
surfaces render as next evaluation. The two disagree exactly while a missed
slot is pending, and that disagreement is correct — neither output is a check
on the other.

Both derive every slot from one place so the two views stay anchored
identically. `routines::due::next_occurrence` is the cron owner: it pins each
occurrence to its minute, as the due path does, so a slot is stable across
polls within one minute. Interval slots share one anchoring rule
(`baseline + n·interval`), one range check, and checked date arithmetic — an
out-of-range interval or an extreme cursor baseline is an error, never a
wrapped slot the scheduler would mistake for a due boundary. Interval
projections need the cursor's baseline to anchor to; cron projections are
absolute and need no cursor.

## 3. Cursor state

`state.rs` stores one cursor per definition in
`<orbit_dir>/state/auto-tasks.json` (`{ baseline_at, last_slot, last_fired_at,
last_task_id, pending? }`). This is workspace-local, gitignored runtime state
(the scoreboard precedent, L-0041), so a scheduler fire never rewrites the
git-versioned definition and a definition edit never races the scheduler.

Admission and persistence share one stable sidecar lock,
`.auto-tasks.json.lock`. The JSON file is replaced by rename, so exclusion is
not tied to the inode being replaced and a reader never observes truncated
JSON. Updates re-read under that lock, so concurrent upserts for different
definitions keep both cursors.

A missing file is empty state and may baseline on first observation. An
existing file that cannot be read or parsed is an explicit error; the bytes
are left unchanged for investigation. The dashboard list surfaces that error
instead of rendering a silent never-observed baseline.

`pending` is durable in-flight evidence: `{ slot, task_id? }`. It is written
before mint and cleared only after the consumed-slot checkpoint. It is not a
cross-store exactly-once token.

## 4. The scheduler pass

`scheduler::run_auto_task_scheduler_at` loads the workspace's definitions,
then per enabled definition holds the sidecar lock, re-reads cursors, and
either baselines, skips, or fires. Dry-run never writes. On first sight it
records a baseline and fires nothing; otherwise it evaluates due-math. On
`Fire`, if `dedupe = skip_if_open` and a task tagged `auto-task:<name>` is
still open, it skips **without claiming or advancing the cursor** — so the
pending occurrence fires (once, collapsed) the moment the queue drains.
Otherwise it claims the slot, mints a `system_created` task from the template
(tagged for provenance, complexity `unassessed`), and checkpoints
`last_slot` / `last_task_id`. Every minted title is `[auto-task] ` followed
by the template title; the prefix is applied at the shared template-to-task
mapping, so definition YAML titles stay clean and an already-prefixed
template is not double-prefixed.

Recovery on the next locked pass:

- Mint failure rolls the claim back; the slot is not consumed and a later
  pass may fire it.
- `pending.task_id` set: checkpoint the already-minted task, do not remint.
- `pending` without `task_id`: report `unresolved_pending` and leave the
  file alone. The scheduler will not silently consume an unminted slot or
  remint an uncertain one. An operator inspects tagged tasks and
  `auto-tasks.json`.
- Checkpoint write failure reports `fired` with the task id and best-effort
  mint evidence; retry reconciles from `pending.task_id` or stays
  unresolved.

The pass is the deterministic `run_auto_task_scheduler` action
(`dispatch.rs`), wrapped in `auto_task_scheduler_pipeline` (`max_active_runs:
1`), fired by the seeded `auto_task_scheduler` routine (`overlap: forbid`,
minutely). Those job/routine knobs reduce overlap; they are not storage-level
idempotency. Because it is a routine, its fires flow to `GET /api/routines`.
**[slated to change]** — the tick calls `run_auto_task_scheduler_at` directly
under the host sweep lock; the job/routine knobs go away (the sidecar lock is
the exclusion, as it already was) and fires stop appearing on `/api/routines`.

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
scheduler evaluation, last minted task id, and a structured next-evaluation
state). Next evaluation is the schedule's next occurrence (§2b) computed by
the scheduler's own arithmetic, never catch-up eligibility: a definition owed
a make-up fire still shows the upcoming slot, not the owed one. It is also
never an unqualified future timestamp: disabled rows show `Disabled` (a
theoretical slot is labeled hypothetical), delivery rows show
waiting-for-deliveries, a missing cursor is never observed, and inspect
failures are unavailable. Last scheduler evaluation is the host-local
cursor; last minted task is the newest tagged instance and is labeled a
manual mint when the two ids differ. Enable/disable writes `enabled` through `auto_task_toggle`
with `expected_enabled` compare-and-swap, operator authorization
(`auto_task.toggle`), and a dashboard-operations audit row. `Mint now` calls
`auto_task_mint` after the operator acknowledges the unconditional warning
(`acknowledge_unconditional: true`); the request is refused without that
disclosure. All-workspace and inactive/unknown workspace selections stay
read-only. Refresh and hash navigation only GET — they never replay a toggle
or mint.

List responses expose separate `capabilities` decisions for auto-task toggle,
manual mint, routine toggle, clock service, and clock cadence. Each decision
uses the same governed operation as its POST endpoint; no client-side grant
is inferred from another action. These five operations currently all require
operator authority, while bounded-window submission has a separate policy.
The legacy `controls_authorized` field remains for existing API consumers.

One session-access explanation appears above Operations. Dashboard authority
comes from the **server process**, not from opening a browser or terminal.
For deliberate operator access, restart the server with
`ORBIT_OPERATOR=1 orbit web serve`, preserving its existing root, port, and
workspace options, then reload the page. No dashboard button grants authority.
Unavailable actions retain a keyboard-focusable explanation, including host
and workspace restrictions.

Toggle and mint controls remain pending through server readback. Failures stay
inline; mint success links to the created task in its originating workspace
and never dispatches delivery. The confirmation warns when an open duplicate
exists and preserves unconditional manual-mint semantics. Workspace changes
invalidate old controls, list responses, and feedback, including switching
away and back while a request is pending. The in-flight guard survives that
switch until the request settles; it is a UI duplicate-click guard, not a
server-side idempotency promise for manual mint. A failed readback preserves
the successful action result and task link, so a refresh failure does not
invite another mint.

Validation uses the shipped modules in the existing Node harness
(`cargo test -p orbit-web --lib operations_actions_preserve`). The same fixture
runs in Chromium with `node crates/orbit-web/src/tests/dashboard_operations_browser.mjs
/absolute/path/to/playwright/index.mjs /evidence/directory` (on one shell line).
The optional runner serves isolated markup, styles and mocked API responses,
checks behavior, and captures routine/auto-task panels at 1440px and 390px;
Rust API tests separately exercise the canonical handlers and persisted state.

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
  whose routine fired it, not a cross-workspace sweep. **[slated to change]** —
  the tick fans out over every registered owner checkout on the host; each
  checkout's definitions are still evaluated against that checkout's own store.
- **Repo-global chores run once per owner** (pending change). Everything the
  scheduler touches is host-local, so N owner checkouts of one repository are N
  independent schedules by design. A definition whose effect lands on the shared
  remote (a dependency bump, a release chore) is minted by each owner's clock;
  `skip_if_open` sees only the local store and cannot dedupe that. Such a
  definition must dedupe against the remote itself or must not ship as an
  embedded default.
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
