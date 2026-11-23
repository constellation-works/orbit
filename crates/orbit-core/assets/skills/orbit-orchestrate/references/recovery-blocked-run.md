# Diagnose a blocked run before retrying

## Diagnose failed runs before retrying

```bash
orbit task show <task-id>
orbit run show <run-id> --json
orbit run logs <run-id> --step <step-id> --json
```

A failed run can leave its task `blocked` with the failure attached. The owner's
clock can dispatch final recovery for an eligible block; inspect its decision
and current task state first. See [automation.md](../../orbit-setup/references/automation.md#built-in-final-recovery-of-blocked-tasks).
If it remains blocked, read the evidence yourself, then
make the transition deliberately — return the task to `backlog` only once you
know why it failed and that a rerun can succeed.

A sandbox denial or provider
failure is not inherently transient; repeated identical failures need a repair
or configuration correction before another attempt.

Every return from `blocked` to `backlog` is authenticated the same way.

`orbit task requeue`, a plain `orbit task update --status backlog`, and the
same move through MCP, HTTP or the dashboard all apply the same publication
pre-flight in the shared core operation before writing: a task whose
specification changed after its run published a pull request, or with a pull
request or preserved candidate no handoff receipt binds, is refused with the
same discard/explicit-source-repair guidance and stays `blocked` with
nothing written.

An authenticated failure-handoff publication (or no
publication on record) returns to `backlog` — with the requeue's
preservation recorded when there is one to resume — and an update with
`--discard-candidate` still discards the candidate and closes its pull
requests so the next run implements fresh.

When the finished candidate has a failure-handoff pull request, the operator can
choose **Approve and land** on its block decision, or use
`orbit decision answer <request-id> land`. It merges the green handoff and completes
the task without another run. Agents, drains and automatic corpus rules cannot
choose this answer. A required pre-landing review must have settled for the exact
candidate before the answer is offered.

At answer time a missing receipt, live run,
changed specification, closed or draft PR, moved head, failed or pending required
check, wrong target or conflicting merge refuses it with the request still open
and nothing changed. A matching `completion.hold` rule is recorded and the human
approval satisfies it. Requeue keeps its normal meaning.

If the operator requeues
first and then merges that same handoff PR on the forge, external-merge settlement
also completes the preserved backlog task; it needs no forced status transition.

When the latest review left a finding open and the reviewed candidate can still
return to the backlog, the block decision also offers an answer that accepts
those findings and resumes the same candidate. It does not change the task
description or acceptance criteria. The successor delivery validates the
candidate and follows its selected review policy; a fresh independent review
runs only when that policy calls for one. The normal landing gates still apply.

An option whose rewrite would change the description or acceptance criteria
says that the reviewed candidate is discarded, and choosing it discards the
candidate even when required checks have not passed. A run that has not
stopped is refused until it stops; that refusal is not the rewrite. On the
dashboard the refusal stays on the decision card across a detail refresh
until it is dismissed or it expires.

When a run stops on a rebase conflict, final recovery can repair the conflict
files and resume. The next conflict recovery adopts those repairs and continues
the stopped rebase, so don't abort it or stage files by hand. Adoption still
refuses leftover conflict markers, whitespace errors, an untouched conflict, and
a moved HEAD or index.

A live process is not stopped merely because a tool observation timed out.
Re-poll the same run and inspect current process liveness. Conversely, a stale
lock file alone does not prove a worker is alive. Follow
[run-debugging-triage.md](run-debugging-triage.md) before process-level
intervention, and never weaken protected-path policies to make a retry pass.
