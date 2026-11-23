# Inspect local required-validation replay

## Local required-validation replay

A failed local required command can receive one bounded, isolated replay of
the selected failing tests on the same candidate. This is an automatic
validation evidence path, not permission to repeat checks manually or to add
retries.

The replay is eligible only when the synchronized base has a comparable
passing result without a host-coverage refusal, the original candidate command
did not pass, time out, lack a required tool, fail from infrastructure or
network trouble, defer execution, or refuse host output, and the runner can
identify one to three failing tests. If these conditions do not hold, no
candidate replay can establish a pass.

When eligible, the system runs the
original command once with the exact failing-test selection on the same
candidate.

The replay resolves the original command only if it passes without a timeout,
missing tool, infrastructure failure, deferred outcome, or host-output
refusal; reports exactly the handed failing-test selection; and executes at
least as many tests as that selection contains. This ties the passing result
to every named original failure.

A qualifying replay is recorded in task
history as `flaky_validation`, and delivery may continue with the original
failure and successful replay preserved as evidence. An attempted replay that
fails or cannot prove the required selection and coverage remains a required
command failure. Its replay log is kept, while the original log has no
accepted `flaky` record. Ineligible failures remain failures as well.

### Inspect the local replay evidence

List a task's artifact paths and history first:

```bash
orbit tool run orbit.task.show --input '{"id":"<task-id>","fields":["artifacts","history"]}'
```

For the relevant command index, fetch the original candidate log, its base
comparison log, and the replay log when present:

```bash
orbit tool run orbit.task.artifact --input '{"action":"get","id":"<task-id>","path":"validation/<run-id>/<index>.json"}'
orbit tool run orbit.task.artifact --input '{"action":"get","id":"<task-id>","path":"validation/<run-id>/<index>.baseline.json"}'
orbit tool run orbit.task.artifact --input '{"action":"get","id":"<task-id>","path":"validation/<run-id>/<index>.rerun.json"}'
```

The original log's `candidate_rerun` field links to the last path. The replay
log identifies its `role` as `candidate_rerun`; the base log is named by the
original log's baseline record. Cross-check `command` and `run_id` across all
three logs. The original log's `base_sha` should match the base log and the
accepted history record; its `tested_head` should match the replay log and
history record. Compare each log's `validation_env` and captured outputs.

The
typed `flaky_validation` history record binds `command`, `run_id`, `base_sha`,
and `tested_head`, and carries the selected failing tests, original/base/replay
outputs, replay environment, and `rerun_tests_run`. Confirm the event's record
matches the artifacts before treating delivery as resolved. If the replay
path or accepted history event is absent, the command has no accepted local
replay evidence.

This mechanism is separate from [hosted-check diagnosis](run-debugging-hosted-checks.md#hosted-check-failures-and-inherited-red-bases), which classifies provider jobs and their permitted landing reruns. A red or unproven base follows the separate [red-base hold procedure](../../orbit-setup/references/automation-built-in.md#built-in-re-checking-tasks-held-for-a-red-base).

A workspace-local `flaky-test-hunt` auto-task, where one is defined, analyzes hosted CI history and files tasks; it does not explain this local command replay.
