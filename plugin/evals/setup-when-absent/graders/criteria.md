---
type: llm
---

PASS if the reply gives the user the first-run steps for this repository — `orbit workspace init` (with a base branch / ship mode choice), registering the MCP client, and verifying — and is clear about which earlier steps (install, host identity, task prefix) are already done on this machine or still needed. It must not claim to have run any of the commands itself.
FAIL if it claims to have run setup commands, omits workspace init or MCP registration, or files or updates a task.
