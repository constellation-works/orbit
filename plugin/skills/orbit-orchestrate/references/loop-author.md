# Search for existing work and author a task

## Search, then author

Search open and closed tasks before filing findings from sweeps, CI, review or
curation. For direct user requests, follow the task-authoring guidance rather
than adding an unnecessary discovery step. Use `orbit_search` with
`kind: "task"`, `all: true`, the explicit workspace, and concrete problem
terms; inspect likely matches and their merge evidence. Use distinctive terms
from the title and description.

A useful task states the observed problem or feature opportunity, its value,
bounded scope, and acceptance criteria describing observable behavior.
Separate independent changes so file reservations do not unnecessarily
serialize them. Do not turn a single incident into a speculative framework.
For research, state a falsifiable hypothesis, validation method, and what
would refute it; a simulation of assumed equations does not validate nature.

See [task-authoring-contract.md](../../orbit/references/task-authoring-contract.md).

Create as `proposed` with required `complexity` (`low`, `medium`, `hard`, or
`xhard`).
Use the current session's preparation authorization; task creation alone does
not grant implementation or completion. Leave `context_files` empty unless
modification targets are already verified. Selectors identify modification/deletion
targets and known creation intent (using supported missing-context input), not
background reading or invented paths.
