---
type: design
summary: "Spec: the resident mandate artifact, the operator-only authority record, grant requests through a resident_authority decision, and free narrowing"
last_validated: 2026-11-02
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

`resident/mandate.json`, written with `orbit.task.artifact.put` or through `orbit resident grant`
(which can write both). Schema version 1:

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

Editing the mandate is an ordinary task write and is not gated. A mandate edit that would widen
the authority's scope is refused: the scope lives in the authority record, and `workspaces` cannot
drop below it.

## Authority record

Stored as the task's `resident_authority` record (beside the task, versioned, with history):

```json
{
  "schema": "resident.authority/v1",
  "rights": ["dispatch", "promote"],
  "workspaces": ["orbit-office"],
  "granted": [
    {"at": "2026-11-02T10:00:00Z", "by": "human:daniel", "via": "decision:DR-…", "added": ["dispatch", "promote"], "quote": "you may dispatch and promote in orbit-office, not merge"}
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
| none | read tools, `orbit.task.add` (filed `proposed`), comments, `orbit.friction.add`, `orbit.decision.raise`, `orbit.notice.post` (agent class), `orbit.resident.checkpoint`, `orbit.evolution.add` |
| `dispatch` | run/ship/auto dispatch, requeue, cancel of runs the resident started |
| `promote` | `proposed → backlog` status writes |
| `merge` | PR merge and landing tools, except merges the workspace reserves for the human |
| `plugin_upgrade` | plugin install and upgrade |

The mapping lives in core beside the tool catalog. A tool not in the map for the effective rights is
not granted to the cycle. A tool call whose workspace selector is outside `workspaces` is refused
with `resident_scope_refused`.

## Widening

`orbit.resident.grant` (CLI `orbit resident grant <task> --add <right>… --workspace <ws>… --quote
<text>`):

- **Operator caller.** Applies at once. Recorded with `by` = the human and `via: operator`.
- **Any other caller.** Writes nothing to the authority. It raises one decision request on the
  `resident_authority` gate, bound to the authority's current revision (the stale guard), with:
  - `question`: "Grant <rights> in <workspaces> to the resident for <task>?"
  - the `quote`, verbatim, and the relaying session (crew, model, session id);
  - answers `grant` and `reject`.

  Answering `grant` as the human applies exactly the requested addition with `via: decision:<id>`.
  `reject`, expiry or an intervening authority change supersedes it, and nothing changes. An open
  request for the same addition is reused, not duplicated.
- Refused: a right outside the enum, a workspace not owned by the assignment's owner host, any
  grant on a mandate in `ended` state.

## Narrowing

`orbit.resident.grant --remove <right>… / --drop-workspace <ws>…`, `orbit.resident.control
pause|stop`, and a deadline shortened in the mandate are narrowing. Any caller that can write the
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
| Authority wider than the mandate's `workspaces` after an edit | The edit is refused |
| Decision channel disabled | Grant requests are still raised and answerable from the dashboard and CLI |

## Agent Signature

Designed by claude (claude-opus-5-5) for [ORB-21229]; built by [ORB-21242].

## Task References

- [ORB-21229] — designed the mandate and authority model
- [ORB-21242] — implements the mandate record, grants, grant requests and narrowing

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
