---
title: Task Publication — Vision
owner: codex
last_updated: 2026-08-30
last_validated: 2026-09-19
status: Draft
feature: task-publication
doc_role: vision
type: design
summary: Evolution gates for private task-publication repositories, authority transfer, aggregation, and bounded retention.
tags: [task-publication, task-artifacts, backup, git, multi-host]
paths: ["crates/orbit-store/src/workflow/task/**", "crates/orbit-registry/**"]
related_features: [task-publication, task-artifacts, remote-access, federated-mcp, host-registry]
related_artifacts: [ORB-11068, ORB-11077]
---

# Task Publication — Vision

Task publication should earn trust first as a one-way durability projection in
one dedicated private repository per workspace. Repository aggregation, offline
mutation, automatic authority movement, and multi-writer convergence are
separate capabilities with different access and safety contracts.

## 1. Open Questions

1. What separately configurable routine trigger and freshness objective are
   justified by observed task-loss and recovery needs? V1 ships only an
   explicit command; a post-task-write hook is excluded.
2. Should an explicit authority transfer carry the last publication generation
   as a fencing token, and what evidence must the old owner provide before the
   new owner can publish?
3. Is direct read-only rendering from fetched Git objects sufficient, or does
   offline inspection need a disposable indexed cache?
4. What corpus size or fetch-latency threshold justifies checkpoint compaction,
   Git LFS, or an object-store attachment backend?
5. Can several workspaces safely share one private repository without creating
   cross-workspace access leakage or serializing independent authorities on one
   branch? V1 answers no and requires separate repositories.
6. Should hosted Orbit reuse the publication manifest as an export format, or
   should hosted backup remain a separate API with different retention and
   authorization guarantees?
7. Which sensitivity checks can run deterministically across supported hosts,
   and should an unavailable scanner fail `include` publication by default?
8. Which Git providers can prove repository visibility and branch policy through
   a stable API without making generic Git publication provider-dependent?

## 2. Prior Work

### Orbit task artifacts

[Task Artifacts](../task-artifacts/1_overview.md) already separates structured
metadata, Markdown prose, append-heavy JSONL, and checksummed binary attachments.
That bundle is the publication unit; publication must not invent a parallel task
schema.

### Orbit task migration

`orbit task export` and `orbit task import` validate portable `tar.zst` archives,
restore attachment bytes, rebuild indexes, and make ID collision behavior
explicit. Publication should reuse those bundle and validation primitives while
keeping migration's `renumber` behavior out of same-authority recovery.

### Live remote access and federated routing

[Remote Access](../remote-access/1_overview.md) and
[Federated MCP](../federated-mcp/1_overview.md) preserve destination-host
authority while making remote state reachable. Publication complements them
when the owner is offline; it does not cache their health, capabilities, runs,
or live mutation surface.

### Retired task sync

The retired Task Sync proposal (removed; see git history) used an
orphan branch in the source-code repository as a shared writable registry. It
required online task mutations, shared allocation, operation-aware replay,
tombstones, and structured conflict resolution. Task publication retains Git as
transport but chooses a separate private repository and rejects the multi-writer
registry contract.

### Dedicated Git repositories and backups

A separate repository provides an independent collaborator, visibility,
retention, and deletion boundary from source code. Snapshot and backup systems
demonstrate a second lesson: replication for recovery can remain one-way even
when consumers exist on many machines. Publication combines those properties.

## 3. What May Be Distinctive

Orbit can make a narrow distinction that Git-backed trackers often blur: a
portable private Git repository need not be a shared writable database. The
publication commit is useful precisely because it is subordinate to one task
authority and carries enough source/workspace provenance to refuse ambiguous
recovery.

The same task bundle can therefore serve three roles without conflation:

- canonical mutable record on the owner;
- immutable snapshot in its dedicated publication repository; and
- validated recovery input on a consumer.

The transition between roles is explicit. Repository access never becomes task
authority merely because a machine can clone or push.

## 4. References

**Orbit-internal**

- [Task Publication design](./2_design.md)
- [Task Artifacts design](../task-artifacts/2_design.md)
- [Remote Access design](../remote-access/2_design.md)
- [Federated MCP design](../federated-mcp/2_design.md)
- [State and backup runbook](../../runbooks/state-and-backup.md)

**External**

- Git reference-update and commit-object semantics.
- Repository-host visibility, branch-protection, and retention behavior.
- Content-addressed backup, credential management, and secret-rotation practices.

## Task References

- [ORB-11068] — chose a dedicated private publication repository and deferred aggregation and synchronization.
- [ORB-11077] — shipped the explicit-only v1 operator workflow and kept routine triggering, authority transfer, and multi-writer synchronization deferred.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
