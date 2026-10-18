---
title: Mid-Run Operator Steering — Design
owner: codex
last_updated: 2026-10-18
last_validated: 2026-10-18
status: Accepted
feature: mid-run-steering
doc_role: design
type: design
summary: Implemented watermark, safe-point delivery and promotion gate for post-dispatch operator comments; additive and default off
tags: [mid-run-steering]
paths: ["crates/orbit-core/src/adapter/engine_host/actions/task_context.rs", "crates/orbit-engine/**", "crates/orbit-core/assets/activities/**"]
related_features: [activity-job, review-gate, distributed-drain, state-compatibility]
related_artifacts: [ORB-16577, ORB-10499, ORB-12467, ORB-16572]
---

# Mid-Run Operator Steering — Design

This doc describes the implemented safe-point delivery contract. The configuration keys (section 5), the snapshot watermark (section 1), the host's delivery at safe points (section 2), the activity checkpoint re-read of comments and the promotion gate (section 3) are implemented, including owner-authoritative delivery and gate readings for claimed follower leaves. It covers the watermark, delivery at safe points, the promotion gate, configuration and authority. It deliberately leaves provider-native mid-session injection, urgency classes that interrupt a running step, dashboard treatment and automation-authored steering to [3_vision.md](./3_vision.md). Executor and pipeline vocabulary follows [activity-job](../activity-job/); compatibility obligations follow [state-compatibility](../state-compatibility/).

Steering is additive and default off. With `workflow.mid_run_steering.enabled = false` (the built-in default), prompt construction, task records and every pipeline checkpoint match the pre-steering baseline. Steering does not change lane or crew selection, and adds no fields to task records.

## 1. Snapshot watermark

When the host builds an agent prompt that embeds task context (`agent_task_context_json` in `crates/orbit-core/src/adapter/engine_host/actions/task_context.rs`), it records the newest task comment included in that prompt as the step's snapshot watermark on the durable run/step record. Comment identity is the comment's position in the task's append-only comment log plus its recorded timestamp; the watermark is the pair, not a clock reading, so a comment written in the same instant as prompt construction is ordered, never ambiguous.

The watermark lives on the run, not the task: two runs of one task (a retry, a claimed leaf) legitimately hold different watermarks. It is optional state in the sense of [state-compatibility](../state-compatibility/) — absent on records written before the feature, ignored by older readers, never required to interpret the task.

## 2. Delivery at safe points

Headless executors are one-shot subprocesses in the generic lane contract (see [docs/runbooks/executor-onboarding.md](../../runbooks/executor-onboarding.md)). The generic engine has no portable way to write into a provider process mid-turn, so version one delivers only where the host already builds a fresh prompt:

- the executor's single post-recovery attempt and step-failure recovery,
- validation-repair rounds,
- final recovery,
- any resume, retry or re-dispatch of an agent step.

At each such safe point, before dispatch, the host re-reads the authoritative task record and computes the comments past the run's watermark. Operator-authored comments in that range form one bounded *steering block*, oldest first, placed ahead of the frozen task context and labelled as operator guidance received after dispatch, with the run and original snapshot named. The block is bounded the way [ORB-12467] bounds recovery diagnostics — head, tail and a marker naming the full size and where to read it (`orbit task show <id>`) — so a long comment thread cannot evict the task itself from the prompt. System-generated history entries are not steering and never enter the block.

Delivery is recorded when the host builds the prompt, as a `steering_delivered` history event naming the run, step and comment identities carried, and the watermark advances past the comments the block carried in full. Nothing in the agent's response can advance, refuse or fake the watermark: response envelopes stay advisory diagnostics, exactly as recovery activities are treated after [ORB-10382] (see [activity-job 2_design.md](../activity-job/2_design.md)).

The run's first task-context prompt records its snapshot. If a persisted run has no watermark, that prompt first delivers outstanding guidance from the same inherited or post-dispatch baseline the gate uses, including after mid-flight enablement. An admitted round also supplies the candidate source the gate inherited. A bounded task-context snapshot therefore cannot cover guidance it omitted. Every later prompt that run builds for the task is a safe point, whichever step builds it. The block is the `steering` key leading the envelope's `task` object: a label, the run id, the snapshot (the newest comment the run's earlier prompts carried) and the bounded comment text, each comment headed with its log position, author and timestamp. The host writes the history event before the watermark, so a failed watermark write leaves the comment to be delivered again rather than recorded as delivered. Because the watermark is a prefix of the comment log, a block whose middle was cut advances it only to just below the oldest comment it cut; comments past that point it carried in full are kept in the watermark's `delivered` set so no later block repeats them. A comment longer than the whole block budget is therefore never delivered in full: it re-enters each later block, cut, until the operator resolves it. Operator-authored means any author the shared automation classifier does not recognise as Orbit itself (`system`, `task-pilot`, `finding-verification`, a machine id, or an agent family or model). A step whose task already refuses implementer writes gets no block, and its comments stay undelivered. A claimed follower leaf builds its block from the owner's task and comment log, never its replica's, and keeps its watermark on the owner against its claim rather than in its own run state: each snapshot and delivery crosses the claim's host-only channel as a claim-scoped steering record, and the owner writes the `steering_delivered` history event, whose note also names the claim and the executing machine. A leaf that cannot record a delivery on its owner fails the prompt, so the comment stays undelivered rather than recorded.

As a complement inside a single long step, the activity instruction's existing checkpoint re-read (established by [ORB-10499] for status and terminal handling) extends to comments: at each checkpoint where the remaining work is still expensive, the agent re-reads the task and treats any operator comment past its snapshot as steering. This pull path is advisory in the same sense as the terminal precheck — it helps a well-behaved agent, and the deterministic gate in section 3 is what makes a miss impossible to ship silently.

As built, the implement, review-repair, final-recovery and step-failure-recovery activities carry that paragraph as a separate `steering_instruction`, appended to the rendered instruction only when `workflow.mid_run_steering.enabled` is on; with steering off the rendered prompt is byte-identical. The snapshot the agent compares against is the newest comment its prompt carried, and the paragraph restates section 4's authority limits and that the re-read records no delivery and advances no watermark. A claimed implement leaf skips the re-read and relies on its injected task, as its terminal guard already does.

## 3. Promotion gate

When steering is enabled, the shipped pipelines gain one deterministic check beside their existing handoff checkpoints ([activity-job 2_design.md](../activity-job/2_design.md) §on `task_pr_pipeline`): before the `commit` step that both `task_pr_pipeline` and `task_local_pipeline` already carry, the host re-reads the task and refuses to proceed while an operator comment past the run's watermark exists. Gating before `commit` (rather than before the local pipeline's later `merge`, or the PR pipeline's push) keeps the candidate still uncommitted in its worktree, so one steering round can repair it in place.

Refusal is not a dead end. The executor routes the run through one bounded steering round: the relevant agent step (implement, or the repair step when a candidate already exists) is re-dispatched with the steering block, under a budget of `workflow.mid_run_steering.max_rounds` rounds per run, enforced the way recovery attempts are already budgeted — each admitted round settles budget even if preparation fails, and a round that cannot observe anything new does not re-run. When the budget is exhausted with steering still undelivered, the run holds and the task is blocked with reason `steering_unconsumed`, naming the outstanding comments; an operator then chooses to requeue, answer differently, or accept the candidate as-is through the normal review record. A claimed follower leaf never decides this locally: the watermark and the comment log are read from the owner's task record, and an owner the leaf cannot reach at the gate fails closed rather than certifying a stale replica, consistent with [distributed-drain](../distributed-drain/).

As built, a job declares the gate as `spec.steering_gate: {before: commit, round: implement_bundle}`; both owner pipelines do, and add `candidate: resume_candidate`. The claimed local and PR pipelines declare it too. Before running `before`, the executor asks the host (`RuntimeHost::steering_gate`) for a reading of each of the run's tasks, decided from the task, its comment log and the run's state alone. With steering off the host reads nothing, so the run is exactly what it was without the gate. On, an operator comment the run's watermark does not cover is outstanding. A run that holds no watermark for the task built no steering-enabled prompt for it, such as a carried candidate, a resume whose implement step was checkpointed, or a run started with steering disabled. Its baseline is the watermark of the run whose prompts produced the candidate: the nearest run of its resume lineage that holds one, then, past the lineage's root, the run the `candidate` step reported as the carried candidate's `source_run_id` when it answered `implement: false`, and that run's own lineage. The executor hands that source to `RuntimeHost::steering_gate` and `admit_steering_round`, and the host accepts it only as a run of the task in its own store; the admitted round records it, so the round's first prompt delivers from the same baseline. Without such a watermark, every operator comment written after the earliest run reached started is outstanding. If the bounded ancestry walk cannot reach a baseline, a carried candidate names no source, or its source is neither a run of the task in this store nor covered by a claim baseline, the gate fails closed rather than substituting a newer dispatch timestamp. Outstanding steering spends one round: `RuntimeHost::admit_steering_round` records it under `steering_rounds` in the run's state before anything is dispatched, and the run goes back to `round`, which runs even when its `when:` is false. Every target in that step receives `input.steering_round` (round, budget and the outstanding comments), and the implementer's next prompt is a safe point, so section 2 delivers the block and moves the watermark. A round whose step fails, including a prompt that cannot read the task, keeps that failure as the run's own and records no delivery; its round stays spent. Back at the gate, the comment log is read again. The host refuses a further round once the run spent `max_rounds`, or when its last round was admitted for exactly the comments still outstanding, so it delivered none of them (a comment longer than the block budget is the usual case). The executor then blocks the task with a `steering_unconsumed` history event and a system comment naming each outstanding comment by log position and timestamp, and the run fails with a `[steering_unconsumed]` message. An unreadable record, or a round the host could not record, fails the run with `[steering_gate_unreadable]` and leaves the task to the ordinary run-failure handling. A task that is `done` or `archived` with steering outstanding stops the run as terminal handling requires, and the gate's record under `commit` in the run's pipeline names the task status and the outstanding comments. None of these stops runs final recovery or the failure activity, and nothing is committed: the candidate stays in its worktree. The gate's verdict covers only the comment log it read; a comment written after that read is the next gate's or run's steering. A claimed leaf's host reads the task, its comment log and the claim's watermark from the owner, never from its replica, so a comment only the replica holds is never steering. When the owner cannot answer, or no longer runs the claim, the reading is an error and the run fails with `[steering_gate_unreadable]`, naming the owner read, before `commit`. The watermark is part of the owner's claim record, which every settlement keeps, including one the owner makes for a leaf it never ran. A run of the task whose own lineage holds no watermark, the task's next claim or the owner's own later run, takes the task's most recent claim watermark as its baseline before falling back to comments written after its lineage started, so no later run delivers that claim's comments again, and a comment written between claims is outstanding even though it predates the run. Steering for a claimed task can be written while the claim is live: the owner admits a human operator's comment-only write (the CLI's `orbit task update <id> --comment` or the dashboard comment path) to a task an execution claim protects, while every other unscoped write stays refused ([distributed-drain](../distributed-drain/2_design.md)). The comment joins the owner's comment log past the live claim's watermark, so the leaf's gate finds it outstanding and its next prompt delivers it. Steering written before the claim or after it settles reaches the task's next run through that baseline.

## 4. Authority and scope

Steering adds no authority to anyone. Only an actor already allowed to comment on the task can write a steering comment, and the comment is prompt input of the same trust class as the task description it amends. Delivered steering cannot:

- widen the run's `context_files` lock surface — that still goes through the existing scope-widening path and its gates,
- change the run's captured completion authority, required validation commands, crew or review policy — mid-run strengthening exists only where a mechanism already defines it (for example the strengthen-only `--review` option of [ORB-16572]),
- move task lifecycle state — status still changes only through `orbit.task.update` and its write gates, including the terminal handling of [ORB-10499].

A comment that *asks* for any of those things is legitimate steering text for the agent to weigh; it takes effect only if the operator also performs the governed write.

## 5. Configuration

Two additive keys in the existing `workflow.*` namespace, documented in [CONFIG.md](../../CONFIG.md#workflowmid_run_steering--operator-comments-after-dispatch):

| Key | Built-in default | Meaning |
|-----|------------------|---------|
| `workflow.mid_run_steering.enabled` | `false` | Record watermarks, deliver at safe points, and enforce the promotion gate for runs in this workspace. |
| `workflow.mid_run_steering.max_rounds` | `2` | Steering rounds a run may consume at the promotion gate before it holds as `steering_unconsumed`. |

`orbit doctor` reports the resolved values beside the workspace's other workflow settings. `orbit init` does not enable steering for new workspaces. Reconsidering the default requires a separate reviewed change.

### Enablement during a run

The runtime refreshes steering settings at prompt and gate checkpoints through the existing configuration refresh path. Enabling while a run is in flight does not create a watermark retroactively: if the run has no watermark or inherited baseline, its gate treats operator comments after dispatch as undelivered. Disabling stops recording and gating at the next checkpoint and leaves existing watermarks and spent-round records inert, not deleted. Invalid config retains the last successfully resolved settings. Operators should enable between drains or accept a steering round or `steering_unconsumed` hold for genuine in-flight guidance. See [Migration](./specs/steering-delivery.md#migration).

The isolated runtime [replay suite](../../../crates/orbit-core/tests/runtime/mid_run_steering/replay.rs) executes the shipped local and PR pipeline heads through the real steering hooks and `git_commit`, with provider work scripted. It replays guidance at least seven minutes after dispatch without a wall-clock wait: a repair safe point carries it before commit, or the gate spends a round to deliver it. Migration cases edit config without reopening the running runtime and verify the no-watermark baseline and inert leftover state. The adjacent gate suite covers exhausted and unobservable rounds holding as `steering_unconsumed`; watermark, delivery and activity-rendering cases cover default-off prompt and record inertness.

## 6. Concerns & Honest Limitations

- **Delivery is not compliance.** The host can prove a comment entered a prompt; it cannot prove the model weighted it correctly. The gate guarantees the agent *saw* the guidance before promotion, and review remains the judge of whether the candidate honoured it. Any stronger claim would have to read the response envelope, which this design refuses to do.
- **Long single steps still drift.** A forty-minute implement step with no recovery, repair or resume in between receives steering only through the advisory checkpoint re-read, or at the promotion gate afterwards — by which point the expensive work is done. Interrupting a running subprocess is provider-specific (each CLI's session protocol differs) and is out of scope for the generic seam; see [3_vision.md](./3_vision.md).
- **The gate can be gamed by timing.** A comment written after the gate's read but before `commit` completes is, by construction, next run's steering. The gate narrows the blind window from "the whole run" to "one checkpoint", it does not close it.
- **Comment volume is now a cost lever.** Every delivery re-enters prompt tokens and may spend a steering round. The budget and the bounded block cap the blast radius, but an operator (or a chatty automation writing comments) can still slow a run down; system entries are excluded precisely so Orbit's own bookkeeping cannot.
- **Conflicting steering has no merge semantics.** Two comments an hour apart that disagree are delivered oldest-first and the agent arbitrates. The record preserves both; the design deliberately does not invent a supersession model for prose.
- **Owner reachability becomes load-bearing for claimed leaves** at exactly one checkpoint. Failing closed there trades availability for never certifying a stale comment log; a leaf-heavy fleet with a flaky owner link will feel that trade.

## Task References

- [ORB-16606] — added the incident replay through real commit and verified enablement, disablement and default-off behavior in both pipelines.

- [ORB-16577] — added the Muse executor lane; the missed 02:07Z operator comment during its implement step is the incident sections 1–3 are shaped around.
- [ORB-10499] — established the terminal-task precheck and the checkpoint re-read contract through `orbit.task.show`.
- [ORB-12467] — bounded the diagnostics handed to recovery activities (head, tail, size marker), the bounding shape section 2 reuses for the steering block.
- [ORB-16572] — added the strengthen-only `--review` run option, the precedent section 4 cites for what mid-run input may and may not change.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
