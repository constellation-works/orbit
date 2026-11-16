---
title: Resident mandates
description: "Start a resident under a mandate, grant and narrow its authority, relay a human's words to it, pause, wake or stop it, and read its digest and metrics."
---

A resident is an orchestrator agent that Orbit wakes in bounded cycles to work
toward a goal. Its mandate is an assignment task tagged `resident` with a
`resident/mandate.json` artifact. Its authority, the rights and workspaces it
may use, is a separate record that starts empty: a human widens it, and anyone
who can write the assignment task can narrow it.

## Start a resident

```bash
orbit task add --title "Keep the release branch green" --complexity medium --tag resident
orbit task artifact put <task id> ./mandate.json --path resident/mandate.json
orbit resident grant <task id> --add dispatch --add-workspace orbit
orbit routine enable resident-cycle-<workspace>
orbit resident show <task id>
```

1. Create the assignment and attach the [mandate](#mandate-format).
2. As the operator, [grant](#widen-the-authority) the rights it needs.
3. [Enable the cycle routine](#enable-the-cycle-routine). No cycle starts while
   it is off, and it ships off.
4. Check the state, effective authority and latest checkpoint with `show`.

The dashboard's **Tasks dock > Residents** view does the same in its
[**Start** dialog](/how-to/dashboard/#residents), as up to three
writes: mandate, rights, routine. A refused mandate field creates nothing. If a
later write is refused, the message names the step and what already exists.

## Mandate format

The crew must be a name from your `[crews]` table in
[Configuration](/reference/config/#crews).

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
    "heartbeat_minutes": 60
  },
  "continuity": "prefer"
}
```

| Field | Required | Rule |
|---|---|---|
| `schema` | Yes | Exactly `resident.mandate/v1`. |
| `goal` | Yes | One line, not blank, at most 240 characters. |
| `exit_criteria` | Yes | 1 to 8 entries, each one line of at most 240 characters. |
| `workspaces` | Yes | At least one, unique. The authority's scope stays inside this list. |
| `crew` | Yes | The name of a configured crew. |
| `cadence.heartbeat_minutes` | Yes | An integer of at least 5 and at least `floor_minutes`: the most minutes without a cycle. |
| `cadence.floor_minutes` | No | An integer of at least 5: the fewest minutes between two cycles. Defaults to `heartbeat_minutes`. Set it lower only when the mandate must react to a change faster than its heartbeat. |
| `deadline` | No | An RFC 3339 instant. When it passes, the mandate ends with reason `deadline` and the assignment moves to review, whether or not the cycle routine runs. |
| `watch` | No | `tasks` (task IDs) and `tags` whose changes wake the resident: any change to these tasks counts. Both default to empty. |
| `continuity` | No | `prefer` (default) resumes the provider conversation when the adapter can. `off` starts every cycle fresh. |
| `probe` | No | Boolean, default `false`: whether the mandate runs interactively as a probe. |

Unknown keys are refused, also inside `cadence` and `watch`. A mandate that
fails these rules shows as `invalid` and cannot be widened.

### When a resident wakes

Besides the tasks in `watch`, a resident watches the tasks its cycles acted
on until they close. For those it reacts only to a change of state: a task
blocking, entering review, getting a review decision, finishing or being
rejected, or one of its decision requests being answered or expiring. Edits,
retried runs and moves between proposed, backlog and in-progress do not wake
it. An incident or maintenance notice opening or clearing, an answered
request on the assignment, a comment on the assignment and a mandate or
authority change also wake it. Info notices do not.

Each cycle's checkpoint plans the next wake in `next_wake.not_before`. Without
a change that wakes it sooner, the next cycle starts then, but never sooner
than `floor_minutes` and never later than `heartbeat_minutes` after the last
cycle started. A cycle that ended `incomplete` waits for the heartbeat. With
a 20-minute heartbeat and nothing to react to, a resident runs at most three
cycles an hour.

## Inspect a mandate

```bash
orbit resident list
orbit resident list --all
orbit resident show <task id>
```

`list` prints each mandate's task ID, state, effective rights and workspaces,
open grant requests and goal. Mandates whose assignment is done, archived or
rejected appear only with `--all`. `show` reports one mandate in full.

| State | Meaning |
|---|---|
| `active` | The mandate is valid and the assignment is open. Marked `(stalled)` when it is [stalled](#stalled-residents). |
| `handed_off` | The assignment is in review. |
| `paused` | `pause` stopped new cycles until `resume`. |
| `taken_over` | A person took the mandate over. No cycle fires until `release`. |
| `ended` | The assignment is done, archived or rejected, or the mandate was stopped. It takes no widening and no relay. |
| `invalid` | The task is not tagged `resident`, or the mandate is missing or invalid. `state_reason` says which. |

`show --json` returns `state`, `state_reason`, `control`, `stalled`,
`stalled_reason`, `mandate`, `effective_authority`, `authority`,
`open_requests`, `latest_checkpoint` and, while a cycle is live, `live_cycle`.
`mandate_error` or `authority_error` appears when that record cannot be read.
`list --json` adds each mandate's deadline, authority revision and `stalled`
flag.

## Authority

| Right | Lets the resident |
|---|---|
| `dispatch` | Start or requeue leaf runs and drains. |
| `promote` | Move a `proposed` task to `backlog`. |
| `merge` | Land a reviewed pull request. |
| `plugin_upgrade` | Install or upgrade a workspace plugin. |

The effective authority is what the resident can act on now: the granted
rights, in the granted workspaces that the mandate also lists. It is empty for
an `ended` or `invalid` mandate.

| Caller | Widen | Narrow |
|---|---|---|
| Operator at a terminal on this host | Applies at once | Applies at once |
| Any other caller, including an agent or MCP session | Raises a request that a human answers | Applies at once |

One call either widens or narrows. Make two calls to do both.

### Widen the authority

As the operator at a terminal on this host:

```bash
orbit resident grant <task id> --add dispatch --add promote --add-workspace orbit
```

`--add` names a right and `--add-workspace` a workspace that the mandate lists
and this host can resolve. Both repeat. Naming only what the authority already
holds gives the outcome `unchanged`.

From any other caller, the call must carry `--quote` with the human's words,
verbatim, up to 2000 characters:

```bash
orbit resident grant <task id> --add merge --quote "Merge reviewed release pull requests in orbit."
```

This changes nothing yet. Orbit raises one `resident_authority` decision
request with the rights and workspaces asked for, the quote and the relaying
session, and the outcome is `requested`. An identical open request is reused.
Without `--quote` the call is refused. Do not set `ORBIT_OPERATOR` here: it
applies the widening at once as the operator.

The human answers the request. Agents can list and show requests but not answer
them:

```bash
orbit decision list --gate resident_authority
orbit decision show <request id>
orbit decision answer <request id> grant
```

`grant` adds exactly what was requested; `reject` changes nothing. Any authority
change, even a narrowing, supersedes an open request, which then cannot be
answered: raise a new one. See [Decision channels](/how-to/decision-channels/).

The dashboard's Residents view widens only when the server runs with
`--operator`, and only with a quote: it raises the same request and answers it
`grant` in one action. Without `--operator` the dashboard cannot widen, narrow,
control or relay, and returns `authorization_denied`.

### Narrow the authority

Narrowing needs no quote and raises no request:

```bash
orbit resident grant <task id> --remove merge
orbit resident grant <task id> --drop-workspace orbit
orbit resident grant <task id> --deadline 2026-12-01T00:00:00Z
```

| Flag | Effect |
|---|---|
| `--remove <right>` | Takes a right away. Repeats. |
| `--drop-workspace <workspace>` | Drops a workspace from the scope. Repeats. |
| `--deadline <instant>` | Sets an earlier deadline, or a first one, and rewrites it in `resident/mandate.json`. To move a deadline later, edit the artifact. |

Removing a workspace from the mandate's `workspaces` drops it from the
effective authority, but the authority record keeps it until you drop it.

### Audit history

Each applied change appends a `resident_authority_changed` event to the
assignment's history, with its `revision`, `at`, `by`, what changed, and `via`:
`operator`, `decision:<request id>` (with the quote and the relaying session)
or `narrowing`. `orbit resident show` reports them as `authority.granted` and
`authority.narrowed`. If that history is unreadable or inconsistent, `show`
reports `authority_error` and every change is refused.

### Over MCP

The `resident` MCP role and toolset decide which tools a session lists. They
grant no authority: an MCP session is never the operator for a widening, even
with operator capability. See [MCP integration](/how-to/mcp-integration/#toolsets).

| Tool | Arguments |
|---|---|
| `orbit.resident.grant` | `task_id`, `add`, `workspaces` (`--add-workspace`), `remove`, `drop_workspaces`, `deadline`, `quote` |
| `orbit.resident.relay` | `task_id`, `quote`, `source`, `request_id`, `reverses` |
| `orbit.resident.control` | `task_id`, `action`, `reason`, `status_request`, `by_session` |
| `orbit.resident.list` | `include_ended` (`--all`) |

The toolset also carries `orbit.resident.show`, `orbit.resident.checkpoint`,
`orbit.resident.metrics`, `orbit.resident.digest`, `orbit.evolution.add`,
`orbit.evolution.list` and `orbit.evolution.show`.

## Enable the cycle routine

Each workspace is seeded with a disabled `resident-cycle-<workspace>` routine.
Until an operator enables it, no mandate in that workspace starts a cycle.

```bash
orbit routine list
orbit routine enable resident-cycle-<workspace>
```

Run it from that workspace's checkout or add `--workspace <workspace>`.
`orbit routine disable` turns it off again. Both are operator-only and audited,
so an agent or MCP session cannot start a resident by itself. In the dashboard,
press **Enable** on the routine under **Automation**; the server must run with
`--operator`.

If cycles still do not start, `orbit routine show resident-cycle-<workspace>`
reports the effective state:

- A host-local pause holds the routine until `orbit routine resume <name>`.
- A replica checkout does not schedule it. Enable it on the machine that owns
  the workspace.

## Relay a human's words

```bash
orbit resident relay <task id> --quote "Ship CSV first." \
  --source '{"human":"daniel","via":"chat","said_at":"2026-11-02T10:00:00Z"}'
```

`relay` writes one `resident.answer` comment on the assignment, which wakes the
resident on its next due check. The quote is kept verbatim, up to 4000
characters. The reply holds the `task_id`, the `comment` (its 1-based `index`,
`at` and `by`) and the `answer` record.

| Flag | Purpose |
|---|---|
| `--quote` | The human's words. |
| `--source` | Provenance JSON: `human`, `via` (the channel), `said_at` and an optional `session`. Orbit records the relaying session itself. |
| `--request-id` | The resident request or decision request the words answer. A decision request still needs its own answer. |
| `--reverses` | The resident decision this direction undoes. |

A relay never changes authority: input that names rights, workspaces, a status
or any other field is refused, inside `source` too. Also refused are a quote
that would need redaction, such as a secret or a home directory path, and a
relay to an ended mandate.

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
| `stop` | anything but ended | `ended` | No new cycle starts. Once no cycle is live, the assignment moves to review with a synthesized handoff summary. |
| `wake` | `active` | `active` | Due on the next tick despite the heartbeat, never before the cadence floor. `--status-request` makes the cycle report where the mandate stands even when it is idle. |
| `takeover` | `active`, `paused` | `taken_over` | No cycle fires. Records who took over. |
| `release` | `taken_over` | `active` | Due on the next tick. The cycle reads comments written during the takeover as new. |

`--reason` is required. Any caller that can write the assignment may call
`control`, because no action widens authority. An action that does not fit the
current state is refused. A live cycle is never interrupted: it writes its
checkpoint and ends at its boundary. Each action appends a `resident_control`
event to the assignment's history. If `stop` cannot hand the assignment off,
the reply says why, and you move the assignment yourself.

## Stalled residents

An `active` mandate is stalled when any of these holds:

- It has written no checkpoint for twice its heartbeat, counted from the task's
  creation when it has never written one.
- Its last three checkpoints are all `incomplete`.
- Its workspace's cycle routine is disabled.

`orbit attention` lists each one as a `resident_stalled` item, and
`orbit resident show`, `list` and `digest` report the same flag. Next, run
`orbit resident control <task id> wake --status-request`. With the routine
disabled, `wake` cannot start a cycle: enable the routine or stop the mandate.

## Record a checkpoint

```bash
orbit resident checkpoint <task id> --record '<json>'
```

This appends one `resident.checkpoint` v1 record to the assignment as a
comment, for a cycle that is already live. It does not start a cycle.

- The record's `mandate`, `cycle` and `run_id` must match the live cycle, which
  only Orbit publishes. A malformed record, a stale cycle or a second
  checkpoint for the same cycle is refused.
- The record's `authority` field reports what the cycle observed. It grants
  nothing.
- A record holds at most 64 KiB, 50 actions, 20 decisions, 20 costs and 600
  characters of notes. Notes describe only what changed this cycle and what is
  next. Keep standing state in `requests[]` and `decisions[]`, and operator asks
  in `requests[]`/notices. Action keys have the form
  `resident:<mandate>:<cycle>:<n>`, with `<n>` a positive integer.

Each cycle reads the latest checkpoint in full and up to four earlier summaries
(cycle, outcome and at most 600 characters of notes), with no older-notes tail.
Historical checkpoints with longer notes remain readable.

## Evolution ledger

The ledger records capability gaps and their cost. `add` records an interactive
cost event outside a checkpoint; `list` and `show` inspect the resulting
signatures and records.

```bash
orbit evolution add --event '{"kind":"manual_step","what":"Checked CI by hand","minutes":4,"retries":0,"slot_minutes":0,"wished":"Automatic CI recheck"}'
orbit evolution list --state open
orbit evolution show <evolution-id>
orbit evolution decline <evolution-id> --days 30
```

`decline` suppresses proposals for a signature. It is operator-only and not
available over MCP.

## Digest and metrics

Both commands report on the workspace's resident mandates and write nothing.

### Digest

```bash
orbit resident digest
orbit resident digest --since 7d --top 10
```

The digest shows what a person needs to act on:

| Part | Shows |
|---|---|
| Residents | Each mandate that has not ended: title, state, last checkpoint outcome and open request count, marked `(stalled)` when it applies. |
| Open escalations | Open decision requests that residents raised, and open grant requests, oldest first. |
| Decisions awaiting veto | Reversible decisions from checkpoints in the window that no answer has reversed. `--json` gives each ID. To veto one, [relay](#relay-a-humans-words) the human's words with `--reverses`. |
| Proposals awaiting approval | Proposed tasks linked to evolution records. The digest approves none. |
| Top pains | Unresolved evolution pains from the window, costliest first, with minutes and occurrence, cycle and mandate counts. |
| Metrics | Workspace totals of interventions and escalations over the window. |

| Flag | Effect |
|---|---|
| `--since <window>` | The window for decisions, pains and metrics, in the forms `metrics --since` takes. Default 14 days. The other parts show current state. |
| `--top <n>` | The most pains to show, 1 to 100. Default 5. |

### Metrics

```bash
orbit resident metrics
orbit resident metrics --mandate <task id> --since 2w
orbit resident metrics --baseline <task id>
```

| Flag | Effect |
|---|---|
| `--mandate <task id>` | Reports one mandate. Without it, the report covers every mandate in the workspace and adds `totals`. |
| `--since <window>` | The window start: a span such as `7d`, `24h` or `2w` (units `m`, `h`, `d`, `w`), an RFC 3339 instant or a `YYYY-MM-DD` date. Default 30 days. |
| `--baseline <task id>` | Adds the same measures for an assignment a person worked interactively, for comparison. Alone, it reports only the baseline. |

Each mandate, `totals` and `baseline` get the same four measures:

| Measure | What it counts in the window |
|---|---|
| Interventions | Times a person changed the course of the work: unprompted relays, plus comments, status changes and requeue, cancel, stop or takeover events a person wrote, on the assignment and the tasks it watches. Answers to escalations and grants do not count. |
| Escalation load | Decision requests the resident raised, with the median seconds to an answer, the share answered with the recommended option, and the number that expired unanswered. |
| Reversal rate | The share of the resident's decisions and successful actions, from checkpoints in the window, that a person undid within 7 days: by a relay with `--reverses`, by moving the action's subject to `proposed` or `rejected`, or by recording a revert. |
| Recovery cost | For cycles that ended without a complete checkpoint: the crashed cycles, the actions that ran a second time, and the slot minutes of the recovery cycles. `--json` adds the median and maximum time to the next complete checkpoint. |

Reading the numbers:

- An author is a person when it is `human` or `operator`, or starts with
  `human:` or `human-`.
- Interventions and escalations also show a rate per active mandate-day. Active
  days run from the later of the mandate's creation and the window start to
  now, rounded up, at least one.
- A rate is blank when there is nothing to judge. A baseline with no
  checkpoints reports `not_derivable` for reversal rate and recovery cost.
- A watched task in another registered workspace on this machine contributes
  its history and decision requests, not its comments. A reference no
  registered workspace holds is listed under `references.unreachable`.

`--json` returns `window` and `mandates`, plus `totals` and `baseline` when
they apply. Each measure appears under two keys: `interventions` and
`manual_interventions`, `escalations` and `escalation_load`, `reversals` and
`reversal_rate`, `recovery` and `recovery_cost`.
