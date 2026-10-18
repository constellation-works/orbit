---
title: Mid-Run Operator Steering — Overview
owner: muse
last_updated: 2026-10-17
last_validated: 2026-10-17
status: Draft
feature: mid-run-steering
doc_role: overview
type: design
summary: Proposed delivery of operator comments written after dispatch to the running agent, gated on the durable task record
tags: [mid-run-steering]
paths: ["crates/orbit-core/src/adapter/engine_host/actions/task_context.rs", "crates/orbit-engine/**", "crates/orbit-core/assets/activities/**"]
related_features: [activity-job, review-gate, distributed-drain, state-compatibility]
related_artifacts: [ORB-16577, ORB-10499]
---

# Mid-Run Operator Steering — Overview

Mid-run operator steering is a proposed, default-off mechanism that delivers task comments written *after* a run has been dispatched to the agent executing that run, at the next safe point, and refuses to promote the candidate while operator guidance the agent has never seen is outstanding. Today an agent works from the prompt snapshot built at dispatch; a correction an operator writes minutes later is invisible until review, when acting on it costs a repair round instead of a sentence. Steering closes that gap through the durable task record — the same record every other gate already reads — rather than through a side channel or the agent's response envelope.

## 1. Motivation

The motivating incident is [ORB-16577] (add Muse Code as a headless executor). The implement step started at 02:00Z from its prompt snapshot. At 02:07Z the operator commented the confirmed model ids, the three crew tiers to document, and a failure-classification requirement. The implementation finished at 02:27Z having never seen that comment; the miss was caught by a human re-reading the task afterwards, not by any gate. Nothing failed: every step succeeded, and the run still produced the wrong artifact.

This is structural, not a prompting lapse. `agent_task_context_json` (`crates/orbit-core/src/adapter/engine_host/actions/task_context.rs`) freezes the task into the prompt at dispatch. The existing mitigation is advisory pull: after [ORB-10499] the activity contract requires the agent to re-read task *status* through `orbit.task.show` at expensive checkpoints so a task that goes terminal mid-run can stop the work. There is no equivalent for comments, no record of which comments a given prompt contained, and no gate that notices unseen operator guidance before commit and promotion. An operator who comments on an in-progress task today has no way to know whether anyone read it.

## 2. Core Concepts

- **Steering comment** — an operator-authored task comment written while a run for that task is live, after the prompt snapshot the run started from.
- **Snapshot watermark** — the durable high-water mark, recorded by the host on the run/step record, naming the newest task comment included in a prompt the host has built for that run.
- **Safe point** — a step boundary where the host builds a new prompt: a recovery attempt, a validation-repair round, a resume or re-dispatch. In the first version these are the only delivery points; no bytes are injected into a running provider subprocess.
- **Delivery** — the host including the comments past the watermark in a newly built prompt and advancing the watermark. Delivery is a fact about what the host wrote, recorded as a history event; it is not a claim the agent understood or obeyed.
- **Promotion gate** — the deterministic check, before the pipeline's commit/promotion checkpoints, that no undelivered steering comment remains when steering is enabled.

## 3. At a Glance

All paths below name where the mechanism lives or would live. The configuration keys, the snapshot watermark and delivery at safe points are implemented; the promotion gate is not ([2_design.md](./2_design.md)).

| Concern | File | Task |
|---------|------|------|
| Prompt construction and watermark capture | [task_context.rs](../../../crates/orbit-core/src/adapter/engine_host/actions/task_context.rs) | [ORB-10499] |
| Step dispatch, recovery and safe points | [docs/design/activity-job/](../activity-job/) | [ORB-10499] |
| Motivating missed comment | — (task record) | [ORB-16577] |
| Delivery invariants | [specs/steering-delivery.md](./specs/steering-delivery.md) | [ORB-16577] |

## Task References

- [ORB-16577] — added the Muse executor lane; its implement run missed an operator comment written seven minutes after dispatch, which motivated this design.
- [ORB-10499] — made an implement invocation able to cancel itself when its task stops accepting writes, and established the re-read-through-`orbit.task.show` checkpoint contract this design extends from status to comments.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
