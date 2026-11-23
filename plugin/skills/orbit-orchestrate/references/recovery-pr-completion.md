# Recover completion when a PR exists

## A PR exists but completion failed

Inspect the failed step and GitHub state independently. A `complete_pr` error
can be a repository merge-method or auto-merge setting mismatch even when
there are no rebase conflicts. A failure-handoff PR is preserved work, not
proof that it is safe to merge or that the task is done.

For a failed hosted check, follow
[hosted-check diagnosis](run-debugging-hosted-checks.md#hosted-check-failures-and-inherited-red-bases)
before treating the candidate as damaged or the run as stuck. Owner landing
classifies the failure and may wait for inherited base recovery without an
agent repair. Inspect its recorded wait, deadline and stop evidence before
choosing a recovery path; a red check alone grants no retry or merge authority.

Check the candidate head/base, mergeability, checks, and repository settings.
Use an allowed merge method through the supported recovery path. Change a
repository setting only when the user's authorization covers that change;
opening a PR does not grant that permission. Verify actual merge and reconcile
task state with the evidence. Do not rerun implementation solely because the
completion step failed.
