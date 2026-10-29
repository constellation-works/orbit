---
type: context
summary: "Optimistic Merging: Orbit's preferred delivery mode. Merge fast, review after landing, keep the queue draining"
last_validated: 2026-10-29
tags: [delivery, review, throughput, scheduling, philosophy]
related_features: [review-gate, auto-tasks, routines, activity-job, distributed-drain]
---

# Optimistic Merging

Orbit's preferred delivery mode. Required CI checks gate merges to the integration branch, while independent code review can run after landing. In that mode, the declared footprint is released at merge and review findings become follow-up work instead of holding the queue.

Public write-up: [Delivery philosophy: optimistic merging](https://danieljhk.com/blog/orbit-optimistic-merging#delivery-philosophy-optimistic-merging).

## Why: throughput

Orbit schedules declared file footprints by conflict-free availability. A task with a nonempty `context_files` footprint can be admitted when those selectors do not overlap active reservations; selector-free tasks have no file reservation. Reservations protect the declared footprint through landing.

A long review gate keeps a declared file reservation held while nothing is changing. Every queued task with an overlapping declared footprint waits, concurrency drops, and the queue stalls behind review latency rather than implementation time.

With after-landing review, the required checks pass before merge, then the footprint is released and review runs in parallel while the queue keeps draining.

## The flow

- **Required PR checks gate merges.** PRs to the integration branch (`agent-main` in this repository) require `Check / Clippy / Test` and `Test shard 1/3`, `Test shard 2/3`, and `Test shard 3/3` to pass. The release branch `main` is separate.
- **Post-landing CI and QA findings feed the queue.** The `ci-failure-sweep` routine files deduplicated proposed findings for CI failures, pilots them, and admits only current warning-free repairs. The `qa-sweep` auto-task exercises recent changes through user-facing paths and files tasks for non-duplicate issues it finds.
- **Review is independent and can run after landing.** When enabled, the `delivery-code-review` auto-task reviews landed changes in batches; the shipped after-landing auto-task is disabled by default. Crew selection prefers a crew that did not implement the reviewed work, falling back when the available pool has no alternative. It records confirmed findings as follow-up bug tasks and does not change an already-landed delivery. After-landing review does not grant or withhold permission to merge.
- **No re-review of an identical tree.** A change that lands with exactly the code tree that was already reviewed is covered and is not reviewed again ([review gate, delivery coverage](design/review-gate/2_design.md#7-delivery-coverage)).

## Where the operator stays in control

Optimistic merging moves control earlier, to admission, rather than removing it.

- **Filing is not starting.** New tasks land in `proposed`. Moving a task to `backlog` is a separate approval.
- **Declare a footprint for file reservations.** Tasks should carry `context_files` before backlog admission. Orbit still admits selector-free backlog tasks in some paths, but an empty context holds no file lock. Each parallel run gets its own worktree; declared footprints reserve overlapping files so those tasks do not run at the same time.

## What fast delivery does not give up

From [Orbit Positioning](POSITIONING.md):

- Every change starts as a task, and its `task_id` is in the commit message, so intent stays traceable.
- Inspectable audit records cover commands, workflow runs, and captured provider/tool activity; the [auditability design](design/auditability/1_overview.md) describes coverage limits. When auditability conflicts with speed, ergonomics or features, auditability wins.
- Execution is sandboxed by default, uses the operator's own credentials, and reports token and wall-clock cost.

## Other review modes

Optimistic merging is the preferred default, not the only option. Operators who want a gate can enable one review layer before landing (`review.timing` selects one; see [CONFIG.md](CONFIG.md)):

| Mode | Config | Behaviour |
| --- | --- | --- |
| After landing (preferred) | `delivery-code-review` auto-task enabled | Batches review of landed changes; confirmed findings become follow-up bug tasks. |
| Before PR | `review.timing = "before_pr"` | A fresh reviewer fixes findings before the PR is opened. |
| Before landing | `review.timing = "before_landing"` | The PR opens first; only the head the review settled is merged. |

The legacy `operation.review_policy` values (`after-landing`, `before-pr`, `none`) and the `review.before_pr` and `review.before_landing` booleans are still accepted with a deprecation warning. New configuration uses `review.timing` and the `delivery-code-review` auto-task flag. Before-PR and before-landing review hold a task's declared footprint for the length of the review, which is the throughput cost described above.

## Decision lens

Would it hold up for an engineer who insists on engineering rigor while driving multiple agents against real code? A discipline has to make good practice cheaper; one that only adds ceremony gets worked around. Optimistic merging keeps the disciplines (tasks, footprints, audit, independent review) and drops the ceremony of waiting.
