---
type: design
summary: "Glossary: Distributed Drain"
last_validated: 2026-09-18
title: Glossary — Distributed Drain
owner: claude
status: Draft
feature: distributed-drain
tags: [distributed-drain, glossary]
related_features: [distributed-drain, federated-mcp, host-registry, resident-orchestrator]
related_artifacts: [ORB-12488]
---

# Glossary: Distributed Drain

Vocabulary for the pull-based multi-host drain. Host-registry roles (`machine_id`, owner checkout,
replica checkout) and federated-mcp terms (selector, capability, destination, callers file) keep
their existing meanings and are not redefined here.

| Term | Meaning |
|------|---------|
| **Carried** | A task with a current execution claim, including the period before local run binding. Status locks continue to protect in-progress/review work. [Design §3.1](../2_design.md#31-attempt-ownership-and-recovery) |
| **Claim** | Durable authority for one attempt, identified by `claim_id` and bound to the authenticated execution machine and then one leaf run. [Pull spec](../specs/task-pull.md#claim-lifecycle-contract) |
| **Epic (tag)** | After retirement, a size hint for one large task; no special admission, worktree, or reservation class. [Design §7.1](../2_design.md#71-epic-machinery) |
| **Follower** | Replica checkout executing against the owner's task authority; run state and worktrees stay local. [Design §1](../2_design.md#1-roles-one-owner-n-followers) |
| **Handoff** | Durable candidate and validation/review evidence accepted by the owner with promotion to review and closure of execution writes. [Design §3.2](../2_design.md#32-durable-review-and-landing-handoff) |
| **Landing** | Owner-side reconciliation and completion of a pinned handoff with explicit merge authority and verified delivery evidence. [Design §3.2](../2_design.md#32-durable-review-and-landing-handoff) |
| **Owner** | The workspace's coordination authority for tasks, claims, locks, and landing. [Design §1](../2_design.md#1-roles-one-owner-n-followers) |
| **Pull** | One idempotent admission returning a claim or a recorded idle result. [Pull spec](../specs/task-pull.md) |
| **Pull mode** | Local drain execution against a persisted owner selector, with restart-safe pending admissions. [Design §3](../2_design.md#3-pull-mode-drain-and-the-pulled-leaf-pipeline) |
| **Pulled leaf** | A run created once per claim, bound before launch, bypassing backlog rediscovery and new lock acquisition, with explicit settlement. [Design §3](../2_design.md#3-pull-mode-drain-and-the-pulled-leaf-pipeline) |
| **Ready queue** | Logical owner-side ordered query over dependency-ready backlog tasks; no required materialized queue. [Design §2](../2_design.md#2-the-ready-queue-and-orbittaskpull) |
| **Request receipt** | Durable result for one caller/workspace/request ID; replay never creates another admission. [Pull spec](../specs/task-pull.md#idempotency-and-admission) |
| **Slot** | Local capacity consumed by live leaves and pending admissions not yet counted as live leaves. [Design §3](../2_design.md#3-pull-mode-drain-and-the-pulled-leaf-pipeline) |
| **Version parity** | Binary/schema compatibility precondition, distinct from provider, crew, policy, and toolchain availability. [Design §4](../2_design.md#4-follower-preconditions) |
