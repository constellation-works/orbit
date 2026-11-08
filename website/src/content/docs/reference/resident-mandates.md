---
title: Resident mandates
description: "Prepare a resident mandate, check its fields, inspect its effective authority, widen or narrow that authority, relay a human's words to it, and pause, stop, wake or take over a resident."
---

A resident mandate is an assignment task tagged `resident` that carries a `resident/mandate.json` artifact. The artifact says what the resident works toward and which workspaces it concerns. The mandate's authority, the rights and workspaces the resident may use, is a separate record. It starts with no rights. A human widens it. Anyone who can write the assignment task can narrow it.

This page covers the mandate format, the authority commands, the routine that starts cycles, the manager's relay and control commands, and the digest and metrics reports in this release. The [checkpoint command](#record-a-checkpoint) records only a cycle that is already live.

## Prepare the assignment

Create the task with the `resident` tag, then attach the mandate as `resident/mandate.json`:

```bash
orbit task add --title "Keep the release branch green" --complexity medium --tag resident
orbit task artifact put <task id> ./mandate.json --path resident/mandate.json
```

Use the ID of the new task in place of `<task id>`.

The dashboard's **Automation › Residents** view does both steps in one: its **Start a resident** flow takes the mandate fields, checks them with the same rules as below, and creates nothing when a field is refused. The same flow can also grant rights and turn the cycle routine on, as up to three separate writes; if a later write is refused, the message names the step and says what already exists. See [From the dashboard](#from-the-dashboard).

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
| `deadline` | No | An RFC 3339 instant. When it passes, the mandate ends with reason `deadline` and its assignment moves to review — enforced by the sweep clock on the owning host, whether or not the cycle routine runs. A cycle that starts first is observe-only and only hands off. |
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

`list` prints one row per mandate with its task ID, state (marked `(stalled)` when it is [stalled](#stalled-residents)), effective rights, effective workspaces, number of open grant requests and goal. `--json` adds each mandate's deadline, authority revision and `stalled` flag. Assignments that are done, archived, or rejected appear only with `--all`; a stopped mandate (state `ended`) whose assignment moved to review is still listed without it.

`show` reports one mandate in full. Its `state` is one of:

| State | Meaning |
|---|---|
| `active` | The task is tagged `resident`, the mandate is valid, and the assignment is open. |
| `handed_off` | The assignment is in review. The resident has handed off. |
| `paused` | `orbit resident control … pause` stopped new cycles until `resume`. `state_reason` names who paused it and why. |
| `taken_over` | A person took the mandate over with `takeover`. No cycle fires until `release`. |
| `ended` | The assignment is done, archived or rejected, or the mandate was stopped with `stop`. The mandate takes no widening and no relay. |
| `invalid` | The task is not tagged `resident`, or the mandate is missing or invalid. `state_reason` says which. |

Add `--json` to get the full payload. Its keys are `state`, `state_reason`, `control` (the control state and its last action, caller and reason), `stalled` and `stalled_reason`, `mandate`, `effective_authority`, `authority` (with its `granted` and `narrowed` history), `open_requests`, `latest_checkpoint` and `live_cycle`, which appears only while it is unexpired. `mandate_error` or `authority_error` appears when that record cannot be read.

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

A widening from an agent, an MCP session, a shell that no terminal or declaration identifies, or any caller that is not the operator on this host changes nothing. It needs no operator override; do not set `ORBIT_OPERATOR`, which would apply the widening at once as the operator. It must carry `--quote` with the human's words, verbatim, up to 2000 characters:

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

### From the dashboard

The dashboard's **Automation › Residents** view widens only when the dashboard server runs with `--operator`. Its widening is not a direct grant. It raises the same `resident_authority` request with the quote you type, naming the dashboard as the relaying session, and answers it `grant` in the same action. The authority change is then recorded `via: decision:<request id>`, with your quote, and the dashboard writes an audit row for it. A widening without a quote is refused.

A dashboard served without `--operator` cannot widen, narrow, control or relay, and cannot answer `resident_authority` requests. It returns `authorization_denied`. Agents and MCP sessions still only raise requests.

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

The `resident` MCP role and the `resident` toolset decide which tools an MCP session lists and dispatches. They grant no authority. The toolset carries `orbit.resident.grant`, `orbit.resident.relay`, `orbit.resident.control`, `orbit.resident.show`, `orbit.resident.list`, `orbit.resident.checkpoint`, `orbit.resident.metrics`, `orbit.resident.digest`, `orbit.evolution.add`, `orbit.evolution.list` and `orbit.evolution.show`. See [MCP integration](../../how-to/mcp-integration/) for roles and toolsets.

A widening applies directly only when the caller is the operator, and the caller's role does not decide that. An MCP session is never the operator for this purpose, even one that holds operator capability, so its widening becomes a request. Its `orchestrator` and `model` arguments record attribution on that request and never grant anything.

| Caller | Widen with `add` or `workspaces` | Narrow |
|---|---|---|
| Operator at a terminal on this host | Applies at once | Applies at once |
| Any other caller, including an MCP session | Raises a request. The authority changes only when a human answers `grant`. | Applies at once |

Over MCP the same operations take these argument names: `task_id`, `add`, `workspaces`, `remove`, `drop_workspaces`, `deadline` and `quote` for `orbit.resident.grant`, `task_id`, `quote`, `source`, `request_id` and `reverses` for `orbit.resident.relay`, `task_id`, `action`, `reason`, `status_request` and `by_session` for `orbit.resident.control`, and `include_ended` for `orbit.resident.list`. `workspaces` and `drop_workspaces` are the CLI's `--add-workspace` and `--drop-workspace` flags.

## Enable the cycle routine

A mandate runs only when its workspace's cycle routine fires. Each workspace carries a seeded `resident-cycle-<workspace>` routine, and it ships disabled. Until an operator enables it, no cycle starts for any mandate in that workspace, however complete the mandate and its authority are.

Enable it from the CLI:

```bash
orbit routine list
orbit routine enable resident-cycle-<workspace>
```

Use the workspace's name in place of `<workspace>`, and run the command from that workspace's checkout or add `--workspace <workspace>`: a command that selects another workspace's routine is refused and changes nothing, so the audit record always names the workspace whose definition changed. `orbit routine disable resident-cycle-<workspace>` turns it off again. Both commands change the routine definition's versioned `enabled` field, the same change the dashboard's toggle makes, and both are audited. They are operator-only: an agent or an MCP session that runs them is refused, so a manager cannot start a resident on its own.

Or in the dashboard, open **Automation**, find the `resident-cycle-<workspace>` routine and press **Enable**. The button works only on a dashboard served with `--operator`.

An enabled routine can still be held by a host-local pause, which `orbit routine resume <name>` clears. A replica checkout does not schedule this routine, so enable it on the machine that owns the workspace. `orbit routine show resident-cycle-<workspace>` reports the effective state.

## Record a checkpoint

`orbit resident checkpoint <task id> --record '<json>'` appends one `resident.checkpoint` v1 record to the assignment task as a comment. It is a record operation for a live cycle. It does not start a cycle or make a resident run unattended.

- The record's `mandate`, `cycle` and `run_id` must match the mandate's live, unexpired cycle. Orbit publishes that cycle itself. No CLI or MCP call sets it.
- Malformed records, records for a stale cycle, and a second checkpoint for the same cycle are refused and write nothing.
- The record's `authority` field reports the rights a cycle observed. It grants nothing.
- A record holds at most 64 KiB, 50 actions, 20 decisions, 20 costs and 2000 characters of notes. Each action key has the form `resident:<mandate>:<cycle>:<n>`, where `<n>` is a positive integer.

`show` reports the latest checkpoint.

## Relay a human's words

```bash
orbit resident relay <task id> --quote "Ship CSV first." \
  --source '{"human":"daniel","via":"chat","said_at":"2026-11-02T10:00:00Z"}'
```

`relay` writes one `resident.answer` comment on the assignment. The quote is kept verbatim, up to 4000 characters, with its provenance: `human` (who said it), `via` (the channel), an optional `session` and `said_at`. Orbit records the relaying session as `relayed_by` from the caller, never from input. The comment wakes the resident on its next due check. It returns `{task_id, comment, answer}`: `comment` names the written comment by its 1-based `index` with `at` and `by` (task comments carry no ids), and `answer` is the `resident.answer` record as written.

- `--request-id` names the resident request or decision request the words answer. A relayed answer is a direction: the human still answers the decision request itself.
- `--reverses` names a resident decision the direction undoes.

A relay never changes authority. Input that names rights, workspaces, a status or any field not listed above is refused, inside `source` too. Widen through [`orbit resident grant`](#widen-the-authority). A quote that would need redaction, such as a secret or a home directory path, is refused instead of rewritten. A relay to an ended mandate is refused.

## Control a resident

```bash
orbit resident control <task id> pause --reason "Holding while the release is cut"
orbit resident control <task id> resume --reason "Release is out"
orbit resident control <task id> wake --status-request --reason "Where does slice 3 stand?"
orbit resident control <task id> takeover --by-session <session> --reason "I'll drive this one"
orbit resident control <task id> release --reason "Back to you"
orbit resident control <task id> stop --reason "Goal dropped"
```

| Action | From | To | Effect |
|---|---|---|---|
| `pause` | `active` | `paused` | No new cycle starts. |
| `resume` | `paused` | `active` | Due on the next tick. |
| `stop` | anything but ended | `ended` | No new cycle starts. The assignment moves to review with a synthesized handoff summary once no cycle is live. |
| `wake` | `active` | `active` | Due on the next tick despite the heartbeat, never before the cadence floor. `--status-request` makes the cycle report where the mandate stands even when it has nothing else to do. |
| `takeover` | `active`, `paused` | `taken_over` | No cycle fires. Records who took over. |
| `release` | `taken_over` | `active` | Due on the next tick. The cycle reads the comments written during the takeover as new. |

An action that does not apply to the current state is refused and writes nothing. A live cycle is not interrupted: it keeps its grant, writes its checkpoint and ends at its boundary. A stopped assignment in `proposed` is moved through `in-progress` to `review`. If the lifecycle refuses a handoff, the reply says why, and you move the assignment by hand.

Any caller that can write the assignment may call `control`, because every action narrows or restores firing under the existing authority. `resume` and `release` never widen it. Each action appends a `resident_control` event to the assignment's history with the action, the resulting state, the caller, the reason and any `--by-session`.

## Stalled residents

An `active` mandate is stalled when it has not written a checkpoint for twice its heartbeat (or, never having checkpointed, for twice its heartbeat since the task was created), when its last three checkpoints are all `incomplete`, or when its workspace's cycle routine is disabled — no cycle can fire at all then, however recently it checkpointed. A paused, taken-over, handed-off or ended mandate is not stalled.

`orbit attention` lists each stalled mandate as a `resident_stalled` item. When the workspace's `resident-cycle-<workspace>` routine is enabled, the suggested next step is `orbit resident control <task id> wake --status-request`. When that routine is disabled, `wake` cannot start a cycle, so the suggested next step is instead enabling the routine (`orbit routine enable resident-cycle-<workspace>`) or stopping the mandate (`orbit resident control <task id> stop`); the stall reason names the disabled routine as the cause. The suggestion enables nothing by itself: running it is still the operator's action. `orbit resident show`, `list` and `digest` report the same flag; the digest marks the resident's line with `(stalled)`.

## Evolution ledger

The `resident` toolset also carries the evolution capability-gap ledger: `orbit.evolution.add` records an interactive cost event outside a checkpoint, and `orbit.evolution.list` and `orbit.evolution.show` inspect the resulting signatures and records.

```bash
orbit evolution add --event '{"kind":"manual_step","what":"Checked CI by hand","minutes":4,"retries":0,"slot_minutes":0,"wished":"Automatic CI recheck"}'
orbit evolution list --state open
orbit evolution show <evolution-id>
```

`orbit evolution decline <evolution-id> [--days <n>]` suppresses proposals for a signature. It is operator-only and CLI-only, so it is not listed over MCP.

## Digest and metrics

`orbit resident digest` and `orbit resident metrics` report on the workspace's resident mandates. Neither command writes state. Both only read tasks, comments, history, decision requests and checkpoints; the digest also reads evolution records and the `resident.evolution` settings. Neither records anything on a task, a request or the authority, and neither starts, pauses or changes a resident.

### Digest

```bash
orbit resident digest
orbit resident digest --since 7d --top 10
```

The digest is the manager's view of the bound workspace. Read it to see what a person needs to act on: what each resident is waiting on, which escalations are open, which decisions could still be vetoed, and which costs keep recurring. Its text output has these parts, in order:

- **Residents**: one line per non-ended mandate, with its title, state, last checkpoint outcome and open request count. Ended mandates are omitted. A mandate that [is stalled](#stalled-residents) is marked `(stalled)`.
- **Open escalations**: open decision requests that residents raised, and open grant requests, oldest first.
- **Decisions awaiting veto**: reversible decisions from checkpoints in the window that no answer has reversed yet. `--json` gives each decision's ID. To veto one, relay the human's words with `--reverses` (see [Relay a human's words](#relay-a-humans-words)).
- **Proposals awaiting approval**: proposed tasks linked to evolution records. The digest lists them and approves none of them.
- **Top pains**: unresolved evolution pains from the window, costliest first. Each shows its minutes plus slot minutes, and its occurrence, cycle and mandate counts. See the [Evolution ledger](#evolution-ledger).
- **Metrics**: the workspace totals of interventions and escalations over the same window.

Its flags:

- `--since <window>` sets the window for the decisions, the pains and the metrics line. It takes the forms that `metrics --since` takes. The default is 14 days. Open escalations, proposals and residents are current state, so `--since` does not limit them.
- `--top <n>` caps the pains shown, from 1 to 100. The default is 5.

### Metrics

```bash
orbit resident metrics
orbit resident metrics --mandate <task id> --since 2w
orbit resident metrics --baseline <task id>
```

Its flags:

- `--mandate <task id>` reports one mandate. Without it, the report covers every resident mandate in the workspace and adds a `totals` block. The text output leaves `totals` out when exactly one mandate is listed, because it would repeat that mandate.
- `--since <window>` sets the window start. Use a span such as `7d`, `24h` or `2w` (units `m`, `h`, `d` and `w`), an RFC 3339 instant, or a `YYYY-MM-DD` date. The window ends when you run the command. The default is 30 days.
- `--baseline <task id>` computes the same measures for an assignment task that a person worked interactively, not through a resident. Use it to compare resident work with the same kind of work done by hand. With `--baseline` alone, the report covers only the baseline.

The text output prints one block per mandate, labelled by its task ID, then `totals` and `baseline`. Each block has four measures.

**Interventions** counts the times a person changed the course of the work in the window. It counts unprompted relays (a `resident.answer` with no request ID) whoever sent them, because a relay carries a person's words. It counts other comments, status changes, and history events that name a requeue, cancel, stop or takeover only when a person wrote them. An author counts as a person when it is `human` or `operator`, or starts with `human:` or `human-`. Answers to escalations, grants and the resident's own events do not count. The count covers the assignment and the tasks it watches: its `watch` list, tasks with its watched tags, related tasks, and the subjects of its actions. The text shows the count and the count per active mandate-day.

**Escalation load** counts the decision requests the resident raised in the window. The text also shows the rate per active mandate-day, the median seconds from raised to answered, the share answered with the recommended option, and the number that expired unanswered. The recommended share counts only answered requests that offered a recommendation. The median is blank when no request in the window has been answered, and the share is blank when no answered request offered a recommendation.

**Reversal rate** is the share of the resident's decisions and successful actions, recorded in checkpoints within the window, that a person undid within 7 days. Each counts once. A decision is undone when a `resident.answer` names its ID in `reverses`. An action is undone when a `resident.answer` names its key, when a human write moves its subject to `proposed` or `rejected`, or when a human write records a revert. The text shows `reversed/total (rate …)`, and the rate is blank when there is nothing to judge.

**Recovery cost** covers cycles that ended without a complete checkpoint (an `incomplete` outcome). For each, `--json` gives the time from the dead cycle's start to the next complete checkpoint, as a median and a maximum, and the human interventions in that span. The text shows three numbers: the crashed cycles; the repeated actions, meaning successful actions that ran a second time, including a dead cycle's completed action that a recovery cycle ran again instead of skipping; and the recovery slot minutes, the minutes from each recovery cycle's start to its checkpoint.

How the window and the baseline work:

- Only events between the window start and now count. A mandate's active days run from the later of its creation and the window start to now, rounded up to whole days and at least one. Per-mandate-day rates divide by those days. In `totals`, they divide by the summed active days of all mandates. The per-human-day escalation rate divides by the calendar length of the window instead.
- The baseline uses the same window and its own active days. A baseline with no checkpoints reports `not_derivable` for the reversal rate and recovery cost, never zero. Its interventions and escalation load still compute.
- A watched task in another registered workspace on this machine contributes its history and decision requests, but not its comments, which cannot be read across workspaces. A reference that no registered workspace holds is listed under `references.unreachable` and skipped.

`--json` returns the payload: `window` and `mandates`, plus `totals` and `baseline` when they apply. Each measure appears under two keys, for example `interventions` and `manual_interventions`, `escalations` and `escalation_load`, `reversals` and `reversal_rate`, `recovery` and `recovery_cost`.

## Related pages

- [CLI commands](../cli/) lists every `orbit resident` command.
- [MCP integration](../../how-to/mcp-integration/) covers roles and toolsets.
- [Decision channels](../../how-to/decision-channels/) covers answering decision requests.
- [Dashboard](../../how-to/dashboard/#residents) covers the Residents view.
- [Configuration](../config/) covers crews and the MCP role setting.
