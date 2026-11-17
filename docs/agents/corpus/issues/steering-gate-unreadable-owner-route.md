---
type: issue
applies_to: [final_recovery]
status: active
links: [ORB-24530, ORB-25348]
---

# Claimed-leaf steering gate unreadable on the owner route

## Signature

`[steering_gate_unreadable] step 'commit' did not run: the promotion gate could not read task <id> for run <run>: execution failed: a claimed leaf reads task <id>'s comment log and steering watermark from its owner, which could not answer (federated destination unreachable ... timed out waiting for 'initialize')`; nothing is committed and the candidate stays in its worktree.

## Mechanism

A claimed leaf running on a follower host reads the task's comment log and steering watermark from the owner over the federated route at the steering gate, before commit. When the owner-side federated RPC `initialize` times out, the gate cannot certify the leaf's replica of steering state, so the leaf stops before commit. The failure is a live owner-route read timeout at gate time, not a missing run on another host and not a settlement that omits leaf evidence. Final recovery then escalates because recovery on the follower cannot restore owner connectivity by itself. Observed with friction F2026-11-276.

## Resolution

Restore and verify federated connectivity from the follower host to the owner (including `initialize` and authoritative task/comment reads), then requeue; the uncommitted candidate remains in the follower worktree. In the recorded occurrence the operator requeued with a "first occurrence, retrying" note and the task later delivered. The block-triage recommendation on ORB-24530 names the same remedy and the retained candidate location.

## Fix task

None filed by this pass. ORB-25401 creates this corpus document only so future passes can append occurrences instead of filing repeated follow-ups.

## Occurrences

Observed by pilot-block-patterns pass ORB-25348; evidence is the ORB-24530 task history below.

| Date (UTC) | Task | Run | Matched signature | Outcome |
| --- | --- | --- | --- | --- |
| 2026-11-14 10:23 | ORB-24530 | jrun-20261114-1006-c1 | steering_gate_unreadable at the commit steering gate; owner federated RPC initialize timeout (friction F2026-11-276) | claim_failed 2026-11-14T10:23:36Z; final_recovery (run jrun-20261114-1023-t1) escalated 2026-11-14T10:24:37Z; operator requeued 2026-11-14T10:27:51Z; task later delivered (done) |
