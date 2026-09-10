---
title: Resident Orchestrator — Overview
owner: codex, grok, claude
last_updated: 2026-09-10
last_validated: 2026-09-10
status: Draft
feature: resident-orchestrator
doc_role: overview
type: design
summary: An epic owns one worktree and one branch; its children land into that branch sequentially and the epic agent finishes the work inside it. workspace_auto_pipeline drains the workspace for a caller-supplied window instead of taking one action per tick. The fire clock lives outside Orbit.
tags: [resident-orchestrator, epic, jobs, mcp]
paths: [".orbit/resources/jobs/**", "crates/orbit-core/assets/jobs/**", "crates/orbit-core/assets/activities/**"]
related_features: [resident-orchestrator, activity-job]
related_artifacts: [ORB-10332, ORB-10775, ORB-10776, ORB-10779, ORB-10788, ORB-10815, ORB-10816, ORB-10817, ORB-10818, ORB-10819]
---

# Resident Orchestrator — Overview

> **Status: Draft, landing incrementally.** This folder specifies the v2 contract ([ORB-10815]).
> The epic worktree, sequential child drain, epic finisher, epic-scoped completion, and drain
> window are live. Each child task can still flip its own section's claims as behavior changes —
> see the status column in [§3](#3-at-a-glance).
> [§10](./2_design.md#10-what-v1-did-and-why-it-changed) records what v1 did.

V1 is two Orbit jobs, not one command. V2 keeps that split and changes what each job *is*.

`epic_pipeline` owns a body of work end to end. It opens **one worktree and one branch** for the
epic root. Child tasks — when the epic has any — land into that branch **one at a time** in local
mode and reach `done` on merge. Then the epic agent works **inside that worktree**: it validates
the merged result, resolves what the children missed, and finishes the epic with subagents as
needed. The job delivers the branch once, as a PR or a local merge, per the workspace's ship mode.

Sub-task breakdown is optional. A human or a higher-up orchestrator decomposes an epic when the
decomposition earns its keep. An epic with no children is normal: the agent does the whole thing.

`workspace_auto_pipeline` is the drain ([ORB-10819]). `orbit run auto --for <duration>` polls and
re-lists the backlog until the window expires, so work created *after* the run started still ships.
The window gates starting new work, never in-flight work. Loose leaves and an epic proceed
concurrently whenever their `context_files` do not overlap; the epic excludes what it actually
touches by holding one reservation, not by freezing the workspace.

The **resident supervisor clock** that fires `epic_pipeline` directly is not an Orbit routine. A
cron (or a human, or a front-door session) on a separate knowledgebase checkout, with Orbit MCP
wired in, calls `orbit run auto` or `orbit run job epic_pipeline`. A workspace may instead opt
into the existing `ship_sweep` routine, which invokes the `workspace_ship_pipeline` wrapper.
Selection still lives there.

## 1. Motivation

V1's sequencer was strictly less useful than `orbit run ship` in the common case. One tick did one
thing; `decision: hold` froze every conflict-free chore behind an unrelated epic; and backlog
membership was sampled once, so anything created a second later waited for the next external fire.

The deeper problem was the orchestrator's shape. It could create and dispatch tasks but not edit
the tree, so one epic fragmented into N worktrees, N PRs, and N review items, and nothing ever saw
the epic's combined result before it reached the base branch. That constraint made sense when the
epic had no worktree of its own. Giving it one removes the reason for the constraint.

## 2. Core Concepts

**Epic worktree.** One stable worktree and branch per epic root, keyed by the epic's id through
`worktree_setup`'s `run_id` / `branch_prefix` inputs. It is reattachable across runs of the same
epic and is retained by `worktree_gc` for as long as the root is non-terminal.

**Child drain.** Non-terminal descendants land into the epic branch sequentially through
`task_local_pipeline` (`base_sync: local`, no push, `landing_branch` set to the real base). A child
is `done` on merge into the epic branch — the epic root is the single review artifact.

**Epic agent.** `epic_orchestrator`: one `backend: cli` agent_loop that runs **inside** the epic
worktree with write access scoped to it. It validates and finishes the work, delegating to
subagents as needed, and may author further children when decomposition is warranted. It still must
not merge PRs reserved for the human merge authority, edit a second workspace, or invent approval
policy.

**Epic completion.** Epic-scoped, not workspace-scoped: the run is finished when the root has no
non-terminal descendants and the agent reported completion. A leftover set at the iteration ceiling
fails the run closed. Success is never inferred from agent prose.

**Unresolved work / scan.** `scan_unresolved_work` keeps its workspace-wide shape — every task in
`proposed`, `backlog`, or `blocked`, plus every `failed`/`timeout` run and unresolved `check_later`
note. It is no longer `epic_pipeline`'s completion gate; it serves the workspace drain.

**Drain window.** `orbit run auto --for <duration>`: how long the auto run keeps polling and
flushing the backlog. Absent or zero means one tick. The deadline stops *starting* new work; in-flight
children finish on their own.

**Conflict admission.** An `epic`-tagged root holds one reservation over the union of its
descendants' `context_files` — live since [ORB-10816] via `lock_context_files_for_task`. Loose
leaves are then admitted by the ordinary overlap check. The former `decision: hold` gate was
deleted by [ORB-10819], so conflict-free leaves continue while an epic is active.

**Session log.** Workspace-scoped append-only notebook, kinds `status`, `note`, `check_later`.
Unresolved `check_later` rows are a scan wake reason. This is the memory between fires;
conversation resume stays out of scope. The store is an internal Core capability; the
`orbit.session_log.*` agent tools were withdrawn ([ORB-11097]). In the v2 epic path, the finisher
may edit its owned worktree directly; children remain optional.

**External clock.** Cron / knowledgebase supervisor / front door for a resident supervisor. No
seeded routine directly targets the resident jobs; the optional `ship_sweep` routine wraps the
workspace drain.

**Epic tag.** Supervisor delegation for a root body of work ([Epic tag is a supervisor delegation signal, not the job predicate](./4_decisions.md#epic-tag-is-a-supervisor-delegation-signal-not-the-job-predicate)). It is **not** `epic_pipeline`'s pickup
key. It **is** the leaf-ship exclusion key: auto `list_backlog` skips the root and its descendants,
and explicit ship of the root is refused.

## 3. At a Glance

| Concern | Where | Task | Status |
|---------|-------|------|--------|
| This split | this folder | [ORB-10776] | live |
| Epic tag = supervisor delegation signal | [Epic tag is a supervisor delegation signal, not the job predicate](./4_decisions.md#epic-tag-is-a-supervisor-delegation-signal-not-the-job-predicate) | [ORB-10776] | live |
| Clock and supervisor stay outside Orbit | [The supervisor clock is not an Orbit primitive](./4_decisions.md#the-supervisor-clock-is-not-an-orbit-primitive) | [ORB-10776] | live |
| Session log (notes / check-later / status) | workspace session-log store (internal; public tools withdrawn) | [ORB-10784], [ORB-11097] | live |
| `scan_unresolved_work` + `epic_pipeline` v1 | catalog | [ORB-10779] | live |
| Epic owns one worktree; children land sequentially | [An epic owns one worktree and one branch](./4_decisions.md#an-epic-owns-one-worktree-and-one-branch) | [ORB-10816] | live |
| Epic reservation over the descendant context union | `lock_context_files_for_task` | [ORB-10816] | live |
| Epic agent edits the tree instead of dispatching | [The epic agent works in the worktree instead of dispatching](./4_decisions.md#the-epic-agent-works-in-the-worktree-instead-of-dispatching) | [ORB-10817] | live |
| Epic-scoped completion + inlined delivery | [Epic completion is epic-scoped](./4_decisions.md#epic-completion-is-epic-scoped-not-workspace-scoped) | [ORB-10818] | live |
| Drain window, no `hold`, detached epic dispatch | [Auto drains for a window instead of taking one action](./4_decisions.md#auto-drains-for-a-window-instead-of-taking-one-action) | [ORB-10819] | live |
| Child delivery inside an epic | `task_local_pipeline` onto the epic branch | [ORB-10816] | live |
| HTTP epic retirement | removed assets | [ORB-10332] | live |

## Task References

- **[ORB-10332]** — Remove the unused HTTP epic pipeline.
- **[ORB-10775]** — Epic: drain job in Orbit; supervisor clock stays external.
- **[ORB-10776]** — Accept the v1 contract; epic-tag and external-clock decisions.
- **[ORB-10779]** — Ship the scan, the orchestrator activity, and `epic_pipeline`.
- **[ORB-10784]** — session-log store (status / note / check_later).
- **[ORB-11097]** — withdrew `orbit.session_log.*` from the public agent tool surface.
- **[ORB-10788]** — v1 sequencer job, leaf-ship exclusion, `orbit run auto`.
- **[ORB-10815]** — Epic-owned worktree and continuous workspace drain (this revision).
- **[ORB-10816]** — Epic worktree; sequential child drain; epic reservation.
- **[ORB-10817]** — `epic_orchestrator` becomes a code-editing finisher.
- **[ORB-10818]** — Epic-scoped completion gate and inlined delivery.
- **[ORB-10819]** — Drain window, classifier collapse, detached epic dispatch.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
