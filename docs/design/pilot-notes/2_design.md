---
title: Pilot Notes — Design
owner: claude
created_date: 2026-11-27
updated_date: 2026-11-28
last_validated: 2026-11-28
status: Accepted
feature: pilot-notes
doc_role: design
type: design
summary: "Approved design in build: a config table that maps a note label to an instruction file, the section the pilot's prompt gains, `notes` in the assessment and what apply does with them, and where a note is read"
tags: [pilot-notes, task-pilot]
related_features: [task-pilot, plugins, workspace-agent-corpus]
related_artifacts: [ORB-29567]
---

# Pilot Notes — Design

Read the [overview](./1_overview.md) first. The operator approved this design on 2026-11-27
([ORB-29567]). [ORB-29586] adds the config table and prompt section; [ORB-29588] adds the notes in the assessment and their readouts. The design extends
the [task pilot](../task-pilot/1_overview.md),
whose assessment artifact and readout it reuses, and it borrows the file-reading rules of tag
instructions ([`workflow.tag_instructions.<tag>`](../../config/workflow-tags.md#workflowtag_instructionstag--extra-instructions-for-tagged-tasks)).

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
- Unset by default: a workspace with no table gets no `Pilot notes` section and no audit event.
  The activity's output schema has `notes` for every workspace, and a pilot given no note
  returns none ([§8](#8-acceptance)).

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

An instruction should say what to do when the tool is not there, because a pilot on an owner
without the plugin will not have it. The owner runs every pilot ([Claimed pilots](#6-claimed-pilots)).

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
| its label appears twice in the entry; the first stands and each repeat is dropped | `duplicate_label` |
| its text is empty, over 1 KiB, or not plain text (a control character other than newline or tab) | `empty`, `too_large`, `not_plain_text` |
| it is the fifth or later note of the entry, checked first by position, so a fifth note is `too_many` whatever its label | `too_many` |

"Given" means the instructions the run's prompt carried. Apply resolves them as dispatch does,
from the config and the prepared source revision, and does not trust any instruction the pilot
repeats. Both `notes` and `notes_dropped` are host-owned: a pilot-supplied `notes_dropped` never
survives, an entry that is not an object counts as a note with no label, and a `notes` value
that is not an array is ignored.

A dropped note never fails the pilot, never holds the task and is never repaired by a
correction round. The assessment records `notes_dropped` with each label and reason.

**Where a note lives.** In the assessment artifact, with everything else the pilot said about
the task. Apply already stores the complete assessment as `pilot/assessment-<operation-id>.json`,
sealed with its digest, written under the task lock before the receipt, and passed through
the shared redactor. A note needs no storage of its own
([decision](./4_decisions.md#a-note-lives-in-the-assessment-not-in-a-task-comment)).

What follows from that:

- **The latest applied assessment is the truth.** A later pilot's notes replace the earlier
  ones as a whole. A label the later pilot left out is gone. A re-check is no exception
  ([Re-check runs](#re-check-runs)).
- **A note is not a pilot input.** Nothing in `workflow.task_pilot_freshness` looks at notes,
  and a note cannot make an assessment stale.
- **A note is not a finding.** It carries no `holds_promotion`, and admission, attention and
  the drain never read it.
- **Each stored note names its instruction.** The assessment records, per label, the file's
  path, revision and SHA-256, so a reader can tell which wording produced a note.

## 5. Reading a note

- **`orbit.task.show`** gains `pilot_notes`: for the task's current assessment, one entry per
  note with `label`, `text`, `instruction_sha256`, and the assessment's `operation_id` and `at`.
  It is `[]` when the assessment has none, and absent when the task has no assessment (a
  `fields` projection answers `null`).
- **`orbit.task.list`** returns the same field when `fields` asks for it, so a plugin that
  scans a workspace's tasks does not call show once per task.
- **`orbit task show`** prints a `Pilot notes:` block after the comments, where the pilot summary
  is, one label and its text per entry, without control characters.
- **The dashboard** lists them under the pilot summary on a task's detail, as plain text.
  Note text is never rendered as markup.
- **An unreadable assessment** degrades as the pilot readout does today: that task shows a
  `pilot_assessment_unavailable` finding and no notes, and other tasks are unaffected.
- **Nothing injects a note.** No prompt Orbit builds contains note text. An agent that wants
  it reads the task.

A reader must treat a note as untrusted. It is a model's text about a task description that
anyone who can file a task wrote. A plugin that acts on a note checks every claim in it
against its own data first.

### Re-check runs

A re-check is the pilot's targeted review of a held assessment. Apply stores its result as a
whole assessment of its own, and the host-owned `HOST_FIELDS` of the re-check merge name the
fields never carried over from the earlier one. `notes` and `notes_dropped` are among them.
A re-check is therefore handed no earlier note (the re-check input's `prior_assessment`
omits both, so no earlier note text reaches a pilot prompt) and returns its own notes under the
same prompt section and the same checks as a full pilot. A label it leaves out is gone, as for
any later pilot. This keeps one rule, that the latest applied assessment is the truth, at the
price that a re-check that says nothing leaves a held task with no notes.

## 6. Claimed pilots

**Task-pilot runs on the owner.** A claimed task's pilot, prompt and notes come from the owner.
No leaf captures note instructions, and the claim receipt carries none. The operator chose this
on 2026-11-28 ([decision](./4_decisions.md#task-pilot-stays-on-the-owner-so-claimed-tasks-get-no-note-capture)).
The trace below is from code at base `1d5d716ea`. It was not run on a live host.

**Who starts a pilot.** Owner-side paths submit `task_pilot_pipeline`:

- The member scheduler submits the owner's member attempt, with `state_automation` and the
  attempt's `source_revision` (`crates/orbit-core/src/application/automation/members/member_host/host.rs:174-190`).
- The owner drain (`crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml:196`) and the
  CI-failure sweep (`crates/orbit-core/assets/jobs/ci_failure_sweep_pipeline.yaml:157`) submit
  the same job.
- An operator submits explicitly with `orbit run task-pilot`
  (`crates/orbit-cli/src/command/run/task_pilot.rs:37-48`), from the dashboard
  (`crates/orbit-core/src/app/run/dashboard.rs:269`), or from a task write
  (`crates/orbit-core/src/app/task/write.rs:335-342`). The dashboard and task-write paths call
  `submit_explicit_task_pilot`, which refuses a replica checkout
  (`crates/orbit-core/src/application/job/pilot_submit.rs:227-240`). The CLI path does not (§9).
- A replica schedules only its own worktree and store GC (`crates/orbit-core/src/app/routine/loader.rs:36-39`).

**Where the owner prepares the prompt.**

1. `prepare` (`crates/orbit-core/assets/jobs/task_pilot_pipeline.yaml:168-169`) selects tasks.
   The explicit branch (`crates/orbit-core/src/application/task/pilot/prepare/run.rs:150-236`)
   records a non-assessable status as `status_not_eligible` (`:188-196`) and reads no execution
   claim. The automatic branch reads the live-claim set (`:256-257`) and uses it in the
   stale-repilot check (`:288-293`).
2. `pilots` runs `task_pilot` (`task_pilot_pipeline.yaml:193-214`). `prepare_provider_envelope`
   builds its prompt (`crates/orbit-engine/src/activity_job/cli_runner/orchestrator/dispatch/provider/preparation.rs:35`)
   and appends the `Pilot notes` section (`preparation.rs:164-177`).
3. That section comes from `OrbitRuntime::pilot_notes_for_activity`
   (`crates/orbit-core/src/application/tag_instructions.rs:70-85`). It reads the owner's
   `[workflow.pilot_notes]` table and each committed file at the pilot input's
   `source_revision`. A leaf's config and files never reach it.
4. `apply` (`task_pilot_pipeline.yaml:220-221`) writes the assessment (§4).

**A claimed worker's explicit submission.** A claimed leaf's agent is not denied the `orbit`
binary. Its tool deny list names Orbit tools such as `orbit.pipeline.invoke`, but not the
`orbit run` command, and its program deny list names no `orbit` binary
(`crates/orbit-core/assets/activities/agent_implement.yaml:690-712`). A worker-bound CLI call
stays local (`crates/orbit-cli/src/command/host_route.rs:116-126`). The submission then runs
as follows:

1. `PilotRequest::submit` refuses any explicit task that is not `proposed` or `backlog`
   (`crates/orbit-core/src/app/run/job.rs:61-89`). `dispatch_pilot` then submits the run
   (`job.rs:300-320`).
2. Submission persists the run and calls `spawn_pipeline_worker` in the same process
   (`crates/orbit-core/src/application/job/pipeline/admission.rs:430-435`).
3. `spawn_process` registers the child with the authority store
   (`crates/orbit-core/src/application/job/pipeline/worker/supervisor.rs:315-317`).
   `RuntimeKernel::register_worker_process` binds the child's PID to the nested process's worker
   invocation (`crates/orbit-runtime/src/run/worker_coordination.rs:72-78`). The nested `orbit`
   is a worker because its binding resolves through a live ancestor
   (`crates/orbit-runtime/src/stored/recovery_authority/worker.rs:86-93`).
4. That bind writes `state/recovery-authority/authority.db`
   (`crates/orbit-runtime/src/stored/recovery_authority/root.rs:11-16`). The directory is created
   with mode 0700 (`root.rs:21-24`). Both OS profile builders call `append_terminal_denies`
   (`crates/orbit-core/src/adapter/engine_host/actions/sandbox/resolve.rs:168`, `:233`), which adds
   a `modify` deny on that root after the convenience grants (`resolve.rs:262-268`, `:705-719`;
   `crates/orbit-runtime/src/stored/recovery_authority/root.rs:185-195`). The store is protected
   by location, not by a secret (`crates/orbit-runtime/src/stored/recovery_authority/mod.rs:15-17`).
5. The refused registration returns an error. `spawn_process` kills and reaps the child
   (`supervisor.rs:315-316`, `:924-930`). The run is finalized as a startup failure, and the
   submit call returns that error (`admission.rs:442-458`). No `prepare` step runs in the leaf.

A landing-review diagnosis recorded for run `jrun-20261128-0907-c7` reached the same block: the
sandbox blocks registration of the nested worker. That is run evidence, not a test.

**The live-claim fence on pilot writes.** Apply checks each task under its lock before it writes
(`crates/orbit-core/src/application/task/pilot/persist/apply_task.rs:203-208`; also
`apply/task.rs:88` and `apply/unresolved.rs:77`). `superseded_with_current` settles the write as
`superseded`, reason `execution_claim`, when the task is `in-progress` or `review` and a
footprint-protecting execution claim holds it (`crates/orbit-core/src/application/task/pilot/persist/reports.rs:95-105`).
The clause needs a live claim. The test at `crates/orbit-core/tests/runtime/distributed_drain/pilot.rs:135-161`
(registered at `:682`) asserts that outcome and that the pipeline still succeeds.

**CLI submission, execution and application are separate steps.**

- *Submission.* The CLI reports a run as `submitted` or `queued` (`job.rs:309-320`). A submit that
  fails at worker startup returns an error, not a run (`admission.rs:458`).
- *Execution.* A submitted run can fail at worker registration, as above, or later in `prepare`,
  `pilots` or `apply` (`task_pilot_pipeline.yaml:168-277`).
- *Application.* A run can succeed without writing an assessment: a `superseded` outcome still
  succeeds the pipeline (`pilot.rs:152-159`). Whether an assessment was applied is read from the
  task's `pilot/assessment-*` artifact (§4), not from the run's state.

## 7. Phasing

1. **Local notes.** The config table and its validation, the prompt section, the audit event and
   the `doctor` finding ([ORB-29586]); `notes` in the output schema, the checks in
   apply, and the readouts in `orbit.task.show`, `orbit.task.list` and `orbit task show`
   ([ORB-29588]).
2. **The dashboard.** The notes on the task's detail ([ORB-29589], backlog). Claimed pilots get
   no build: task-pilot stays on the owner ([ORB-29590], documentation only; §6).

## 8. Acceptance

- Given a workspace with no `[workflow.pilot_notes]` table, the pilot's prompt has no `Pilot notes`
  section and no audit event is recorded. The activity's own instruction and output schema
  describe `notes` for every workspace, and a pilot that is given none returns none.
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
- **A re-check forgets the earlier notes.** A re-check re-examines earlier holding findings and
  does not assess afresh, and it returns its own notes rather than carrying the earlier ones
  forward ([Re-check runs](#re-check-runs)). A held task whose re-check says nothing is left with
  no notes until a later pilot answers again.
- **Apply resolves the given instructions again.** It reads the same config and the same
  committed files at the same prepared revision the dispatch read, rather than the dispatch's
  audit record. A config change between the two would change which labels are known; the
  note is then dropped as `unknown_label`, never trusted.
- **Plugin tool access from the pilot is verified in an isolated fixture ([ORB-29586]).** A real
  `task_pilot_pipeline` run in a disposable workspace with an enabled fixture plugin (`qaplugin@0.1.0`
  exposing `qaplugin.probe` as `read_only` and `qaplugin.mutate` as `mutating`, not
  `agent_callable`) proved both properties through the pilot's `orbit tool run` path: `qaplugin.probe`
  succeeded (exit 0) and invoked the backend once, while `qaplugin.mutate` was refused before the
  backend ran (`capability_denied`: `plugin.tool.mutating` requires `operator` or `runner`, while the
  pilot resolved as `agent-envelope` holding `[agent]`). Because `task_pilot` uses a deny list,
  registered non-disallowed plugin tools still appear in the envelope's `tools` list and enforcement
  for ordinary mutating plugin tools happens at the capability chokepoint; a plugin tool explicitly
  marked `agent_callable: true` with operator `agent_tools` consent would be reachable unless added
  to `tool_disallow_list`.
- **The claimed-worker path is traced in code, not run.** §6 rests on the code at `1d5d716ea`.
  In the traced path, the sandbox's authority-root deny is the only barrier. A leaf launch that
  could register its nested worker would prepare the pilot from its own config and files, and
  the trace did not cover such a launch.
- **A claimed task in backlog has no fence on the explicit branch.** Prepare's explicit branch
  reads no execution claim (`crates/orbit-core/src/application/task/pilot/prepare/run.rs:150-236`),
  and the apply fence covers only in-progress and review tasks
  (`crates/orbit-core/src/application/task/pilot/persist/reports.rs:95-105`). Whether a claimed
  task can be in backlog was not traced. This matters only if the registration block is absent.
- **The CLI explicit path has no replica refusal.** `PilotRequest::submit` and `dispatch_pilot`
  do not call the replica guard in `submit_explicit_task_pilot`
  (`crates/orbit-core/src/application/job/pilot_submit.rs:234-240`, `:289-295`). Nothing else on
  the traced path refuses a replica checkout. Whether it should is for the owner of the
  explicit-submit guard to decide. Not run.
- **Four notes and 1 KiB are guesses.** One use is known. The bounds are set to keep the
  pilot's prompt and the task readout small, and can be raised when a second use needs it.

## Task References

- [ORB-29567] — this design.
- [ORB-29586] — adds `[workflow.pilot_notes.<label>]`, the prompt section, the audit event and the `doctor` finding, and verifies plugin tool access in an isolated fixture.
- [ORB-29588] — adds `notes` to the assessment, the checks in apply and the `pilot_notes` readout.
- [ORB-29589] — backlog; shows pilot notes on the dashboard's task detail.
- [ORB-29590] — documents where task-pilot runs; claimed tasks take the owner's notes (§6).

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
