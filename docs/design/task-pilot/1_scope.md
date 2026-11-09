---
title: "Task pilot — assessment persistence"
owner: orbit
last_updated: 2026-11-09
status: Draft
feature: task-pilot
doc_role: scope
type: design
summary: "Pilot assessment artifacts, compact receipt comments, and compatibility with inline audits."
stage: shipped
next: "—"
---

# Task pilot — assessment persistence

A pilot still decides selectors, crew, complexity and admission using the same
assessment schema. Apply stores the complete existing audit object, including
`assessment`, applied material, holds, recheck basis and run identity, as the task
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
fails resolution; it never selects an older ready verdict as a substitute.

All assessment consumers resolve the reference, including attention, warning
holds and admission, preparation and drain reuse, requirement edit tracking,
legacy targetless markers, rechecks, post-deploy obligations, no-diff closure,
codebase grading, pilot scorecards and task hands. Raw task/comment projections
keep the compact message; readers that need inline audits use a resolved view.
Historical malformed inline records retain each reader's existing failure policy.

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

- ORB-23677 — move pilot assessments into task artifacts and retain compact receipts.
