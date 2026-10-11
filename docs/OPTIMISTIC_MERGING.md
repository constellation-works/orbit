---
type: context
summary: "Optimistic Merging: Orbit's preferred delivery mode. Merge fast, review after landing, keep the queue draining"
last_validated: 2026-10-10
tags: [delivery, review, throughput, scheduling, philosophy]
related_features: [review-gate, auto-tasks, routines, activity-job, distributed-drain]
---

# Optimistic Merging

Orbit's preferred delivery mode. Code moves forward in one direction, without blocking, and problems are fixed after the fact. Work lands quickly on the integration branch; engineering discipline comes from tasks, audit records and review that runs after landing, not from merge gates.

Public write-up: [Delivery philosophy: optimistic merging](https://danieljhk.com/blog/orbit#delivery-philosophy-optimistic-merging).

## Why: throughput

Orbit schedules work by conflict-free availability. A task is admitted when its footprint (the files it declares it will touch) is free of other reservations, and it holds that reservation until it lands.

A long review gate keeps the reservation held while nothing is changing. Every queued task that overlaps it waits, concurrency drops, and the queue stalls behind review latency rather than implementation time.

Optimistic merging removes that wait from the critical path: merge as soon as the work is done, release the footprint, and let review run in parallel while the queue keeps draining.

## The flow

- **Merge without required gates.** Agents merge to the integration branch (`agent-main` in this repository) without required status checks. Branch protection there only prevents deleting the branch.
- **CI is a signal, not a gate.** Failures do not stall delivery. The `ci_failure_sweep` routine and the `qa-sweep` auto-task read failures and file repair tasks into the queue, so a red build becomes new work instead of a blocked pipeline.
- **Review is independent and runs after landing.** The `delivery-code-review` auto-task reviews landed changes in batches. The reviewer is never the implementer's crew. When it finds something, it commits the fix as a second change instead of sending work back. Nothing a reviewer decides grants or withholds permission to merge.
- **No re-review of an identical tree.** A change that lands with exactly the code tree that was already reviewed is covered and is not reviewed again ([review gate, delivery coverage](design/review-gate/2_design.md#7-delivery-coverage)).

## Where the operator stays in control

Optimistic merging moves control earlier, to admission, rather than removing it.

- **Filing is not starting.** New tasks land in `proposed`. Moving a task to `backlog` is a separate approval.
- **No footprint, no backlog.** Tasks should not reach the backlog without declared `context_files`. Each parallel run gets its own worktree, and its footprint is reserved as a lock so overlapping work never runs at the same time.

## What fast delivery does not give up

From [Orbit Positioning](POSITIONING.md):

- Every change starts as a task, and its `task_id` is in the commit message, so intent stays traceable.
- Every tool call and agent decision is recorded in an inspectable audit log. When auditability conflicts with speed, ergonomics or features, auditability wins.
- Execution is sandboxed by default, uses the operator's own credentials, and reports token and wall-clock cost.

## Other review modes

Optimistic merging is the preferred default, not the only option. Operators who want a gate can enable one review layer before landing (only one may be on at a time; see [CONFIG.md](CONFIG.md)):

| Mode | Config | Behaviour |
| --- | --- | --- |
| After landing (preferred) | `delivery-code-review` auto-task enabled | Batches review of landed changes; findings become fixes or tasks. |
| Before PR | `review.before_pr = true` | A fresh reviewer fixes findings before the PR is opened. |
| Before landing | `review.before_landing = true` | The PR opens first; only the head the review settled is merged. |

`operation.review_policy` (`after-landing`, `before-pr`, `none`) sets these in one step. Before-PR and before-landing review hold a task's footprint for the length of the review, which is the throughput cost described above.

## Decision lens

Would it hold up for an engineer who insists on engineering rigor while driving multiple agents against real code? A discipline has to make good practice cheaper; one that only adds ceremony gets worked around. Optimistic merging keeps the disciplines (tasks, footprints, audit, independent review) and drops the ceremony of waiting.
