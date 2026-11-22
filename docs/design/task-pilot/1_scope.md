---
title: "Task pilot — assessment persistence"
owner: orbit
last_updated: 2026-11-22
status: Draft
feature: task-pilot
doc_role: scope
type: design
summary: "Pilot assessment artifacts, compact receipt comments, and compatibility with inline audits."
stage: shipped
next: "—"
---

# Task pilot — assessment persistence

[ORB-23677] stores the assessment as a task artifact and leaves a one-line comment. A pilot still
decides selectors, crew, complexity and admission using the same assessment schema. Apply stores
the complete existing audit object, including `assessment`, applied material, holds, recheck basis and run identity, as the task
artifact `pilot/assessment-<operation-id>.json`. The operation id makes replays
reuse one artifact without colliding with a later assessment from the same run.

The `task-pilot` comment is one line of JSON: `summary` names the selector counts,
crew, complexity change and short reason; `assessment_artifact` names the artifact;
`assessment_sha256` binds its bytes; and `operation_id` repeats the comment's
existing typed receipt. The history summary and receipt remain unchanged.
The full assessment no longer consumes the task's comment budget. To read it,
list the task's comments with `orbit.task.show` and fetch the named artifact with
`orbit.task.artifact` action `get`, passing `id` and `path`.

Apply publishes the artifact under the task write lock before committing the
receipt and task mutation. Failure can leave an unreferenced artifact, but cannot
publish a receipt before its evidence. Existing operation receipts still make
replay idempotent. Artifact bytes pass through the shared redactor before the
digest is sealed. Readers verify that digest, and shared artifact admission reserves
`pilot/assessment-` for the pilot's system writer, including claimed-worker
attachments. Ordinary attachments cannot replace sealed evidence. Pipeline
replays remain idempotent. Thus a later attachment cannot change the trusted
assessment or make its receipt unreadable. Missing or altered referenced evidence
fails resolution; it never selects an older ready verdict as a substitute. Read
projections (task list, task show and the dashboard list) degrade that one task's
readout to a `pilot_assessment_unavailable` finding and keep serving every other
task. Drain admission, re-pilot and attention still fail closed on the same
resolution. The dashboard warnings path, which had aborted the whole list on one
missing artifact, was corrected to degrade per task by [ORB-23707].

All assessment consumers resolve the reference, including attention, warning
holds and admission, preparation and drain reuse, requirement edit tracking,
legacy targetless markers, rechecks, post-deploy obligations, no-diff closure,
codebase grading, pilot scorecards and task hands. Raw task/comment projections
keep the compact message; readers that need inline audits use a resolved view.
Historical malformed inline records retain each reader's existing failure policy.

## A no-diff verdict tags the task

[ORB-26836] A `verified_no_diff` assessment that says the task expects no repository diff adds
the `no-diff-expected` tag in the same atomic write as the assessment, next to the `ui` and `os:`
routing tags. The pilot clears the task's selectors in that write, so without the tag the
`missing_context_files` readiness gap blocks a task the pilot has just assessed.

Until then the drain held every `verified_no_diff` task of a person's or an auto-task's making as
`pilot_verified_no_diff`. That hold was deliberate and stays for its original case: the disposition
also carries "the work is already on the base branch", which approving would run an implementer
over for nothing. A person decided on such a task, and only a task Orbit automation filed, once a
cited commit passed the proof, was archived. The hold could not tell that case from a no-diff
deliverable (a parity audit, a report, filed tasks), so the pilot had to be corrected by hand.

What replaces the hold for a deliverable is the typed shape of the verdict. The tag is added only
when every one of these holds:

- the disposition is `verified_no_diff` and apply keeps no selectors (an operator's creation grant
  turns the verdict into an `already_landed` hold with its selectors, so it never tags);
- the assessment carries no `already_landed`, `duplicate_of`, `release_action_required` or
  `finding_invalid` finding, and its `evidence` and rationale cite no commit;
- the task is not a review, QA or CI-sweep finding (`code-review`, `delivery-code-review`,
  `qa-full-sweep`, `qa-sweep`, `security-review` or `ci-failure-sweep`, also as `auto-task:<name>`),
  where `verified_no_diff` means the finding no longer holds;
- the task has no host-computed `comment-hygiene` batch, where an empty batch means no work;
- the task is not already tagged.

Any other `verified_no_diff` verdict keeps the `pilot_verified_no_diff` hold, its commit proof and
its archival rule unchanged. A `selectors` verdict that found no selectors is not a verdict that no
diff is expected: it adds no tag and keeps its gap. A task that already holds context files keeps
the `selectors` path and gets no tag.

A drain judges the task as the tagged task it becomes, so the pass that applies the verdict also
approves it when no other hold applies. Tagging is never removed by a later pilot. A person who
disagrees removes the tag, or tags the task `no-auto-approve`.

## Compatibility

No persisted type, task bundle layout, tool schema, receipt identity or assessment
schema changes. Artifacts use the existing task artifact manifest and blob store.
The inline audit reader remains for tasks piloted by older writers. The owner's
internal `comments` callback expands artifact-backed audits for older follower
binaries, which still expect inline JSON. New followers send the optional
`pilot_artifacts` hint for their own claimed task, keeping implementation prompt
comments compact; older owners ignore the hint and return their inline audits.
Other tasks' audits remain expanded because their artifacts are outside the
worker's claim. Public task reads also keep the compact comment. This bridge and the inline reader are
compatibility paths from 2026-11-09: retain them for at least 30 days, then apply
the release, consumer and remaining-record retirement checks in
[state compatibility](../state-compatibility/4_decisions.md#compatibility-paths-expire-on-a-30-day-release-gated-window).

## Task References

- [ORB-23677] — move pilot assessments into task artifacts and retain compact receipts.
- [ORB-26836] — a no-diff deliverable verdict tags the task `no-diff-expected`.
- [ORB-23707] — makes the dashboard pilot-warnings readout degrade per task when an artifact is missing.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
