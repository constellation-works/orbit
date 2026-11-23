# Resume a preserved task candidate

## Contents

- [Re-running a task with a preserved candidate](#re-running-a-task-with-a-preserved-candidate)

## Re-running a task with a preserved candidate

When a `task_pr_pipeline` run fails after implementation, its failure handoff
keeps the candidate on the task's `orbit/<task>-…` branch (usually behind a
`[BLOCKED]` PR) and records it on the run. The same handoff also preserves a
candidate when the implementation step itself fails. When the task is requeued,
the next `task_pr_pipeline` or `task_local_pipeline` run resumes that candidate
instead of discarding it.

Its `resume_candidate` step squash-applies the
candidate onto the new base as uncommitted changes, then:

| Outcome | When | Implementation step |
|---|---|---|
| `resumed_carried` | A completed owner-local candidate failed at `commit` or later and applies cleanly. No required validation runs at resume time. | Skipped. The candidate goes straight to commit, base synchronization where applicable, final validation, review and delivery. |
| `resumed_held` | The last run was held on named external evidence, that evidence arrived (`review_evidence_received` is the task's latest decision), and the held commit applies cleanly. This includes a claimed leaf's hold that the owner's own run resumes from the published held candidate. | Skipped, and validation does not run at resume time. The candidate goes to commit, validation and the fresh review that finds the evidence. When the hold named only evidence a host-evidence rule owed, the held review settles without a reviewer. |
| `resumed_repaired` | The failed step is the implementation or any step before `commit`, or the candidate conflicts with the new base, or the before-PR review refused it, or the owner's evidence run reported CodeQL findings on the held candidate (`review_evidence_findings` is the task's latest decision). | Starts from the applied candidate. An unfinished implementation carries trigger `implementation` and the failed step; otherwise the conflict paths or the review findings. Reported CodeQL findings carry trigger `review`, with each result's rule, location and data flow from `review-evidence-findings.json` as the output. |
| `fresh` | No candidate was preserved, an operator discarded it, the task's description or acceptance criteria changed since that run, the run is a bundle, or the commit is unreachable. | Implements from scratch. The reason and its `reason_code` are recorded. |

Context selectors are preparation hints, not what the task means: a task
pilot or operator editing them keeps the candidate, and the resumed review
reads the current selectors.

A bound source-repair admission is different: tags, plan, selectors and other
task-digest inputs are frozen. Only the exact system-recorded host selector
widening and source-preserving system crew redraw exceptions count. Stop and
review changed metadata; when scope materially changes, obtain a supported
fresh operator decision before proceeding. Generic candidate resume does not
reauthorize a bound source repair.

A candidate held for a red base resumes like any candidate from `commit` or
later. That includes one the before-PR review held because its only failed
check also fails on the pinned base. It is `resumed_carried` once the hold
lifts. The final validation gate judges the committed, synchronized candidate
and invokes existing recovery for a candidate failure; infrastructure refusals
and baseline-red holds keep their distinct handling. A fresh review follows.

A claimed leaf's resume always runs the implementer.

A resumed held candidate on the hold's own base has the held tree, so the
evidence matches it directly. On a moved base the review gate counts the
evidence only while the candidate's whole patch over its base is unchanged
(`git patch-id --stable`); the review manifest and certificate then record
`evidence_carried` with the two trees and the patch id.

A conflict resolution
or any other change re-requests the evidence, and the admission output's
`evidence_carry` names the typed reason (`patch_changed` or
`source_unavailable`). The same rule applies when completion rebases a
reviewed head for re-review.

Whenever a candidate was found, the outcome, the source run, branch and SHA
are written to the task's history as a `candidate_resume` event and returned
in the step's output. Resume runs no required validation commands. The
pipeline's `validate` step runs them on the committed, synchronized head that
is delivered, including a `resumed_carried` candidate. No old candidate evidence
waives this gate. An unfinished implementation goes to the implementer first.

To throw a candidate away, run
`orbit task update <task-id> --discard-candidate --status backlog`. This
records a `candidate_discarded` history event, and the next run implements
fresh. It is refused while the task is `in-progress`.

Scope limits:

- Generic resume of a claimed leaf is refused. A new claim can carry the
  committed candidate the owner preserved from a failed attempt. The leaf
  fetches its durable ref from `origin` when needed, applies it onto the new
  base, and always runs the implementer with `continuation`, `review` or
  `conflict` repair context. An unpublished candidate that could not be carried
  to a durable ref resumes only on its original host.

  - A repair claim after a
    stopped owner landing instead supplies `landing` or `conflict` context.
    The handoff needs this attempt's implementation output; validation judges
    the resulting candidate. See [distributed-drain-setup.md](../../orbit/references/setup/distributed-drain-setup.md).
- A claim blocked after its push and `pr_open` (a before-landing review
  failure, say) keeps its candidate with that pull request when it is moved
  back to `backlog`. The next claim continues the same branch and reuses
  the pull request (`reused_branch` in `resume_candidate`'s output).

  - When it
    cannot (`branch_reuse_refused`), it publishes on its own branch and its
    `pr_open` closes the earlier pull request with a comment naming the new
    one; `superseded_pull_request.decision` `close_failed` leaves the earlier
    pull request for the operator to close.
- A claim whose before-landing review held for named evidence releases with
  its open pull request (`held_pull_request`). The owner keeps it with the
  held candidate; the run that resumes that candidate after the evidence
  arrives publishes on its own branch, and its `pr_open` closes the earlier
  pull request the same way.
- A claimed leaf cancelled or interrupted while its reviewer is working
  (a forced drain cancel included) keeps its candidate and the reviewer's
  uncommitted repair. The host that stopped it commits that repair as a
  marked partial-repair commit on the candidate head, kept at
  `refs/orbit/candidates/<task>/<run>-review`, and the release names that
  ref. The owner adds the attempt's report findings.

  - The next claim applies the repair with the candidate and runs the
    implementer with trigger `review` and `repair.interrupted_review`: the
    findings, plus whether the repair applied. It continues an earlier pull request as
    above. An owner's own run stopped mid-review keeps the same commit and
    ref, with a task comment naming them. Its next run does not apply it
    automatically; see [recovery-salvage-candidate.md](recovery-salvage-candidate.md#salvage-an-incomplete-candidate).
- A failure after the PR opened (completion, CI on the published PR) leaves
  the task in `review` with its PR, not a preserved candidate.
