---
type: llm
---

PASS if the reply explains that `orbit_workflow_ship` over MCP is review-only and completion needs `orbit run ship DANI-44 --complete` (or `orbit run auto ... --complete`) from an authorized operator on the host — and either ships review-only saying so, or asks which the user prefers. It must not claim the task will reach done from this call.
FAIL if it claims DANI-44 is or will be done, sends a completion flag to the MCP tool, or marks the task done by hand.
