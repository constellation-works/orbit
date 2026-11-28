---
title: Pilot Notes — Decisions
owner: claude
created_date: 2026-11-27
updated_date: 2026-11-28
last_validated: 2026-11-28
status: Accepted
feature: pilot-notes
doc_role: decisions
type: design
summary: "Pilot Notes — Decisions: a note lives in the assessment and not in a task comment, a note never stops the pilot, and task-pilot stays on the owner so claimed tasks get no note capture"
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

## Task-pilot stays on the owner, so claimed tasks get no note capture

**Recorded:** 2026-11-28 · [ORB-29590]

### Context

The approved design (§6) gave a claimed task's pilot a note capture, so the leaf would apply
the owner's instruction text. Tracing the claimed path at base `1d5d716ea` found that a claimed
agent can submit `orbit run task-pilot` from its checkout. The submitted run's worker must
register in the protected authority store, and the leaf's sandbox denies that write
(`crates/orbit-core/src/adapter/engine_host/actions/sandbox/resolve.rs`, `append_terminal_denies`).
So no leaf prepares a prompt. Prepare's explicit branch reads no execution claim, while its
automatic branch does (`crates/orbit-core/src/application/task/pilot/prepare/run.rs`). The apply
fence covers only in-progress and review tasks
(`crates/orbit-core/src/application/task/pilot/persist/reports.rs`, `superseded_with_current`).

### Decision

Task-pilot runs on the owner. The owner prepares, runs and applies every pilot and its notes,
from its own config and committed files. The claim receipt carries no note capture, and no leaf
applies one. The alternative was an authorized claimed-worker pilot that captures the owner's
instruction text; the operator chose the owner-only path on 2026-11-28.

### Consequences

- No receipt field and no leaf-side apply path. The owner's mapping reaches every pilot prompt,
  and a replica's mapping never does.
- Cost: this rests on the sandbox denial. A leaf launch whose nested worker could register would
  prepare the pilot from its own config and files, and no note capture would cover it. Revisit
  this decision then. A claimed task's notes are also the owner's, written against the owner's
  checkout.

## Task References

- [ORB-29567] — designed pilot notes; the operator approved both decisions with it.
- [ORB-29590] — documents where task-pilot runs; claimed tasks take the owner's notes.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
