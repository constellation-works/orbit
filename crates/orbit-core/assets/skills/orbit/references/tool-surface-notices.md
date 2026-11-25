# Tools: fleet notices

Contents:

- [Fleet notices](#fleet-notices)

## Fleet notices

A fleet notice is a short note every agent and operator in its scope should see
until it stops being true. It is not a lease notice: lease notices are events on
one path lease. The kinds are a `warn` (the operator is needed: it names the exact
ask), an `incident` (something is broken now), `maintenance` (planned work to
hold off or plan around) or `info`.

A notice never
grants authority, and no delivery gate reads it. An open `maintenance` notice does
narrow resident cycles in its scope to observe-only, as does a failure to read
the notices in scope; a notice never widens a resident's rights (see
[Manager](manager-mandates.md) and [Resident cycle](resident-cycle-act.md)). The workspace owner
holds it.

`orbit.notice.post` takes `kind` and `title` (at most 120 characters), and
optionally `body`, `scope` (`workspace`, the default, or `global`),
`dedupe_key`, `linked_task`, `close_on_linked_done` (default true with a link),
`evidence` (up to 8 `{type, value}`, type `task`, `run`, `friction`,
`pull_request`, `decision`, `url` or `commit`), `hosts` (maintenance only), and
`expires_in_hours` or `expires_at`.

The notice closes when its linked task is
done and is deduplicated by `dedupe_key`. It returns `{notice, outcome}`, and a
lost reply is safe to retry:

- **created** when no open notice of the workspace holds the dedupe key. An
  omitted key is `task:<linked task>`, else the kind and the slugged title.
- **updated** when you may edit the open notice holding the key: its content
  is replaced, `revision` rises only if something changed.
- **corroborated** when you may not: only its report count rises. Post the
  same subject again rather than a new notice when you hit a known problem.

Who you post as comes from your session, never from input:

| Class | Who | May post | Expiry cap |
|---|---|---|---|
| operator | operator capability | anything; closes anything | 720 h |
| orchestrator | an agent session with orchestrator attribution naming a configured crew (`--orchestrator` or a per-call `orchestrator`) | any scope and kind; edits and closes orchestrator- and agent-authored notices, never the operator's | 168 h warn, incident and info, 72 h maintenance |
| agent | a managed run (worker binding or run-stamped `orbit tool run`) | `workspace` scope, `incident` or `info`, with `linked_task` or evidence; edits and closes only its own task's notices | 24 h (also the default) |

A resident cycle posts as the agent of its mandate's assignment, with its own
terms: it may also post `warn`, needs no linked task or evidence, and keeps one
`info` status line under `resident:<mandate>:status` whose expiry Orbit sets to
twice the mandate's heartbeat (naming an expiry there is refused).

Each post of
a resident's notice renews it: a warn lasts 72 h from the post, at most 168 h.
An omitted key on a resident's warn is `resident:<mandate>:` plus the usual
derived key. Keys under `resident:` are refused for every other poster. Its
limits are 10 open non-status notices per mandate, with its mandatory status
line exempt from that count. All open notices, including the status line,
count toward the 20-per-workspace limit; the post rate is 30 an hour per
mandate. See
[Resident cycle](resident-cycle-escalate.md#6-status-line-and-warns).

Anyone else is refused `notice_poster_unidentified`. Defaults when no expiry is
named: warn and incident 72 h, maintenance 12 h, info 168 h. A request beyond the cap is
refused (`notice_expiry_exceeds_cap`), never clamped; one less than five
minutes ahead is `notice_expiry_invalid`.

Limits refuse with
`notice_limit_exceeded` naming the limit: 20 open per workspace, 10 open global
per owner, 2 open per agent task, 5 posts per agent task per day (corroborations
count) and 30 per orchestrator crew per hour.

Other refusals:
`notice_scope_refused`, `notice_kind_refused`, `notice_evidence_required`,
`notice_close_refused`, `notice_not_found` and `notice_invalid_field`; a replica
refuses every notice tool, reads included, with `capability_refused` — address
the owner by its federated selector instead, and a managed agent on a follower
posts through its worker binding's owner route.

A `linked_task` must be a task
the home owner holds, in the home or another of its workspaces; a task on
another host is `notice_invalid_field`. Every write and refusal leaves an
audit row with target type `notice`. The limits and caps are fixed.

A managed `agent_implement`, `agent_review_repair` or `final_recovery` run
carries `orbit.notice.post`, `orbit.notice.show` and `orbit.notice.close` in
its tool baseline. Use `orbit tool run` unless a notice tool is an admitted
requirement or grant on the MCP surface. Such a
run posts only for a problem outside its task that affects other agents,
with a linked task (prefer the fix) or evidence, after checking the brief's
open notices; re-posting the same subject corroborates it.

`orbit.notice.close` takes `id` or `dedupe_key` and a `reason`; the operator
may close any notice, an orchestrator may close its own crew's notices and any
orchestrator- or agent-authored notice, and a managed agent (a resident cycle
included) may close only its own task's notices. A notice that is
already closed or expired comes back unchanged with `already_inactive: true`. A notice
whose linked task reaches `done` closes itself. `orbit.notice.list` filters by
`status` (`open` by default, `closed`, `expired` or `all`), `include_global`,
`scope`, `kind`, `linked_task`, `author_class`, `host` and `limit` (1–200, default 50), and orders
warn, incident, maintenance, info, newest first.

Called without a workspace in a
federated session (or as `orbit notice list` outside any workspace on a host
with registered hosts) it merges every reachable owner's notices, deduplicated
by id and limited once after the merge, with each unreachable owner named in
`degraded`; `all: true` is the per-owner hop that aggregation issues.

Each listed notice carries its kind, scope, title, body, author class, linked
task, status, expiry, report count and effective status; the output is
`{notices, degraded}`, so a shorter list never silently hides an unreachable
owner, and the tool is read-only.

With a workspace, `orbit.notice.list` returns
that workspace's notices plus global notices homed elsewhere on its owner.
`orbit.notice.show` takes `id`, and
for the operator includes the history. Status is computed when read: closed,
then linked task done, then expired, else open.

The CLI mirrors the tools: `orbit notice post --kind <k> --title <t> [--body <text> | --body-file <path>]
[--scope global] [--link <task>] [--keep-after-done] [--evidence type=value]
[--host <machine>] [--expires-in <hours> | --expires-at <time>]`, `close <id>
--reason <why>` (or `--dedupe-key`), `list` and `show <id>`, all with `--json`.
