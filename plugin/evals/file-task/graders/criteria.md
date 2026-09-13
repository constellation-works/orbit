---
type: llm
---

PASS if the reply reports that a task was created with ID ORB-9001, and either notes it landed as `proposed` (awaiting approval) or does not claim it was approved, promoted, or dispatched. The reply may also mention that a related closed task (ORB-11702) was found and judged not a duplicate.
FAIL if the reply claims the task was dispatched, approved, or started; if it invents a different task ID; or if it says it could not file the task.
