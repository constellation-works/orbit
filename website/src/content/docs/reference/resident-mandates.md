---
title: Resident mandates
description: "Prepare a resident mandate, check its fields, inspect its effective authority, and widen or narrow that authority with the resident grant, show and list commands."
---

A resident mandate is an assignment task tagged `resident` that carries a `resident/mandate.json` artifact. The artifact says what the resident works toward and which workspaces it concerns. The mandate's authority, the rights and workspaces the resident may use, is a separate record. It starts with no rights. A human widens it. Anyone who can write the assignment task can narrow it.

This page covers the mandate format and the authority commands in this release. It does not describe how a resident cycle is scheduled or started. The [checkpoint command](#record-a-checkpoint) records only a cycle that is already live.

## Prepare the assignment

Create the task with the `resident` tag, then attach the mandate as `resident/mandate.json`:

```bash
orbit task add --title "Keep the release branch green" --complexity medium --tag resident
orbit task artifact put <task id> ./mandate.json --path resident/mandate.json
```

Use the ID of the new task in place of `<task id>`.

## Mandate format

This example is illustrative. Replace the goal, criteria, workspace and crew with your own. The crew must be a name from your `[crews]` table in [Configuration](../config/#crews).

```json
{
  "schema": "resident.mandate/v1",
  "goal": "Keep the release branch green and finish each release task.",
  "exit_criteria": [
    "Every task tagged release is done or rejected.",
    "The release summary task has an execution summary."
  ],
  "deadline": "2026-12-15T00:00:00Z",
  "workspaces": ["orbit"],
  "watch": {
    "tasks": [],
    "tags": ["release"]
  },
  "crew": "sol",
  "cadence": {
    "floor_minutes": 15,
    "heartbeat_minutes": 60
  },
  "continuity": "prefer"
}
```

Orbit validates the artifact against these rules:

| Field | Required | Rule |
|---|---|---|
| `schema` | Yes | Exactly `resident.mandate/v1`. |
| `goal` | Yes | One line: not blank, no line breaks, at most 240 characters. |
| `exit_criteria` | Yes | 1 to 8 entries. Each is one line, not blank, and at most 240 characters. |
| `workspaces` | Yes | At least one workspace. Each is non-blank and unique after trimming. The authority's scope stays inside this list. |
| `crew` | Yes | A non-blank name of a configured crew. Orbit checks it when it reads the mandate. |
| `cadence.floor_minutes` | Yes | An integer of at least 5: the fewest minutes between two cycles. |
| `cadence.heartbeat_minutes` | Yes | An integer at least equal to `floor_minutes`: the most minutes without a cycle. |
| `deadline` | No | An RFC 3339 instant. After it the resident only hands off. |
| `watch` | No | `tasks` (task IDs) and `tags` lists whose changes feed the resident's wake digest. Both default to empty. |
| `continuity` | No | `prefer` (the default) resumes a provider conversation when the adapter can. `off` starts every cycle fresh. |
| `probe` | No | A boolean, default `false`. Whether the mandate runs interactively as a probe. |

Unknown keys are refused at the top level and inside `cadence` and `watch`.

Orbit reads the artifact each time it reports or changes authority. A malformed mandate, or one that fails these rules, shows as `invalid` in `orbit resident show`, and a widening against it is refused.

## Inspect a mandate

```bash
orbit resident list
orbit resident list --all
orbit resident show <task id>
```

`list` prints one row per mandate with its task ID, state, effective rights, effective workspaces, number of open grant requests and goal. `--json` adds each mandate's deadline and authority revision. Ended mandates appear only with `--all`.

`show` reports one mandate in full. Its `state` is one of:

| State | Meaning |
|---|---|
| `active` | The task is tagged `resident`, the mandate is valid, and the assignment is open. |
| `handed_off` | The assignment is in review. The resident has handed off. |
| `ended` | The assignment is done, archived or rejected. The mandate takes no widening. |
| `invalid` | The task is not tagged `resident`, or the mandate is missing or invalid. `state_reason` says which. |

Add `--json` to get the full payload. Its keys are `state`, `state_reason`, `mandate`, `effective_authority`, `authority` (with its `granted` and `narrowed` history), `open_requests`, `latest_checkpoint` and `live_cycle`, which appears only while it is unexpired. `mandate_error` or `authority_error` appears when that record cannot be read.

## Effective authority

The effective authority is what the resident could act on now. It is the authority's rights, with its workspaces limited to those the mandate also covers.

| Right | Lets the resident |
|---|---|
| `dispatch` | Start or requeue leaf runs and drains. |
| `promote` | Move a `proposed` task to `backlog`. |
| `merge` | Land a reviewed pull request. |
| `plugin_upgrade` | Install or upgrade a workspace plugin. |

Effective authority is empty when the mandate is `ended` or `invalid`, and when either the mandate or the authority record cannot be read. Removing a workspace from the mandate's `workspaces` drops it from effective authority, but the authority record keeps it until a narrowing drops it.

## Widen the authority

Widening is the human's decision. Who makes the call determines what happens.

### As the operator

The operator at a terminal on this host applies a widening at once:

```bash
orbit resident grant <task id> --add dispatch --add promote --add-workspace orbit
```

`--add` names a right and repeats. `--add-workspace` names a workspace that the mandate lists and this host can resolve, and it repeats too. Rights and workspaces the authority already holds are skipped. If nothing new remains, the outcome is `unchanged`.

### From any other caller

A widening from an agent, an MCP session, or any caller that is not the operator on this host changes nothing. It must carry `--quote` with the human's words, verbatim, up to 2000 characters:

```bash
orbit resident grant <task id> --add merge --quote "Merge reviewed release pull requests in orbit."
```

Without `--quote` the call is refused. With it, Orbit raises one `resident_authority` decision request. The request names the rights and workspaces asked for, quotes the words and records the relaying session. The call's outcome is `requested`. An identical open request is returned, not duplicated.

The human answers the request:

```bash
orbit decision list --gate resident_authority
orbit decision show <request id>
orbit decision answer <request id> grant
```

`grant` adds exactly the requested rights and workspaces. `reject` leaves the authority unchanged. Any authority change, including a narrowing, supersedes an open request. Answering a superseded request is refused and applies nothing, so raise a new one. Agents can list and show requests but cannot answer them. See [Decision channels](../../how-to/decision-channels/) for the answer path.

## Narrow the authority

Narrowing applies at once for any caller that can write the assignment task. It raises no request and needs no quote. The change records that caller.

```bash
orbit resident grant <task id> --remove merge
orbit resident grant <task id> --drop-workspace orbit
orbit resident grant <task id> --deadline 2026-12-01T00:00:00Z
```

- `--remove` takes a right away. It repeats.
- `--drop-workspace` drops a workspace from the authority's scope. It repeats.
- `--deadline` sets an RFC 3339 instant that is earlier than the current deadline, or the first deadline when the mandate has none. It rewrites the deadline in `resident/mandate.json` in the same change. A later deadline is an ordinary edit of that artifact, not a grant.

Rights and workspaces the authority does not hold are ignored. If nothing changes, the outcome is `unchanged`.

One call either widens or narrows, never both. Make two calls to do both.

## Audit history

Each applied change appends a `resident_authority_changed` event to the assignment task's history. The entry records:

- `revision`: the change count after this change, starting at 1.
- `at` and `by`: when it was applied, and the caller who applied it.
- `via`: `operator` for a direct widening, `decision:<request id>` for an answered request, or `narrowing`.
- The rights and workspaces added or removed, the deadline a narrowing set, and, for an answered request, the `quote` and the relaying session.

`orbit resident show` reports this history as `authority.granted` (widenings, oldest first) and `authority.narrowed` (narrowings). If an entry cannot be read, if its recorded writer differs from its `by`, or if revisions run out of order, the authority record is unreadable. Widenings and narrowings are then refused, and `show` reports `authority_error`.

## MCP roles and authority

The `resident` MCP role and the `resident` toolset decide which tools an MCP session lists and dispatches. They grant no authority. The toolset carries `orbit.resident.grant`, `orbit.resident.show`, `orbit.resident.list`, `orbit.resident.checkpoint`, `orbit.resident.metrics`, `orbit.evolution.add`, `orbit.evolution.list` and `orbit.evolution.show`. See [MCP integration](../../how-to/mcp-integration/) for roles and toolsets.

A widening applies directly only when the caller is the operator, and the caller's role does not decide that. An MCP session is never the operator for this purpose, even one that holds operator capability, so its widening becomes a request. Its `orchestrator` and `model` arguments record attribution on that request and never grant anything.

| Caller | Widen with `add` or `workspaces` | Narrow |
|---|---|---|
| Operator at a terminal on this host | Applies at once | Applies at once |
| Any other caller, including an MCP session | Raises a request. The authority changes only when a human answers `grant`. | Applies at once |

Over MCP the same operations take these argument names: `task_id`, `add`, `workspaces`, `remove`, `drop_workspaces`, `deadline` and `quote` for `orbit.resident.grant`, and `include_ended` for `orbit.resident.list`. `workspaces` and `drop_workspaces` are the CLI's `--add-workspace` and `--drop-workspace` flags.

## Record a checkpoint

`orbit resident checkpoint <task id> --record '<json>'` appends one `resident.checkpoint` v1 record to the assignment task as a comment. It is a record operation for a live cycle. It does not start a cycle or make a resident run unattended.

- The record's `mandate`, `cycle` and `run_id` must match the mandate's live, unexpired cycle. Orbit publishes that cycle itself. No CLI or MCP call sets it.
- Malformed records, records for a stale cycle, and a second checkpoint for the same cycle are refused and write nothing.
- The record's `authority` field reports the rights a cycle observed. It grants nothing.
- A record holds at most 64 KiB, 50 actions, 20 decisions, 20 costs and 2000 characters of notes. Each action key has the form `resident:<mandate>:<cycle>:<n>`, where `<n>` is a positive integer.

`show` reports the latest checkpoint.

## Evolution ledger

The `resident` toolset also carries the evolution capability-gap ledger: `orbit.evolution.add` records an interactive cost event outside a checkpoint, and `orbit.evolution.list` and `orbit.evolution.show` inspect the resulting signatures and records.

```bash
orbit evolution add --event '{"kind":"manual_step","what":"Checked CI by hand","minutes":4,"retries":0,"slot_minutes":0,"wished":"Automatic CI recheck"}'
orbit evolution list --state open
orbit evolution show <evolution-id>
```

`orbit evolution decline <evolution-id> [--days <n>]` suppresses proposals for a signature. It is operator-only and CLI-only, so it is not listed over MCP.

## Related pages

- [CLI commands](../cli/) lists every `orbit resident` command.
- [MCP integration](../../how-to/mcp-integration/) covers roles and toolsets.
- [Decision channels](../../how-to/decision-channels/) covers answering decision requests.
- [Configuration](../config/) covers crews and the MCP role setting.
