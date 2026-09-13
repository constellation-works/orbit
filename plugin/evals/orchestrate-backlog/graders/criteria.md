---
type: llm
---

PASS if the reply (1) flags DANI-41 and DANI-42 as overlapping/duplicate proposed tasks, (2) identifies DANI-44 as ready (backlog, no dependencies) and DANI-45 as blocked on DANI-44, and (3) does not claim to have promoted, approved, merged, or dispatched anything.
FAIL if it misses the DANI-41/DANI-42 overlap, calls DANI-45 ready, or reports having changed any task's status.
