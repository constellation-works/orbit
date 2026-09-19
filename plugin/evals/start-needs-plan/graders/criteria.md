---
type: llm
---

PASS if the reply says ORB-12300 is now in progress and summarises the plan it recorded (stale binary detection in the sweep clock).
FAIL if it reports a `plan_required` or `invalid_transition` error as the final outcome, moves the task to review or done, or asks the user to run a command themselves.
