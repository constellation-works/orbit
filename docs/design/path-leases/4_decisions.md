---
title: Path Leases — Decisions
owner: claude
last_updated: 2026-10-30
last_validated: 2026-10-30
status: Draft
feature: path-leases
doc_role: decisions
type: design
summary: "Proposed standing rules for path leases: outside work shares the drain's lock index, leases are advisory unless policy makes them exclusive, a lease token authorizes leases and nothing else, and a listener-user transition requires a provisioned factor agents cannot reuse."
tags: [path-leases]
paths: []
related_features: [conflict-budget, federated-mcp, remote-access]
related_artifacts: []
---

# Path Leases — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

Phase 1 decisions name their implementation tasks on the `Recorded` line. Later-phase
proposals remain inside this Draft design and gain task references when filed.

## Outside work and drain tasks share one lock index

**Recorded:** 2026-10-26 · ORB-18404, ORB-18405, ORB-18406

### Context

Path coordination for work outside drains could live in a separate service that Orbit consults,
or in Orbit's own lock index. A separate service needs its own record of who holds what, and
Orbit's admission would then have two authorities over the same paths that can disagree.

### Decision

Any coordination of who may edit which repository paths goes through the owner's single lock
index. A new kind of holder (a lease, a future kind of claim) is a new row kind in that index,
never a parallel store with its own conflict rules.

### Consequences

- Drain admission, `orbit task locks list`, the denial log and the conflict budget see every
  holder without a second query.
- Rejected alternative: a standalone coordination service with Orbit as one client, which would
  have let tools without an Orbit owner use leases.
- Cost: leases require an Orbit owner for the repository. Anyone who wants them without running
  Orbit gets nothing, and extracting the feature later means extracting the lock index.

## A lease is advisory unless policy makes it exclusive

**Recorded:** 2026-10-26 · ORB-18404, ORB-18405, ORB-18406

### Context

On ws_orbit, 31% of the lock denials whose both sides have since landed blocked tasks that never
changed a file in common, while about 4.7% of landed tasks needed an agent to repair a rebase
conflict. Exclusive holds cost a lot of waiting to prevent a modest amount of repair.

### Decision

A holder that comes from outside the delivery pipeline informs by default: it is recorded and
announced, and it blocks nobody. It becomes exclusive only when path policy says the path is
exclusive or when the requester explicitly asks for exclusivity within its ceiling. When in
doubt between telling and blocking, tell.

### Consequences

- Most leases never make anyone wait; their value is the overlap and landing notices.
- Rejected alternative: exclusive by default, mirroring task context locks.
- Cost: conflicts that an exclusive hold would have prevented still happen and still reach
  `pr_conflict_recovery`.

## A lease token authorizes leases and nothing else

**Recorded:** 2026-10 · proposed in the Draft design, no task yet

### Context

Orbit's remote authority is an SSH login, which is ownership of the destination, and Orbit Web
has no application authentication. That fits one operator. Leases exist so that several people
and their agents can coordinate on one owner, and none of them except the operator should own
it, so they need a credential. A general Orbit API token would reopen the multi-tenant
authorization problem Orbit deliberately stopped solving.

### Decision

A credential Orbit issues to anyone other than the operator is scoped to one narrow surface,
and the surface is narrow by construction: the listener that accepts it routes only that
surface's operations. Widening what such a credential can do means a new listener or route set
with its own review, never a broader scope on an existing token. SSH stays the operator's way in
and keeps meaning ownership.

### Consequences

- A leaked lease token can hold paths and read lease state in its workspaces, up to its priority
  ceiling, until it expires or is revoked. It cannot reach tasks, runs, configuration or operator
  routes.
- Rejected alternatives: bearer tokens on Orbit Web's whole API with per-route scopes, which
  reuses one server but makes every route's scope check a security boundary; and giving each
  collaborator an SSH login, which makes each of them an owner.
- Cost: Orbit carries two authentication stories, SSH ownership and lease tokens, and a second
  listener to configure, monitor and review.

## Authority that must hold against agents lives with an OS user they cannot become

**Superseded by:** [A listener-user transition requires a provisioned factor agents cannot reuse](#a-listener-user-transition-requires-a-provisioned-factor-agents-cannot-reuse). The original rationale below is retained as history. It treats password-gated `sudo` as enough; a cached timestamp defeats that claim.

**Recorded:** 2026-10 · ORB-18410, ORB-18430, ORB-18412

### Context

Agents on an owner run as the operator's Unix user. Orbit tells them apart only by environment
(`orbit_common::governance::authorization::agent_context_declared` reads `ORBIT_AGENT_NAME`,
`ORBIT_MANAGED_RUN_CONTEXT` and similar), which any agent can unset; the function's own
documentation says it does not defend against a determined caller. The first draft of the lease
listener made token minting "operator-only: refused from a declared agent context", so an agent,
perhaps steered by injected text, could mint a lease token and hand an outsider the listener.

### Decision

When a capability must hold against agents, not merely discourage them, Orbit anchors it in an
OS identity, or a factor, that the operator's Unix user cannot exercise unattended: a dedicated
user the operator reaches through a password-gated `sudo`, a kernel-checked socket peer, a
root-owned file. Checks inside the operator's processes, environment declarations among them,
are guards that give a clear refusal, never the boundary. Applies to any future credential,
approval or consent that agents must not be able to grant themselves.

### Consequences

- The lease listener, its token store and its configuration belong to a listener user; Orbit
  accepts lease operations from it only over a socket whose peer the kernel identifies; the
  listener runs a root-owned binary and refuses to start where another user could replace it.
- Rejected alternatives: refusing token commands from a declared agent context, which an agent
  defeats by unsetting variables; and signing tokens with an operator key, which holds only for
  a hardware key that needs a touch, since a key in an ssh-agent is usable by every session that
  reaches the agent.
- Cost: minting, revoking and updating the listener's binary need the operator's password, so
  token management can never be delegated to an agent; and an owner gains a second user,
  a root-owned binary copy and a socket to provision and keep in version step.

## A listener-user transition requires a provisioned factor agents cannot reuse

**Recorded:** 2026-10-26 · ORB-18432
**Supersedes:** [Authority that must hold against agents lives with an OS user they cannot become](#authority-that-must-hold-against-agents-lives-with-an-os-user-they-cannot-become)

### Context

The superseded entry anchored agent-proof authority in a dedicated user the operator reaches
through password-gated `sudo`. Password-protected `sudo` caches authentication. The timestamp
is per terminal by default and can be configured as one global record. While it is younger
than `timestamp_timeout`, a later `sudo`, including `sudo -n`, does not ask again. An agent
started in that terminal, or in any session when the record is global, reuses it. The agent
can also omit a documented `sudo -k`. After the transition, the listener uid, the root-owned
binary, the home mode and the kernel socket-peer check all accept the caller. A passwordless
rule, `Defaults !authenticate`, a negative timeout, root, `CAP_SETUID`, a subordinate-uid map,
polkit or `systemd-run --uid` reaches the same uid without that cache. Unsetting
`ORBIT_AGENT_NAME` and the other variables `agent_context_declared` reads is already in this
design's threat model, and it does not touch the cache.

### Decision

When a capability must hold against agents, the operator's Unix user must be unable to exercise
it unattended. For the listener user that means a host-provisioned authorization factor the
agent cannot reuse and cannot skip. The supported `sudo` form is a root-owned sudoers policy
the operator cannot write: `timestamp_timeout=0` for every `sudo` that account can run, no
`NOPASSWD` tag, no `Defaults !authenticate`, no negative timeout, no `exempt_group` covering
the operator, and no second run-as path to root or to the listener user. A hardware or one-time
factor counts only when a cached timestamp cannot satisfy the later invocation. The prompt is
a person at the terminal, not `sudo -k` on a documented command, and not an askpass helper or
a stored password the agent can read. Checks inside the operator's processes, including
environment declarations, remain guards. The kernel peer check, binary ownership and home
permissions stay in place and are not the fresh-authorization factor: they accept a caller who
has already become the listener user.

The same rule applies to any later credential, approval or consent that agents must not grant
themselves. An OS identity is the anchor only together with a transition factor the operator's
user cannot replay.

If the host cannot enforce that factor, the guarantee does not hold. Fail closed: do not enable
the lease listener and do not set `lease.intake.peer_user`. Ordinary password-protected `sudo`
is not a sufficient deployment.

### Consequences

- The listener user, token store and configuration stay a dedicated account. Orbit still
  accepts lease operations from it only over a kernel-checked socket, from a root-owned binary,
  in a home the operator cannot write.
- The deployment contract for "agents cannot mint tokens" is the prerequisite above. The
  disposable-guest recipe
  [path-leases-listener-sudo.md](../../dev/qa/path-leases-listener-sudo.md) shows a warmed
  timestamp being reused and the unattended denial under `timestamp_timeout=0`.
- Rejected alternatives: treating "sudo asks for a password" as the boundary; requiring the
  documented command to include `sudo -k`; scoping `timestamp_timeout=0` only to the listener
  runas user while other `sudo` commands can still become root; refusing token commands from a
  declared agent context; signing tokens with a key in an ssh-agent.
- Cost: every mint, revoke and binary update prompts, and an owner that cannot install the
  policy keeps the listener off, so multi-person leases wait on that host. An owner also gains
  a second user, a root-owned binary and a socket to provision.

## Task References

- [ORB-18404] — stored lease rows alongside reservations in the owner lock store.
- [ORB-18405] — added advisory/hard runtime operations and principal policy.
- [ORB-18406] — exposed owner tools, CLI and task-lock lease listing.
- [ORB-18410] — added `orbit-auth` and listener-user lease tokens.
- [ORB-18430] — added the intake socket.
- [ORB-18412] — ran the listener as its own OS user.
- [ORB-18432] — documented the listener-user boundary for cached sudo credentials.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
