---
title: "Eval Harness — Overview"
owner: claude
last_updated: 2026-11-14
last_validated: 2026-11-14
status: Draft
stage: designing
next: "Daniel answers the open questions in 2_design.md §14; no implementation task is filed yet"
feature: eval-harness
doc_role: overview
type: design
summary: "An evaluation plugin with two jobs: replay closed tasks against chosen crews and score the candidates, and bake off one new task across crews as candidate pull requests a person picks from"
tags: [eval-harness, plugins, evaluation, experiments, crews]
related_features: [plugins, experiments, agent-scorecards, model-ranking, review-gate, distributed-drain]
related_artifacts: [ORB-24839]
---

# Eval Harness — Overview

The eval harness is a proposed Orbit plugin (`eval`) with two jobs. **Replay** takes closed tasks that have a known good merged diff, runs each again at the commit before that merge under several crews, and scores what each crew produced. **Bake-off** takes one new task, has several crews implement it as separate candidate pull requests, and lets a person pick the one to merge. Nothing here is built. [2_design.md](./2_design.md) is the design, written for [ORB-24839]; it ends with the build order and the questions Daniel has to answer first.

## 1. Motivation

Orbit can compare crews only on live work today. [Experiments](../experiments/1_scope.md) assign each new task to one arm when it is first admitted, and [scorecards](../agent-scorecards/1_overview.md) and the [model ranking](../model-ranking/1_overview.md) summarise what landed. All three have the same two limits:

1. **One crew per task.** No task is ever implemented twice, so a difference between two crews is always mixed with a difference between the tasks they happened to get. It takes weeks of live traffic to average that out.
2. **The candidate is real delivery.** Trying an unproven model means letting it ship work.

Replay removes both limits for measurement: the same 20 to 30 tasks go to every crew, off the delivery path, and the merged diff gives each task a reference. Bake-off is the delivery-side counterpart: when one task matters enough to pay for n implementations, a person chooses between finished candidates, and each choice is a labelled preference that replay can reuse.

Daniel's request (2026-11-14) asked for a plugin: given a task and chosen models, find the base commit and run n parallel jobs; and, as a separate job, do the same for a new task so the user can pick the best to merge.

## 2. Core Concepts

- **Arm.** One crew (provider, model and effort) under test. Arms name crews, never bare models, because every Orbit run resolves its model through a crew.
- **Candidate task.** A copy of a task, with one arm's crew set explicitly, that an ordinary Orbit run implements. Both jobs are built on it: neither ever runs the original task twice. See [2_design.md §1](./2_design.md#1-the-shared-primitive).
- **Evaluation workspace.** A separate Orbit workspace on a Linux box, over its own clone of the repository, where replay's candidate tasks live. It has its own task store, locks, drain and analytics, so replay cannot touch live delivery records.
- **Snapshot.** The task as its first implementer received it: title, description, acceptance criteria, selectors, tags, type and tier, and nothing written afterwards.
- **Reference.** The merged diff of the original task. It supplies the base commit (its first parent) and the held-out tests. It is a reference, never an answer key: a candidate that solves the task differently can score full marks.
- **Held-out tests.** Test files the reference added or changed. The candidate never sees them; they are copied over its checkout after it finishes and run there.
- **Eval set.** A named, versioned list of entries, each holding a snapshot, a base commit, a reference commit and a proven held-out test command.
- **Readout.** The per-arm result of one replay over one eval set: pass rates with intervals, cost, time and diff size.
- **Pick.** The bake-off decision: which candidate was merged, by whom and why. A pick is a labelled preference between diffs for the same task.

## 3. At a Glance

Nothing is implemented. The table maps each concern to where the design settles it and to the existing code it rests on.

| Concern | Settled in | Existing code it rests on |
|---------|-----------|---------------------------|
| Whether Orbit can already run a task detached | [2_design.md §1](./2_design.md#1-the-shared-primitive) | [worktree/setup.rs](../../../crates/orbit-engine/src/executor/automation/vcs/worktree/setup.rs), [transitions/admission.rs](../../../crates/orbit-core/src/application/task/transitions/admission.rs), [task_context.rs](../../../crates/orbit-core/src/adapter/engine_host/actions/task_context.rs) |
| Replay: base commit and snapshot | [2_design.md §3](./2_design.md#3-replay-inputs-base-commit-and-snapshot) | [landed_diff_stats.rs](../../../crates/orbit-store/src/contracts/landed_diff_stats.rs), [audit_envelope.rs](../../../crates/orbit-types/src/workflow/activity_job/audit_envelope.rs) |
| Replay: leakage | [2_design.md §4](./2_design.md#4-replay-leakage) | [task_context.rs](../../../crates/orbit-core/src/adapter/engine_host/actions/task_context.rs) |
| Replay: scoring, eval sets, readout | [2_design.md §5](./2_design.md#5-replay-outputs-and-scoring) to [§7](./2_design.md#7-replay-readout) | [experiment readout](../../../crates/orbit-core/src/application/experiment/readout), [model_prices.yaml](../../../crates/orbit-common/assets/model_prices.yaml) |
| Bake-off: lifecycle, locks, review, pick, cleanup | [2_design.md §8](./2_design.md#8-bake-off-job) | [locks/index.rs](../../../crates/orbit-runtime/src/task/locks/index.rs), [pr/open/superseded.rs](../../../crates/orbit-engine/src/executor/automation/vcs/pr/open/superseded.rs), [worktree/gc/sweep.rs](../../../crates/orbit-engine/src/executor/automation/vcs/worktree/gc/sweep.rs) |
| Capacity and spend | [2_design.md §9](./2_design.md#9-capacity-and-spend) | [task.rs dispatch order](../../../crates/orbit-types/src/task/model/task.rs), [provider_limit/budget.rs](../../../crates/orbit-config/src/provider_limit/budget.rs) |
| Relation to experiments | [2_design.md §10](./2_design.md#10-relation-to-live-experiments) | [experiments scope](../experiments/1_scope.md) |
| Plugin location, tools, schemas, authority | [2_design.md §11](./2_design.md#11-plugin-shape) | [plugins scope](../plugins/1_scope.md) |
| Build order and open questions | [2_design.md §13](./2_design.md#13-build-order) and [§14](./2_design.md#14-open-questions-for-daniel) | — |

## Task References

- [ORB-24839] — wrote this design; it changes no code or configuration and files no implementation task.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
