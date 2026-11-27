---
title: Pilot Notes — Design
owner: claude
created_date: 2026-11-27
updated_date: 2026-11-27
last_validated: 2026-11-27
status: Accepted
feature: pilot-notes
doc_role: design
type: design
summary: "Approved design, not yet built: a config table that maps a note label to an instruction file, the section the pilot's prompt gains, `notes` in the assessment and what apply does with them, and where a note is read"
tags: [pilot-notes, task-pilot]
related_features: [task-pilot, plugins, workspace-agent-corpus]
related_artifacts: [ORB-29567]
---

# Pilot Notes — Design

Read the [overview](./1_overview.md) first. The operator approved this design on 2026-11-27
([ORB-29567]); nothing in it is built. It adds to the [task pilot](../task-pilot/1_scope.md),
whose assessment artifact and readout it reuses, and it borrows the file-reading rules of tag
instructions (`docs/CONFIG.md`, `[workflow.tag_instructions.<tag>]`).

## 1. Configuration

```toml
[workflow.pilot_notes.review_focus]
instruction = "docs/agents/pilot-review-focus.md"
```

- **One table per note.** The table's name is the note's label: lower-case letters, digits,
  `_` and `-`, starting with a letter, at most 32 characters.
- **`instruction` is a path relative to the repository root,** with `/` separators and no `..`
  or absolute path, as a tag instruction's is.
- **At most four notes per workspace.** A fifth table is a config error.
- **The tables layer like tag instructions.** A workspace table replaces the global one of the
  same label. `instruction = false` in a workspace switches off a note the global config
  defines.
- Unset by default: a workspace with no table gets today's pilot, byte for byte.

## 2. The pilot's prompt

When a pilot run starts for a workspace that maps notes, the host reads each instruction file
and adds one `Pilot notes` section after the activity's built-in instruction.

- **The text comes only from the committed file,** read at the source revision the pilot is
  prepared against, never from a worktree. A task's own text never supplies it.
- **The section says what a note is.** It states that the notes add to the assessment and
  cannot change any other part of it, that a note is at most 1 KiB of plain text, and that a
  label with nothing to say is left out. Then it gives each instruction under its label, in
  sorted order.
- **A file is at most 4 KiB.** Four notes therefore add at most 16 KiB to a prompt.
- **A file that cannot be used switches off its own note for that run.** Missing at that
  revision, not UTF-8, empty or over the bound: the pilot runs without that label, and the gap
  is recorded (below) and raised as a `doctor` finding naming the label, the file and the
  reason. The pilot is never refused over a note
  ([decision](./4_decisions.md#a-note-never-stops-the-pilot)).
- **What applied is audited.** Each pilot dispatch records an `activity.pilot_notes_applied`
  event naming each label, its file, the revision, the SHA-256 and the size, or the reason it
  was switched off. It does not carry the text. `orbit run show` prints it as it prints tag
  instructions.

## 3. What the pilot may call

Nothing changes here, and that is why this design has no tool mapping.

The pilot declares a deny list, not an allow list
(`crates/orbit-core/assets/activities/task_pilot.yaml`). Its callable set is every registered
tool the list does not name (`tools_allowed_by_disallow_list`). A plugin's `read_only` tool is
open to an agent caller by the plugin standard and is not on the list, so a pilot in a
workspace that enables the plugin can already call it. A note instruction may therefore tell
the pilot to call such a tool. A mutating tool stays out of reach: the pilot holds no
operator capability and the deny list names the task and workflow mutations.

An instruction should say what to do when the tool is not there, because a pilot on a host
without the plugin will not have it ([Claimed pilots](#6-claimed-pilots)).

## 4. Notes in the assessment

Each task entry of the pilot's output gains one optional field:

```json
"notes": [
  {"label": "review_focus", "text": "The retry loop in claim settlement; the lock order in apply."}
]
```

**Apply checks each note and keeps the rest of the assessment whatever it finds.**

| A note is dropped when | Recorded as |
|---|---|
| its label is not one the run was given | `unknown_label` |
| its label appears twice in the entry | `duplicate_label` |
| its text is empty, over 1 KiB, or not plain text (a control character other than newline or tab) | `empty`, `too_large`, `not_plain_text` |
| it is the fifth or later note of the entry | `too_many` |

A dropped note never fails the pilot, never holds the task and is never repaired by a
correction round. The assessment records `notes_dropped` with each label and reason.

**Where a note lives.** In the assessment artifact, with everything else the pilot said about
the task. Apply already stores the complete assessment as `pilot/assessment-<operation-id>.json`,
sealed with its digest, written under the task lock before the receipt, and passed through
the shared redactor. A note needs no storage of its own
([decision](./4_decisions.md#a-note-lives-in-the-assessment-not-in-a-task-comment)).

What follows from that:

- **The latest applied assessment is the truth.** A later pilot's notes replace the earlier
  ones as a whole. A label the later pilot left out is gone.
- **A note is not a pilot input.** Nothing in `workflow.task_pilot_freshness` looks at notes,
  and a note cannot make an assessment stale.
- **A note is not a finding.** It carries no `holds_promotion`, and admission, attention and
  the drain never read it.
- **Each stored note names its instruction.** The assessment records, per label, the file's
  path, revision and SHA-256, so a reader can tell which wording produced a note.

## 5. Reading a note

- **`orbit.task.show`** gains `pilot_notes`: for the task's current assessment, each note's
  `label` and `text`, the instruction's SHA-256 and the assessment's operation id and time.
  It is empty when the assessment has none, and absent when the task has no assessment.
- **`orbit.task.list`** returns the same field when `fields` asks for it, so a plugin that
  scans a workspace's tasks does not call show once per task.
- **`orbit task show`** prints a `Pilot notes:` block under the pilot summary, one label and
  its text per entry.
- **The dashboard** lists them under the pilot summary on a task's detail, as plain text.
  Note text is never rendered as markup.
- **An unreadable assessment** degrades as the pilot readout does today: that task shows a
  `pilot_assessment_unavailable` finding and no notes, and other tasks are unaffected.
- **Nothing injects a note.** No prompt Orbit builds contains note text. An agent that wants
  it reads the task.

A reader must treat a note as untrusted. It is a model's text about a task description that
anyone who can file a task wrote. A plugin that acts on a note checks every claim in it
against its own data first.

## 6. Claimed pilots

A leaf that holds a claim can pilot the task it claimed, and its results are applied under
that claim. For such a pilot the owner captures each note's instruction text, read from the claim's authoritative base, in the claim's receipt, as it captures tag
instructions. The claimed run applies that capture and not its replica's mapping or files. A
note the owner switched off stays off on the leaf. A claim from an older owner that captured
nothing gets no notes.

The tools are the leaf's own. A pilot on a host where the plugin is not installed or not
enabled cannot call its tool, and returns no note or a poorer one. Routing a pilot to a host
by what it has installed is out of scope.

## 7. Phasing

1. **Local notes.** The config table and its validation, the prompt section, the audit event,
   the `doctor` finding, `notes` in the output schema, the checks in apply, and the readouts in
   `orbit.task.show`, `orbit.task.list` and `orbit task show`.
2. **Claimed pilots and the dashboard.** The capture in the claim receipt, and the notes on
   the task's detail.

## 8. Acceptance

- Given a workspace with no `[workflow.pilot_notes]` table, the pilot's prompt and output
  schema are unchanged and no audit event is recorded.
- Given a mapped label and a readable file, a pilot run's prompt contains the file's text
  under that label, and the run records `activity.pilot_notes_applied` with the file's
  SHA-256 and no text.
- Given a pilot that returns a note under a mapped label, `orbit.task.show` returns it in
  `pilot_notes` with the instruction's SHA-256, and the task has no new comment.
- Given a note with an unmapped label, a note over 1 KiB and a fifth note, apply keeps the
  assessment, drops those notes and records each reason in `notes_dropped`.
- Given a second pilot that returns no note, `pilot_notes` is empty.
- Given a mapped file that is missing at the pilot's revision, the pilot runs and assesses
  the task without that label, and `orbit doctor` names the label, the file and the reason.
- Given a task edit that changes only what a note would say, no pilot is queued.
- Given an assessment with a note, admission, attention and the drain decide exactly as they
  do for the same assessment without it.
- Nothing in the change names a particular plugin, checklist or adopter.

## 9. Concerns & Honest Limitations

- **A note is a model's answer.** It can be wrong, and it can be steered by the task's own
  text. That is why Orbit neither acts on it nor feeds it to another model.
- **A changed instruction does not re-pilot.** Tasks keep the notes their last pilot wrote
  until something else pilots them again. The recorded SHA-256 lets a reader see that a note
  is from an older wording.
- **Notes cost every pilot.** Each mapped note adds its instruction to every pilot prompt in
  the workspace, and any tool it sends the pilot to is called for every task. The bounds cap
  the text; they do not cap what an instruction asks the pilot to do.
- **Re-check runs are not settled.** A re-check re-examines earlier holding findings and does
  not assess afresh. Whether it carries the earlier notes forward or returns its own is to be
  fixed in the first build task, from how the re-check assessment is stored.
- **That a plugin's read-only tool reaches the pilot is read from the code, not run.** The
  first build task proves it with a test before anything relies on it.
- **The claimed path is read from tests, not traced end to end.** Apply accepts pilot results
  under a claim; where the leaf's pilot prompt is prepared, and so where the capture has to be
  applied, is for the phase 2 task to establish.
- **Four notes and 1 KiB are guesses.** One use is known. The bounds are set to keep the
  pilot's prompt and the task readout small, and can be raised when a second use needs it.

## Task References

- [ORB-29567] — this design.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
