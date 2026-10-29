---
title: Path Leases — Design
owner: claude
last_updated: 2026-10-29
last_validated: 2026-10-26
status: Draft
feature: path-leases
doc_role: design
type: design
summary: "Proposed mechanics for path leases: lease rows on the owner's reservation store and lock index, advisory and hard modes, a priority queue, TTL plus heartbeat and release on landing, durable notices read by poll or SSE, MCP/CLI/REST surfaces, and a lease-only token listener, run as its own OS user with credential code in an orbit-auth crate, so several people and their agents coordinate on one owner."
tags: [path-leases, locks, admission, reservations, notifications, authentication]
paths: ["crates/orbit-runtime/src/task/locks/**", "crates/orbit-store/src/driver/sqlite/task_reservation_store/**", "crates/orbit-web/src/api/**", "crates/orbit-mcp/**", "crates/orbit-auth/**"]
related_features: [conflict-budget, distributed-drain, remote-access, federated-mcp]
related_artifacts: []
---

# Path Leases — Design

This document proposes how path leases work. The lease row of §2, the advisory/hard grant rules
of §3, the SSH principal of §7 and the phase-1 keys of §8 are implemented as
runtime operations, CLI, MCP and loopback REST surfaces (see §6). Phase 3 durable notices,
including poll and SSE replay on Orbit Web and the lease listener, owner-clock lifetime
handling, the phase-4 hard-lease queue with aging and expansion, and the dashboard Leases view
are implemented; preemption remains later work. §1 is what exists today and what the proposal reuses. It leaves the pricing of shared paths to
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
| `state` | `queued`, `granted`, `released` or `expired`. Preemption is later work (§9). |
| `ttl_seconds`, `expires_at`, `heartbeat_at` | Lifetime (§4). |
| `notice_cursor` | The newest notice the holder has acknowledged (§5). |

A task never holds a lease, and a lease never becomes a task. Work that should run through a
drain is filed as a task.

**Stored so far.** Schema v61 (`task_reservation_leases`) adds `kind` (`task_reservation` by
default, or `lease`) and `mode` (`hard` by default, or `advisory`) to `task_reservations`, plus
`principal`, `principal_class`, `machine_id`, `priority`, `tags_json`, `base_branch`,
`bound_branch` and `heartbeat_at`. A lease row keeps `scope = 'files'`, its selectors in
`files_json`, no task ids and no owner run, so stale-reservation cleanup never touches it. The
store inserts (refusing a hard lease that overlaps a task reservation or hard lease), renews,
releases, reads and lists leases, and lazy expiry ends them as it ends reservations. `state` is
derived: `queued` while an active request waits, `granted` while active otherwise, `released` or
`expired` from `release_reason`. Schema v67
(`task_reservation_lease_queue`) adds `queued_files_json`, `queued_at` and `queue_kind`
(`request` or `expansion`). A queued request keeps `files_json` empty, so every overlap reader
skips it without a new filter; a queued expansion keeps its held selectors there and its waiting
ones in `queued_files_json`. Schema v65 (`lease_notices`) adds `notice_cursor`
(default 0, reserved for explicit holder acknowledgement; polling and SSE never advance it)
and an append-only owner notice table. Sequence numbers increase across the owner database,
so they increase within each workspace and may have gaps. Existing leases start with no
backfilled grant; new grants, release and lazy expiry write their notices transactionally. Every overlap reader (owner pull
admission, `orbit.task.locks.reserve` and the gate's reservation check) skips advisory rows and
names a hard lease as `held_by: "lease"`. `orbit task locks list` lists every active lease,
advisory and hard, beside reservations in `by_lease`/`total_leases`, rendering each with its
mode and principal.

The migration is additive. A binary without it reads every lease row, advisory or hard, as an
exclusive file reservation and refuses overlapping reserves (checked against 0.33.0, the
previous release), and the rows it writes read here as hard task reservations.

**Runtime operations.** `crates/orbit-runtime/src/task/locks/lease/` holds one function per
verb: `request` in `lease/request.rs`, and `renew`, `release`, `show`, `list`, `check` and
`notices` in `lease/operations.rs`, each refused naming `lease.enabled` while it is false, and
`lease/queue.rs` holds `expand` and the grant pass.
`request` applies §3: advisory is granted and names the holders it overlaps; hard drops
`unlocked_context_paths` from the stored surface and, when what is left overlaps a task context
lock, task reservation, hard lease or a queued request ranked ahead of it, queues with the holders
named (or, with `wait = false`, is refused naming them); a class set to `lease.hard = "never"` is
granted advisory with a notice; a priority above the class ceiling is refused. `check` reads
without expiring or auditing anything. Grant, queueing, refusal, expansion, deadlock refusal,
renewal, release and lazy expiry each write a `lease` audit event.

## 3. Granting, queueing and the drain

**Advisory** requests are always granted at once, within a lease token's resource bounds
(§7.2). The lease is recorded as a holder of its paths,
every overlapping lease holder gets an `overlap` notice (drain tasks never receive notices), and nothing is refused. Under
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
request older than `lease.aging_minutes` moves up one priority level, capped at `high`. Aging does
not outrank newer higher-priority requests, so a stream of overlapping `high` requests can hold an
aged `low` request until its TTL expires.

**Precedence with drains.** Drain admission treats a granted hard lease exactly like a held task
context lock: an overlapping candidate is deferred and the denial names the lease. A queued lease
holds nothing. A hard lease never preempts a task or another hard lease. When a task or hard lease
holds the paths, a new request waits in the queue or, with `wait = false`, is refused. Lease
preemption remains later work (§9).

**Expansion.** `expand` adds selectors to a granted lease. Free paths are added at once; for
blocked ones the expansion queues while the lease keeps what it holds. Two leases each waiting on
the other's expansion are a deadlock; the owner breaks it by refusing the expansion ranked
last in queue order (lowest effective priority, then youngest) with a `deadlock` notice.

**Implemented queue.** The owner store keeps queue order transactionally: rank is the priority
plus one level once `now - queued_at` reaches `lease.aging_minutes` (capped at `high`), then the
older `queued_at` goes first. A new request also waits behind any queued request that is ranked
at or above it and wants an overlapping path, so a later arrival never jumps a waiter. A grant
pass walks the queue in order and grants each waiter whose paths are free of task context locks,
task reservations, hard leases and the waiters still ahead of it; an expansion takes whichever of
its waiting paths are free and keeps waiting for the rest. The pass runs after a release, after
any operation that lazily expires a lease, and on every owner clock tick, which is how a task
settling or a task reservation's release reaches the queue; between ticks a waiter stays queued.
A request granted from the queue keeps the TTL it asked for, counted from the original request,
so an abandoned waiter still ends at its TTL. The deadlock check follows edges from an expansion's
waiting paths to the hard lease that holds them; on a cycle the expansion ranked last (lowest
priority, then youngest) loses its waiting paths, keeps what it holds and gets the `deadlock`
notice.

## 4. Lifetime

- **TTL and heartbeat.** A lease carries `ttl_seconds` (same bounds as reservations) and expires
  unless renewed. `renew` moves `expires_at` forward. An
  `expiring` notice goes out at 80% of the TTL.
- **Explicit release.** `release` ends it; this is the normal path for interactive work.
- **Release on landing.** A lease bound to a branch or pull request is released when that work
  lands on its base. The owner checks bound leases on its existing sweep cadence: a pull request
  in `merged` state, or the branch head reachable from the base. Squash merges make ancestry
  useless, so a pull-request binding is preferred when the forge has one.
- **Backstop.** Whatever else fails, the TTL ends the lease. Nothing is held by a client that has
  gone away for longer than one TTL.

**Implemented lifetime handling.** Each non-dry-run owner clock tick expires elapsed leases
and writes an `expiring` notice once at or after 80% of the interval from `heartbeat_at` to
`expires_at`. Renewal starts a new interval and permits one new warning. Maintenance also
runs during a host shutdown admission hold, and never runs in a replica checkout. Reads do
not renew leases; renewal remains explicit.

The existing `branch` / `bound_branch` binding accepts a local branch name or an explicit
`#<pull-request-number>`. A branch lookup prefers the newest forge PR for that branch,
including open or closed-unmerged PRs: only `merged` on the lease's base releases it. A PR
on a different base does not permit ancestry fallback. Without a PR,
the local `refs/heads/<branch>` head must be reachable from `refs/heads/<base>`.
Missing refs, an unavailable forge or a failed lookup leave the lease to its TTL; the clock
does not fetch refs. Explicit PR bindings never fall back to ancestry. A landing releases
the row with reason `landed` and transactionally appends a `released` notice with that reason.
The current tick finishes its local expiry/warning transaction before checking the forge,
and a failed landing check does not stop checks for the remaining leases.

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
| `deadlock` | §3. |
| `released` | Explicit, on landing, or by the operator. |

`overlap_landed` is computed when codebase facts record a landed commit's paths, which is the same
source the conflict measurements above used. Drain tasks do not receive notices: their rebase
already handles a moved base.

**Push and poll.** A client pushes a request, then reads notices after a cursor:
`notices(after = seq)`. SSE serves the same stream with `Last-Event-ID` as the cursor, so a
dropped connection resumes without loss and poll and stream never disagree. Webhooks are not
part of v1 (§9).

**Implemented notices.** `granted` includes the effective mode, principal and selectors;
`overlap` identifies the new lease, reservation or task and the overlapping paths.
`released` and `expired` are emitted exactly once at their state transition. Landed facts
scans emit `overlap_landed` once per lease and commit, only for a local base ref containing
that commit and a lease created before the landing. Payloads name the commit, changed
paths and delivering task. Both sides of renames count. Tasks receive no notices.

`notices` accepts `after` (exclusive, default 0), optional `lease_id` and `limit`
(default 100, range 1–500). It returns `{notices, cursor}` with the last returned sequence,
or the input cursor on an empty page. Repeated reads acknowledge nothing and do not expire
leases. `orbit lease notices --follow` drains pages then polls, with JSONL in JSON modes.
Web events emit each notice as JSON with its `seq` as the SSE ID, using `Last-Event-ID`
in preference to `after`, and a `: keep-alive` comment for each empty page (about one a
second while idle). Invalid cursors are refused. Orbit Web's own streams are bounded to eight
concurrent clients; the listener's count toward its limits (§7.3). Streams close on
disconnect or server shutdown. Both servers serve these routes from `lease_router`, framing
the same pages, and the listener's pages are read on the owner by the same page loop.

## 6. Surfaces

All surfaces call one runtime operation per verb, so they cannot drift.

**Phase 1 CLI and MCP.** Request, renew, release, show, list and check are
implemented as the `orbit.lease.*` tools and `orbit lease` commands (ORB-18406),
over the lease rows (ORB-18404) and runtime operations (ORB-18405). All six are
owner control-plane operations: reads answer for the same owner lock state as
writes. Identified agents, operators and runners may call them; workspace lease
policy still sets mode and priority ceilings. Show and list can expire stale
leases, so they are mutating tools; check is read-only. `orbit task locks list`
adds separate `by_lease` and `total_leases` fields beside its task/reservation
projections and renders mode and principal in human output.

Request uses `<selector>... --principal <name> [--mode advisory|hard | --hard]
[--priority low|normal|high] [--ttl 30m] [--branch <name>] [--base <name>]`.
Renew takes `<lease-id> [--ttl 30m]`; check takes `<path>... [--lease-id <id>]`.
Every verb accepts `--json`; a hard request that is refused or queued, and an expansion refused
to break a deadlock, exit 3. TTL is 1–14400 seconds,
default 1800. Branch bindings are recorded only. The table below also includes
later-phase surfaces; `notices` is added in phase 3 and `expand`, queueing and `--no-wait`
in phase 4.

| Verb | MCP tool | CLI | REST |
|---|---|---|---|
| request | `orbit.lease.request` | `orbit lease request <selector>… [--hard] [--priority] [--branch] [--ttl] [--no-wait]` | `POST /api/leases` |
| renew | `orbit.lease.renew` | `orbit lease renew <id>` | `POST /api/leases/<id>/renew` |
| expand | `orbit.lease.expand` | `orbit lease expand <id> <selector>…` | `POST /api/leases/<id>/expand` |
| release | `orbit.lease.release` | `orbit lease release <id>` | `DELETE /api/leases/<id>` |
| show, list | `orbit.lease.show`, `orbit.lease.list` | `orbit lease show <id>`, `orbit lease list` | `GET /api/leases[/<id>]` |
| notices | `orbit.lease.notices` | `orbit lease notices [--follow]` | `GET /api/leases/notices?after=` and `GET /api/leases/events` (SSE) |
| check | `orbit.lease.check` | `orbit lease check <path>…` | `POST /api/leases/check` |

**Implemented loopback REST surface.** Orbit Web serves request, renew, release, show, list
and check, plus notices polling and SSE replay, through the same runtime operations and returns their JSON unchanged. An overlapping
hard request returns `granted: false` with the runtime's typed `conflicts`, naming each holder
by kind and ID, and `queued: true` with its `queue_position` unless it asked `wait: false`.
Expansion returns `added`, `queued`, `conflicts`, `queue_position`, `deadlock` and the lease;
notices and events are implemented on Orbit Web and the lease listener. Select a workspace with `?workspace=<id>` or use the server's
default. Request and check bodies are the corresponding operation's JSON input; renew accepts
its TTL fields and permits an omitted body to use the default TTL. Expansion requires
`selectors` in its body. For show, renew and release the URL supplies `lease_id`, overriding a body ID.
Request declares `principal` in its JSON body or query string; other verbs may declare it too.
If both body and query declare a principal, they must match. Release accepts query attribution.
Writes (request, renew, expand and release) require the dashboard's operator session and its
normal matching loopback Host/Origin headers. Check remains readable without the operator session,
even though its HTTP method is POST; its Origin check still applies.

`orbit_web::lease_router(backend)` exposes only these `/leases` routes for nesting under `/api`.
Its public `LeaseBackend` receives `LeaseCaller` (workspace and declared principal) separately
from the operation JSON and executes on the blocking pool. The dashboard adapter selects its
runtime from a pinned workspace snapshot and enforces operator write authority. Other servers
supply their own backend and authentication/Origin policy. The router also serves
`GET /leases/notices` and `GET /leases/events`; a backend reads one notice page and opens a
stream of pages, admitting its own streams. A server that authenticates puts a
`LeaseCallerToken` in the request's extensions; the router then takes the principal from it and
ignores declared ones. The lease listener (§7.3) is that server.

`check` answers "may I edit these paths under my lease, and does anyone else hold them?" without
changing state. It is what an editor hook calls: a Claude Code `PreToolUse` hook on `Edit` and
`Write` runs `orbit lease check` for the target path and, outside the session's lease, either
asks for an `expand` or warns. Enforcement stays advisory unless the hook is configured to deny.

The dashboard gains a Leases view: active and queued leases per path, holders, and recent
landing notices. **Implemented:** `#leases` reads `GET /api/leases` and pages
`GET /api/leases/notices` through Orbit Web, with no operator session or lease token, and is
read-only ([user-interface design §9b](../user-interface/2_design.md#9b-leases)). `orbit task locks list` and the denial log show leases beside reservations.

## 7. Authority and authentication

Two ways in, with different authority models.

**Over SSH (phase 1).** Federated MCP and the CLI reach the owner as today. An SSH login is
ownership of the destination ([federated-mcp decision](../federated-mcp/4_decisions.md#an-ssh-login-to-a-destination-is-ownership-of-it)),
so these callers are trusted, and the lease's principal is attribution: the declared person or
agent name plus `--remote-caller-machine-id`. Priority ceilings and `lease.hard` per principal
are **guards**, applied the way `agent_context_declared` guards operator propagation: a managed
agent run gets the `agent` principal class and its ceiling, and nothing stops an operator from
lifting it. This serves the operator's own machines and agents.

**The lease listener (phase 2).** Leases exist so that several people, and their agents, can
coordinate on one repository. Those people do not have, and should not get, an SSH login to the
owner, because that login is ownership of it. So the owner runs a separate listener that serves
only the lease routes of §6, for every principal that is not the operator: teammates, their
agents, cloud agents and CI.

The listener is the first Orbit surface with application authentication. It ships with an
STD-05 security review in the same phase, and it does not reintroduce caller authorization on
any SSH path: the operator keeps reaching the owner over SSH as today.

### 7.1 The listener user

Orbit tells an agent from the operator by environment: `agent_context_declared`
([env.rs](../../../crates/orbit-common/src/governance/authorization/env.rs)) reads variables such
as `ORBIT_AGENT_NAME` and `ORBIT_MANAGED_RUN_CONTEXT`, and its own documentation says it is "not
defending against a determined caller". Agents on an owner run as the operator's Unix user, so
any rule enforced inside the operator's processes is a guard, and a token store the operator's
user can write is one any agent can write. A token minted by an agent, perhaps steered by
injected text, and handed to someone outside would give that person the listener.

The listener, its token store and its configuration belong to a dedicated OS user (`orbit-lease`
by convention). Becoming that user — or becoming root, which can rewrite the account, the
sudoers files and the root-owned binary — is allowed only through a fresh authorization the
host provisions and an agent cannot reuse
([4_decisions.md](./4_decisions.md#a-listener-user-transition-requires-a-provisioned-factor-agents-cannot-reuse)).

Password-protected `sudo` does not provide that factor. It records a timestamp and accepts a
later `sudo`, including `sudo -n`, while the timestamp is younger than `timestamp_timeout`.
The `sudoers_timestamp(5)` manual for Sudo 1.9.17p2, and the sudo-rs 0.2.13 sudoers manual,
both document 15 minutes as that default and a separate record per terminal. Both define
`timestamp_timeout=0` as always prompting. Classic sudo can instead keep one global record
(`timestamp_type=global`, or `tty_tickets` off), which another session can reuse. An agent
started in a terminal whose timestamp is still valid can run `sudo -n -u orbit-lease` with
no password. Putting `sudo -k`
on the command the operator is shown does not close this: the agent chooses the command and
can omit the flag. After the transition the uid, the root-owned binary, the home mode and the
kernel socket-peer check all accept the caller. None of them can tell that no person was
present for this invocation.

**Prerequisite.** The listener may be enabled only when every item below holds. Otherwise it
stays off.

- Sudoers is root-owned and not writable by the operator user, including every `#includedir`.
  For every `sudo` that account can run, the effective `timestamp_timeout` is `0`, which
  `sudoers(5)` defines as always prompting, so a cached timestamp is never accepted. The
  policy has no `NOPASSWD` tag, no `Defaults !authenticate`, no negative `timestamp_timeout`,
  and no `exempt_group` that includes the operator.
- The account agents run as is not root and holds none of `CAP_SETUID`, `CAP_SETGID`,
  `CAP_DAC_OVERRIDE` or `CAP_FOWNER`. No `/etc/subuid` or `/etc/subgid` range maps the listener
  uid or gid to that account. No other mechanism runs a command as the listener user or as
  root without the same fresh prompt: `su`, `runuser`, `doas`, `pkexec`, `systemd-run --uid`,
  `machinectl`, or a setuid helper.
- The listener account has a locked password, no authorized keys and a non-login shell.
- The prompt is answered by a person at the terminal. The operator password is not stored
  where the operator's user can read it, and no `SUDO_ASKPASS` helper is available for an
  agent to satisfy `sudo -A` or `sudo -S`.
- A hardware or one-time factor may be what that prompt requires. It counts only together
  with `timestamp_timeout=0`. A timestamp from the first success must not satisfy the next
  `sudo`.

`Defaults>orbit-lease timestamp_timeout=0` by itself is not this prerequisite. A cached
authorization to root can rewrite sudoers, replace the binary and start a process as the
listener user. The timeout has to cover every `sudo` the operator can run.

Where the host cannot enforce the list, the guarantee is the narrow one: this design does not
claim that agents cannot mint tokens. Fail closed. Leave `lease.listener.bind` and
`lease.intake.peer_user` unset, and do not add a sudoers rule that lets the operator become
the listener user. Ordinary password-protected `sudo` is that narrower case, not a sufficient
deployment. The disposable-guest recipe for a warmed timestamp and for the denial under
`timestamp_timeout=0` is
[path-leases-listener-sudo.md](../../dev/qa/path-leases-listener-sudo.md). It does not change
a live owner's sudoers.

- **The user.** A system user with no login shell and its own Orbit home (for example
  `/var/lib/orbit-lease/.orbit`). That home holds only the listener's configuration
  (`lease.listener.*`, §8) and the token store `lease-tokens.db`, created `0600` in a `0700`
  directory. It has no workspaces and no provider or forge credentials.
- **Minting.** The operator runs `sudo -u orbit-lease orbit lease token create --principal <name>
  --workspace <ws> [--class human|agent] [--verbs read|lease] [--max-priority]
  [--hard allow|never] [--expires <duration>]`. `list` (never prints secrets) and `revoke <id>`
  run the same way. That command line is not the control and does not include `sudo -k`. Under
  the prerequisite, sudoers denies a following `sudo -n -u orbit-lease` in the same terminal.
  A person's agent tokens are named under the person (`alice/claude-code`). The commands open
  only the token store in the running user's Orbit home, and refuse to run as root or as the
  user that owns the intake socket (§7.2). Run as the operator, they find no store and say how
  to run them. The `agent_context_declared` refusal stays, as a guard that gives an agent a
  clear message; it is not the boundary.
- **A binary agents cannot replace.** The listener user runs a root-owned copy of `orbit` (for
  example `/usr/local/bin/orbit`), not the operator's `~/.cargo/bin/orbit`. `orbit lease listen`
  and `orbit lease token` refuse to start when their executable, the listener user's Orbit home,
  or any directory above either is writable by a user other than root or the listener user.
  Without that check an agent could replace the binary and run code as the listener user.
- **Linux first.** The user, its systemd unit and the root-owned binary are documented for
  Linux owners. On macOS the same checks apply; creating the user is the operator's.

**Implemented token commands.** `orbit-auth` and `orbit lease token create|list|revoke` exist;
the lease listener (§7.3) is the network surface that accepts them. Orbit never creates `lease-tokens.db`: root provisions
it empty (`0600`, in the `0700` Orbit home) when it creates the listener user, and Orbit adds
its schema on first open. That is how a command run as the operator finds no store, for
`create` too, and refuses with how to run it as the listener user. The Orbit home is `$HOME/.orbit`,
so the documented command line is `sudo -H -u orbit-lease …`; under a preserved operator
`HOME` the home is the operator's and the ownership checks refuse it. A token's workspaces come
from the global `--workspace` flag, comma-separated. The commands open no workspace runtime,
pin no executable generation and write no Orbit audit row; the token store audits mints,
revocations and refused verifications itself. The execution-environment check reads ownership
and mode bits, accepts a sticky directory above the checked path, and does not read ACLs. Setup
is in the orbit-setup skill's remote-access reference.

### 7.2 The intake socket

The listener user cannot open the operator's Orbit store, so it hands authenticated lease
operations to Orbit over a local socket.

- **Serving.** When `lease.intake.peer_user` names the listener user, Orbit Web, the owner's
  long-running service, also serves a Unix socket at `$ORBIT_HOME/run/lease-intake.sock`. The
  socket's directory is group-accessible to the listener user only.
- **Peer check.** Every connection's peer user is read from the kernel (`SO_PEERCRED` on Linux,
  `getpeereid` on macOS) and must be `lease.intake.peer_user`. Orbit refuses a configuration
  naming root or its own user, so the operator's user, and every agent running as it, is
  refused.
- **Operations only.** The socket accepts the §6 lease operations and nothing else. Each carries
  the authenticated principal, its class, the token id, the token's scope (workspaces, verbs,
  priority ceiling, `hard`) and the client address. Orbit applies the token's scope and the
  workspace's class policy (`lease.hard`, `lease.max_priority`) together, taking the stricter,
  so a token cannot exceed what its workspace allows its class.
- **Versioned.** The protocol carries a version. A listener and an Orbit of different protocol
  versions refuse each other with a message naming both, because the root-owned copy is updated
  separately from the operator's binary (an owner's update procedure gains a `sudo install`
  step for it).
- **Audit in two places.** Orbit audits every intake operation with the principal, token id and
  client address. Owner lease audit payloads and intake errors redact known secret shapes, including tokens embedded in selectors; the mint response still returns the secret once. The listener audits what never reaches Orbit, failed authentication and
  rejected requests, in its own store (§7.3). A refusal before a token is verified (no token, an
  unknown one, an unresolvable address, a rate limit) records the remote address, route and
  reason but no principal, since none was authenticated; principal and token id appear only when
  the token was valid.

**Implemented intake socket.** `orbit_web::lease_intake` holds the versioned request and
response types, the server `orbit web serve` runs, and the blocking `LeaseIntakeClient` the
listener will call. `lease.intake.peer_user` is read only from the global `config.toml`. The
`run/` directory (`0750`) and the socket (`0660`) belong to the operator and the peer user's
primary group, which the operator's user must be a member of. Frames are a 4-byte big-endian
length and a JSON body of at most 64 KiB; each must arrive within 10 seconds, and 16 connections
are served at once. Both sides send `{"protocol_version": 2}` first and refuse a mismatch,
naming both versions; version 2 added `notices` (one page) and `events`. Any change to the
frames or operations changes the version. `events` is audited once, when it opens, and then
streams one response per page on its connection until either side closes it or Orbit Web
shuts down; it holds one of the 16 connections meanwhile. A notice page whose response would
exceed 64 KiB is shortened to a prefix whose cursor is its last notice, so a client resuming
from the cursor misses nothing; a single notice over the limit is `frame_too_large`. A
request's token scope must name its workspace (a registered id or name) and the operation's
verb. The runtime grants the lower of the token's and the class's priority ceilings, and hard
only when both allow it. The lease row stores the token id (store migration 66), and every
operation writes one `lease.intake.<operation>` audit row with principal, token id, client
address and outcome. A token renews and releases only leases whose principal is its own; any
other lease, like an unknown one, is refused with `lease_not_held`, which names no holder,
because a token authorizes leases and nothing else (`4_decisions.md`). Renewal is refused without
changing the lease when its mode or priority exceeds the renewing token or the workspace class
policy (`lease.hard`, `lease.max_priority`) admitted from the current global or workspace
`config.toml`; release remains allowed to the same principal. `orbit web serve` applies the
`LeasePolicy` admitted from that file on the next intake operation after either file's mtime or
length changes, without a process restart; a same-length edit inside one mtime tick can still be
missed. Show, list and check read
every lease in the token's workspace. The operator-session REST routes keep full authority over
every lease.

The server checks the serialized response size before writing any frame bytes. A response
that would exceed 64 KiB becomes a bounded `frame_too_large` operation error; the connection
remains usable. If a request just granted a lease whose success response is too large, the
server releases that grant before returning the error. The grant and compensating release
retain their normal audit rows, and the intake operation is audited as a failure. That release
is a backstop only: a compensated grant has already written its `overlap` notices, so the
runtime sizes token answers before writing (below) and a token request does not reach it. A
token expansion whose complete deadlock cycle cannot fit is refused inside the store
transaction before that transaction commits, so it does not reach this release either.

**Token resource bounds.** A token's request rate alone does not bound the owner's work: at 20
writes a minute and a 4-hour TTL it allows about 4,800 unexpired disjoint leases, and each
later directory request would write an `overlap` notice to every one and answer with every
selector pair. So the runtime bounds token callers (`LeaseTokenGrant`), before any write:

- **Active leases.** At most 64 active (granted or queued) leases per principal in a workspace.
  `Store::insert_lease` counts the principal's active lease rows in its immediate transaction,
  so concurrent requests cannot both pass, and a replacement token for the same principal, a
  listener restart or an Orbit Web restart starts from the same count. Rows written over SSH
  under the same principal count too. Past the limit the request answers `granted: false`,
  `reason: "active_lease_limit"`, with the count and limit, writes no row or notice, and is
  audited as a `lease.request.refused` denial. A release or expiry frees a place.
- **Selectors.** A token's lease holds and waits for at most 64 selectors together. A request
  over it is refused with `invalid_input`; an expansion is checked in the runtime and again in
  `Store::expand_lease`'s transaction, and refused unchanged. So one request writes at most one
  `overlap` notice per active holder, and compares at most 64 selectors with each holder's.
- **Answer size.** The intake passes its frame limit as the grant's `max_response_bytes`. The
  runtime sizes the largest answer the request or expansion could get with empty holder lists
  (its selectors, tags, branches and a worst-case lease record), plus a 512-byte margin; when that
  cannot fit, it is refused with `invalid_input` before anything is written. Otherwise
  `overlaps`, `conflicts` and `queued_ahead` are built only up to the prefix the remaining
  budget fits, never as a full cross product, and the answer carries `truncated: true`. Each
  entry costs its JSON plus one byte, and once one does not fit nothing after it is shown.
  Every holder still decides the outcome: a hard request or expansion scans each task lock,
  frozen claim, task reservation, hard lease and queue entry to decide grant, queue or refusal
  (and which expansion selectors wait), but keeps only the conflicts that sort first, at most
  the remaining bytes over an empty conflict's size, before cutting them to the bytes that fit.
  The lists are ordered as follows. `overlaps` (advisory) lists tasks (`in-progress`, then
  `review`, oldest first), then task reservations, then leases oldest first. `conflicts` (hard)
  is ordered by requested selector, then task locks, reservations and hard leases, then holder
  id. `queued_ahead` follows queue order and gets only what `conflicts` left. A queued request
  or expansion writes the same prefix to its `queued` notice (`holders`, and `queued_ahead` for
  a request) with `truncated: true`, so the notice fits the frame too. An expansion's fixed
  answer sizes `deadlock` as JSON `null` (four bytes). The bytes left for `conflicts` are one
  budget shared with the deadlock object. Inside `Store::expand_lease`'s immediate transaction,
  after the cycle is known and before any queued or deadlock notice, the store measures this
  expansion's complete `deadlock` value. When this lease is the victim and that value exceeds
  the budget even with `conflicts` empty, the transaction returns `invalid_input` and rolls
  back: the expansion's queued surface is unchanged, no queued or deadlock notice is written,
  no other lease's queue is changed, and lazy expiry from that same transaction is not
  committed. When the object fits, it is returned whole (the cycle is never shortened) and
  `conflicts` keeps only the prefix that still fits beside it, with `truncated: true` when that
  prefix is short. The victim remains the expansion ranked last (lowest effective priority,
  then youngest). Its `deadlock` notice, written only when the expansion commits, still carries
  the full cycle. A deadlock that refuses a different lease leaves this answer's `deadlock`
  null. SSH and dashboard callers pass no budget, so they keep the full cycle and every
  conflict. Ranking still reads every
  waiting queue entry, which is queue state rather than answer data. A truncated grant is
  delivered, not released, so each `overlap` notice names a lease that exists. `check`, a read,
  answers per path and stays bounded only by the frame.

Notices and ended lease rows are retained as before, so history grows with the write budget,
but no longer by more than one notice per active holder for each write. SSH and dashboard
callers keep their documented semantics: no lease, selector or answer limit, and
`truncated` is always false.

With the §7.1 prerequisite in force, an agent running as the operator can still stop Orbit Web,
or edit the operator's configuration to turn intake off or name another peer user. That denies
service and grants nothing. The agent cannot mint or revoke tokens, read their hashes, or run
code as the listener user, because it cannot complete a fresh transition. Without the
prerequisite, a cached sudo timestamp or another run-as grant does complete that transition, and
the peer check, the home mode and the root-owned binary then accept the caller.

### 7.3 Tokens, routes and limits

- **Bearer lease tokens.** 256 bits from the OS random source with an `orbl_` prefix (so
  redaction can recognize one), shown once at creation and stored only as a SHA-256 hash,
  compared in constant time. Each names one principal and class, its workspaces, its verbs
  (`read` or `lease`), its priority ceiling, its `hard` permission and an expiry. Revocation and
  expiry take effect on the next request.
- **Lease-only routes.** The listener's router contains the §6 routes and `/healthz`, nothing
  else, so a token cannot reach a task, run, config or operator route by construction
  ([4_decisions.md](./4_decisions.md#a-lease-token-authorizes-leases-and-nothing-else)). The
  routes and their request and response types are the ones Orbit Web serves on loopback, behind
  a backend that calls the runtime there and the intake socket here.
- **Binding.** The listener binds an address the operator names (a tailnet address in practice)
  and refuses `0.0.0.0` or `::` unless TLS is configured on it or `lease.listener.behind_proxy`
  says a terminating proxy is in front. Orbit Web itself stays loopback-only.
- **Client address.** Without a proxy it is the TCP peer, and forwarding headers are ignored.
  Behind a proxy, `X-Forwarded-For` is read only when the peer is in
  `lease.listener.trusted_proxy`, and the client is the rightmost entry that is not a trusted
  proxy, since entries further left are whatever the client sent. A malformed entry is refused
  rather than replaced by the proxy's address; `Forwarded` is not read.
- **Ceilings are boundaries here.** Unlike the SSH path, a token holder cannot edit the owner's
  configuration, so its priority ceiling, `hard` permission and workspace scope are enforced.
- **Limits.** Per client address before any token lookup (IPv6 counted per /64 by default), with
  a stricter limit on failed authentication; per token for reads and, lower, for writes; and
  for the whole listener, connections, requests in flight, body size, and header and handler
  timeouts. A rejection answers 429 (with `Retry-After`), 413, 408 or 503, and is audited once per key
  and window so a flood cannot fill the audit store. Limiter state is in memory and bounded.
- **No browser sessions.** The listener has no cookie or Origin-based session, so it has no
  CSRF surface; the dashboard's Leases view reads through Orbit Web as today.
- **One token per person, one per agent.** A person's agents get their own tokens under the
  person's name (`alice/claude-code`), with the `agent` class ceiling, so notices and audit rows
  say which of Alice's sessions holds a path and revoking one agent leaves the others working.

**Implemented listener.** `orbit lease listen` runs `orbit_web::lease_listener` as the listener
user. Before binding it runs `orbit-auth`'s execution-environment check, refuses when the running
user owns the intake socket at `lease.listener.intake_socket` (or, before Orbit Web has created
it, its directory), validates `lease.listener.*` from the running user's global `config.toml`
(bind required; an unspecified address only with native `tls` or `behind_proxy`; `behind_proxy`
only with a non-empty `trusted_proxy`; native `tls = { cert, key }` checked before binding), and opens only the token
store. Native TLS uses rustls with ring, supports TLS 1.2 and 1.3 with no plaintext fallback,
and loads the PEM chain and matching private key at startup; Unix group/other key permissions
are refused. Certificate rotation requires a restart, not SIGHUP. It never opens Orbit's main
store and pins no executable generation. The router is
`/healthz` (unauthenticated liveness) plus `lease_router` nested at `/api` behind a route layer,
so unmatched paths are 404 without authentication. The layer resolves the client address through
`orbit_auth::client_address`, applies `orbit_auth::limiter`, reads `Authorization: Bearer`, and
verifies the token for the route's verb (`read` for GET and `POST …/check`, otherwise `lease`)
and `?workspace=` (default: the token's only workspace; several without one is 400). Unknown,
revoked and expired tokens and missing ones are 401 `unauthorized`; a missing verb or workspace is
403. The backend strips any declared principal and forwards the token id, scope and client address
over the intake socket, one connection per operation; intake codes map to HTTP (`scope_refused`
403, `lease_not_held` and `unknown_workspace` 404, `invalid_input` 400,
`operation_failed` 500 with the detail logged, an unreachable socket 503).

The listener now applies the complete limits slice. `orbit_auth::limiter::KeyedLimiter`
provides bounded fixed windows, configurable IPv6 prefix keys, separate token read/write
budgets, token in-flight and stream allowances, conservative overflow buckets and coalesced
rejection snapshots. Address admission surrounds the whole router (including health, 404 and
method errors), before token lookup. Failed authentication uses the same limiter. A valid token
is then checked against its address-independent budget. The client-address rule above is the
only reader of forwarding headers; malformed chains use the TCP peer for rejection admission.

`lease.listener.limits.*` defaults are 120 address requests/minute; 10 failed authentications
per 300 seconds; IPv6 /64; 60 token reads and 20 token writes/minute; 2 streams and 8 in-flight
requests/token; 64 connections; 16 in-flight requests globally; 65536 body bytes; 5 seconds
for headers/TLS handshake and 10 seconds for the handler/body read; 4096 tracked keys including
two reserved overflow identities. Configured ranges and settings are in [CONFIG](../../CONFIG.md#lease-listener).
Live budgets are never evicted at capacity; new addresses and new tokens share distinct
conservative overflow keys. Idle keys are reclaimed during periodic maintenance.

Tower HTTP layers bound bodies and handlers (413 and 408); response-lifetime allowances bound
request concurrency (503), including open streams. Blocking intake operations retain a
separate allowance until socket I/O really finishes, even if their HTTP handler timed out.
The shared HTTP/1 transport bounds connections before TLS, caps headers at 64 fields and a
16 KiB buffer, times out stalled headers and closes each connection after its response.
One additional bounded refusal lane can negotiate TLS and send 503; if that lane is occupied,
further saturated TLS connections close immediately and plain HTTP gets a best-effort 503.
A rate refusal returns 429 with a rounded-up `Retry-After`, naming only the limit.
The listener serves notice polling and SSE through the intake socket's `notices` and
`events` operations, with the token's workspace and the `read` verb. Opening a stream spends a
token read. An open stream holds its connection, one global and one token in-flight allowance,
a token stream allowance and an intake allowance until it closes, so `max_streams_per_token`,
`max_in_flight_per_token`, `max_in_flight_requests` and `max_connections` all count it, and
their refusals are the 429 or 503 of every other route. The handler timeout bounds opening a
stream, not its life. A stream ends when the client goes, when Orbit closes it, after 30
seconds without a page from Orbit, or at listener shutdown, without waiting out the drain.

Refusals that never reach Orbit are aggregated in the listener user's own token store,
`lease_listener_limit_audit` (schema v3), by canonical key, limit/reason and window, with count
and first/last rejection times. These rows carry no principal, secret, query or route. Token
keys use public ids only after authentication; failed authentication uses address keys.
First snapshots persist promptly; subsequent counts are batched every second and at graceful
shutdown, upserting the same row. Abrupt exit may lose unflushed counts. Token administration
retains its existing `lease_token_audit` trail. Orbit retains its per-operation intake audit.
These bounds are listener-only; Orbit Web on loopback is unchanged.

### 7.4 The `orbit-auth` crate

Credential code lives in one crate, `orbit-auth`, so one place carries the security review and
no surface reimplements a check.

- **What it owns.** The token format, generation, hashing and constant-time verification; the
  token store (its own SQLite file and schema in the listener user's home, opened through
  `orbit-common`'s SQLite support, not Orbit's main store); principals, classes, verbs, scopes
  and the allow-or-deny decision; client-address resolution; and a keyed, bounded rate limiter.
- **What it does not.** No HTTP framework, no runtime handle and no Orbit store: `orbit-web`
  wraps it as middleware for the listener, `orbit-cli` exposes the token commands, and the
  intake socket's server and client sit beside the listener and Orbit Web. It depends only on
  `orbit-common` and `orbit-types`.
- **Reach.** SSH paths do not use it. A later credential-bearing surface reuses it instead of
  growing its own token or limiter code.

## 8. Configuration

| Key | Default | Meaning |
|---|---|---|
| `lease.enabled` | `false` | Accept lease requests in this workspace. |
| `lease.default_mode` | `"advisory"` | Mode when a request names none. |
| `lease.hard` | `{ human = "allow", agent = "allow" }` | Per principal class: `allow` or `never` (granted as advisory). |
| `lease.max_priority` | `{ human = "high", agent = "normal" }` | Priority ceiling per principal class; tokens carry their own. |
| `lease.aging_minutes` | `30` | Queue age after which a request or expansion moves up one priority level (1–10080). |
| `lease.intake.peer_user` | unset | The listener user allowed on the intake socket; unset means no socket (§7.2). |
| `lease.listener.bind` | unset | Address for the lease listener; unset means no listener. |
| `lease.listener.tls` | unset | Certificate and key paths, required for a non-loopback bind unless `behind_proxy`. |
| `lease.listener.behind_proxy` | `false` | A terminating proxy supplies TLS in front of the listener. |
| `lease.listener.trusted_proxy` | unset | Proxy addresses or ranges whose `X-Forwarded-For` is read; required with `behind_proxy`. |
| `lease.listener.intake_socket` | unset | Path of the operator's intake socket. |
| `lease.listener.limits.*` | conservative | Per-address, per-token and whole-listener limits (§7.3). |

`lease.intake.*` lives in the operator's configuration; `lease.listener.*` lives in the listener
user's Orbit home, which the operator's user cannot write. `lease.listener.bind` and
`lease.intake.peer_user` stay unset unless the §7.1 prerequisite is in force. Unset is the
fail-closed deployment: no listener and no intake socket. TTL bounds reuse the reservation limits. Path policy (`unlocked_context_paths`,
`generated_paths`, and the conflict budget's `exclusive_context_paths`) applies to leases
unchanged.

## 9. Phasing

1. **Lease rows and SSH surfaces.** Advisory and hard leases, TTL and renew, explicit release,
   `check`, MCP tools and CLI, drain admission honoring hard leases, leases in
   `orbit task locks list` and the denial log.
2. **Lease listener.** The `orbit-auth` crate and lease tokens, the listener user, the intake
   socket, the lease-only router, TLS or proxy binding, audit and limits, with its STD-05 review.
   From here several people can coordinate on one owner.
3. **Notices.** The notice stream, `overlap` and `overlap_landed`, poll by cursor, SSE on both
   Orbit Web and the lease listener, and the Claude Code hook recipe in the skill docs.
4. **Queue and lifetime.** Waiting instead of refusal, aging, expansion, release on landing for
   bound branches and pull requests, and the dashboard Leases view.
5. **Later.** Preemption, webhooks with signed payloads, and conflict-budget pricing that weighs
   advisory holders by their measured landing rate.

Phase 1 already makes the operator's outside work visible to drains; phase 2 is what makes the
feature multi-person.

## 10. Concerns & Honest Limitations

- **Advisory leases depend on clients.** A session that never asks is still invisible, and one
  that under-declares (footprints miss about a third of touched files today) still collides. The
  editor hook narrows the gap for Claude Code only.
- **Orbit gains a network-facing, authenticated surface.** Orbit removed its
  caller-authorization machinery because, for a single user, SSH login already is ownership.
  Coordinating several people breaks that premise for leases, so the listener brings application
  authentication back for this one surface, and with it token storage, rotation, TLS and a
  public attack surface to keep reviewed. The lease-only router bounds what a stolen token can
  do; it does not remove the need to operate the listener carefully.
- **Fresh authorization is a host prerequisite.** Every mint, list, revoke and replacement of the
  root-owned binary is a `sudo` transition. It stays out of an agent's reach only when sudoers
  sets `timestamp_timeout=0` for every `sudo` the operator can run and grants no passwordless
  path. Ordinary password-protected `sudo` records a timestamp and honors it for a later
  `sudo -n`. A cached root timestamp, `NOPASSWD`, `Defaults !authenticate`, root, or another
  run-as path is enough to become the listener user. The socket peer check and the root-owned
  binary do not undo a transition that already succeeded. Where the host cannot enforce the
  prerequisite, the listener stays off. The prerequisite also fails when the agent already has
  the operator's password or an askpass helper that prints it. An update that skips the
  root-owned copy still leaves a running listener refusing on a protocol mismatch until that
  copy is installed.
- **The listener user is trusted for identity.** Orbit believes the principal and scope the
  intake socket sends. Code running as the listener user can therefore create leases as any
  principal, within each workspace's class policy, though nothing beyond leases. Root defeats the
  whole arrangement, as it would any OS-user boundary.
- **Limits are per process.** Limiter state lives in memory, so it resets on restart and is not
  shared between two listeners.
- **Advisory leases make honesty a social contract.** With several people, a teammate who never
  leases, or leases a whole directory, degrades everyone's notices. Orbit can show
  declared-versus-landed accuracy per principal; it cannot make anyone use leases well.
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

- ORB-18404 — lease rows in the reservation store; advisory leases lock-free (§2, §3).
- ORB-18405 — lease runtime operations, principal class and the phase-1 `lease.*` keys (§3, §7, §8).
- ORB-18406 — `orbit.lease` tools and the `orbit lease` CLI.
- ORB-18410 — the `orbit-auth` crate and lease tokens for the listener user (§7.1, §7.3, §7.4).
- ORB-18411 — lease REST routes over a backend trait (§6, §7.3).
- ORB-18430 — the intake socket (§7.2).
- ORB-18412 — the lease listener as its own OS user (§7.1–§7.3).
- ORB-18413 — native TLS on the listener.
- ORB-18421 — listener rate limits and request bounds (§7.3).
- ORB-18414 — STD-05 security review of phase 2.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
