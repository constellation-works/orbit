---
title: Path Leases — Design
owner: claude
last_updated: 2026-10-25
last_validated: 2026-10-25
status: Draft
feature: path-leases
doc_role: design
type: design
summary: "Proposed mechanics for path leases: lease rows on the owner's reservation store and lock index, advisory and hard modes, a priority queue, TTL plus heartbeat and release on landing, durable notices read by poll or SSE, MCP/CLI/REST surfaces, and a lease-only token listener for principals without SSH."
tags: [path-leases, locks, admission, reservations, notifications, authentication]
paths: ["crates/orbit-runtime/src/task/locks/**", "crates/orbit-store/src/driver/sqlite/task_reservation_store/**", "crates/orbit-web/src/api/**", "crates/orbit-mcp/**"]
related_features: [conflict-budget, distributed-drain, remote-access, federated-mcp]
related_artifacts: []
---

# Path Leases — Design

This document proposes how path leases work. Nothing in §2–§9 is implemented; §1 is what
exists today and what the proposal reuses. It leaves the pricing of shared paths to
[conflict-budget](../conflict-budget/1_scope.md), and open directions to
[3_vision.md](./3_vision.md).

## 1. What exists today

- **Task reservations.** `orbit.task.locks.reserve` reserves either a task's surface or a list of
  file selectors, with `ttl_seconds` (default 1,800, at most 14,400), and
  `orbit.task.locks.release` releases one by its `reservation-<id>`
  ([commands.rs](../../../crates/orbit-runtime/src/task/locks/commands.rs)). A refused reserve is
  written to the denial log that `orbit codebase` and the pilot scorecard read; 832 such denials
  were recorded on ws_orbit between 2026-10-11 and 2026-10-25, some naming a `reservation` rather
  than a task as the holder.
- **Task context locks.** An `in-progress` or `review` task holds its declared context unless it
  is tagged `no-diff-expected` (`holds_context_lock` in
  [index.rs](../../../crates/orbit-runtime/src/task/locks/index.rs)). Admission for local waves and
  owner pull reads the same index.
- **Path policy.** `workflow.unlocked_context_paths` takes no lock and `workflow.generated_paths`
  regenerates a conflicting file at landing ([CONFIG.md](../../CONFIG.md)). The conflict budget
  proposes `workflow.exclusive_context_paths` and per-path holder caps.
- **Landed paths.** Codebase facts record the paths each landed task changed
  ([scan.rs](../../../crates/orbit-core/src/application/codebase/facts/scan.rs)).
- **Transports.** Federated MCP reaches the owner over SSH stdio; Orbit Web is loopback-only,
  reached remotely through `orbit web connect`, and has no application authentication
  ([remote-access](../remote-access/1_overview.md#security-boundary)). Log tails already stream
  over SSE ([log.rs](../../../crates/orbit-web/src/api/log.rs)).

What a reservation lacks for outside work: a holder that is not a task or run, a non-blocking
mode, waiting instead of refusal, renewal, a tie to the branch whose landing ends it, and any way
to tell the holder that something changed.

## 2. The lease record

A lease is a reservation row with lease columns, stored in the owner's reservation store so the
lock index sees it without a second index ([4_decisions.md](./4_decisions.md#outside-work-and-drain-tasks-share-one-lock-index)).

| Field | Meaning |
|---|---|
| `lease_id` | `lease-<id>`, minted by the owner. |
| `workspace` | The owner workspace; a lease never spans workspaces. |
| `selectors` | `file:` and `dir:` selectors, the same syntax as `context_files`. |
| `mode` | `advisory` or `hard`. |
| `priority` | `low`, `normal`, `high`; capped by the principal's ceiling (§7). |
| `tags` | Free-form, for filtering and for path policy that keys on tags. |
| `principal` | Who holds it (§7), plus the calling machine for attribution. |
| `base` and `branch` | The base branch it works against, and optionally the branch or pull request whose landing releases it (§4). |
| `state` | `queued`, `granted`, `released`, `expired`, `preempted`. |
| `ttl_seconds`, `expires_at`, `heartbeat_at` | Lifetime (§4). |
| `notice_cursor` | The newest notice the holder has acknowledged (§5). |

A task never holds a lease, and a lease never becomes a task. Work that should run through a
drain is filed as a task.

## 3. Granting, queueing and the drain

**Advisory** requests are always granted at once. The lease is recorded as a holder of its paths,
every overlapping holder (lease or task) gets an `overlap` notice, and nothing is refused. Under
the conflict budget an advisory lease counts toward `holders(path)`, so a busy path costs drain
candidates more, but it never makes one wait by itself.

**Hard** requests are granted when no selector overlaps a hard lease or a task context lock,
after path policy:

1. Paths matching `unlocked_context_paths` are dropped from the hard surface, as for tasks.
2. Paths matching `exclusive_context_paths`, once the conflict budget lands, are hard whatever
   mode was asked for.
3. A principal whose policy forbids hard leases (`lease.hard = "never"`, §8) is granted the
   request as advisory and told so in the grant notice.

An overlapping hard request **waits** in the lease queue unless it asked `wait = false`, which
returns today's denial with the holders named. The queue orders by priority, then by age; a
request older than `lease.aging_minutes` moves up one priority level, so a broad refactor is not
starved by a stream of narrow requests.

**Precedence with drains.** Drain admission treats a granted hard lease exactly like a held task
context lock: an overlapping candidate is deferred and the denial names the lease. A queued lease
holds nothing. A lease never preempts an in-progress or review task; when a task holds the
paths, the lease waits for the task to settle or for its own TTL. A higher-priority hard request
may preempt a lower-priority hard *lease* only when `lease.preemption` is on and the requester's
ceiling allows it; the preempted holder gets a `preempted` notice and its lease drops to
advisory rather than vanishing.

**Expansion.** `expand` adds selectors to a granted lease. Free paths are added at once; for
blocked ones the expansion queues while the lease keeps what it holds. Two leases each waiting on
the other's expansion are a deadlock; the owner breaks it by refusing the younger, lower-priority
expansion with a `deadlock` notice.

## 4. Lifetime

- **TTL and heartbeat.** A lease carries `ttl_seconds` (same bounds as reservations) and expires
  unless renewed. `renew` (or any call that names the lease) moves `expires_at` forward. An
  `expiring` notice goes out at 80% of the TTL.
- **Explicit release.** `release` ends it; this is the normal path for interactive work.
- **Release on landing.** A lease bound to a branch or pull request is released when that work
  lands on its base. The owner checks bound leases on its existing sweep cadence: a pull request
  in `merged` state, or the branch head reachable from the base. Squash merges make ancestry
  useless, so a pull-request binding is preferred when the forge has one.
- **Backstop.** Whatever else fails, the TTL ends the lease. Nothing is held by a client that has
  gone away for longer than one TTL.

## 5. Notices

Every lease has a durable notice stream on the owner. A notice has a monotonically increasing
`seq` within the workspace, a `lease_id`, a `kind` and a small payload.

| Kind | When |
|---|---|
| `granted` | A request or expansion is granted, including "granted as advisory". |
| `queued` | A hard request waits; payload names the holders and the queue position. |
| `overlap` | Another holder (lease or task) took paths that overlap this lease. |
| `overlap_landed` | A landed commit on the lease's base changed paths this lease holds; payload names the commit, the paths and the task or lease that landed it. |
| `expiring`, `expired` | TTL warnings and expiry. |
| `preempted`, `deadlock` | §3. |
| `released` | Explicit, on landing, or by the operator. |

`overlap_landed` is computed when codebase facts record a landed commit's paths, which is the same
source the conflict measurements above used. Drain tasks do not receive notices: their rebase
already handles a moved base.

**Push and poll.** A client pushes a request, then reads notices after a cursor:
`notices(after = seq)`. SSE serves the same stream with `Last-Event-ID` as the cursor, so a
dropped connection resumes without loss and poll and stream never disagree. Webhooks are not
part of v1 (§9).

## 6. Surfaces

All surfaces call one runtime operation per verb, so they cannot drift.

| Verb | MCP tool | CLI | REST |
|---|---|---|---|
| request | `orbit.lease.request` | `orbit lease request <selector>… [--hard] [--priority] [--branch] [--ttl] [--no-wait]` | `POST /api/leases` |
| renew | `orbit.lease.renew` | `orbit lease renew <id>` | `POST /api/leases/<id>/renew` |
| expand | `orbit.lease.expand` | `orbit lease expand <id> <selector>…` | `POST /api/leases/<id>/expand` |
| release | `orbit.lease.release` | `orbit lease release <id>` | `DELETE /api/leases/<id>` |
| show, list | `orbit.lease.show`, `orbit.lease.list` | `orbit lease show <id>`, `orbit lease list` | `GET /api/leases[/<id>]` |
| notices | `orbit.lease.notices` | `orbit lease notices [--follow]` | `GET /api/leases/notices?after=` and `GET /api/leases/events` (SSE) |
| check | `orbit.lease.check` | `orbit lease check <path>…` | `POST /api/leases/check` |

`check` answers "may I edit these paths under my lease, and does anyone else hold them?" without
changing state. It is what an editor hook calls: a Claude Code `PreToolUse` hook on `Edit` and
`Write` runs `orbit lease check` for the target path and, outside the session's lease, either
asks for an `expand` or warns. Enforcement stays advisory unless the hook is configured to deny.

The dashboard gains a Leases view: active and queued leases per path, holders, and recent
landing notices. `orbit task locks list` and the denial log show leases beside reservations.

## 7. Authority and authentication

Two ways in, with different authority models.

**Over SSH (phase 1).** Federated MCP and the CLI reach the owner as today. An SSH login is
ownership of the destination ([federated-mcp decision](../federated-mcp/4_decisions.md#an-ssh-login-to-a-destination-is-ownership-of-it)),
so these callers are trusted, and the lease's principal is attribution: the declared person or
agent name plus `--remote-caller-machine-id`. Priority ceilings and `lease.hard` per principal
are **guards**, applied the way `agent_context_declared` guards operator propagation: a managed
agent run gets the `agent` principal class and its ceiling, and nothing stops an operator from
lifting it. That is enough for one person's machines and agents, which is all Orbit has today.

**The lease listener (phase 4).** For principals with no SSH login to the owner — another
person, a cloud agent, CI — the owner can run a separate, opt-in listener that serves only the
lease routes of §6.

- **Bearer lease tokens**, minted by the operator on the owner
  (`orbit lease token create --principal <name> --workspace <ws> [--max-priority] [--hard allow|never] [--expires]`),
  shown once, and stored only as a salted hash. Each token names one principal, its workspaces,
  its verbs (`read` or `lease`), its priority ceiling and an expiry. `orbit lease token revoke`
  takes effect on the next request.
- **Lease-only routes.** The listener's router contains the §6 routes and `/healthz`, nothing
  else, so a token cannot reach a task, run, config or operator route by construction
  ([4_decisions.md](./4_decisions.md#a-lease-token-authorizes-leases-and-nothing-else)).
- **Binding.** The listener binds an address the operator names (a tailnet address in practice)
  and refuses `0.0.0.0` unless TLS is configured on it or `lease.listener.behind_proxy` says a
  terminating proxy is in front. Orbit Web itself stays loopback-only.
- **Ceilings are boundaries here.** Unlike the SSH path, a token holder cannot edit the owner's
  configuration, so its priority ceiling, `hard` permission and workspace scope are enforced.
- **Audit.** Every listener call writes an audit row naming the token's principal and the
  remote address. Failed authentication is rate-limited per address.
- **No browser sessions.** The listener has no cookie or Origin-based session, so it has no
  CSRF surface; the dashboard's Leases view reads through Orbit Web as today.

The listener is the first Orbit surface with application authentication. It needs an STD-05
security review before it ships, and it does not reintroduce caller authorization on any SSH
path.

## 8. Configuration

| Key | Default | Meaning |
|---|---|---|
| `lease.enabled` | `false` | Accept lease requests in this workspace. |
| `lease.default_mode` | `"advisory"` | Mode when a request names none. |
| `lease.hard` | `{ human = "allow", agent = "allow" }` | Per principal class: `allow` or `never` (granted as advisory). |
| `lease.max_priority` | `{ human = "high", agent = "normal" }` | Priority ceiling per principal class; tokens carry their own. |
| `lease.aging_minutes` | `30` | Queue age after which a request moves up one priority level. |
| `lease.preemption` | `false` | Allow a higher-priority hard request to downgrade a lower-priority hard lease. |
| `lease.listener.bind` | unset | Address for the lease listener; unset means no listener. |
| `lease.listener.tls` | unset | Certificate and key paths, required for a non-loopback bind unless `behind_proxy`. |
| `lease.listener.behind_proxy` | `false` | A terminating proxy supplies TLS in front of the listener. |

TTL bounds reuse the reservation limits. Path policy (`unlocked_context_paths`,
`generated_paths`, and the conflict budget's `exclusive_context_paths`) applies to leases
unchanged.

## 9. Phasing

1. **Lease rows and SSH surfaces.** Advisory and hard leases, TTL and renew, explicit release,
   `check`, MCP tools and CLI, drain admission honoring hard leases, leases in
   `orbit task locks list` and the denial log.
2. **Notices.** The notice stream, `overlap` and `overlap_landed`, poll by cursor, SSE on Orbit
   Web, and the Claude Code hook recipe in the skill docs.
3. **Queue and lifetime.** Waiting instead of refusal, aging, expansion, release on landing for
   bound branches and pull requests, and the dashboard Leases view.
4. **Lease listener.** Tokens, the lease-only router, TLS or proxy binding, audit, after an
   STD-05 review.
5. **Later.** Preemption, webhooks with signed payloads, and conflict-budget pricing that weighs
   advisory holders by their measured landing rate.

Each phase is useful alone; phase 1 already makes outside work visible to drains.

## 10. Concerns & Honest Limitations

- **Advisory leases depend on clients.** A session that never asks is still invisible, and one
  that under-declares (footprints miss about a third of touched files today) still collides. The
  editor hook narrows the gap for Claude Code only.
- **Authentication cuts against a recent choice.** Orbit removed its caller-authorization
  machinery because SSH login already is ownership. The listener brings application auth back
  for one narrow surface. It is worth it only if principals without SSH actually appear; until
  then phase 4 should stay unbuilt.
- **Release on landing is heuristic.** Ancestry fails on squash merges and a pull-request
  binding needs a forge. A missed landing is caught only by the TTL, so a careless 4-hour TTL
  holds a hard lease for up to 4 hours.
- **The owner must be reachable.** Leases live on the owner. When it is down, clients should
  proceed as if granted advisory and say so; hard exclusivity is lost for that window.
- **Notices lag landing.** `overlap_landed` fires when codebase facts record the landed paths, not
  at the merge itself.
- **The motivating numbers are one workspace.** They come from ws_orbit's
  `codebase_task_facts`, `task_lock_denials`, `invocations` (`pr_conflict_recovery`), the
  recovery-authority attempt log and the pilot scorecard, read on 2026-10-25. Other repositories
  will differ, and the 31% unnecessary-denial figure covers only the 326 denials whose both sides
  have landed paths recorded.
- **Two lock worlds stay distinct.** A lease cannot be promoted to a task and a task does not
  hold a lease, so moving interactive work into a drain means filing a task and releasing the
  lease by hand.

## Task References

None yet; the design is unfiled.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
