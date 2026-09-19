---
type: llm
---

PASS if the reply says jrun-8f3a2c (task ORB-12310) failed in the validate step because `cargo test` failed on `worktree::gc::skips_dirty_worktree`, classifies it as a validation failure (implementation completed, tests failed), notes the process is not running and no push/PR happened, and recommends a next step.
FAIL if it blames the implementation step or a provider/sandbox error, claims to have resumed or fixed the run, or changes the task's status.
