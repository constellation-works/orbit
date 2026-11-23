# Distributed drain: recovery

Contents:

- [Recovery](#recovery)
- [Leftover epic and review state](#leftover-epic-and-review-state)

## Recovery

Generic resume of a claimed leaf is refused:

```bash
orbit job resume <run-id>
```

Inspect the claim, task, locks, and the run on the execution machine named by
`job_run_machine`. Reconcile an uncertain merge intent before any reassignment — a
revocation or recovery is refused with `uncertain_merge_intent` until it is.

Handoff approval is an owner-operator action on the owner's dashboard
(`handoff.approve`). It has no CLI verb and no registered tool. It records one
candidate-scoped authorization and a landing-start request for the exact
candidate and base the operator was shown, and it never merges. A stale page is
refused with `stale_claim`. Do not substitute a status write on the handed-off
review, `--complete` on a follower, or `orbit job resume`: a plain status write
is refused while the claim is active.

Revocation and recovery are supported operator commands with matching operator
tools. Both require trusted operator authority, are unavailable to managed runs,
take a required reason of at most 2000 bytes, and replay when the same `--request`
key is resubmitted with the same input. The `--request` key is 1 to 128 bytes of
ASCII letters, digits, `-` or `_` after trimming; the owner refuses any other key
before it records anything:

```bash
orbit task revoke-handoff <handoff-id> --candidate <commit> --base <commit> --reason <reason> --request <key>
orbit task recover-claim <claim-id> --phase <phase> --status <status> --reason <reason> --request <key>
```

- `revoke-handoff` (`orbit.task.recover` action `revoke_handoff`) cancels the pending landing
  request for the accepted candidate and base you name. The task stays in
  `review`.
- `recover-claim` (`orbit.task.recover` action `recover_claim`) revokes the live claim,
  invalidates pending landing authority and releases its reservation in one
  transaction, and moves the task to the status you choose: `blocked` to
  diagnose, `backlog` to retry, or `review`, `done`, `archived`, `rejected` or
  `someday`.
  - The target must be a status the task lifecycle allows from the
    task's current one (`in-progress` for a running claim, `review` for a
    handed-off one, `blocked` for a failed one), and recovery never leaves the
    task `in-progress`.
  - `done` follows only `review` and keeps the ordinary
    completion gates (an execution summary or successful run, any owed
    post-deploy measurement, applied mutation proposals). A refused target names
    the rule. Pass the claim phase you observed; a claim that moved on is
    refused.

Both refuse with `stale_claim` when the owner moved on, and a replica refuses
them with `replica_checkout`. A returning worker after revocation receives
`stale_claim`. Managed agents cannot call either command or tool. Do not invent
a workaround, do not ship the same task again while the old claim is unfenced,
and do not treat `--complete` on a follower as landing. Followers never merge.

A recovered follower pull request that merged at a head other than its
handed-off candidate completes only after an operator reconciles that merged
head on the owner. Run `orbit task reconcile-review inspect <task-id>`, then
`orbit task reconcile-review submit <task-id> --request <key>`, and follow it
with `orbit task reconcile-review status <task-id>`.

The submitted run (`task_review_reconciliation_pipeline`)
validates and reviews exactly that head under the contract `submit` froze: the
accepted handoff's captured commands (or, when that acceptance explicitly
required none, the owner's configured commands, labelled as such) and the
review crew. Later configuration edits apply only to a new request key.
`status` names the next step,
including `accept-baseline` for a failure the base already had. Agents
cannot submit or dispose a reconciliation.

`accept-baseline` reruns the same required check at the named landed
remediation commit in a detached checkout and records its output. It refuses
the disposition unless that check passes; merged-head validation stays
recorded as incomplete. The remediation must contain the commit the provider
reports the pull request landed as (merge, squash or rebased commit), never
just the head, which a squash landing does not keep.

A fix that landed before
the merge is refused before anything runs: land the fix on top of the landed
commit and name that commit. If the provider's answer changed, or the record
predates binding the landed commit, submit a new request key.

## Leftover epic and review state

There is no epic execution path. Gather evidence with ordinary commands:

```bash
orbit run history -j epic_pipeline --limit 50
orbit task list --tag epic
orbit task show <task-id> --fields status,context_files,job_run_id,job_run_machine
orbit task locks list
orbit doctor
```

Do not treat the workspace as migrated while an old epic/child run, reservation,
or uncertain landing is live, even if the root is already `review`. Inherited-only
epic roots (no own context, descendants that have some) need operator-supplied
own context; nothing inherits the old union.

The owner's clock can dispatch final recovery for an eligible blocked task;
inspect its decision, `blocked` status and `job_run_machine` before a manual
re-backlog. See [automation-built-in.md](../../../orbit-setup/references/automation-built-in.md#built-in-final-recovery-of-blocked-tasks).
