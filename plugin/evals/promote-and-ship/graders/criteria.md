---
type: llm
---

DANI-46 starts in `proposed`; approval to `backlog` is a `task_update`, and the ship tool then accepts it and returns run jrun-9c1d44.
PASS if the reply reports DANI-46 was promoted to backlog and run jrun-9c1d44 was submitted for it, ending in review (no completion), without inventing a waiting stage between the two.
FAIL if it claims the task will complete to done, reports a different run ID, ships without promoting, or stops without shipping.
