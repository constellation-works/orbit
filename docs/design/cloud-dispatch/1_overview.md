---
title: Cloud Dispatch — Overview
owner: claude
created_date: 2026-09-23
updated_date: 2026-11-28
last_validated: 2026-11-05
status: Draft
stage: designing
next: "Review the Copilot existing-PR recovery companion; the Claude phase 0 spike remains pending"
feature: cloud-dispatch
doc_role: overview
type: design
summary: "Proposed Claude cloud task delivery, with a companion scope for Copilot repair of existing PRs under shared repair ownership, budgets and exact-head evidence."
tags: [cloud, dispatch, claude, copilot, routines, pr, polling]
---

# Cloud Dispatch — Overview

Cloud dispatch is a proposed delivery mode that ships a task through a Claude Code cloud session instead of a local agent subprocess, so throughput is no longer bounded by the host. Orbit creates the session headlessly, polls GitHub until its PR is ready and adopts that PR into the normal review path.

The [Copilot existing-PR recovery companion](specs/copilot-pr-recovery.md)
extends this design area with a separate, unimplemented repair scope. It reuses
remote intent/observation principles, but retains the existing PR and Orbit's
shared repair ledger instead of creating a new Claude delivery. Review and
phase that scope independently; it does not select the Claude dispatch method
in [2_design.md](./2_design.md), imply that either proposal is built, or authorize a live cloud call.

Status: proposal, not started. Phase 0 is a manual spike with no code, against `orbit` first.
Open questions resolved 2026-09-23 (see [Resolved questions](./2_design.md#resolved-questions-2026-09-23)).

## 1. Motivation

Bearing: the workspace gate rule reads durable artifacts and task state, never the execution
summary as an oracle (`crates/orbit-core/assets/activities/agent_implement.yaml`); this design
applies the same rule to the agent's final message. The cloud session is untrusted remote
execution; GitHub is the only record Orbit believes.
Precedents: the distributed-drain split where a follower opens the PR and the owner re-reads
everything from GitHub (`task_claimed_pr_pipeline`, `task_landing_pipeline`, `vcs/landing/`);
`pr_complete`'s blocking GitHub poll (`vcs/pr/complete/`).

### The gap

Every Orbit delivery runs an agent as a local subprocess in a local worktree
(`crates/orbit-engine/src/activity_job/cli_runner/orchestrator/dispatch/preparation.rs`,
`run_cli_backend_for_step`; entry point `run_cli_backend` in `entry.rs`). Throughput is bounded by
the host:
about ten concurrent agents on either the Mac or a Linux server before CPU, ports and
worktrees contend, and larger codebases lower that ceiling. Claude Code cloud sessions run
in Anthropic-managed VMs and keep running when the host sleeps. During the 2026-09-23
investigation, their billing used subscription usage followed by available usage credits; plan
terms and promotions can change. Orbit has no way to use them.

The obvious spelling, `claude -p --cloud "<task>"`, does not exist for this purpose. The CLI
rejects `--cloud <description>` in non-interactive runs. With `-p`, `--cloud <session_id>`
only queues a follow-up message into an existing session. Plain `claude --cloud "<task>"`
does work, but only with a TTY (§1). The 2026-09-23 spike did not find a documented status
API for sessions created by this CLI path; status APIs for other Claude products are outside
that finding.

### Intended outcome

A task whose workspace opts in can be shipped with `mode: cloud`. Orbit:

1. creates a cloud session headlessly, carrying the task's contract;
2. polls GitHub until the session's PR is ready;
3. adopts that PR by stamping the `github-pr` external ref and filling `execution_summary`
   from the PR, then moves the task to `review` exactly as `pr_promote` does today.

The host spends one sleeping poll per in-flight cloud task, not one agent.

#### Out of scope for v1

- Orbit MCP inside the cloud session. The sandbox cannot reach the box, and exposing Orbit's
  MCP on the public internet is not worth it. The session gets everything it needs in the
  dispatch payload and reports back only through git and the PR.
- A new `Provider` or crew backend. `Provider` is a closed enum with a contract fixture and
  `ProviderRegistry` is crate-private. The plugin scope defers new providers to its phase 5.
  Cloud is a delivery mode (a job), not a provider.
- Self-hosted runners (`--environment ccpool_…`). They are Team/Enterprise only; see §6.
- Merging from inside the cloud session. Orbit owns the merge: after review, the PR lands
  through `pr_complete`, which reads merged state back from GitHub (§5.1).
- Reading session content. Orbit parses only the session ID the dispatch command prints. It
  never reads the session's transcript or its final message.

The mechanisms are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## 2. Core Concepts

- **Cloud session.** A Claude Code session that runs in an Anthropic-managed VM and keeps running when the host sleeps; the untrusted remote execution this design dispatches to.
- **`mode: cloud`.** The delivery mode a task opts into when its workspace enables cloud dispatch; the host spends one sleeping poll per in-flight cloud task, not one agent.
- **Dispatch payload.** The task's contract, carried in the dispatch command, which is everything the session gets; it reports back only through git and the PR.
- **GitHub as the record.** The only evidence Orbit believes: it adopts the session's PR from GitHub, never from the session's transcript or final message.
- **Adoption.** Stamping the `github-pr` external ref and filling `execution_summary` from the PR, then moving the task to `review` as `pr_promote` does today.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Dispatch mechanism | [2_design.md](./2_design.md#1-dispatch-mechanism) | — |
| Configuration and the delivery payload | [2_design.md](./2_design.md#3-delivery-contract-the-payload) | — |
| Pipeline, idempotent dispatch and polling | [2_design.md](./2_design.md#4-pipeline) | — |
| Adoption, review and merge | [2_design.md](./2_design.md#5-adoption-and-verification) | — |
| Self-hosted adapter (later, plan-gated) | [2_design.md](./2_design.md#6-self-hosted-adapter-later-plan-gated) | — |
| Copilot existing-PR recovery companion | [specs/copilot-pr-recovery.md](./specs/copilot-pr-recovery.md) | — |
| Standing rules | [4_decisions.md](./4_decisions.md) | — |

## Task References

- None yet.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
