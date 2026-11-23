# Select a crew and record orchestration attribution

## Select an allowed crew

Inspect configured crews and the user's current cost/provider constraints.
Persist the selected task crew, using the workspace's complexity mapping;
record a reason for a deliberate exception. Do not silently remap tasks or
undo an operator's crew change. Reuse existing authorization rather than
asking for the same permission again.

When comparing the implementer models behind candidate crews, read the
per-model ranking in [model ranking](model-ranking.md): `orbit agents rank`
pools each model's crews and effort levels, awards tie bands only on a
qualified board, and leaves sparse or unmeasured evidence unranked. It is
not the model-and-effort scorecard, and an observation from it grants no
authority to change crew policy or a task's crew.

If you take over implementation personally, assign your own configured crew
before continuing and record the takeover; do not retain the delegation choice
as though that crew performed the work. `orchestrator` remains supervision
attribution and does not correct a stale execution crew.

Task crew and system-activity crew are different. Inspect the effective job
and resolved provider/model, not just its crew label. The shipped
`task_pilot_pipeline` draws its pilot crew from `workflow.purpose_crews.task_pilot`,
else `workflow.low_complexity_crews` (`crew_config_key` on the pilot step) and has no per-run crew input: passing
`--input crew=...` does not override that step.

Step-failure and PR conflict
recovery draw from `workflow.purpose_crews.failure_recovery`, else
`workflow.medium_complexity_crews`. Each run draws once per
pool and records it in its state; an empty pool falls back to
`workflow.default_crew`. A host `provider_unavailable` or `provider_capacity`
hold withholds the task-pilot routine while it covers every crew that draw
would use, and the draw skips a held crew while another remains.

A success
that started after the failure, or the end of the backoff, resumes the routine;
a recovered auth probe clears unavailability.
Inspect those pools in the active config; change
persistent configuration only within the user's authorization.

## Record who orchestrated the work

Orchestrator attribution is a separate field from the execution crew. It names
the crew that prepared and supervised the task; it selects nothing, grants no
authority, and never changes which crew or model executes the task. Set it on
creation, when the task is `proposed` or `backlog` — after that it is fixed.

A session can carry the value so every task it creates is attributed without
each call remembering to pass it. Start the server with the crew you orchestrate
as:

```bash
orbit mcp serve --workspace <selector> --orchestrator <your-crew>
```

Precedence is explicit-then-session: a call that passes `orchestrator` uses that
value, a call that omits it inherits the session's, and a session started
without the flag attributes nothing at all. The name is resolved against the
crews of whichever workspace the call lands in, so an unconfigured crew fails
that call rather than being replaced by another one. The default applies only to
tasks the session creates: existing tasks are never rewritten.

Creation also
records the crew's configured model as the task's `orchestrator_model`; pass
`orchestrator_model` on the call only when your session runs a different model.

```bash
orbit tool run orbit.task.add --input '{"workspace":"<selector>","title":"<title>","description":"<description>","complexity":"medium","orchestrator":"<crew>"}'
```

Treat the flag as configuration, not as evidence of who is calling. An MCP
connection outlives a model switch in the client, so a session started under one
crew keeps stamping that crew after the client moves to another. When the
orchestrating crew actually changes, pass `orchestrator` on the individual call
or restart the connection with the new value. `--operator` is unrelated and
unchanged: it decides what a session may do, this decides only what gets
recorded.
