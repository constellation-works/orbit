---
summary: "Task Artifacts — Vision"
type: design
title: "Task Artifacts — Vision"
owner: codex
last_updated: 2026-08-29
last_validated: 2026-08-16
status: Draft
feature: task-artifacts
doc_role: vision
tags: ["task-artifacts", "task-publication"]
related_features: [task-artifacts, task-publication, remote-access, federated-mcp]
related_artifacts: [ORB-11068]
---

# Task Artifacts — Vision

This document captures open questions for the task-artifacts reset, prior work that should inform the design, what may be distinctive about Orbit's task model, and references for future implementers. The near-term design is in [2_design.md](./2_design.md); this file is deliberately forward-looking.

---

## 1. Open Questions

### 1.1 How broad should allocation authorities be?

`ORB-00000` can mean at least three things:

1. Unique on one machine across all local workspaces.
2. Unique in one synced repository registry.
3. Unique across hosted Orbit Team.

The v2 design picks the narrowest authority that serves the product surface being implemented. For OSS local-first, one machine-local allocator across all local workspaces is enough. For task sync, repository-registry-global is enough. Hosted Team may later introduce org-global or tenant-global allocation. A bare `ORB-00000` is therefore scoped by its authority; cross-authority references need registry or workspace context.

### 1.2 What happens beyond five digits?

The formatter uses five digits as a minimum display width, while task-id parsing accepts a wider decimal suffix and the allocator's numeric ceiling is `u32::MAX`. The open question is whether a hosted authority should impose a narrower policy cap while keeping the flat bundle layout.

### 1.3 How structured should acceptance criteria become?

Markdown checkboxes are pleasant and reviewable. Structured checks are better for automation. Three candidate paths:

- Keep `acceptance.md` as source of truth and parse checkboxes for display.
- Add optional `checks.yaml` for machine-verifiable criteria.
- Make acceptance criteria fully structured and generate Markdown from them.

The reset should start with Markdown because task authors need low-friction prose. Add `checks.yaml` only when an automation consumer demands stronger typing.

### 1.4 Should task history be event-sourced?

The v2 artifact introduces append-only logs, but the envelope remains a snapshot. Full event sourcing would make events the source of truth and materialize the envelope. That improves audit and conflict handling but increases implementation cost and read complexity. The reset should stop at snapshot plus append-only audit unless sync conflict data proves it insufficient.

### 1.5 How much local execution context belongs in a task?

`workspace_path` and `repo_root` are local checkout bindings, not task identity, and belong in the local task registry's workspace-checkout table. The checkout points at that binding with `.orbit/config.yaml` and `workspace_id`. A future sync registry may add a portable workspace selector, but the task envelope should not carry machine-local paths.

### 1.6 Should old task IDs survive the reset?

No. Orbit is pre-release, so v2 should not carry old `T<YYYYMMDD>-<N>` IDs as supported aliases. A cutover command may print a local old-to-new mapping for humans, but commits and docs after the reset should cite only `[ORB-00000]` IDs.

### 1.7 Are review threads task artifacts or PR artifacts?

Historical task records stored review threads on tasks. The current v2 contract treats legacy `review-threads/` directories as inert sidecars and the review-thread surface as retired. A future GitHub-native review mode may instead treat PR comments as the canonical source and task threads as a projection. The answer depends on whether Orbit remains the source of review truth or simply mirrors host review systems.

### 1.8 Should archived tasks be compacted?

Status-neutral directories make old tasks easy to keep forever. Generated indexes should restore the old ability to list terminal tasks by month, but compaction remains a separate retention question. Possible compaction strategies:

- Keep the envelope and summaries, drop bulky artifacts.
- Move archived tasks into a cold store.
- Generate a signed archive bundle and remove the live task directory.

No compaction should land until retention and audit requirements are clearer.

### 1.9 How are task bundles exposed to a checkout?

Task and artifact tools resolve canonical bundles under `~/.orbit/tasks/workspaces/<workspace-id>/` through the workspace binding and registry. Orbit intentionally has no checkout-local task projection or filesystem-specific fallback.

---

## 2. Prior Work

### 2.1 Orbit task store

The pre-reset `orbit-store::file::task_store` implementation is the baseline. It proves that directory-per-task bundles are inspectable, easy to back up, and easy for agents to reason about. Its main weakness was that it overused `task.yaml` as both metadata envelope and append log.

### 2.2 Retired decision artifacts

The retired decision artifact store used the same envelope-plus-Markdown pattern this design recommends. Its separate monotonic IDs and lifecycle were removed after tasks proved to be the durable tracking primitive; only the storage-shape precedent remains relevant to task artifacts.

### 2.3 Orbit task sync

[docs/design/_archive/task-sync/](../_archive/task-sync/1_overview.md) documents
the retired shared-writer Git registry. Live visibility and mutation now route to
the owning host through [remote-access](../remote-access/1_overview.md) and
[federated-mcp](../federated-mcp/1_overview.md), so the multi-writer registry
remains superseded.

The draft [task-publication](../task-publication/1_overview.md) design uses a
dedicated private Git repository for a narrower purpose: one declared owner
publishes immutable task-bundle snapshots for remote durability, read-only
inspection, and explicit recovery. It does not revive shared allocation,
online-only mutation, operation-aware merging, or replicated execution state.
[ORB-11068] records that distinction.

### 2.4 Issue trackers

Jira, Linear, and GitHub Issues separate short metadata from long discussion and activity streams. Their IDs are globally meaningful inside a tenant or repository. Their weakness for Orbit is that most relation graphs are hand-authored and sparse, which makes automation unreliable.

### 2.5 Git-backed trackers

Tools like `git-bug` show that issue/task data can live inside git without a server. Their strongest lesson for Orbit is that append-friendly records and stable IDs matter more than mimicking a SaaS issue tracker schema.

### 2.6 Event sourcing and CRDTs

Event sourcing treats the event log as the source of truth. CRDTs provide mathematically convergent replicated state. Orbit does not need the full machinery for every task field, but append-only comments and history should borrow the same instincts: append rather than rewrite, merge by stable event IDs, and avoid last-writer-wins for human prose.

---

## 3. What May Be Distinctive

### 3.1 Agent-readable intent as a first-class artifact

Most issue trackers optimize for human triage and reporting. Orbit tasks optimize for agents that need to resume work safely. The artifact should make intent, context, plan, validation, and residual risk available without requiring the agent to scrape chat history.

### 3.2 Local-first with an upgrade path to shared authority

The same task format should work in a single local workspace, an opt-in git registry, and a hosted team product. That is why ID allocation is explicit and why local execution bindings should not be baked into portable task identity.

### 3.3 Searchable prose with structured edges

The design treats prose as Markdown, not as unstructured junk. Search and semantic indexing read it field-by-field. Relations are structured separately so agents can traverse dependencies and lineage without text-parsing prose.

### 3.4 Audit without making YAML unreadable

The reset keeps `task.yaml` readable by moving audit traffic into append-only logs. That preserves the "open the file and understand the task" experience while giving audit and sync systems a better substrate.

### 3.5 Recoverable local-first task state

Orbit tasks should feel local and inspectable without forcing every user into a conflict protocol. Keeping canonical bundles under `~/.orbit/tasks/workspaces/` gives local-first durability, while task tools and local registry metadata provide inspection, allocation, and workspace resolution without a checkout projection.

---

## 4. References

### 4.1 Orbit-internal

- [docs/design/CONVENTIONS.md](../CONVENTIONS.md) - design folder layout and ADR rules.
- [docs/design/_archive/task-sync/](../_archive/task-sync/1_overview.md) - archived (superseded) git-sync proposal over the existing task layout; see [remote-access](../remote-access/1_overview.md).
- [docs/design/task-publication/](../task-publication/1_overview.md) - draft one-way publication proposal using a dedicated private repository while preserving one task authority.
- [.orbit/adrs/](../../../.orbit/adrs/) - internal precedent for envelope plus Markdown body and monotonic artifact IDs.
- [docs/design/orbit-search/](../orbit-search/) - per-field indexing of task text.
- [docs/POSITIONING.md](../../POSITIONING.md) - product doctrine that currently treats task IDs as local search keys.

### 4.2 External

- GitHub Issues - repository-scoped issue IDs, comments, timeline events, and PR linking.
- Linear - globally meaningful workspace issue identifiers, typed relations, and workflow states.
- Jira - project-key issue IDs and rich workflow metadata.
- `git-bug` - git-backed issue tracker using repository data as the persistence substrate.
- Martin Kleppmann, *Designing Data-Intensive Applications* - event logs, replication, and conflict-resolution background.

---

## Task References

- [ORB-11068] — separated authority-owned task publication from the retired multi-writer sync proposal.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
