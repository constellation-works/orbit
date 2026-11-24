# Inspect delivery repair without a handoff

## Review deliveries without a handoff

A dashboard ship, `orbit run ship` and an owner drain leaf all deliver through
the same `task_pr_pipeline`, so they behave the same. Without `--complete` or
`--auto-fix`, the leaf ends at `review` with its pull request open, and no
owner landing follows: that runs only for a follower's handoff. Instead the
leaf submits a detached `task_pr_watch_pipeline` (step `watch`, activity
`pr_checks_watch`). It records one `landing_checks_observed` entry in the task
history.

Its JSON note holds `state` (`passed`, `failed`, `conflicting`,
`blocked` or `pending`), `pr_number` and `head_sha`. A `failed` note adds
`failed_checks`, plus a `classification` and `detail` that use the classes in
the [classification table](run-debugging-hosted-checks.md#interpret-the-classification-before-choosing-recovery).

A watch whose job listing read is rate-limited by the provider
records classification `deferred`; a non-throttle listing failure or no
deciding job after log reads records `inconclusive`. An individual non-throttle unreadable
failed-job log does not void a readable job's classification; a rate-limited failed-job
log read records `deferred` with the provider's reset evidence. The watch retries authenticated
transient CI failures using that same bounded policy, then resumes observing.
A status read lost to a transient provider outage keeps the watch polling until its
deadline; if the outage, a status read, or a CI retry/deferred-read poll wait reaches deadline
expiry while the task stays in review, the watch records a `pending` note with `hold_reason` and
no `head_sha`. A permanent read failure (credentials, permission, not found) still fails the
watch at once.

To repair a red or conflicting pull
request, use the dashboard's **Fix PR** or an `--auto-fix` drain under
[auto-fix authorization](orchestration-auto-fix.md#auto-fix-authorization). Fix PR
resumes the succeeded leaf at its completion step under the leaf's own
`completion`. A drain pass still resumes only stopped leaves.

A watch that
ended without an entry (the task left review, or the pull request merged or
closed) shows `recorded: false` and its `ended` reason in
`orbit run show <watch-run-id> --step watch --json`.

These outcomes describe existing owner landing, not new operator authority.
For completion-watcher repairs use [auto-fix authorization](orchestration-auto-fix.md#auto-fix-authorization);
its repair and single base-update limits remain separate. A **missing**
protected result uses that reference's narrowly proved HTTP-prerequisite
infrastructure rerun, not this failed-job classification.

Local required
validation failures use [red-base validation holds](../../orbit-setup/references/automation-built-in.md#built-in-re-checking-tasks-held-for-a-red-base),
not hosted landing waits. None of these diagnoses authorizes manual requeue,
a second drain, weaker checks or a merge. A stopped authorized owner landing
with typed `waiting_gate` evidence resumes automatically after a fresh
observation confirms the gate has cleared, subject to fresh candidate and
authority checks.

The owner clock sweep allows at most three retries (landing
attempts 2–4), with minimum backoffs of 60, 120 and 240 seconds; explicit
retries count toward that ceiling. Use the operator [retry landing](orchestration-retry-landing.md#retry-a-stopped-owner-landing)
procedure for other stops and after the automatic retry budget is exhausted.
