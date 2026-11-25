# Acting as a manager: checkpoints, digest and control

Contents:

- [Read checkpoints and the digest](#read-checkpoints-and-the-digest)
- [Control a resident, and take one over](#control-a-resident-and-take-one-over)

## Read checkpoints and the digest

Residents report only through Orbit; there is no inbox to check.

- `orbit.resident.list` — every mandate in the workspace: state, goal,
  deadline, effective rights and workspaces, authority revision and open
  grant request count. Assignments that are done, archived, or rejected are
  hidden unless `include_ended` is set; a stopped mandate (state `ended`)
  whose assignment moved to `review` is still listed without it.
- `orbit.resident.show` with `task_id` — the mandate artifact (or why it is
  missing or invalid), the effective authority (rights and workspaces both the
  authority and the mandate cover) with its grant and narrowing history, open
  grant requests, the live cycle, the status line (the mandate's one `info`
  notice: title, body, when it was posted and lapses, and stale once lapsed),
  the mandate's state (`active`, `handed_off`, `ended` or `invalid`), and the
  latest checkpoint: its outcome, actions, decisions, open requests and costs.
  Read-only. This is the status answer; quote the checkpoint, do not
  reconstruct progress from chat.
- `orbit.resident.digest` — the derived manager view (read-only; defaults to
  `14d` and the top `5` pains): resident state, latest outcome, open requests
  and stalled flag; open escalations oldest first; reversible decisions in the
  window still awaiting veto; top recurring evolution pains by minutes plus slot
  minutes with occurrence, cycle and mandate counts; evolution proposals
  awaiting human approval; and resident metrics totals. Surface it at the start
  of a manager session, on a regular cadence the human chooses, and on request.
- `orbit resident metrics` (`orbit.resident.metrics`) — the widening measures
  behind widening decisions, derived read-only from task history, decision
  requests, checkpoints and audit rows.
  - It reports:
    - manual interventions (per
      mandate and per active mandate-day),
    - escalation load (per mandate-day and
      per human-day, median answer latency, share answered with the recommended
      option, and the answered, open, expired-unanswered and superseded counts),
    - reversal rate (resident decisions and actions a human undid within 7 days,
      each counted once), and
    - crashed-cycle recovery cost (cycles without a
      checkpoint, time to the next good checkpoint, recovery slot minutes, repeated
      actions, human interventions needed).
  - Unscoped, it covers the workspace's
    `resident`-tagged mandates whose active span overlaps the window, with
    workspace totals. `--mandate` reports the named task instead, tagged or not,
    when it carries the mandate artifact, with totals over that one mandate. It
    writes nothing. It is part of the `resident` toolset.

Run it as `orbit resident metrics [--mandate <MANDATE>] [--since <SINCE>]
[--baseline <BASELINE>]`, or call `orbit.resident.metrics` with the same
`mandate`, `since` and `baseline` inputs. `--mandate` limits the report to
one mandate's assignment task.

`--since` sets the window as a span such as
`7d`, `24h` or `2w`, or an RFC 3339 instant; without it the window is the
last `30d`. `--baseline` names an interactive assignment task and computes
the same measures for it, so the resident's numbers can be compared against
unmanaged work; a baseline measure that cannot be derived reports
`not_derivable`, never zero.

The digest is the operational view for the shift — state, open escalations,
reversible decisions awaiting veto, top pains, pending proposals, plus
metrics totals. The metrics report is the evidence behind those totals:
per-mandate and per-day rates to read before a widening decision.

Per-mandate-day
rates divide by each mandate's active days; the per-human-day escalation rate
divides by the window's calendar days, because one person handles escalations
across the whole window. To verify, run `orbit resident metrics --json` and
read the returned measures.
- `orbit.attention` — everything waiting on the human, including a stalled
  resident (`resident_stalled`: no checkpoint for twice its heartbeat,
  three `incomplete` checkpoints in a row, or its workspace's cycle routine
  disabled — no cycle can fire at all then, however recently it
  checkpointed) and a handed-off mandate waiting
  in `review` (`review_waiting`). Read `degraded` before trusting an empty
  result; see [Attention](attention-reading.md).

If the latest checkpoint is staler than the human wants, ask for a fresh one
with `orbit.resident.control` action `wake` and `status_request: true`; the
next cycle then checkpoints even when idle. The cadence floor still applies.

## Control a resident, and take one over

`orbit.resident.control` takes `task_id`, `action`, a non-empty `reason`
(recorded as a history entry with the caller), optional `status_request` (only
with `wake`: makes the cycle checkpoint even when idle) and optional
`by_session`. Any caller that can write the assignment may call it; every action
narrows or restores firing under the existing authority and none widens it.
Output is `{task_id, action, state, effective_at}`.

| Action | From | Effect |
|---|---|---|
| `pause` | `active` | No new cycle starts |
| `resume` | `paused` | Firing restored; due on the next tick |
| `wake` | `active` | Due on the next tick, ignoring the heartbeat |
| `takeover` | `active`, `paused` | No cycle fires until `release` |
| `release` | `taken_over` | Firing restored; due on the next tick |
| `stop` | any but ended | Mandate ends; assignment moves to `review` once no cycle is live |

An action that does not apply to the current state is refused and writes
nothing. On an invalid mandate (`orbit resident show` reports
`state: invalid`, for example a missing or unreadable
`resident/mandate.json`), `pause`, `resume`, `wake`, `takeover` and
`release` are refused; `stop` stays allowed from any but ended, so a broken
mandate can be disposed of.

Pause and stop do not cancel a live cycle: it keeps its frozen grant,
checkpoints (or receives a synthesized checkpoint), and exits at its boundary.
The deadline is the exception: a live cycle's next right-gated write reads the
current deadline, so a passed deadline, including one shortened during the
cycle, refuses that write while the cycle can still checkpoint for handoff.

To take a mandate over: `takeover` with `by_session` naming your session,
then work in the same assignment task and comment as usual — no export or
import. Working and commenting in the assignment needs the `tasks-write`
toolset (`orbit.task.update` and comments through it); a plain `manager` MCP
session must hand the takeover work to a session with that surface.

When you
hand it back, `release`; the next cycle reads the
takeover-period comments as new non-resident comments and resumes from them.
While taken over, do not act as if a cycle will run: nothing fires.
