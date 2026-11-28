---
title: Task Pilot — Overview
owner: orbit
created_date: 2026-11-09
updated_date: 2026-11-28
status: Draft
stage: shipped
next: "—"
feature: task-pilot
doc_role: overview
type: design
summary: "Pilot assessment artifacts, compact receipt comments, no-diff verdict tagging, and compatibility with inline audits."
tags: [task-pilot]
---

# Task Pilot — Overview

The task pilot prepares proposed tasks before promotion by settling `context_files`, assessing complexity, recording holds, and deciding whether a task expects a repository diff. This overview summarizes the assessment-persistence and no-diff-verdict contracts; full mechanisms and compatibility rules live in [2_design.md](./2_design.md), and decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## 1. Motivation

### The gap

Storing complete pilot assessment audits inline in task comments consumed the task's comment budget and bloated comment projections. In addition, holding every `verified_no_diff` verdict as `pilot_verified_no_diff` could not distinguish a legitimate no-diff deliverable (such as an audit, report, or task-filing brief) from a task whose work had already landed on the base branch, forcing manual operator intervention.

### Intended outcome

Persist each complete pilot assessment as a sealed task artifact (`pilot/assessment-<operation-id>.json`) referenced by a compact one-line receipt comment ([ORB-23677]), keep admission fail-closed while degrading read projections per task when evidence is missing or unreadable ([ORB-23707], [ORB-23750], [ORB-23760]), automatically tag eligible `verified_no_diff` deliverable tasks `no-diff-expected` in the same atomic apply write ([ORB-26836]), and retain backward compatibility with historical inline audits.

## 2. Core Concepts

- **Pilot assessment.** The complete record of one task-pilot run: settled `context_files`, complexity, holds and the no-diff verdict. It is persisted as a sealed task artifact, `pilot/assessment-<operation-id>.json`.
- **Receipt comment.** The compact one-line task comment that references the assessment artifact instead of embedding the audit.
- **No-diff verdict.** The pilot's judgment on whether a task expects a repository diff. An eligible `verified_no_diff` deliverable (an audit, report or task-filing brief) is tagged `no-diff-expected` in the same atomic apply write.
- **Fail-closed admission.** Admission refuses when pilot evidence is missing or unreadable, while read projections degrade per task.
- **Historical inline audit.** An older assessment stored inline in a task comment; it stays readable.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Assessment artifacts and receipt comments | [2_design.md](./2_design.md#assessment-persistence) | [ORB-23677] |
| Degraded reads for missing or unreadable evidence | [2_design.md](./2_design.md#assessment-persistence) | [ORB-23707], [ORB-23750], [ORB-23760] |
| A no-diff verdict tags the task | [2_design.md](./2_design.md#a-no-diff-verdict-tags-the-task) | [ORB-26836] |
| Compatibility with inline audits | [2_design.md](./2_design.md#compatibility) | [ORB-23677] |
| Standing rules | [4_decisions.md](./4_decisions.md) | — |

## Task References

- [ORB-23677] — move pilot assessments into task artifacts and retain compact receipts.
- [ORB-26836] — a no-diff deliverable verdict tags the task `no-diff-expected`.
- [ORB-23707] — makes the dashboard pilot-warnings readout degrade per task when an artifact is missing.
- [ORB-23750] — degrades task show when pilot evidence is unreadable or invalid.
- [ORB-23760] — degrades task show for dangling or corrupt pilot blobs.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
