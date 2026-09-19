---
title: Task Publication — Decisions
owner: codex
last_updated: 2026-08-30
last_validated: 2026-09-19
status: Accepted
feature: task-publication
doc_role: decisions
type: design
summary: Standing repository, authority, failure, and data-exposure rules for task publication.
tags: [task-publication, task-artifacts, backup, git, multi-host]
paths: ["crates/orbit-store/src/workflow/task/**", "crates/orbit-registry/**"]
related_features: [task-publication, task-artifacts, remote-access, federated-mcp, host-registry]
related_artifacts: [ORB-11068, ORB-11077]
---

# Task Publication — Decisions

These standing rules govern the shipped implementation and future task-publication choices.
Task references carry provenance; superseded decisions remain in place so their
original reasoning stays legible. See
[CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule.

## Publication uses one dedicated private repository per workspace

**Recorded:** 2026-08-29 · [ORB-11068]

### Context

An orphan branch in the source-code repository reuses existing transport but
forces task visibility, collaborator access, retention, and repository lifecycle
to follow the source code. Sharing one separate repository across several
workspaces would reintroduce path-level access leakage and serialize independent
authorities on one branch.

### Decision

Every v1 workspace publishes to its own dedicated private Git repository using
an ordinary configured branch. The source repository carries no task-publication
ref. Repository aggregation is a future feature with a separate access and
concurrency design.

### Consequences

- Task visibility and retention can differ from source-code policy.
- One repository branch has one workspace authority and one compare-and-swap
  stream.
- Source branches cannot accidentally merge publication data.
- Cost: operators provision and manage an additional private repository and its
  credentials for every publishing workspace.

## Publication never creates a second mutation authority

**Recorded:** 2026-08-29 · [ORB-11068]

### Context

A Git snapshot is portable and writable by repository tooling, which makes it
easy for a future consumer to treat a clone as another live task store. Doing so
would create competing allocators and lifecycle writers without also replicating
locks, claims, runs, audit, or execution ownership.

### Decision

For every future publication consumer, cloning, inspecting, caching, and
restoring a snapshot remain distinct from authority. Only the registry-declared
workspace owner may mutate canonical tasks or advance the publication branch.

### Consequences

- Read-only consumers may exist on any number of machines without becoming
  replicas of the control plane.
- Repository write access alone never grants task authority.
- New offline-write features must define authority transfer or a complete
  multi-writer contract before changing this rule.
- Cost: a consumer cannot continue normal task mutation while the owner is
  unavailable merely because it has a recent publication.

## Publication does not participate in task-write acknowledgement

**Recorded:** 2026-08-29 · [ORB-11068]

### Context

Putting fetch, commit, and push on every task mutation would make local task
durability depend on network reachability and publication-repository credentials.
A backup channel would become a synchronous coordinator.

### Decision

Future task publication is explicit or routine-driven derived work. A task write
is acknowledged by the canonical local store; publication records and reports
its own last-success generation independently.

### Consequences

- Network and Git-host failures do not block task lifecycle operations.
- Operators can select a publication frequency proportional to their recovery
  objective.
- Cost: the newest acknowledged task mutation may be absent from the last remote
  publication.

## A moved publication branch is an authority conflict

**Recorded:** 2026-08-29 · [ORB-11068]

### Context

Git can merge or force-update divergent histories, but either response would
hide the fact that more than one writer advanced an authority-owned durability
branch.

### Decision

Every future publisher must compare-and-swap against the fetched branch tip. A
non-fast-forward result stops publication and surfaces an authority conflict;
the implementation never merges, replays task operations, creates an alternate
branch, or force-pushes as an automatic recovery.

### Consequences

- Out-of-band and competing writes fail visibly at the repository boundary.
- Publication history stays linear and each snapshot names an unambiguous
  predecessor.
- Cost: benign manual edits and stale authority handoffs require operator
  reconciliation before publication resumes.

## Private repository does not mean secret vault

**Recorded:** 2026-08-29 · [ORB-11068]

### Context

Private-repository access can still include collaborators and Git-host
administrators. Git history preserves deleted blobs, and generic Git transport
cannot prove provider visibility or classify secrets.

### Decision

Every future implementation treats repository privacy as an operator-managed
access boundary, not encryption or guaranteed erasure. Attached bytes require an
explicit policy with independent path, sensitivity, per-file, and total-size
checks. Omission is recorded as incomplete backup state.

### Consequences

- Orbit never claims that private hosting makes arbitrary task contents safe.
- Recovery can state exactly which attachment bytes were not protected.
- Provider-specific privacy verification may strengthen status reporting without
  weakening generic Git behavior.
- Cost: the safest default may refuse publication until the operator configures
  how existing attachments should be handled.

## Task References

- [ORB-11068] — recorded the dedicated-repository, authority, acknowledgement, branch-conflict, and privacy rules.
- [ORB-11077] — shipped those rules through an explicit CLI-only operator surface and deferred unattended publication.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
