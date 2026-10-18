---
type: design
summary: "Spec: Steering Delivery"
last_validated: 2026-10-18
---

# Spec: Steering Delivery

When `workflow.mid_run_steering.enabled` is `true`, no candidate for a task is promoted while an operator comment written after the run's prompt snapshot remains undelivered to an agent step of that run. Delivery, refusal and budget are all decided from the durable task and run records; agent output is never consulted.

## Why This Exists

Without this contract, an operator correction written during a run can be silently ignored by every green step and surface only in review as avoidable repair work, as in [ORB-16577]. The contract converts that silent miss into either a delivery or a named, operator-visible hold.

## Invariants

1. **Monotonic watermark.** A run's snapshot watermark never moves backwards, and advances only when the host builds a prompt containing the comment it advances past.
2. **Deliver at most once.** The same comment enters prompts of one run at most once via the steering block; the advisory checkpoint re-read may show it to the agent again but never re-records delivery.
3. **Oldest first, bounded.** A steering block lists undelivered operator comments oldest first and is size-bounded (head, tail, marker naming the full size and `orbit task show <id>`); a truncated block still advances the watermark only past comments whose full text it carried.
4. **Gate before promotion.** The promotion check runs before the `commit` step of both `task_pr_pipeline` and `task_local_pipeline`, while the candidate is still uncommitted in its worktree. Undelivered steering at the gate consumes a steering round; exhausting `workflow.mid_run_steering.max_rounds` holds the task blocked with reason `steering_unconsumed`, naming the outstanding comments. It never fails the run silently and never proceeds on a failed read.
5. **Owner is authoritative.** For a claimed leaf, the comment log and watermark are read from the owner's record. An unreadable owner at the gate fails closed; a replica's comment log is never certified.
6. **No authority through steering.** Delivering a comment changes no captured run input: lock surface, completion authority, required validation, crew and review policy remain what admission froze. Lifecycle writes still go through `orbit.task.update` gates.
7. **Envelope-independent.** No field of any agent response can advance the watermark, consume or refund a round, or satisfy the gate.
8. **Default off is byte-inert.** With steering disabled, prompt bytes, task records, run records and checkpoint behaviour are identical to a build without the feature. Watermark fields are optional everywhere they persist.

## Failure Modes

- **Task goes terminal with steering outstanding.** Terminal handling wins ([ORB-10499]): the run stops as it does today; outstanding steering is reported in the step outcome, not delivered to a dying step.
- **Comment arrives during the gate's own read.** The gate's verdict covers the watermark it read; the new comment is undelivered steering for the next gate or run. It is never retroactively counted as delivered.
- **A steering round cannot observe the comment** (task read fails at dispatch preparation). The round does not count as delivery, its admitted budget is still settled, and the original failure is preserved — mirroring recovery-attempt accounting.
- **Two runs of one task overlap** (retry lineage). Watermarks are per-run; delivery by one run does not advance another's, and the gate judges the run asking to promote.

## Migration

Enabling steering in a workspace with runs already in flight: those runs hold no watermark, so their gate reads treat every operator comment written after their dispatch timestamp as undelivered. Operators enable steering between drains, or accept one `steering_unconsumed` hold per in-flight run that had genuine post-dispatch comments. Disabling steering mid-flight stops recording and gating at the next checkpoint; existing watermark state is left inert, not deleted.
