---
title: Task Publication — Overview
owner: codex
last_updated: 2026-08-30
last_validated: 2026-09-19
status: Accepted
feature: task-publication
doc_role: overview
type: design
summary: Explicitly publish authority-owned task snapshots to a dedicated Git repository for labelled inspection and deliberate recovery.
tags: [task-publication, task-artifacts, backup, git, multi-host]
paths: ["crates/orbit-store/src/workflow/task/**", "crates/orbit-registry/**"]
related_features: [task-publication, task-artifacts, remote-access, federated-mcp, host-registry]
related_artifacts: [ORB-11068, ORB-11072, ORB-11073, ORB-11074, ORB-11075, ORB-11076, ORB-11077]
---

# Task Publication — Overview

Task publication is a shipped, explicit one-way durability surface: the machine
that owns a workspace publishes validated task-bundle snapshots to a dedicated
Git repository, while other machines may inspect those snapshots and the
declared owner may deliberately restore them without creating a competing task
authority. Orbit reports repository
privacy as operator-managed; generic Git cannot verify provider visibility.

V1 deliberately stops short of the retired source-repository orphan-branch and
multi-writer task-sync proposal. No task mutation, post-write hook, or default
routine publishes automatically.

## 1. Motivation

Orbit's canonical task bundles live under
`~/.orbit/tasks/workspaces/<task-workspace-id>/`. That keeps task mutation and local
execution state under one machine's authority, but it also makes the bundle
corpus dependent on host backup unless the operator exports it deliberately.

Existing capabilities solve adjacent problems:

- [Remote Access](../remote-access/1_overview.md) and
  [Federated MCP](../federated-mcp/1_overview.md) route live reads and mutations
  to the owning host. They require that host to remain reachable.
- `orbit task export` and `orbit task import` move validated archives between
  registries. They are explicit migration tools rather than a continuously
  refreshed remote backup channel.
- The retired Task Sync design stored a
  shared writable registry on an orphan branch in the source repository. It
  made mutations network-dependent and required operation-aware multi-writer
  conflict resolution.

Task publication fills the narrower remaining gap: keep a remote,
access-controlled, inspectable copy of task intent and selected attachments while
separating task visibility and retention from the source-code repository.

## 2. Core Concepts

### Publication repository

An operator-provisioned, dedicated Git repository containing publication data
for exactly one Orbit workspace in v1. Operators should make it private, but
Orbit does not claim privacy unless a future provider integration proves it. It
contains no source code and uses an ordinary configured branch.

### Publication binding

Machine-local registry state that binds a workspace, source-repository
fingerprint, publication repository, branch, publication lineage, and declared
authority. The selected runtime keeps the logical workspace identity used for
publication metadata distinct from the task-registry partition used to
enumerate bundles. Credentials remain in the operator's Git configuration or
credential helper.

### Publication authority

The workspace's declared owner is the only machine allowed to advance its
publication repository. A successful publication does not transfer task
authority.

### Publication snapshot

A validated tree containing one workspace manifest, task bundles, and the
attached artifact bytes admitted by the configured publication policy. Each
commit records the previous publication as its parent after the repository's
first snapshot.

### Consumer

A machine that fetches the private publication repository for inspection or
disaster recovery. Fetching does not bind the snapshot as a live writable
workspace and does not authorize local task mutation.

### Artifact publication policy

An explicit choice governing attached bytes under `artifacts/files/`. Repository
privacy does not silently authorize every attachment and does not make Git an
encrypted secret store.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Shipped repository binding, publication, inspection, and recovery protocol | [2_design.md](./2_design.md) | [ORB-11077] |
| Evolution gates and deferred aggregation/synchronization | [3_vision.md](./3_vision.md) | [ORB-11068] |
| Standing authority, privacy, and safety rules | [4_decisions.md](./4_decisions.md) | [ORB-11068] |
| Canonical task-bundle format | [Task Artifacts](../task-artifacts/2_design.md) | — |
| Current live cross-machine access | [Remote Access](../remote-access/1_overview.md) | — |
| Current host-qualified MCP routing | [Federated MCP](../federated-mcp/1_overview.md) | — |
| Same-authority publication recovery | [`orbit-store` restore workflow](../../../crates/orbit-store/src/workflow/task/restore.rs) | [ORB-11076] |
| Operator CLI and network-free end-to-end contract | [`orbit-cli` publication commands](../../../crates/orbit-cli/src/command/task/publication.rs) | [ORB-11077] |
| Portable migration archive primitives | [`orbit-store` task workflow](../../../crates/orbit-store/src/workflow/task/mod.rs) | — |

## Task References

- [ORB-11068] — designed authority-owned task publication through a dedicated private Git repository.
- [ORB-11072] — implemented owner-local publication bindings and lineage validation.
- [ORB-11073] — implemented deterministic publication snapshots and attachment policy.
- [ORB-11074] — implemented owner-only compare-and-swap Git publication.
- [ORB-11075] — implemented labelled read-only publication inspection.
- [ORB-11076] — implemented fail-closed same-authority publication restore.
- [ORB-11077] — shipped the explicit CLI workflow, end-to-end contract, and operator runbook.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
