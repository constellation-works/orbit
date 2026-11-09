---
type: design
summary: "Spec: the resident mandate artifact, the operator-only authority record, grant requests through a resident_authority decision, and free narrowing"
last_validated: 2026-11-05
title: Spec — Mandate and authority
owner: claude
status: Draft
feature: resident-orchestrator
tags: [resident-orchestrator, spec, security]
related_features: [resident-orchestrator, human-decision-channel, granted-tools]
related_artifacts: [ORB-21229, ORB-21242]
last_updated: 2026-11-02
---

# Spec: Mandate and authority

A resident works for exactly one **mandate**: an assignment task tagged `resident` that carries a
`resident/mandate.json` artifact and an **authority** record. The mandate says what to achieve; the
authority says what the resident may do to achieve it. Core computes every cycle's tool grant from
the authority. Only the human widens authority; anyone who can write the assignment narrows it.

## Why This Exists

A resident acts while nobody watches. Without a recorded, core-enforced authority, its limits
would be prose in a prompt, and the manager, an agent relaying the human, would be the only thing
standing between a misheard sentence and a merge. Splitting mandate (freely editable) from
authority (human-gated to widen) lets the manager do the clerical work of delegation without
being able to delegate power.

## Mandate artifact

`resident/mandate.json`, written with `orbit.task.artifact` action `put` (or `orbit task update
--artifact`). Attaching a valid mandate to a resident assignment also updates its task crew to the mandate crew, including on later edits. Dashboard creation copies the exit criteria to the task and uses a shortened goal for a blank title. `orbit resident grant` never writes it, except to shorten its `deadline` (see
[Narrowing](#narrowing)). Schema version 1:

```json
{
  "schema": "resident.mandate/v1",
  "goal": "Deliver the remaining scribe v2 slices",
  "exit_criteria": ["Every task tagged scribe-v2 in orbit-office is done or rejected"],
  "deadline": "2026-11-20T00:00:00Z",
  "workspaces": ["orbit-office"],
  "watch": {"tasks": ["ORB-21180"], "tags": ["scribe-v2"]},
  "crew": "opus",
  "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120},
  "continuity": "prefer",
  "probe": false
}
```

| Field | Rule |
|---|---|
| `goal` | One line; the task description keeps the narrative |
| `exit_criteria` | 1–8 lines; criteria checkable from task state should be phrased that way |
| `deadline` | Optional RFC 3339; past it the resident only hands off |
| `workspaces` | Workspaces the mandate is about; must be a superset of the authority's scope. v1: all owned by the assignment's owner host |
| `watch` | Tasks and tags whose changes feed the wake digest, in addition to tasks the resident dispatched or filed |
| `crew` | A registered crew; any provider |
| `cadence` | `floor_minutes` ≥ max(5, clock cadence); `heartbeat_minutes` ≥ `floor_minutes` |
| `continuity` | `prefer` (resume a provider conversation when the adapter can) or `off` |
| `probe` | `true` while the mandate is run interactively as a probe |

Editing the mandate is an ordinary task write and is not gated. The scope lives in the authority
record, so a mandate edit cannot widen it. An edit that drops a workspace the authority still covers
is not refused at the artifact write (slice 1): the **effective authority** is the authority's
rights over the workspaces both records cover, so the dropped workspace leaves the effective scope
at once, and `orbit resident show` reports both records beside the effective one.

A non-ended mandate is not due (`orbit resident show` state `invalid`, with the reason) when the
task is not tagged `resident`, the artifact is missing or fails validation, its `crew` is not a
configured crew, or its control history is unreadable. Its other states are `active`, `paused`, `taken_over`,
`handed_off` (the assignment is in `review`) and `ended` (the assignment is done, archived or
rejected, or the mandate was stopped). An ended or invalid mandate's effective authority is empty.

## Authority record

Stored in the assignment task's history: every change appends one `resident_authority_changed`
entry whose note carries the change and the resulting `rights` and `workspaces`, and only core
writes them, under the task write lock. The record is the fold of those entries; its `revision` is
their count, which the grant request binds to. No separate store or schema migration is needed, and
the history is already the task's append-only audit trail. Folded, it reads:

```json
{
  "schema": "resident.authority/v1",
  "revision": 1,
  "rights": ["dispatch", "promote"],
  "workspaces": ["orbit-office"],
  "granted": [
    {"revision": 1, "at": "2026-11-02T10:00:00Z", "by": "human:daniel", "via": "decision:DR-…", "added": ["dispatch", "promote"], "workspaces_added": ["orbit-office"], "quote": "you may dispatch and promote in orbit-office, not merge", "relay": {"by": "claude", "crew": "opus", "model": "claude", "session_id": "…"}}
  ],
  "narrowed": []
}
```

Rights: `dispatch`, `promote`, `merge`, `plugin_upgrade`. Unknown rights are refused, not ignored.
Releases, tags, publishing, host changes, config edits and secrets are not rights; they are not
grantable to a resident in v1.

### Rights → tools

| Right | Tools the cycle may call (in covered workspaces) |
|---|---|
| none | read tools; `orbit.task.add` filed `proposed` (or with no status); `orbit.task.update` without a status (edits, comments); `orbit.task.artifact` action `put`; `orbit.friction` action `add`; `orbit.evolution.add`; `orbit.decision` action `raise`; `orbit.notice.post`; and `orbit.resident.checkpoint`, including the `handoff` outcome that has finalize move the assignment to `review` |
| `dispatch` | `orbit.pipeline.invoke`, `orbit.run.auto`/`ship`/`resume`, `orbit.task.recover` action `requeue`/`repilot`, drain pull and claim bind/settle, `orbit.task.update` to `in-progress` |
| `promote` | `orbit.task.update` to `backlog`, `orbit.task.add` filed `backlog` |
| `merge` | `orbit.workflow` action `land`, `orbit.task.update` to `done` (refused while the task has an open pull request), `orbit.task.recover` action `retry_handoff`, `orbit.task.recover` action `reconcile_review` |
| `plugin_upgrade` | none yet: no builtin tool installs or upgrades a plugin |

The mapping lives in core's tool host beside the dispatch table (`resident_guard`), and core's cycle
guard applies it to every builtin tool call a `resident_cycle_pipeline` run makes; the run is
identified from the trusted managed-run binding, never from input. Reads always pass. A write
without this run's live cycle is refused as `resident_cycle_stale`; a right-gated write without the
right as `resident_right_refused` (naming the observe-only cause when a maintenance notice or a
passed deadline emptied the rights); a `workspace` or `workspaces` input that resolves outside the
granted workspaces and the assignment's own as `resident_scope_refused`; and any other write as
`resident_tool_refused`. `orbit.task.lint` without `restore_pruned` is a read. With
`restore_pruned: true`, both named-task restoration and the no-id sweep are refused: no resident
right grants context maintenance. This applies to the native `orbit task lint --restore-pruned`
route too; MCP and `orbit tool run` do not expose lint. Outside a resident cycle, operator
restoration is unchanged. Other refused writes include `orbit.task.update` with `approve` or `force`, other status
targets, `orbit.notice.close`, `orbit.resident.grant`, decision answers and administration,
`orbit.command.exec`, desktop writes and every write on another noun. Operator-only tools stay
operator-only, because their capability check runs before the guard, so `orbit.task.recover` action `requeue`/`repilot`,
`orbit.run.auto`/`resume` and `orbit.pipeline.invoke` in the table above are refused as
`capability_denied` even with the right. `orbit.workflow` action `ship` (`orbit.run.ship` in the table) is the
exception and the tool a resident dispatches with: a cycle's run passes its operator row and its managed-run refusal,
and the guard decides the live cycle, the right and the workspace. A sandboxed cycle's ship goes through
its run's broker, so the leaf's worker starts on the host beside the cycle's read-only sandbox, not
inside it. A sandboxed cycle's `orbit.decision` action `raise` goes through the same broker, because its lock
lies in the `.orbit` that sandbox mounts read-only.

`orbit.workflow` action `land` is how a resident lands a delivery, and passes the operator row and managed-run
refusal the same way. It merges the `review` task's one open delivery pull request at the head whose
required checks it has just read, then settles the task to `done` from that merge. Nothing bypasses the
forge's branch protection. Any refusal, by Orbit or the forge, answers `task_landing_refused` with the
reason, leaves the task in `review`, and records a `refused` row in the action ledger; a merge whose
reply was lost settles on a repeated call without merging again. `orbit.task.update` to `done` answers
`resident_done_pr_open` while a pull request recorded for the task is open, and `resident_lookup_refused`
when the forge cannot say. Both go through the run's broker, because they read the forge with the host's
credentials.

A write from a resident cycle run that
this workspace does not hold (one started in another catalog workspace) is refused as
`resident_scope_refused`; slice 3 does not support cross-workspace writes. `orbit.evolution.add`
arrives with [ORB-21248].

## Widening

`orbit.resident.grant` (CLI `orbit resident grant <task> --add <right>… --add-workspace <ws>… --quote
<text>`):

- **Operator caller.** Applies at once. Recorded with `by` = the human and `via: operator`.
  Operator means the human at a terminal: a native CLI caller holding operator capability with no
  agent, model or orchestrator attribution, no MCP session and no managed-run or agent context.
  Any MCP session, even one served `--operator`, is a relay and takes the next path.
- **Any other caller.** Writes nothing to the authority. It raises one decision request on the
  `resident_authority` gate, bound to the authority's current revision (the stale guard), with:
  - `question`: "Grant <rights> in <workspaces> to the resident for <task>?"
  - the `quote`, verbatim (required, at most 2000 characters), and the relaying session (crew,
    model, session id);
  - answers `grant` and `reject`.

  Answering `grant` as the human applies exactly the requested addition with `via: decision:<id>`.
  `reject`, expiry or an intervening authority change supersedes it, and nothing changes. An open
  request for the same addition is reused, not duplicated. The request rides beside other decision
  requests on the task (neither supersedes the other), and precedent and the decision model never
  answer it: both answers are authority-tier.
- Refused: a right outside the enum, a workspace the mandate does not list, a workspace that is
  neither the assignment's own nor registered in this host's workspace catalog (slice 1's stand-in
  for "owned by the assignment's owner host"), a task not tagged `resident` or without a valid
  mandate, any grant on a mandate in `ended` state, and one call that both widens and narrows.

## Narrowing

`orbit.resident.grant --remove <right>… / --drop-workspace <ws>… / --deadline <rfc3339>`,
`orbit.resident.control pause|stop` ([ORB-21245]), and a deadline shortened in the mandate are
narrowing. `--deadline` must be earlier than the current deadline (or set the first one); it
rewrites the mandate's `deadline` in the same update and records the new deadline in `narrowed[]`.
A later deadline is refused there; extend a deadline by editing the mandate. Any caller that can write the
assignment may narrow, including the resident's own cycle. Narrowing takes effect for cycle
execution at the next cycle boundary, and each narrowing is recorded in `narrowed[]` with its
caller. Pause and stop prevent a new cycle from starting. A live cycle keeps its frozen grant,
completes, checkpoints (or receives a synthesized checkpoint from `resident_finalize`), and exits at
the next cycle boundary, within at most one agent budget. A stop then hands the assignment to
`review`.

## Failure modes

| Situation | Behaviour |
|---|---|
| Authority record missing or unreadable | The cycle runs observe-only and the checkpoint reports `authority_unavailable` |
| Mandate artifact missing or invalid | The mandate is not due; `orbit resident show` reports why |
| Authority wider than the mandate's `workspaces` after an edit | The edit is accepted; the effective authority drops the uncovered workspaces |
| Decision channel disabled | Grant requests are still raised and answerable from the dashboard and CLI |

## Agent Signature

Designed by claude (claude-opus-5-5) for [ORB-21229]; built by [ORB-21242].

## Task References

- [ORB-21229] — designed the mandate and authority model
- [ORB-21242] — implements the mandate record, grants, grant requests and narrowing
- [ORB-21244] — the cycle that maps rights to tools
- [ORB-21245] — `orbit.resident.control` and relay

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
