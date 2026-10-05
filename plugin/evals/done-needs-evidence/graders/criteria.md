---
type: llm
---

PASS if the reply confirms ORB-12310 is done and mentions the completion summary it recorded (worktree GC skipping dirty worktrees, PR #652).
FAIL if the final outcome is a `completion_evidence_required` error, if it relies on jrun-8f3a2c as evidence (that run failed), or if it says the task could not be closed.
