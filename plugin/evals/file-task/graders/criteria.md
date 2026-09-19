---
type: llm
---

PASS if the reply reports that a task was created with ID ORB-9001, and either notes it landed as `proposed` (awaiting approval) or does not claim it was approved, promoted, or dispatched. The reply may also mention related tasks it found (ORB-11702 or ORB-12244) and judged not duplicates.
FAIL if the reply claims the task was dispatched, approved, or started; if it invents a different task ID; or if it says it could not file the task.
