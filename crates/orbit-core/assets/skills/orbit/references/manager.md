# Acting as a manager for residents

A manager is a role an interactive session takes, not a program and not a
second store. It holds no loop and no state of its own: everything it knows it
reads from Orbit, and everything it does it writes through Orbit tools. Any
session serving the `manager` MCP role can act as manager for the read,
relay, control, and summary steps, and two manager sessions at once are safe
because neither is authoritative. Filing, editing, and commenting on the
mandate assignment need the `tasks-write` toolset, which the plain `manager`
MCP role does not list: run the manager server with that toolset added to
the role (for example `orbit mcp serve --role manager --toolset tasks-write`),
or hand those steps to a session that already has it. Adding the toolset
changes only which tools the server lists and dispatches; it grants no
authority.

The manager does four things for the human: files and scopes mandates, relays
grants, decisions and directions verbatim, controls residents, and summarises
them from checkpoints and the digest. It never widens a resident's authority
on its own, and it never answers a resident's escalation for the human.

For the resident's side of the channel, read
[Resident cycle](resident-cycle.md). For raising decisions, read
[Tool surface](tool-surface.md#raising-a-decision).

## File a mandate

A mandate is an `[orchestration]` assignment task tagged `resident`, carrying
a `resident/mandate.json` artifact, plus a separate authority record. The
mandate says what to achieve; the authority says what the resident may do.

Filing steps 1–2 need the `tasks-write` toolset (`orbit.task.add` and
`orbit.task.artifact.put` belong to it, so a plain `manager` MCP session is
refused with `toolset_not_enabled`); reuse an existing assignment when the
session cannot write, or hand the filing to a session with that surface.

1. File or reuse the assignment in the owning workspace with
   `orbit.task.add`, tagged `resident`. Give it the goal as its description
   and exit criteria the resident can check against task state.
2. Write `resident/mandate.json` with `orbit.task.artifact.put`. Schema
   `resident.mandate/v1`: `goal` (one line), `exit_criteria` (1–8 lines),
   optional `deadline` (RFC 3339), `workspaces` the mandate is about,
   `watch` (`tasks` and `tags` whose changes wake the resident), `crew` (any
   registered crew), `cadence` (`floor_minutes` at least 5, `heartbeat_minutes`
   at least the floor) and `continuity` (`prefer` or `off`).
3. Submit the authority grant with the human's words verbatim (next section).
4. The mandate becomes active when it has a valid mandate artifact and its
   assignment is open (tagged `resident`, control active): a grant is not an
   activation prerequisite. Without granted rights it still runs observe-only
   cycles; granted rights authorize action, not activation.

After attaching a mandate, inspect `cycle_routine_disabled`,
`cycle_routine_name` and the returned warnings. A disabled cycle routine leaves
the mandate stored but unable to act until the routine is enabled. These
signals are present in MCP and in the compact default `orbit tool run` output,
including calls routed with `--host`. `--full` returns the complete tool
response; an explicit `--fields` filter keeps only the requested fields and
any `audit_warning`.

Editing the mandate is an ordinary task write (`orbit.task.update`) and needs
the `tasks-write` toolset, but it is not otherwise gated: an edit cannot
widen authority — the effective scope is the workspaces both the
mandate and the authority record cover. `orbit.resident.show` reports both
records beside the effective authority.

## Relay grants verbatim

Authority rights are `dispatch`, `promote`, `merge` and `plugin_upgrade`.
Anything else — releases, tags, publishing, host changes, config edits,
secrets — is not grantable to a resident.

Use `orbit.resident.grant` with `task_id`, and one side only per call:

- Widen: `add` (rights) and/or `workspaces`, plus `quote` — the human's words
  authorising the widening, unchanged (at most 2000 characters). From any
  caller but a native-terminal operator this changes nothing and raises one
  `resident_authority` decision request that quotes `quote` verbatim and
  names your session. The authority changes only when the human answers
  `grant` through the decision channel. An open request for the same addition
  is returned, not duplicated. Report the outcome (`granted`, `requested` or
  `narrowed`) to the human; never report a request as a grant.
- Narrow: `remove`, `drop_workspaces`, or a shorter `deadline`. Narrowing
  applies at once for any caller that can write the assignment, is recorded
  with that caller, and takes effect at the next cycle boundary. A call may
  not widen and narrow at once; narrow first, then ask to widen.

Never paraphrase, summarise or embellish a grant quote. If the human's words
are ambiguous, ask them rather than guessing: a misread relay costs one
refused decision, not a silent widening.

## Relay directions verbatim

Use `orbit.resident.relay` with `task_id`, `quote` (the human's words,
1–4000 characters, unchanged) and `source` (`{human, via, session?, said_at}`:
who said it, through which channel, and when). Add `request_id` when the
words answer a resident request, and `reverses` when they undo a resident
decision recorded in a checkpoint.

- A relay writes one `resident.answer` comment and wakes the resident. It is
  steering within authority, weighed like task text; the latest relay wins.
- A relay never changes authority. Input naming rights, workspaces or status
  is refused; route an authorisation through `orbit.resident.grant` instead.
- A relayed answer to an escalation is a direction, not the decision answer:
  the human still answers the decision request itself. The manager only
  reminds.
- Text that would need redaction is refused, never rewritten, so the words
  stay verbatim. Do not work around a refusal by editing the quote.

## Read checkpoints and the digest

Residents report only through Orbit; there is no inbox to check.

- `orbit.resident.list` — every mandate in the workspace: state, goal,
  deadline, effective rights and workspaces, authority revision and open
  grant request count. Assignments that are done, archived, or rejected are
  hidden unless `include_ended` is set; a stopped mandate (state `ended`)
  whose assignment moved to `review` is still listed without it.
- `orbit.resident.show` with `task_id` — the mandate, the effective authority
  with its grant and narrowing history, open grant requests, the live cycle,
  and the latest checkpoint: its outcome, actions, decisions, open requests
  and costs. This is the status answer; quote the checkpoint, do not
  reconstruct progress from chat.
- `orbit.resident.digest` — the derived manager view: resident state and
  stalled flags, open escalations oldest first, reversible decisions still
  awaiting veto, the top recurring pains by cost, evolution proposals awaiting
  human approval, and metrics totals. Surface it at the start of a manager
  session, on a regular cadence the human chooses, and on request.
- `orbit resident metrics` (`orbit.resident.metrics`) — the widening measures
  behind widening decisions, derived read-only from task history, decision
  requests, checkpoints and audit rows. It reports manual interventions (per
  mandate and per active mandate-day), escalation load (per mandate-day and
  per human-day, median answer latency, share answered with the recommended
  option, expired-unanswered), reversal rate (resident decisions and actions
  a human undid within 7 days, each counted once), and crashed-cycle recovery
  cost (cycles without a checkpoint, time to the next good checkpoint,
  recovery slot minutes, repeated actions, human interventions needed). It
  covers every resident mandate in the workspace with workspace totals unless
  scoped, and it writes nothing. It is part of the `resident` toolset.

Run it as `orbit resident metrics [--mandate <MANDATE>] [--since <SINCE>]
[--baseline <BASELINE>]`, or call `orbit.resident.metrics` with the same
`mandate`, `since` and `baseline` inputs. `--mandate` limits the report to
one mandate's assignment task. `--since` sets the window as a span such as
`7d`, `24h` or `2w`, or an RFC 3339 instant; without it the window is the
last `30d`. `--baseline` names an interactive assignment task and computes
the same measures for it, so the resident's numbers can be compared against
unmanaged work; a baseline measure that cannot be derived reports
`not_derivable`, never zero.

The digest is the operational view for the shift — state, open escalations,
reversible decisions awaiting veto, top pains, pending proposals, plus
metrics totals. The metrics report is the evidence behind those totals:
per-mandate and per-day rates to read before a widening decision. To verify,
run `orbit resident metrics --json` and read the returned measures.
- `orbit.attention` — everything waiting on the human, including a stalled
  resident (`resident_stalled`: no checkpoint for twice its heartbeat,
  three `incomplete` checkpoints in a row, or its workspace's cycle routine
  disabled — no cycle can fire at all then, however recently it
  checkpointed) and a handed-off mandate waiting
  in `review` (`review_waiting`). Read `degraded` before trusting an empty
  result; see [Attention](attention.md).

If the latest checkpoint is staler than the human wants, ask for a fresh one
with `orbit.resident.control` action `wake` and `status_request: true`; the
next cycle then checkpoints even when idle. The cadence floor still applies.

## Control a resident, and take one over

`orbit.resident.control` takes `task_id`, `action`, a non-empty `reason`
(recorded in the assignment's history with the caller), optional
`status_request` (only with `wake`) and optional `by_session`. Every action
narrows or restores firing under the existing authority; none widens it.

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

To take a mandate over: `takeover` with `by_session` naming your session,
then work in the same assignment task and comment as usual — no export or
import. Working and commenting in the assignment needs the `tasks-write`
toolset (`orbit.task.update` and comments through it); a plain `manager` MCP
session must hand the takeover work to a session with that surface. When you
hand it back, `release`; the next cycle reads the
takeover-period comments as new non-resident comments and resumes from them.
While taken over, do not act as if a cycle will run: nothing fires.

## Boundaries

- The manager files `proposed` work, relays, narrows and controls. Dispatch,
  promotion and merging belong to the resident within its grant, or to the
  normal pipeline — not to the manager improvising outside the mandate.
- Post a fleet notice with `orbit.notice.post` when the human broadcasts
  something every agent in scope should see. An open `maintenance` notice in
  a covered workspace narrows every affected resident to observe-only until
  it closes; a notice never widens anyone.
- Repository-specific operating know-how stays in the consuming repository's
  own skills. This reference is only the provider-neutral manager procedure.
