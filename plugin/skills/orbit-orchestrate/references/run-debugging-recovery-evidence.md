# Read recorded step recovery evidence

## Use Orbit Inspection Commands First

Use the public inspection surface to inspect the run:

```bash
orbit run show <run_id> --json
orbit run events <run_id> --json
orbit run trace <run_id>
orbit run logs <run_id> --json
```

### Inspect recovery evidence before falling back to audit files

The authoritative run-show response includes `recovery_attempts`, a bounded
projection of persisted `step.recovery_attempted` events. Its `state` is
`unavailable` for legacy runs with no v2 audit trail, `not_attempted` when a
trail exists but recovery did not run, or `recorded` when `items` contain
attempts. Each item names the durable run/event and failed-step identifiers,
the recovery activity, outcome, failure phase, and a redacted bounded
diagnostic.

`limit` and `truncated` say when older attempts were omitted.

An item's `outcome` says only whether the recovery activity completed. It is
not the decision-file verdict, the final host admission, or the retry's own
result. Those three records stay separate.

The decision-file verdict is the file `step_failure_recovery` writes under the
worktree's `.orbit/tmp/step-recovery/`. Read it from `decision.status` and
`decision.verdict`, never from the `recovered` field of the activity response.
`decision.status` is `verified` (with `verdict` `retry`, `not_recovered` or
`external_blocker`), `absent` (no file), `invalid` (malformed, stale, for
another run/step/attempt, or an unsafe path) or `unavailable` (the host could
not read it).

`detail` names why a file was rejected or what an absent file
observed. `invalid` and `unavailable` never admit a retry.

Final host admission is `retry_admitted`. It starts from that verdict: a
verified `retry` admits a post-recovery attempt, an absent file admits one
only when the engine observes a worktree, base, or validation-environment
change since the failure, and every other verdict does not. The host can still
change that admission after the file is read, without rewriting the verdict.

A verified `retry` whose repair the host refuses to commit — a protected path,
a non-regular file, an unexplained CI-repair path, a no-diff route, or a
candidate already under review — keeps `decision.verdict` as `retry` and sets
`retry_admitted` to `false`. The step returns that host refusal
(`recovery_commit_refused:…`) with the original failure, and the no-retry
warning names host admission and the same refusal, not the decision file.

| Recovery attempt retention |
|---|
| One other host adjustment applies to an admitted `--auto-fix` check repair: when `step_failure_recovery` leaves no worktree delivery edit and writes no scope record, an `absent` or verified `not_recovered` decision still admits one post-recovery check-rerun attempt once the host records the no-edit rerun (`decision` preserves the decision file's actual `status` and `verdict`, and `retry_admitted` is `true`; if the no-edit check or rerun ledger write refuses, `retry_admitted` stays `false` and no retry runs). |

Older runs and
other recovery activities carry no `decision`.

The retry outcome is recorded separately, and only when an attempt ran: the
`step.post_recovery_attempt` audit event and the
`orbit.job.step_post_recovery_attempt` log line. It is absent when the
decision file or final host admission refused the attempt.

An implementer response refused only for its shape (such as a bare file name in
`task_artifacts`) is corrected once inside the implement step: the
`step.contract_correction` audit event (`orbit.job.step_contract_correction` log
line) names the field, and no recovery event follows when the correction works. A
response still malformed after it fails with the original refusal.

An implementer that declares a task-level stop has two equivalent shapes:
`blocker: {kind, evidence}` on a successful envelope (the preferred shape), and
a `status: "failed"` envelope (exit 0) whose `error.code` names the stop and
whose `error.message` gives non-empty evidence.

The code names a stop when it is
exactly `unresolved_policy_decision` or contains blocker, blocked, contradiction
or unavailable as a whole word split on `_ . : -` (for example,
`validation_blocker` or `live_fixture_unavailable`).

The step boundary normalizes the
second shape into the first: the step fails with the `[task_blocked_by_agent]`
marker and `kind=<error.code>`, retry, step recovery and final recovery are
skipped, the failure activity receives `error_code=task_blocked_by_agent`, the
candidate is preserved uncommitted, and the run ends `held`, not `failed`.

Codes Orbit already reserves — the typed holds (`owner_route_unavailable`,
`owner_signoff_required`, `footprint_unavailable`), the provider classes,
`validation_environment`/`validation_route_unavailable`, `transient_failure`,
`stalled`, and `implementation_incomplete` (its continuation path) — keep
their existing outcomes and are never re-read as a blocker kind. A malformed
envelope (no well-formed `error.code`/`error.message`) is not a blocker and
keeps its ordinary failure.

Keep `error_code` and `error_message` from the run itself as the original
workflow failure. A recovery attempt is secondary evidence: `succeeded` does
not rewrite that original failure, and a failed `authorization`, `decision`,
preparation, `dispatch`, or `activity` attempt explains why recovery did not
complete.
