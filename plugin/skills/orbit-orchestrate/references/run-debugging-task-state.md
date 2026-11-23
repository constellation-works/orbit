# Match a run to its task state

## Historical agent invocations

Operator invocations and delegated explorers are no longer submitted. Stored
runs retain their answers and captured output through `orbit run show <RUN_ID>`
and `orbit run logs <RUN_ID>`. An old trusted-host admission cannot authorize
a new run or a resume; use a task workflow for new work.

## Check Task State

```bash
orbit tool run orbit.task.show --full --input '{"id":"<task_id>"}'
```

Check status/history, plan/execution_summary, comments, workspace_path, external_refs/PR metadata, dependencies/resolved_dependencies. If implementation succeeded but a later workflow step failed, the task may already have a useful execution summary — preserve that context.

### Replay task-pilot assessment (operator only)

When an acceptance criterion depends on task-pilot behavior, replay it through
the operator path with disposable fixture tasks. File the fixtures through
the normal task flow so they land in `proposed`; tag each `no-drain` and
`no-auto-approve`. Include a fixture expected to produce the promotion hold
and a control whose complete, selectable request should not be held. These
tags keep automatic drains and proposed-task approval away from the fixtures
while still allowing pilot assessment.

For each fixture, request a full assessment with operator authority, then
inspect both the task and the task-level attention view:

```bash
ORBIT_OPERATOR=1 orbit task repilot <id> --full
orbit task show <id>
orbit attention --kind pilot_warning --task <id> --json
```

Compare the assessment and any `pilot_warning` promotion hold with the
expected fixture outcome; the control should have no pilot warning. Archive
all fixtures with `orbit task archive <id>` after recording the results.
