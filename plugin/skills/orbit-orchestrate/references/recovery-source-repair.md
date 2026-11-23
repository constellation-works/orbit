# Repair a stopped owner publication

## Contents

- [Explicit source repair of stopped owner publications](#explicit-source-repair-of-stopped-owner-publications)

## Explicit source repair of stopped owner publications

An operator on the owning host can inspect a published candidate without
resuming its failed attempt:

For version coordination across owner and replica hosts, see [Upgrading a
workspace together](../../orbit-setup/references/multi-host.md#upgrading-a-workspace-together).

```bash
orbit tool run orbit.run --input '{"action":"resume","id":"<source-run>","source_repair":{"action":"inspect","task_id":"<task-id>"}}'
```

Supported order is: make sure a valid execution plan is present when the
task's status requires one, freshly inspect the exact immutable source
binding/policy, then submit the unchanged binding with the required acceptance
evidence and a fresh request key.

Submit the returned `binding` unchanged with `action: "submit"`, the same
`task_id`, a new bounded `request_key`, and a `diagnosis` naming the observed
failure and remaining source work. A comment on the task, with or without that
call, does not start or change a repair. Submit refuses a missing or placeholder
plan before creating a reservation when the task's status requires a plan
before transitioning to `in-progress`. `backlog` and `in-progress` are exempt;
their managed run authors the plan at start time.

When the only correction is
an empty or placeholder plan on an existing reservation with no admitted run,
use `action: "reconcile"` with the original key, its inspected
`reservation_digest`, a fresh binding and otherwise unchanged
evidence/policy. This is an explicit source decision. It creates a new
attempt with current required commands, configured review,
`completion: "review"` and `auto_fix: false`.

It grants no completion, merge
or auto-fix authority, extends no historical deadline, and inherits none from
the source run. Keep the identical key, binding, diagnosis, `unmet_acceptance`
and `policy_digest` when a submission reply is lost: the durable
reservation/link returns the same successor, even after it stops. Each newly
published head needs another explicit inspection and fresh key; automatic
fixes and explicit generations share the PR repair budget.

Generic resume
still cannot replay an admitted source-repair attempt.

A successful owner publication whose published candidate leaves acceptance
unmet takes the same explicit route, not ordinary continuation. Select the
original publishing `Success` run explicitly and inspect it: inspection
returns the binding plus `review`, the current `policy_digest`, reservations
and `reservation_digests`.

Submit the exact binding with the diagnosis,
explicit `unmet_acceptance` evidence identifying the unmet criteria on this
published candidate, the inspected current `policy_digest`, and a fresh key.
Green requeue remains the route for carrying an unchanged candidate forward
without authorizing implementation; successful-publication rework authorizes
review-only source work on the same PR.

In addition to cancelled publications and failed publications with an applied
requeue, inspection supports these conclusively stopped failed owner shapes
after final recovery escalated:

- **The pipeline published its PR and the failure hook kept it.** Select the
  original publishing run explicitly, including when newer attempts failed at
  candidate adoption without producing source. Its kept-publication receipt
  must bind the unchanged task specification, and its original successful host
  publication chain, repository, branch, PR head and remote lease must agree.
  Inspection returns `binding.superseded` for every stopped completion
  retry and empty successor.

  - Completion retries must preserve the original
    checkpoint prefix, deadlines and recovery evidence without mutation or
    repair provenance. Submit the inspected current `policy_digest` and explicit
    `unmet_acceptance` evidence with the exact binding, key and diagnosis. This
    admits source repair even with red hosted checks and expired completion;
    it does not extend historical deadlines or inherit merge/auto-fix authority.

  - Current policy and complete stopped lineage are checked before source
    effects. Dirty worktrees, active or ambiguous lineage and changed heads or
    specifications refuse. Green requeue remains the route for carrying the
    unchanged candidate without authorizing implementation.
- **Final recovery resumed the publishing run in place and it failed again.**
  Select that same run. Its checkpoints must hold the host's applied `resume`
  receipt for this task and run, with no host repair commit, beside the
  kept-publication receipt above, so final recovery cannot run again. The
  binding's head is the PR's current head, reached only through the
  `pr_auto_fix_pushed` ledger entries that run recorded from its original push.

  - Submit with the same `unmet_acceptance` and `policy_digest`. The attempt is
    review-only, counts against the PR's repair budget, renews no deadline and
    leaves the receipt, deadlines and auto-fix ledger untouched.
- **An admitted source repair pushed before `pr_open` failed.** Its original
  system reservation and unique run link, successful branch adoption, host
  commit, prepare, synchronization, review/publication chain and push must
  agree. The original admission supplies the existing PR identity when no
  successful `pr_open` checkpoint exists. The old source evidence remains
  immutable.

  - Host-recorded selector additions from that attempt are accounted
    for when comparing its original task scope; other meaning/scope changes
    refuse admission.
- **The terminal failure hook published a repair.** The durable
  `pr_failure_handoff` checkpoint must name the exact task, run, failed step,
  setup owner/base, preservation commit, push and PR. Its commit must have an
  authenticated host candidate or immutable setup base as its preservation
  parent. The handoff's recorded task specification must still match.

  - That
    persisted specification covers description and acceptance criteria; it does
    not certify historical selector hints or review approval. Inspection binds
    the full current meaning/scope for submission and source application. The
    retained handoff commit is selected even when it advances past the failed
    validation candidate. If its worktree and local branch are gone, existing
    exact commit objects and the unchanged remote branch lease permit
    reconstruction.

  A local branch present at another head still refuses.

None of these routes treats publication as validation. The new worktree integrates
the exact retained candidate onto the current remote base, preserves its PR
and branch under the exact lease, and requires fresh complete host gates and
normal configured review. A handoff repair was not certified by the old
candidate's checks. Failure receipts and historical checkpoints are retained;
there is no requeue/history rewrite or claimed held-evidence amendment here.

Both inspection and submission freshly observe the origin repository,
remote branch and provider PR number, URL, head, branch and base branch. The
PR must remain open and unmerged with automatic merge explicitly disabled;
a draft remains a draft. Missing, contradictory or changed evidence refuses
before reservation. A live/foreign/claimed source or a newer attempt that
may have written a candidate cannot be bypassed by selecting an older head.

Newer owner attempts between the source and the task's current binding are
bound together in `binding.superseded`, and a change to any of them voids
the binding. Each must be conclusively stopped and either cancelled, or failed
at or before candidate adoption with a settled (escalated or requeued) final
recovery, no failure handoff other than one that published nothing
(`no_candidate`), and a clean retained worktree.

A failed attempt that reached
implementation or left work behind is a resume candidate, not a successor to
set aside
([stuck job runs](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/stuck-job-runs.md#a-cancelled-published-source-repair)).
Diagnose the named missing receipt or identity mismatch, then inspect again;
do not invent a checkpoint, copy another run's admission, or weaken gates.
