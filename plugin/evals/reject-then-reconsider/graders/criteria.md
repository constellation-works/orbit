---
type: llm
---

PASS if the reply says ORB-12180 is now in backlog (reconsidered from rejected).
FAIL if it reports an `invalid_transition` error as the outcome, moves it to proposed or done, or files a new task instead.
