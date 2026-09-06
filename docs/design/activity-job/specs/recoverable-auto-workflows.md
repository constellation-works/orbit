---
type: design
summary: "Spec: Recoverable automatic workflows"
tags: ["activity-job", "workflow", "recovery", "semantic-search"]
last_validated: 2026-09-05
---

# Spec: Recoverable Automatic Workflows

## Status and Scope

This document specifies a planned opt-in durability mode for automatic workflow execution:

```text
orbit run auto --recover [--for <duration>]
```

The initial scope is `workspace_auto_pipeline` and its loose task leaves. The recovery primitive
is deliberately reusable by `epic_pipeline`, but adopting it for epic descendants is a separate
change. Without `--recover`, the current fail-fast behavior remains unchanged.

`--recover` is not a promise that every task will become `done`. It is a promise that every task
admitted by the auto run will be carried as far as workspace delivery policy permits and will not
be left stranded in `in-progress` when the parent finishes. In a PR workspace, ordinary changed
work stops at `review`; Orbit never merges merely because recovery mode is enabled.

## Problem

Automatic draining currently joins a child `task_auto_pipeline` run with
`pipeline_success_guard`. One failed leaf makes the guard fail, which ends the workspace drain even
when sibling tasks succeeded and unrelated backlog remains. Direct shipment steps may already
declare `step_failure_recovery`, but that hook repairs one failed step inside one child run. It does
not give the parent a durable policy for completing, classifying, or isolating the failed task.

No-repository-diff work exposes the same missing contract. An implementing agent can discover that
the requested change already landed, but its response content is advisory. Taking the agent's word
would weaken the durable delivery boundary; treating a clean worktree as unconditional failure
turns valid duplicate or already-satisfied work into a workflow-wide outage.

## CLI and Job Input

`orbit run auto` gains a boolean `--recover` flag. The CLI persists it as
`input.recover: true|false` on `workspace_auto_pipeline`; run inspection and replay therefore retain
the selected policy. Job defaults set `recover: false` so existing CLI, MCP, dashboard, and routine
callers keep fail-fast semantics until they opt in explicitly.

The first version has a fixed bounded budget: one recovery-agent dispatch and at most one linked
resume or replacement shipment per failed task. Public attempt-count and timeout knobs wait for
production evidence; an unbounded recovery loop would make a drain window meaningless.

## Durability Invariant

When a recover-mode auto run becomes terminal, every task it admitted has one durable outcome:

| Disposition | Required evidence | Task state |
|---|---|---|
| `delivered` | Commit/local merge or confirmed PR handoff produced by the task workflow | `done` when workspace policy completed delivery; otherwise `review` |
| `already_satisfied` | An existing delivered change satisfies the task and the current acceptance checks pass | `done` |
| `side_effect_only` | The task declared no repository diff and its required durable artifacts or external effects are verified | policy-valid terminal state |
| `blocked` | Recovery exhausted its bounded budget or could not prove a safe completion | `blocked` with a precise reason |

The disposition is typed workflow state, not a keyword parsed from prose. An agent may propose a
disposition and supporting evidence, but only a deterministic gate may persist a successful
completion disposition or transition the task to `done`.

## Failure Isolation

Recovery mode distinguishes task-local failures from systemic failures using typed error classes,
not message substrings.

Task-local failures include implementation, validation, commit, branch, push, PR handoff, and
no-diff classification failures tied to a specific admitted task. They dispatch task-scoped
recovery and do not terminate the workspace drain.

Systemic failures include an unloadable job/activity catalog, an unavailable task/run store,
failure to classify the workspace backlog, loss of the workspace claim, or inability to execute
the recovery machinery itself across the workspace. These terminate the parent: mass-blocking
tasks during a shared infrastructure outage would destroy useful state rather than recover it.

## Recovery Flow

In recover mode, the parent consumes structured per-task child results instead of applying one
aggregate `pipeline_success_guard`:

```text
for each drain iteration:
    classify admissible work
    run task_auto_pipeline and wait for all selected task gates
    for each failed task result:
        dispatch task_recovery_pipeline(task_id, source_run_id, base, mode)
        wait within the bounded recovery budget
        persist the verified disposition
    continue draining unrelated work
```

`task_recovery_pipeline` receives the exact task, source run, worktree/branch identity, pinned base,
failed step, and typed error. It first inspects durable checkpoints and current Git/task/PR state.
It then chooses one of four paths:

1. Repair state narrowly and submit a linked resume from successful checkpoints.
2. Submit one replacement task shipment when the implementation itself must run again.
3. Propose `already_satisfied` or `side_effect_only` evidence to the deterministic completion gate.
4. Persist a block reason when none of the above is safe.

The parent records task IDs it has already recovered in its pipeline checkpoint so a later drain
iteration cannot redispatch the same failure. A task that recovery leaves `blocked`, `review`, or
`done` is naturally absent from backlog discovery as well.

Recovery-dispatch errors must be preserved in the audit and task history. They must not collapse to
`recovery_succeeded: false` with the underlying error discarded.

## Proving That No New Work Is Needed

Semantic similarity is a candidate generator, never completion authority. A summary that merely
mentions the current task can rank above the task that implemented the fix, so neither top rank nor
a cosine threshold proves equivalence.

The recovery agent uses `orbit search similar <task-id>` to retrieve a small candidate set. The
deterministic completion gate then verifies all of the following:

1. The candidate task is terminal and predates the current recovery decision.
2. The candidate identifies concrete delivered commit(s) or another durable artifact.
3. Required commits are reachable from the current pinned base or landing branch.
4. Candidate modification targets and behavior cover the current task's requested outcome; a text
   match or task mention is insufficient.
5. The current task's acceptance checks pass against the current base.

The persisted completion evidence contains the current task ID, candidate task IDs, commit/artifact
identifiers, pinned base SHA, validation commands and results, semantic model ID, candidate scores,
and timestamp. `orbit run show` exposes the evidence and task history links the recovery/source run.

`no-diff-expected` remains a pre-dispatch declaration for work known to be side-effect-only. It is
not retroactively inferred from an agent response. `already_satisfied` is the separate outcome for
work discovered to have landed before or during execution.

## Similarity Calibration

Calibration uses labeled historical pairs split into tuning and held-out sets. Measure recall at a
small `K`, false-candidate rate, and verifier workload per recovered task. The selected raw-cosine
threshold only decides whether a candidate is worth deterministic verification; falling below it
does not prove work is required, and exceeding it cannot transition a task.

Thresholds are tied to the embedding model ID because model changes can shift score distributions.
Hybrid reciprocal-rank-fusion scores are ranking values and must not be used as confidence
thresholds. Until the labeled trial supports a threshold, recovery inspects a fixed small top-K
candidate set and records the score distribution for later analysis.

## Parent Run Outcome

A recover-mode parent succeeds when it completed orchestration and every admitted task has one of
the durable dispositions above. Task-local blocks are reported as
`workflow_status: completed_with_blocks`, with counts and task/recovery run IDs, but do not make the
parent a systemic failure. A caller can alert on `blocked_count` without losing the signal that the
drain itself remained healthy and continued shipping work.

The parent fails only for systemic errors or when it cannot durably account for an admitted task.
It must never report success while a claimed task remains `in-progress` without a live child or
recovery run.

## Safety and Authority

- Agent response prose and advisory result fields never satisfy delivery or completion gates.
- Semantic scores never mutate lifecycle state.
- Recovery never merges a PR or bypasses workspace delivery policy.
- A clean worktree is not sufficient evidence of `already_satisfied`.
- Every repair, resume, disposition, and failed recovery dispatch is auditable by run and task ID.
- Recovery is idempotent across parent replay: existing verified outcomes are reused, not repeated.

## Implementation Slices

1. Add `--recover`, persist the input, and expose recover-mode output fields.
2. Preserve per-task child results and isolate task-local failure from the parent drain loop.
3. Add the bounded task recovery job, linked resume/replacement flow, and complete recovery errors.
4. Add the typed completion-evidence gate and `already_satisfied` disposition.
5. Add semantic candidate retrieval, labeled calibration fixtures, and recovery reliability metrics.

Each slice must include an end-to-end job test. The critical regression proves that one leaf can
fail, recover or block, and still allow a later unrelated task to ship in the same auto window.
