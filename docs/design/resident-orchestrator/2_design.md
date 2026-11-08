---
title: Resident Orchestrator — Design
owner: claude
last_updated: 2026-11-08
last_validated: 2026-11-08
status: Draft
feature: resident-orchestrator
doc_role: design
type: design
summary: "Roles, mandate and authority, resident lifecycle and fires, the bounded cycle, the manager channel, notices, escalation, widening metrics, migration and pilot, and the evolution loop"
tags: [resident-orchestrator]
paths: ["crates/orbit-core/src/application/resident/**", "crates/orbit-core/assets/jobs/**", "crates/orbit-core/assets/activities/**", "crates/orbit-types/src/workflow/automation/members.rs", "crates/orbit-types/src/task/decision/request/**"]
related_features: [routines, condition-triggers, activity-job, human-decision-channel, operator-attention, fleet-notices, mid-run-steering, decision-assistance, granted-tools]
related_artifacts: [ORB-21229, ORB-21263]
---

# Resident Orchestrator — Design

This document specifies how a resident orchestrator works and answers the nine design questions
of [ORB-21229]. Slices 1–9 ([ORB-21242] to [ORB-21250]) have landed, slice 3 ([ORB-21244], the cycle) as [§5.1](#51-slice-3-as-implemented) describes; slice 10, the pilot ([ORB-21251]), is approved (`backlog`). Exact record shapes and tool contracts are in the specs:
[mandate-and-authority](./specs/mandate-and-authority.md),
[checkpoint-contract](./specs/checkpoint-contract.md),
[manager-channel](./specs/manager-channel.md) and
[evolution-ledger](./specs/evolution-ledger.md). The question → section map:

| Question in ORB-21229 | Answered in |
|---|---|
| 1. The resident's lifecycle | [§4](#4-lifecycle) |
| 2. The checkpoint and resume contract | [§5](#5-the-cycle), [checkpoint-contract](./specs/checkpoint-contract.md) |
| 3. The channel between manager and resident | [§6](#6-the-manager--resident-channel), [manager-channel](./specs/manager-channel.md) |
| 4. Notices | [§7](#7-notices) |
| 5. Escalation | [§8](#8-escalation) |
| 6. Metrics before widening | [§9](#9-metrics-that-gate-widening) |
| 7. Migration from today | [§10](#10-migration-and-pilot) |
| 8. A pilot | [§10](#10-migration-and-pilot) |
| 9. The evolution loop | [§11](#11-the-evolution-loop), [evolution-ledger](./specs/evolution-ledger.md) |

## 1. Roles

| Role | What it is | Holds a loop? | Holds state? | Writes |
|---|---|---|---|---|
| Human | Daniel, the operator | — | — | Authority grants, decisions, approvals |
| Manager | An interactive session of any registered crew; the human's only conversational surface | No | No: everything it knows it reads from Orbit | Mandates, relays, control actions, notices |
| Resident | One per mandate; Orbit cycles, no process between them | No (the OS clock fires it) | Only in Orbit: mandate, checkpoints, task state | Checkpoints, dispatches and promotions within authority, escalations, cost events |
| Leaf executor | The existing drain and leaf pipeline | — | — | Unchanged |

The manager is a role, not a program. Orbit ships it an MCP role (`orbit mcp serve --role manager`)
and a skill reference
([ORB-21250]), not a process. Any session that loads them can act as manager, and two manager
sessions at once are safe because neither holds state: both read and write the same records. The
manager does four things for the human: files and scopes mandates; relays grants, decisions and
directions verbatim; posts fleet notices; and summarises residents on request or on the digest
cadence ([§11.4](#114-the-manager-digest)).

A resident owns **coordination only**. It has no worktree, no pinned drain slot and no admission
path of its own; leaf tasks it dispatches run on the normal pipeline under the normal footprint
rule. The epic-is-a-tag decision of [ORB-12488] stands
([4_decisions.md](./4_decisions.md#coordination-only-the-resident-owns-no-execution)).

## 2. The mandate

A mandate is an `[orchestration]` assignment task, the record constellation already keeps, with two
additions ([specs/mandate-and-authority.md](./specs/mandate-and-authority.md)):

- the `resident` tag, which makes it a resident's mandate;
- a versioned `resident/mandate.json` artifact: goal (the task description stays the narrative),
  exit criteria, deadline, covered workspaces, the tasks or tags it watches, crew, cadence floor,
  heartbeat and continuity preference.

The mandate's **authority** is a separate record on the same task, with its own write rule (§3).
Keeping it apart from the mandate means the manager can draft and edit a mandate freely while
authority changes stay human-only.

The assignment task's own lifecycle carries the resident's: `in-progress` while it runs, `review`
when it hands off, `done` with an execution summary. A resident never moves its own assignment past
`review`: closing a mandate is the human's call, like every other `review → done`.

## 3. Authority

Authority answers one question per action: may this resident do this, here, now? Core answers it,
not the prompt.

- **Rights.** `dispatch` (start or requeue leaf runs and drains), `promote` (move a `proposed` task
  to `backlog`, where the drain can admit it), `merge` (land a reviewed PR), `plugin_upgrade`
  (install or upgrade a workspace plugin). Task-pilot assessment is a separate gate: it can assess
  eligible `proposed` or `backlog` tasks. Reading, commenting, filing `proposed` tasks and friction,
  and checkpointing need no right. Anything not listed (releases, tags, publishing, host changes,
  config edits, secrets) is not grantable to a resident in v1.
- **Scope.** A list of covered workspaces. A tool call naming another workspace is refused.
- **Enforcement.** The cycle's tool grant is computed from the authority at cycle start and frozen
  for the cycle, like a dispatched attempt's [granted tools](../granted-tools/1_scope.md). A right the
  mandate lacks is a tool the cycle cannot call; a workspace outside scope fails the call in core.
  Core also refuses `resident.*` writes whose cycle id is not the mandate's live cycle.
- **Who widens.** Only the human. The operator at a native terminal writes a grant directly
  (`orbit resident grant`); any MCP session, even one served `--operator`, counts as a relay. The
  manager, which is an agent session, submits the same input and gets a
  **grant request** instead: a decision request on a new `resident_authority` gate that quotes the
  human's words verbatim, names the relaying session and lists the exact rights added. The authority
  changes only when the human answers it through the decision channel (dashboard, CLI or a
  registered channel such as email). The manager therefore cannot widen authority on its own, and a
  fabricated or misread relay costs one refused decision, not a merge.
- **Widening from the dashboard.** The dashboard Residents view ([§6.1](#61-the-dashboard-path)) widens
  only when its server runs `--operator`, through the governed operation `resident.write`. It does
  not grant like a native terminal. It raises the `resident_authority` request with the operator's
  quote, naming the dashboard as the relaying session, and answers it `grant` as the human in the
  same action. So the change is recorded `via: decision:<id>`, and the request, the answer and a
  `dashboard.operations` audit row all name it. A dashboard served without `--operator` cannot
  widen, narrow or answer a `resident_authority` request (403 `authorization_denied`). Agents and
  MCP sessions are unchanged: they only raise requests.
- **Who narrows.** Anyone who can write the assignment: the human, the manager, and the resident
  itself. Removing a right, dropping a workspace, shortening the deadline, pausing and stopping are
  all narrowing. Widening is gated and narrowing is free
  ([4_decisions.md](./4_decisions.md#narrowing-is-free-widening-needs-the-human)).
- **What always goes to the human.** Product scope, architecture, security, material cost, and
  anything beyond the mandate. [§8](#8-escalation) lists the classes.

## 4. Lifecycle

### 4.1 Creation

1. The human tells the manager what they want ("finish the scribe v2 slices; you may dispatch and
   promote in orbit-office, not merge").
2. The manager files or reuses the `[orchestration]` assignment in the owning workspace with the
   `resident` tag and writes `resident/mandate.json` (goal, exit criteria, deadline, covered
   workspaces, watched tasks, crew, cadence).
3. The manager submits the authority grant with the human's words verbatim. From an MCP session
   that always raises a `resident_authority` decision request, and the human answers it.
4. The mandate becomes `active` when it has a valid mandate artifact and its assignment is open
   (tagged `resident`, control active): a grant is not an activation prerequisite. Its
   first fire follows on the next clock tick.

From the dashboard, steps 2 and 3 are one start flow in the Residents view
([§6.1](#61-the-dashboard-path)): it creates the tagged assignment with its
`resident/mandate.json`, widens it with the human's words as an answered request, and turns the
workspace's cycle routine on when it is off. These are three separate writes; a refusal names the
step and says what already exists.

Granted rights authorize action, not activation. A mandate without granted rights still runs
observe-only cycles by default. That is useful for a resident that only watches and reports.

### 4.2 Fires

Residents ride the existing clock: there is no resident daemon
([4_decisions.md](./4_decisions.md#residents-ride-the-clock-no-resident-daemon)). One seeded,
default-disabled routine per owner workspace, `resident-cycle-<workspace>`, uses a new typed state
trigger kind, `resident_due`. Its observation rule, deterministic and model-free like
`preparation_eligible`, evaluates every active mandate whose assignment task lives in that
workspace and marks it due when:

- its **wake digest** differs from the digest in its last checkpoint; or
- its **heartbeat** has elapsed since the last checkpoint (default 2 h); or
- a control action asks for a cycle (`wake`, `resume`, `release`);

and in every case only when the **cadence floor** has elapsed since the last cycle started (default
10 min, minimum 5 min) and no cycle is live.

The wake digest is a hash over the facts that can change what a cycle should do:

| Event | Observed from |
|---|---|
| A watched task's status, revision or pilot assessment changed | task store |
| A watched task's run finished, failed or stalled; its PR or CI state changed | run store, the existing PR/CI evidence |
| A decision request the resident raised was answered or expired | decision requests |
| A notice in a covered workspace opened, changed or closed | notice store ([fleet-notices](../fleet-notices/1_overview.md)) |
| A new comment on the assignment not written by the resident (a relay, an operator note) | assignment history |
| The mandate or authority changed | assignment artifacts |

Watched tasks are the mandate's explicit list plus the tasks it dispatched or filed (from its
checkpoints), capped at 200 per mandate. Above the cap, the observation hashes only status and
revision.

Waiting costs one store read per mandate per tick and starts no agent. That is what "waiting is a
cheap no-op" means here.

Why a typed kind and not a [condition trigger](../condition-triggers/1_scope.md)? A condition fires
once per subject edge of a predicate. A resident needs "anything in this set changed since my last
checkpoint", with the checkpoint as the cursor, plus a heartbeat and a floor. That is one
observation rule, not an expression. Workspaces may still add condition-triggered routines that call
`orbit resident control wake`, for situations the digest does not cover.

### 4.3 Cadence floor

The floor bounds cost: at the default 10 min a resident runs at most 144 cycles a day, and in
practice far fewer, because cycles start only on change or heartbeat. The floor cannot go below the
clock cadence or 5 min. A busy mandate whose digest changes on every tick runs once per floor
interval and sees all accumulated changes together.

### 4.4 Ending

A mandate ends when one of these is recorded, and the cycle that sees it writes a final checkpoint
with `outcome: ended` and the reason:

- **exit criteria met**, judged by the resident and verified against task state where the criterion
  is checkable (all watched tasks `done`, for example). The resident writes an execution summary
  and a `handoff` checkpoint; `resident_finalize` moves the assignment to `review` and the human
  closes it.
- **deadline passed**. The resident stops dispatching and writes the same handoff.
- **human stop** (`orbit resident control stop`, through the manager or directly). The stop then
  hands the assignment to `review`.
- **takeover**. The human or an interactive session takes the mandate; firing stops until `release`
  ([§6](#6-the-manager--resident-channel)).

Pause and stop prevent a new cycle from starting. A live cycle keeps its frozen grant, completes,
checkpoints (or receives a synthesized checkpoint from `resident_finalize`), and exits at the next
cycle boundary, within at most one agent budget.

### 4.5 Handoff to the human

A handoff is the final checkpoint plus the assignment's execution summary: what was done, what is
open, which decisions are pending, which costs recurred. Core, not the resident, sets `review`: the
cycle's own run owns the task, so `resident_finalize` settles the `handoff` checkpoint, and
`resident_observe` settles one whose finalize never ran. Reopening the assignment resumes its cycles.
While the mandate is active core keeps the assignment `in-progress`, set when the mandate is created
or attached and at the latest when a cycle starts: `backlog` would offer it to drains. The
manager surfaces it in the next digest and `orbit attention` lists the review as `review_waiting`.

## 5. The cycle

A cycle is one run of `resident_cycle_pipeline`, a catalog job with three steps:

1. **`resident_observe`** (deterministic). Takes the cycle lease (a fenced lease keyed by the
   mandate; a held lease defers the fire). Loads the mandate, effective authority, the last *K*
   checkpoints (default 5), open requests and their answers, the wake reasons and their facts, the
   notices in scope, and the workspace's standing goals. Computes the cycle's tool grant
   ([§3](#3-authority)), narrowed to observe-only when a maintenance notice is open
   ([§7](#7-notices)). If the previous cycle has no checkpoint, it marks this cycle a **recovery
   cycle** and adds the dead cycle's audit rows (actions taken under its action keys) to the brief.
2. **one `agent_loop`** (the crew from the mandate). Does one bounded cycle of work: check, recover,
   dispatch, promote, merge within the grant; escalate what it must not decide; record costs; and
   write its checkpoint with `orbit.resident.checkpoint`. The loop has a turn and wall-time budget
   (default 30 min); it is coordination, so it should rarely need it.
3. **`resident_finalize`** (deterministic). If the agent wrote no valid checkpoint, writes a
   synthesized one with `outcome: incomplete`, the actions found in the audit log under this cycle's
   action keys and the wake digest it observed, so the next cycle neither repeats nor loses them.
   Releases the lease.

**Durable artifacts are the system of record.** The next cycle reads checkpoints, task state,
decision records and the audit log, never the previous agent's final message. The job declares no
response envelope ([4_decisions.md](./4_decisions.md#a-cycle-is-correct-from-orbit-state-alone)).

**Idempotence.** Every mutating action in a cycle carries an action key
`resident:<mandate>:<cycle>:<n>`, recorded in the checkpoint. Orbit already dedupes other keys, but
none of them resolves a resident key from the cycle's mutating tools today:

- **Action keys on admission.** Run submission resolves an automation attempt's key to an existing
  run before submitting, and task creation reserves it. Landing attempts
  (`landing:{handoff}:{attempt}`) use the same run-submission path, and visual follow-up tasks
  (`visual-follow-up:…`) reserve their keys in task creation.
- **Guarded `orbit.task.add` with `request_id`.** Creation reserves `desktop-create:{request_id}`,
  and a repeat with the same payload returns the existing task. The guarded create takes no
  relations or tags. The dashboard proposal path dedupes `dashboard-create:{request_id}` the same
  way.
- **Guarded `orbit.task.update` with `request_id`.** Edit, comment and review each dedupe on a
  receipt in task history. A promote is a status edit, so its only caller-supplied key is this
  `request_id`.


Both `request_id` forms accept only 1 to 128 ASCII letters, digits, `.`, `_` or `-`, so the `:` in a
resident key is refused. Dispatch, requeue and promote do not accept a resident key on any
agent-callable tool (`orbit.task.requeue` takes `id`, `reason` and `discard_candidate` only), and an
ordinary `orbit.task.add` without `request_id` passes no action key. Slice 3 ([ORB-21244]) therefore takes
the other route: core assigns the key. The cycle's tool guard numbers every write and records it in
an action ledger, and a recovery cycle's identical retry gets the recorded result
([§5.1](#51-slice-3-as-implemented)). No mutating tool takes a caller-supplied resident key.

**Crash recovery.** A killed cycle leaves a held lease that expires (lease TTL = agent budget + 5
min, 35 min by default from lease acquisition) and no checkpoint. The next cycle cannot start
before expiry. With the default 10 min cadence floor, the floor has elapsed by then; if a wake digest
is pending, recovery starts on the next eligible clock tick (60 sec default). Otherwise the next
heartbeat makes the unchanged mandate due within 2 h of its last checkpoint. The finalize step of the
killed cycle never ran, so the next observe treats it as described above. Recovery therefore waits
for the remaining lease TTL and may wait longer for a due heartbeat or eligible clock tick; it is not
bounded by one floor interval. Recovery cost is measured ([§9](#9-metrics-that-gate-widening)).

**Checkpoint and resume contract.** The checkpoint carries the cycle id, the wake reasons and digest,
actions with keys and results, decisions taken with their class and reversibility, open requests,
cost events, a short `notes` for the next cycle, the next planned wake, and the outcome. The
`resident.request` and `resident.answer` shapes cover escalated questions and manager relays. An
expired provider conversation reconstructs from the mandate, the authority, the last *K* checkpoints,
open requests with answers, wake events and notices: the same brief every cycle gets. Continuity, a
resumed conversation, is an adapter that adds context and never changes correctness. Full schemas:
[specs/checkpoint-contract.md](./specs/checkpoint-contract.md).

**Where it runs.** On the owner host of the assignment's workspace, as owner-only automation, like
every state routine. v1 requires every covered workspace to be owned by the same host; a covered
workspace on another owner is refused at grant time ([3_vision.md](./3_vision.md#1-open-questions)).
Slice 1 approximates that check: a granted workspace must be the assignment's own or registered in
the granting host's workspace catalog
([mandate-and-authority](./specs/mandate-and-authority.md#widening)).

**Provider-neutral.** The cycle is an ordinary `agent_loop` with a crew from the mandate, so any
registered crew can run a resident. Provider-specific continuity sits behind the adapter in
[ORB-21246].

### 5.1 Slice 3 as implemented

Slice 3 ([ORB-21244]) ships the fire, the cycle job and the tool guard. Where it differs from the
sections above, this subsection is the authority.

**Fire.** The seeded `resident_cycle` routine (`resident-cycle-<workspace>`, disabled, owner-pinned
like every state routine) uses the `resident_due` state kind with `batch_size: 1` (the kind refuses
any other size). Each tick it reads every `resident`-tagged task in `proposed`, `backlog`,
`in-progress` or `blocked` whose mandate is `active`, and computes:

- the **wake digest**: a sha256 over the mandate, the authority revision, each watched task's id,
  status and `updated_at`, the decision requests on the assignment and watched tasks (id and state),
  the in-scope notices (id, revision, status) and the wake comments. Wake comments are every
  `resident.answer`, and every other non-resident comment except, while a cycle is live, those
  written by the cycle itself (only `human*` writers count inside a live window);
- the **reasons**: `first_cycle`, `digest_changed` (against the last checkpoint's digest),
  `heartbeat` (since the last cycle *start*, or task creation), `deadline` (once, when the deadline
  passed after the last cycle started) and `recovery:<cycle>` (a dead live cycle);
- **withheld**: `cycle_live` (a live cycle that is not dead) or `cadence_floor` (the floor since the
  last cycle start has not elapsed).

A due mandate becomes a member whose fingerprint is `after:<last cycle>|<digest or heartbeat>`, with
its debounce clock at the last cycle start, so it is admitted on the first tick the floor allows.
Admission re-evaluates with the observation's clock and retires a member whose reason changed. A
cycle run settles its member once it stops: applied if it started its cycle, failed (and retried)
otherwise. A control-action wake (`wake`, `resume`, `release`, [ORB-21245]) written after the last
cycle start makes the mandate due with reason `wake` (`status_request` too when asked), as does a
comment on the assignment; `pause`, `takeover` and `stop` stop it firing.

**Lease.** There is no separate fenced lease: the live-cycle record (`resident_cycle_changed`,
compare-and-set) is the lease, with `expires_at` 40 minutes after observe (the agent step's 30 min
wall clock plus margin). A live cycle is dead when it expired, or its run is terminal with a stopped
owner, or the run is gone. Cycle ids are `rcy-<run id>`, so a retried member never reuses one.

**Observe.** `resident_observe` re-checks due state under the run's own clock and either skips
(`cycle_count: 0`, with `skip_reason`), or synthesizes the dead predecessor's `incomplete` checkpoint,
publishes the live cycle and records a `resident_cycle_granted` history entry: the frozen grant
(`rights`, `workspaces`, `observe_only`, `narrowed_by`, `recovery_of`, `digest`, `reasons`). An open
maintenance notice in a covered workspace (or the global scope) empties the rights with
`narrowed_by: notice:<id>`; a passed deadline does the same with `narrowed_by: deadline`. An
unreadable notice store narrows too. The brief carries the mandate, the grant, the last 5
checkpoints, open requests, comments since the last cycle, the wake, the notices (title and body as
`untrusted_*`), and for a recovery cycle the dead cycle's ledger rows. Standing goals and the skill
reference arrive with [ORB-21250]. A resumed run that already holds its cycle re-enters it.

**Finalize.** `resident_finalize` writes the synthesized `incomplete` checkpoint when the agent wrote
none (its `actions[]` from the ledger rows under the cycle's keys) and clears the live cycle. A cycle
whose finalize never ran is closed the same way by the next observe.

**Tool guard.** Core, not the activity's tool list, scopes the cycle. A tool call carries the run id
from the trusted managed-run binding, never from input. For a `resident_cycle_pipeline` run, reads
pass; every write needs this run's live, unexpired cycle (else `resident_cycle_stale`), every
`workspace`/`workspaces` input must resolve to a granted workspace or the assignment's own (else
`resident_scope_refused`), and a right-gated write needs its right (else `resident_right_refused`,
naming the observe-only cause when narrowed). The rights mapping is
[mandate-and-authority](./specs/mandate-and-authority.md#rights--tools). A cycle cannot move a task
to `review` (`resident_handoff_by_checkpoint`): it hands off with a checkpoint
([§4.5](#45-handoff-to-the-human)). Everything else that writes and is not in the mapping is refused as `resident_tool_refused`. Lint without restoration remains a read;
`orbit.task.lint` with `restore_pruned: true` is refused for both a named task and a sweep, including
through native `orbit task lint --restore-pruned`. Context maintenance has no resident right;
operator restoration outside a cycle is unchanged.

**Action ledger.** Each admitted write (except the checkpoint itself) is keyed
`resident:<mandate>:<cycle>:<n>`, numbered under the assignment's write lock. A `resident_action`
history row with phase `started` precedes the call, and a row with `ok`, `failed` or `refused`
follows it; the response carries `resident_action_key`. In a recovery cycle, a call with the same
tool and input digest that the dead cycle recorded `ok` returns
`{"resident_action": "already_done", "key", "ref", "subject"}` without running; one the dead cycle
left `started` is refused as `resident_action_in_doubt` for the agent to check by reading.

**Deviations from §4–§5.**

- Wake reasons are coarse: `digest_changed` does not say which fact changed; the brief carries the
  facts and the agent compares.
- PR and CI state are seen only through watched task revisions (`updated_at`), not read from the PR
  or CI evidence directly.
- Writes from a resident run that this workspace's runtime does not hold (a cycle run in another
  catalog workspace) are refused as `resident_scope_refused`: cross-workspace writes through another
  workspace's MCP server are not supported in this slice.
- `plugin_upgrade` maps to no builtin tool yet; `orbit.task.update` with `approve` or `force`,
  `orbit.notice.close` and `orbit.resident.grant` are refused to a cycle.
- The heartbeat counts from the last cycle start, not the last checkpoint, so a cycle that never
  checkpoints still cannot fire faster than the heartbeat.

## 6. The manager ↔ resident channel

The channel mostly composes existing records. It adds the grant, relay and control writes and the
`show`/`list` reads, all in a `resident` MCP toolset that the `manager` and `resident` MCP roles
serve ([specs/manager-channel.md](./specs/manager-channel.md)):

| Manager wants to | Mechanism | New? |
|---|---|---|
| Delegate a mandate | `orbit.task.add` / `orbit.task.update` for the assignment, plus `orbit.resident.grant` for authority | `orbit.resident.grant` (S1) |
| Relay a decision or direction verbatim, with provenance | `orbit.resident.relay`: a `resident.answer` comment with the human's words, `relayed_by`, the source (session, channel, time) and, when it answers a request, the request id. A wake event | New tool, existing comment store |
| Relay an authority grant | `orbit.resident.grant` → a `resident_authority` decision request the human answers | New gate kind |
| Answer a resident's escalation | The human answers the decision request directly (dashboard, CLI, email); the manager only reminds | Existing `orbit.decision.*` |
| Ask for a status update | Read the latest checkpoint (`orbit resident show`); if it is staler than the human wants, `orbit.resident.control wake --status-request` forces one cycle | `orbit.resident.control` |
| Stop, pause, take over | `orbit.resident.control pause|resume|stop|takeover|release` | `orbit.resident.control` |
| Post a fleet notice | `orbit.notice.post` ([fleet-notices](../fleet-notices/specs/notice-tools.md)) | Existing |
| See everything waiting on the human | `orbit attention`, filtered to the mandates' workspaces | Existing, plus kind `resident_stalled` |

The resident reports back only through Orbit: checkpoints and the execution summary on the
assignment, decision requests for escalations, and `orbit attention` items. Its escalations are
`decision_open` items, its handoffs `review_waiting`, and a resident with no checkpoint for twice its
heartbeat, or three incomplete cycles in a row, is a new `resident_stalled` kind
([operator-attention](../operator-attention/specs/attention-item.md)). The manager reads those; it
needs no inbox of its own.

A relayed direction is steering within authority, weighed like task text, with the latest relay
winning over earlier ones ([mid-run-steering](../mid-run-steering/1_overview.md) is the leaf-level
precedent). A direction cannot change authority: `orbit.resident.relay` refuses input that names
rights or workspaces, and a direction the resident reads as a widening goes back as an escalation.

### 6.1 The dashboard path

The dashboard's Automation › Residents view is the human's own surface for the same records, so a
mandate can be set up and steered without a manager session ([ORB-21649]). Every read and write goes
through the `orbit.resident.*` tools, and every write also leaves a `dashboard.operations` audit row
under the governed operation `resident.write` (operator session only):

| View action | Route | Mechanism |
|---|---|---|
| List mandates, ended ones included, with state, stalled flag, effective rights and workspaces, deadline, open requests, live cycle and latest checkpoint | `GET /api/residents?all=true` | `orbit.resident.list`, then `orbit.resident.show` per mandate |
| Show one mandate, with its grant and narrow history and the latest checkpoint's wake digest | `GET /api/residents/{id}` | `orbit.resident.show` |
| Create a mandate | `POST /api/residents` | One action: the `resident`-tagged assignment and its `resident/mandate.json`. The draft is checked first by the rule every mandate read applies (the `resident.mandate/v1` parser plus a configured crew), and a refused draft returns that message and writes nothing |
| Widen | `POST /api/residents/{id}/grant` with rights or workspaces to add and a quote | The request-and-answer of [§3](#3-authority), recorded `via: decision:<id>` |
| Narrow | the same route with rights or workspaces to remove | `orbit.resident.grant` |
| Answer a `resident_authority` request another caller raised | `POST /api/decisions/{id}/answer` | The decision channel; operator session only for this gate |
| Pause, resume, wake with a status request, take over, release, stop | `POST /api/residents/{id}/control` | `orbit.resident.control` |
| Relay a direction | `POST /api/residents/{id}/relay` | `orbit.resident.relay`, source `via: dashboard` |
| Enable or disable the workspace's resident cycle routine | `POST /api/routines/toggle` | The existing routine toggle, audited like other operator toggles |

The dashboard runs the tools with its own session envelope (Agent, plus Operator when served
`--operator`), never the terminal it was started from. So the tools' own authority rules apply
unchanged.

The start flow offers the assessed complexity tiers `low`, `medium`, `hard` and `xhard`;
the API validates the submitted tier. Repeating a widening whose rights and workspaces are
already held reports unchanged authority and records no grant request. Applied widening is
reported as an answered request; a combined deadline change reports its separate outcome,
including a refused narrowing after an applied or unchanged widening.

**Takeover.** `takeover` sets the resident to `taken_over`: no fires until `release`. The interactive
session works in the same assignment task and comments as usual. On `release`, the next cycle reads
those comments as new non-resident comments and resumes from them; a takeover needs no export or
import.

**Role-scoped MCP surfaces.** Each session serves only its role's surface, through an MCP role
rather than one more add-on toolset ([federated-mcp specs/mcp-roles.md](../federated-mcp/specs/mcp-roles.md),
[ORB-21263]). Every tool belongs to one capability toolset, and `orbit mcp serve --role <role>` (or the
global `machine.mcp.role`) serves the role's toolsets; `--toolset` adds to it. A role grants no
authority: `--operator` still decides operator capability. Today `--role manager` serves task and run
reads, decisions, notices and search, and `--role resident` adds task writes and dispatch for the
bounded cycle. Both also serve the `resident` toolset, which [ORB-21242] introduced with
`orbit.resident.grant`, `show` and `list`; [ORB-21245] added relay and control for the manager, and
later slices the resident's own checkpoints for the cycle. Without a role a server still
serves the pre-role default surface. Daniel (2026-11-02): "mcp surfaces are getting too large lately."

## 7. Notices

Fleet notices ([ORB-21207]) are quoted into every managed agent's brief, which includes a resident
cycle. The resident design adds rules on top; the notice record and poster classes are unchanged.

- **Who posts.** Both, under the existing poster classes. The manager posts as `orchestrator` (or as
  `operator` when the session holds operator authority): `maintenance` and `global` notices, and the
  human's broadcasts. A resident cycle is a run-bound session, so it posts as `agent`: only
  `workspace`-scope `incident` or `info` notices, about what it observed, with a linked task or
  evidence, for at most 24 hours ([notice-authority](../fleet-notices/specs/notice-authority.md)).
  That is enough for "the review gate refuses every candidate, fix ORB-N in flight", and keeps
  residents from broadcasting fleet-wide instructions. No new poster class.
- **Reacting to a maintenance notice.** While a `maintenance` notice is open in a covered workspace,
  or a global one, `resident_observe` narrows the cycle's grant to observe-only: no dispatch,
  promote, merge or plugin upgrade. The cycle still checkpoints, comments and escalates. The notice's
  close is a wake event, so the resident resumes on the next tick after it. A notice can narrow a
  resident; it can never widen one ([4_decisions.md](./4_decisions.md#narrowing-is-free-widening-needs-the-human)).
  This is the resident's own policy reading notice data. Notices still never hold the leaf drain in
  v1.
- **Orbit binary update (polaris RB-15).** The operator or manager posts a global maintenance notice
  before the lockstep deploy. Residents go observe-only and stop starting work, but leaf runs already
  in flight are not cancelled. Because a resident has no process, the first cycle after the notice
  closes runs under the new binary with no restart step. A cycle that is live when its binary is
  replaced finishes on the old process and checkpoints. The next one, if it is a recovery cycle,
  reads that checkpoint under the new binary's schema, so the checkpoint record follows the
  state-compatibility rules (additive fields, versioned schema).
- **Notices as wake events.** Any notice opened, changed or closed in scope changes the wake digest.
  An `incident` notice about a watched task's failure is the usual reason for an early cycle.

## 8. Escalation

The note's rule is: ask when `P(wrong branch) × remaining rework > question cost + delay`. Nobody
can compute those terms, so the design uses two layers: fixed classes that always escalate, and a
rubric for the rest.

### 8.1 Classes that always escalate

| Class | Examples |
|---|---|
| Product scope | Dropping, adding or reshaping a deliverable; changing acceptance criteria the human wrote |
| Architecture | A new crate dependency, a new persisted record, a public contract change, an ADR-level choice |
| Security | Anything touching auth, sandboxing, secrets, a `security` finding's disposition |
| Material cost | Raising concurrency or crew tier beyond the mandate, a spend the workspace's cost policy marks material |
| Irreversible | Force pushes, deleting branches or records, releases, tags, publishing |
| Authority | Any action outside the grant, including one a relay seems to authorise |
| Cross-mandate conflict | Two mandates want the same files or contradict each other |
| Human-reserved | A merge or approval the workspace reserves for the human, even when `merge` is granted |

These map onto the decision record's existing per-option `risk_tier`: `irreversible`, `authority`
and `spend` always escalate, and `routine` and `reversible` go through the rubric.

### 8.2 The rubric for the rest

For a decision outside those classes the resident estimates, in its checkpoint:

- **P(wrong)**, as a bucket (low, medium, high), raised one step when there is no precedent (no
  matching decision in the decision-assistance corpus, [decision-assistance](../decision-assistance/1_scope.md))
  and when this category has a recorded reversal in the window ([§9](#9-metrics-that-gate-widening)).
- **Remaining rework**: how much work follows the choice before anyone would notice it was wrong,
  counted as the leaf tasks it would dispatch or block (from relations) times their complexity
  weights (low 1, medium 2, hard 4, xhard 8).
- **Question cost + delay**: the decision channel's measured median answer time for this workspace,
  plus a fixed attention cost per question (default one leaf-unit), raised when the human's open
  question count is above the daily budget.

Then:

1. A reversible choice with rework ≤ 2 units: **decide**. Record it in the checkpoint as a
   `decisions[]` entry marked `reversible` and continue. The digest lists such decisions for veto.
2. P(wrong) high, or rework > 8 units with no precedent: **escalate**, and keep working on whatever
   the question does not block. A question is non-blocking by default.
3. Otherwise: decide, record, and add it to the digest as a decision awaiting veto.

The thresholds are workspace config (`[resident.escalation]`), not constants in core. The rubric is
an approximation and is tuned by the reversal and escalation metrics, not argued about. If the
reversal rate in a category rises, its P(wrong) rises; if the human answers almost every question
with the recommended option, the residents are over-asking.

### 8.3 How a resident escalates

It composes existing tools. The resident files a human-crew task in the assignment's workspace,
`related_to` the assignment and `blocked_by`-linked from any work it must wait for, and raises a
multiple-choice decision on it with `orbit.decision.raise`: two to four options, one recommended,
each with a `risk_tier`. The human's answer completes that task, which unblocks the dependents and
changes the resident's wake digest. Agent-raised requests are already deduplicated and capped per
filer and per workspace, which bounds escalation load. The checkpoint's `requests[]` lists the
questions and the next cycle reads their answers.

## 9. Metrics that gate widening

Widening means going from `1:1:many:many` to `1:many:many:many`: several managers, for example one
per domain. These four measures gate it. All are derived from records that already exist, or from
the checkpoints above, by `orbit resident metrics` ([ORB-21247]); none needs a new store.

| Measure | Definition | Measured from |
|---|---|---|
| Manual interventions per mandate | Human writes on the mandate's assignment or watched tasks that are not answers to a resident escalation and not grants: requeues, cancels, manual status changes, operator steering comments, unprompted relays, `stop`/`takeover`. Reported per mandate and per active mandate-day | Task history writer labels, audit rows, `resident.answer` comments without a `request_id` |
| Escalation load on the human | Resident-raised decision requests per mandate-day and per human-day across all mandates; median answer latency; share answered with the recommended option; expired-unanswered count | Decision requests filed by resident runs |
| Reversal rate | Share of resident decisions and actions later undone by a human within 7 days: a promoted task sent back to `proposed` or rejected, a reverted merge, a requeue undone, a relay with `reverses: <decision>`, a vetoed digest decision | Checkpoint `decisions[]` and `actions[]` with action keys, joined to later human writes on the same subject |
| Recovery cost of a crashed cycle | Cycles that ended without an agent checkpoint; time from the dead cycle's start to the next good checkpoint; recovery cycle slot minutes; actions repeated (must be 0: slice 3's action ledger answers a retried completed action); human interventions needed | Cycle leases, `outcome: incomplete` checkpoints, run records, action keys (slice 3, [ORB-21244]) |

**Baseline.** `orbit resident metrics --baseline <assignment>` computes the same measures, where
they are derivable, from an interactive orchestration task's history. The first baseline is
[ORB-21178]. Interventions and escalations are partly derivable from its comments. Reversals and
crash recovery are mostly not: a chat loop that died left no record, which is part of the point.
Measures that cannot be derived report `not_derivable`, never zero.

**As implemented ([ORB-21247]).** `orbit resident metrics [--mandate <task>] [--since <window>]
[--baseline <task>]` (tool `orbit.resident.metrics`, read-only; `--since` takes a span such as
`7d`, `24h` or `2w`, or an RFC 3339 instant, default `30d`) reports per mandate and, without
`--mandate`, across mandates with workspace totals. A resident-raised escalation is a decision
request named by a checkpoint `requests[]` or a `resident.request` record, or an agent-raised
request on a task related to the assignment. A reversal is counted once per checkpoint decision
or completed action: a `resident.answer` with `reverses` naming the decision id or action key, or
a human status write on the action's subject to `proposed` or `rejected` (or a revert), within
7 days of the checkpoint. Recovery cost counts `outcome: incomplete` checkpoints, measures to
the next non-`incomplete` checkpoint from the dead cycle's recorded start, sums recovery-cycle
time to its checkpoint as slot minutes, and counts a completed dead-cycle action run again as
`ok` (rather than the ledger's `already_done`) as a repeat. A baseline task with no checkpoints
reports reversals and recovery as `not_derivable`.

**References outside the assignment's workspace ([ORB-21642]).** Run the report in the
workspace that owns the mandate or baseline task. Its references (watched and related tasks,
checkpoint action subjects and request tasks) often live in other workspaces: an orchestration
baseline sits in `ws_constellation` while its work sits in the owning repositories' workspaces.
Each reference not in the assignment's workspace is read through its registered owner on the
same machine, read-only, by the same route that resolves cross-workspace dependencies. Its
history counts toward interventions and reversals, and its decision requests count toward
escalations. Its comments are not read: no cross-workspace comment read exists. These
references are listed under `references.foreign`, with the owning workspace and what was and
was not read. A reference that no workspace registered on the machine holds is listed under
`references.unreachable` with a reason (`missing`, `foreign_authority` or `invalid_id`) and
skipped. The rest of the report is still produced, so one foreign id no longer fails the
whole report with `task_not_found`.

**The widening bar** (proposed, decided by the human at the pilot's go/no-go):

- interventions per mandate no worse than the baseline, and falling over the pilot;
- at most 2 resident escalations per mandate-day and at most 8 per human-day;
- a reversal rate at or below 10 % over at least 30 resident decisions;
- every crashed cycle recovered within one heartbeat with zero repeated actions (via the slice 3 action ledger) and no human step.

## 10. Migration and pilot

### 10.1 What changes for the `orchestration-session` skill

The skill lives in constellation, not in Orbit; this section states what the Orbit design implies
for it. The constellation change is the operator's to schedule, after slice 9.

- The `[orchestration]` assignment task stays the system of record. It gains the `resident` tag, a
  mandate artifact and an authority record. The skill's "Authority limits" prose becomes the
  authority record, written through a grant.
- The wake loop in the on-call procedure, together with its self-scheduled wakeups, monitors and ssh
  polls, becomes the resident cycle. What one wake did becomes the resident-cycle procedure Orbit
  ships ([ORB-21250]). Constellation-specific know-how (hosts, RB-15, repo lists) stays in
  constellation's skills, which the resident's crew loads as it does today.
- "Comment every decision" becomes the checkpoint's `decisions[]` plus prose comments for major
  ones. The decision log is the checkpoint stream.
- "Treat every manual intervention as a defect signal" becomes the cost events and evolution records
  ([§11](#11-the-evolution-loop)), so the signal is counted, not just written.
- The scribe keeps indexing: a resident's major decisions get scribe lines citing the assignment, as
  an interactive session's do.

### 10.2 Interactive orchestration sessions

They stay, as an **override** and as a **probe**, not as the default:

- **Override.** The human, or the manager on the human's behalf, can `takeover` any mandate and run it
  interactively, then `release` it. That covers incidents, delicate releases, and anything the
  resident escalates more than it decides.
- **Probe.** Deliberate interactive runs keep a baseline and catch what no metric shows
  ([§11.5](#115-deliberate-interactive-probes)).
- **Default.** New mandates start as residents once the pilot clears the bar. A session that opens an
  orchestration without a resident is not refused; the skill points it at the manager.

The manager's own session is not orchestration: it holds no loop. Closing it loses nothing.

### 10.3 The pilot

- **Mandate.** The remaining orbit-office scribe v2 slices that [ORB-21178] orchestrates
  interactively today: bounded, mostly leaf delivery, one workspace (ws_orbit-office), and a live
  baseline to compare with.
- **Authority.** `dispatch` and `promote` in ws_orbit-office; no `merge` and no `plugin_upgrade` for
  the first week. `merge` is added through a grant if the reversal rate allows.
- **Exercises.** One deliberately killed cycle, one maintenance notice during the mandate, and one
  takeover and release.
- **Success bar.** The mandate reaches its exit criteria or deadline with no orchestrator chat session
  open, and the human reaches it only through a manager session. The metrics meet the widening bar
  in §9 against the [ORB-21178] baseline. Every exercise is recovered as designed.
- **Owner.** [ORB-21251].

## 11. The evolution loop

Daniel (2026-11-02): once orchestrator sessions are abstracted away, "whatever is happening in that
box stay the way they are. sure, friction reporting fixes issues that arise; but the system itself
will stay static. no innovation." Friction is shaped for repairs. Improvements came from a human
feeling pain summed across sessions, and a resident that works around a gap hides it. The loop below
keeps the pain signal; the record shapes are in [specs/evolution-ledger.md](./specs/evolution-ledger.md).

### 11.1 Cost as data

Every checkpoint carries `costs[]`: one entry per workaround, manual step, retry or wait the cycle
spent, with what it did, what it cost (minutes, retries, slot minutes) and what it wished existed.
Each entry carries a normalised **signature**, a short kebab-case key of the gap
(`manual-pilot-hold-clear`, `drain-relaunch-after-binary-update`), chosen from the open evolution
records when one fits. A cycle with no costs records an empty list, not nothing. Interactive
sessions record the same shape through the skill, so probes and residents share one ledger.

### 11.2 Evolution, not repair

An **evolution record** is a record kind of its own, separate from friction: one open record per
signature per workspace, saying "I keep doing Y by hand; Z would remove it". Cost events with a
signature corroborate it, so the record carries occurrence count, distinct mandates and residents,
and summed cost. Friction stays what it is: something broke or an assumption was contradicted, and a
repair resolves it. A cycle that hits a breakage files friction; a cycle that hits a missing
capability records cost against an evolution signature. Both can apply to one event
([4_decisions.md](./4_decisions.md#evolution-records-are-separate-from-friction)).

### 11.3 The evolution pass

Removed in [ORB-22844]: the routine, `evolution_pass_pipeline` and its rank, draft and apply
activities are gone, and nothing files evolution proposal tasks automatically. The ledger and the
digest (§11.4) stay. This section records the pass as it was designed.

A seeded, default-disabled weekly routine, `evolution-pass-<workspace>`. It is separate from the
residents and shaped like the architecture and prompt audits.

1. A deterministic step ranks open evolution records over the window by summed cost and keeps only
   those over every threshold (§11.6).
2. One agent step drafts at most `max_proposals` (default 3) design proposals, after a duplicate check
   against open tasks, friction and earlier proposals. Each proposal is a `proposed` task in the
   owning workspace with an evidence artifact citing the cycles, occurrences and costs. The record
   moves to `proposed` and links the task.

   The implementation uses a read-only `evolution_draft` agent followed by `evolution_apply`:
   only the deterministic applier files and links the bounded proposals and evidence. The rank step
   persists its window and selection even for an empty pass; replays recover by record tag.
3. The pass never promotes, approves or builds anything. Its tool grant cannot move a task past
   `proposed`. The human decides at `proposed → backlog` what the system becomes; that is ownership,
   and it stays human. A declined proposal moves its record to `declined` with a suppression window.

### 11.4 The manager digest

`orbit resident digest` is a derived view. It lists residents and their state, open escalations,
reversible decisions awaiting veto, the top pains by summed cost with their evidence counts, and
open evolution proposals (filed by the pass before its removal). The manager surfaces it to the human weekly, at the start of a
manager session, or on request. Constellation's session-brief hook can inject it, as it does `orbit
attention`.

### 11.5 Deliberate interactive probes

About one mandate in ten, or one a month (whichever comes first), the human runs a mandate
interactively from the start through a takeover, as a hands-on review. A probe records cost events in
the same shape and is tagged `probe` in its checkpoints, so `orbit resident metrics` can set the
probe's interventions and costs beside residents' on comparable work. The probe also catches what the
metrics cannot see, such as a resident that is correct but slow, or one that is solving the wrong
problem well.

### 11.6 Against busywork

A proposal needed a measured cost and recurrence before the pass filed it. Defaults, in
`[resident.evolution]`. Since [ORB-22844] only `window_days` (the digest window) and
`decline_suppress_days` apply; the other five load with a deprecation warning and are ignored
until 2026-12-07:

| Key | Default | Meaning |
|---|---|---|
| `window_days` | 14 | Window the pass ranks over |
| `min_occurrences` | 3 | Corroborations of one signature in the window |
| `min_distinct_cycles` | 3 | From at least this many cycles (one cycle repeating itself does not count) |
| `min_distinct_mandates` | 1 | Raise to 2 to require the pain in more than one mandate |
| `min_cost_minutes` | 60 | Summed cost in the window |
| `max_proposals` | 3 | Proposals per pass |
| `decline_suppress_days` | 30 | A declined signature is not re-proposed unless its summed cost doubles |

## 12. Placement in the code

Proposals that the slices confirm:

| Concern | Proposed home |
|---|---|
| Mandate, authority, checkpoint and evolution shapes | `crates/orbit-types/src/workflow/resident/` and `crates/orbit-types/src/record/evolution.rs` |
| `resident_authority` gate | `DecisionGate` in `crates/orbit-types/src/task/decision/request/gates.rs` |
| `resident_due` kind and observation | `StateTriggerKind` in `crates/orbit-types/src/workflow/automation/members.rs`; observation beside the existing rules in `crates/orbit-core/src/application/automation/members/member_host/observation.rs` |
| Grants, control, relay, checkpoint, metrics, digest | `crates/orbit-core/src/application/resident/` |
| Cycle job and activities | a new `resident_cycle_pipeline` job under `crates/orbit-core/assets/jobs/` and `resident_*` activities under `crates/orbit-core/assets/activities/` |
| Seeded routines | `crates/orbit-core/assets/routines/` (disabled) |
| Dashboard Residents view | `crates/orbit-web/src/api/residents.rs` and `crates/orbit-web/assets/dashboard/js/operations-residents.js`; one-action creation in `crates/orbit-core/src/application/resident/create.rs` |
| `resident` toolset, `manager` and `resident` MCP roles | `crates/orbit-types/src/tool/mcp_toolset.rs` |
| `resident_stalled` attention kind | the attention item kinds of [ORB-21168] |
| Skill references | `crates/orbit-core/assets/skills/orbit/references/` |

## 13. Slices

Slices 1–9 have landed (slice 3 as [§5.1](#51-slice-3-as-implemented) describes); slice 10 is an approved (`backlog`) task. All are in ws_orbit tagged `resident-orchestrator`. Dependencies are
`blocked_by` links inside ws_orbit; cross-workspace links (the [ORB-21178] baseline) are
`related_to`.

| # | Task | Delivers | Blocked by |
|---|---|---|---|
| 1 | [ORB-21242] | Mandate record, operator-only grant, grant requests, narrowing, `resident show/list`, the `resident` toolset | — |
| 2 | [ORB-21243] | `orbit.resident.checkpoint`, request/answer shapes, stale-cycle refusal | 1 |
| 3 | [ORB-21244] | `resident_due`, cycle job, lease, core-assigned action keys and ledger, authority-scoped tools, maintenance and deadline narrowing, the handoff to `review` | 1, 2 |
| 4 | [ORB-21245] | `orbit.resident.control`, `orbit.resident.relay`, `resident` toolset, `resident_stalled` | 1, 2 |
| 5 | [ORB-21246] | Brief reconstruction, continuity adapter | 3 |
| 6 | [ORB-21247] | `orbit resident metrics`, baseline mode | 2, 3 |
| 7 | [ORB-21248] | Cost events, evolution record kind and tools | 2 |
| 8 | [ORB-21249] | Evolution pass routine, `orbit resident digest` | 6, 7 |
| 9 | [ORB-21250] | Manager and resident-cycle skill references | 4 |
| 10 | [ORB-21251] | Pilot on the scribe v2 mandate and the go/no-go | 3, 4, 6 |

## 14. Concerns & Honest Limitations

- **The authority boundary is only as strong as the tool grant.** A resident with `dispatch` can still
  dispatch the wrong task. The grant bounds the kind of harm, not judgment; reversals measure
  judgment.
- **Operator authority in a manager session.** Slice 1 closes most of this: only a native CLI caller
  with operator capability and no agent, model, orchestrator, MCP-session or managed-run context
  grants directly, so a manager over MCP, even one served `--operator`, always goes through a grant
  request. An agent that shells out to `orbit resident grant` in the operator's own terminal without
  any agent context still resolves as the operator; that is the existing caller model
  ([notice-authority](../fleet-notices/specs/notice-authority.md) records the same limit).
- **The wake digest can miss a relevant change.** Anything outside the watched set (an unrelated task
  that starts touching the same files, say) shows up only at the heartbeat. A short heartbeat costs
  cycles; a long one costs latency. Workspaces can add condition-triggered wakes.
- **The escalation rubric is a heuristic.** Its numbers are guesses until the pilot measures them. The
  fixed classes carry the safety; the rubric only trades question load against rework.
- **Self-reported costs.** Cost events are what the agent says it spent. The pass ranks by recurrence
  across distinct cycles as well as by minutes, and probes provide a check, but a resident that never
  notices its workarounds records none. The intervention metric is the backstop, since a human
  stepping in is recorded whether or not the resident noticed.
- **Same-owner restriction.** v1 residents cover only workspaces owned by their own host. Cross-owner
  mandates wait for federated writes.
- **More automation on the owner host.** Each active resident costs one agent run per change or
  heartbeat. Ten residents at a 2 h heartbeat cost at least 120 runs a day, before change-driven
  cycles. The cadence floor and observe-only cycles bound it, and the cost is visible in the metrics.
- **Checkpoint comments grow the assignment's history.** A long mandate accumulates hundreds of
  checkpoints. The brief reads only the last *K*, and the reader parses the newest ones, but the
  dashboard's comment view gets noisy. A collapsed rendering is a dashboard follow-up.

## Task References

- [ORB-21229] — rewrote this design and filed the slices
- [ORB-12488] — removed the epic pipeline and the earlier resident design; its epic-is-a-tag decision stands
- [ORB-21207] — designed fleet notices, which residents read and post under the existing poster classes
- [ORB-21178] — the interactive scribe v2 orchestration that is the pilot's baseline
- [ORB-21168] — `orbit attention` slice 1, which the `resident_stalled` kind joins
- [ORB-21242] — slice 1: mandate record and authority grant
- [ORB-21243] — slice 2: checkpoint contract
- [ORB-21244] — slice 3: resident cycle
- [ORB-21245] — slice 4: manager channel
- [ORB-21246] — slice 5: brief reconstruction and continuity adapter
- [ORB-21247] — slice 6: metrics
- [ORB-21248] — slice 7: cost events and evolution records
- [ORB-21249] — slice 8: evolution pass and digest
- [ORB-21250] — slice 9: skill references
- [ORB-21251] — slice 10: pilot
- [ORB-21263] — MCP roles: the manager and resident surfaces
- [ORB-21649] — the dashboard Residents view and how a dashboard widening is authorized
- [ORB-21642] — fixed `orbit resident metrics --baseline` failing on a baseline task in another workspace
- [ORB-22844] — removed the evolution pass routine, job and proposal filing; the ledger and manager digest stay

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
