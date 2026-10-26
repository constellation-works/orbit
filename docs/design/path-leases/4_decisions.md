---
title: Path Leases — Decisions
owner: claude
last_updated: 2026-10-26
last_validated: 2026-10-26
status: Draft
feature: path-leases
doc_role: decisions
type: design
summary: "Proposed standing rules for path leases: outside work shares the drain's lock index, leases are advisory unless policy makes them exclusive, a lease token authorizes leases and nothing else, and authority that must hold against agents lives with an OS user they cannot become."
tags: [path-leases]
paths: []
related_features: [conflict-budget, federated-mcp, remote-access]
related_artifacts: []
---

# Path Leases — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

These entries are proposals inside a Draft design. Each gets its task on the `Recorded` line
when the work is filed.

## Outside work and drain tasks share one lock index

**Recorded:** 2026-10 · proposed in the Draft design, no task yet

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

**Recorded:** 2026-10 · proposed in the Draft design, no task yet

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

## Task References

- ORB-18410 — `orbit-auth` and lease tokens for the listener user.
- ORB-18430 — the intake socket.
- ORB-18412 — the listener as its own OS user.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
