---
title: Pilot Notes — Decisions
owner: claude
created_date: 2026-11-27
updated_date: 2026-11-27
last_validated: 2026-11-28
status: Accepted
feature: pilot-notes
doc_role: decisions
type: design
summary: "Pilot Notes — Decisions: a note lives in the assessment and not in a task comment, and a note never stops the pilot"
tags: [pilot-notes]
related_features: [task-pilot]
related_artifacts: [ORB-29567]
---

# Pilot Notes — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

## A note lives in the assessment, not in a task comment

**Recorded:** 2026-11 · [ORB-29567]

### Context

A pilot note has to sit on the task where a plugin or a person can read it. A task comment
under the note's label was the obvious place: comments are already listed, and nothing new
would be needed to show one. Apply also already stores everything the pilot says about a task
as one sealed artifact, and leaves a one-line receipt as the pilot's only comment so that an
assessment does not spend the task's comment budget.

### Decision

A note is part of the assessment and is stored with it. It is read through the task's pilot
readout (`pilot_notes`), never from the comment list. The latest applied assessment's notes
are the task's notes. The same holds for anything else a pilot is later allowed to say about
a task: it joins the assessment and gets a readout, and does not become a comment.

### Consequences

- A re-pilot replaces the notes with no replace-in-place rule, and a note costs no comment
  budget however often a task is piloted.
- A note inherits the assessment's sealing, redaction and replay behaviour with no code of
  its own.
- Cost: a note is not visible in the comment list, so every surface that should show it needs
  the readout added, and a reader that only lists comments never sees it.

## A note never stops the pilot

**Recorded:** 2026-11 · [ORB-29567]

### Context

A tag instruction that cannot be read refuses its activity before the provider starts. That
is safe because it touches only tasks carrying the tag. A note instruction applies to every
task a workspace pilots, and the pilot gates admission, so the same rule would let one
missing file stop all admission in a workspace. The pilot can also return a note that breaks
the bounds.

### Decision

Nothing about a note fails or holds a pilot. An instruction file that cannot be used switches
off its own label for that run, and the gap is recorded in the run's audit trail and raised
by `doctor`. A note that breaks a bound is dropped with a recorded reason, and the rest of
the assessment is applied. A note is never repaired by a correction round.

### Consequences

- Admission does not depend on an advisory text or on the file behind it.
- Cost: a workspace can lose its notes without any task failing. Whoever relies on a note has
  to watch `doctor` or the audit event, and has to cope with a task that has none.

## Task References

- [ORB-29567] — designed pilot notes; the operator approved both decisions with it.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
