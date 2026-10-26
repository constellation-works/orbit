---
title: Path Leases — Decisions
owner: claude
last_updated: 2026-10-25
last_validated: 2026-10-25
status: Draft
feature: path-leases
doc_role: decisions
type: design
summary: "Proposed standing rules for path leases: outside work shares the drain's lock index, leases are advisory unless policy makes them exclusive, and a lease token authorizes leases and nothing else."
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

## Task References

None yet; the design is unfiled.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
