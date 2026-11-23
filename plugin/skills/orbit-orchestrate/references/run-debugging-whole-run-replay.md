# Choose when to replay a whole run

## Whole-run replay from step zero

Use `orbit job resume <run_id>` when the run's successful top-level
checkpoints are valid and the remaining work should continue from them. Resume
creates a linked run that reuses those outputs and skips the completed steps.

Use `orbit job replay <run_id>` when checkpoint outputs cannot be trusted or
the job must restart from the beginning. Replay creates a new run linked to the
source, uses its persisted input with the current job definition, and executes
from step zero; it leaves the source run unchanged. Replay runs in the
foreground and exits nonzero unless the new run succeeds. If you hold the
workspace claim, pass its token with `--claim-token <token>`.

Replay is refused for a replica checkout or when a task is held by a live
execution claim. After replay, use the new run ID with
`orbit run show <new_run_id>` and `orbit run events <new_run_id>` to confirm it
links to the source, starts its steps from the beginning, and reaches the
intended terminal state.
