---
type: issue
applies_to: [agent_implement, final_recovery]
status: active
links: []
match_infra_class: lock_busy
---

# Lock acquisition deadline

## Signature

The engine classifies a deterministic delivery action's failure as `lock_busy`:
`remote tool failed (lock_busy)` or `timed out after …ms acquiring … lock`.

## Mechanism

Another operation owns the coordination lock beyond this action's acquisition
deadline. The failure does not establish whether the proposed task edit is wrong.

## Resolution

Read the owner and lease evidence. Wait for legitimate work to finish and use
only the recovery actions the activity authorizes. Escalate when ownership is
unknown or the bounded retry is exhausted. Never remove another run's lock.

## Fix task

No general fix task: a live owner can legitimately hold the lock. File a cause
fix only with evidence that ownership or lock release is defective.

## Occurrences

No historical incidents have been imported. Engine-classified matches append
structured records through the task-history outbox and reviewed batch path.
