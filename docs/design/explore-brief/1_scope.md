---
title: "Scope: explore briefs from task-pilot"
owner: claude
last_updated: 2026-10-25
last_validated: 2026-10-25
status: Draft
feature: explore-brief
doc_role: scope
type: design
summary: "Scope: task-pilot leaves a bounded, revision-keyed code map (explore brief) as a task artifact, and the implementer starts from it instead of exploring cold; includes the three-arm trial and the slice breakdown"
tags: [explore-brief, task-pilot, task-artifacts, explore, implement-prompt]
---

# Scope: explore briefs from task-pilot

Status: proposed. Direction approved by Daniel on 2026-10-25; nothing is implemented.
Bearing: task-pilot already inspects every prepared task read-only at a pinned revision
before it runs. The `task_pilot_pipeline` job runs `prepare_task_pilot`, fans `task_pilot`
out over partitions of one to five tasks, then validates the results in the deterministic
`apply_task_pilot_results` step, with one repair pass for invalid assessments. The pilot is
triggered by the `preparation_eligible` state trigger (`assets/routines/task_pilot.yaml`) and
piloted again when the task's material fingerprint changes
([`[workflow.task_pilot_freshness]`](../../CONFIG.md)). It runs on the cheap
`workflow.purpose_crews.task_pilot` pool, is a `require_response_envelope: true` activity,
and already returns `context_files_after`, the task's lock footprint.
[Task artifacts](../task-artifacts/2_design.md#5-artifact-manifest) store bounded files per
task. Explorer delegation (`[workflow.explore]`, `orbit.agent.invoke`, [ORB-16814]) and tag
instructions (`[workflow.tag_instructions.<tag>]`) are the two existing ways to change what
an implementer starts with.

## Problem

An implementing agent typically spends its first ten or so turns on the same exploration:
find the module, grep for the symbol, read the neighbouring pattern. Dashboard and UI tasks
show it most. That exploration is paid at the implementer's model price, and its raw output,
often tens of thousands of tokens of grep and file reads, stays in context for every later
turn of the run.

Two earlier answers failed:

- **Caching shared exploration** across tasks needs batch jobs, goes stale with every merge,
  and breaks as soon as two tasks of one kind need different files.
- **Optional explorer delegation** was offered in about 300 implement prompts on 2026-10-24
  and used zero times. The call is synchronous, and the prompt tells the agent to re-read
  whatever it relies on, so delegating never pays off. [ORB-18032] added the `explore-first`
  tag instruction (`docs/agents/explore-first-implement.md`) to force it on a trial share;
  [ORB-18033] reads that trial out.

One fact about the current code shapes the fix. The pilot already does most of this
exploration. To propose an exact lock footprint it reads the task, runs `orbit.search`, finds
the files the change touches and checks each one at `source_revision`. It then throws all of
that away except the selector list and a rationale string. Today
`pilot/persist/apply_task.rs::apply_task` writes task fields, history and one audit comment,
and nothing else. It writes no artifact.

## Goal

Before the run, the pilot leaves a short **explore brief**: which files and symbols to change,
the call path, the existing pattern to copy, the tests that cover it, known gotchas and what
is out of scope. The implementer gets it at the start, verifies what it uses, and makes three
or four targeted reads where it would otherwise make ten exploratory ones. It carries a map of
about 2k tokens instead of the grep dump. Nothing else changes: the task text, criteria and
code stay the authority. Lock footprints, review, validation and delivery work as they do
today. A task without a usable brief runs exactly as it does now.

## Shape

### Brief contents and size cap

The brief is a JSON document (`explore_brief.v1`). Every location in it is an Orbit selector:
`file:<path>`, `dir:<path>` or `symbol:<path>#<symbol>:<kind>`, parsed by
`orbit_common::fs::selector`. **It never contains line numbers**, because they break with the
next merge. Apply refuses any target, or any `path:<digits>` pattern in free text.

| Field | Content | Cap |
|---|---|---|
| `change` | Targets the change creates, modifies or deletes, each with `what` (one sentence) | 12 entries, 200 chars each |
| `call_path` | Ordered targets from entry point to change site, each with `role` | 10 entries |
| `pattern` | Existing code to copy, each with `why` it is the precedent | 3 entries |
| `tests` | Tests and goldens that cover the change targets | 6 entries |
| `gotchas` | Non-obvious constraints, each citing a target or a doc path | 6 entries, 300 chars each |
| `out_of_scope` | Adjacent work the task does not own | 5 entries, 200 chars each |
| `maps` | Checked-in maps this brief builds on (see Shared maps) | 4 entries |
| `confidence` | `high`, `medium` or `low` | — |

Apply adds the provenance: `task_id`, `inspection_revision`, `observed_tree`,
`material_fingerprint`, `receipt_operation_id`, `pilot_run_id` and the pilot's crew.

The whole serialized document is capped at **8 KiB**, roughly 2k tokens and the same limit a
tag instruction has. That keeps it a map, not a transcript. Rules:

1. Every `change` target must be covered by `context_files_after`, either exactly or through a
   `dir:` or `file:` that contains it. A brief that wants to edit outside the lock footprint
   contradicts the pilot's own footprint and is refused.
2. Every target's anchor must exist at `inspection_revision`. The only exception is a creation
   target named by the task's latest `context_creation_authorized` history entry, the same
   rule `context_files_after` follows. A `symbol:` target's name must also occur in its file
   at that revision.
3. Free text describes. It does not paste code: no fenced blocks, and no excerpt longer than
   one line.

### Producer: extend task_pilot

**Recommendation: the brief is an optional per-task output of the existing pilot pass**
(`tasks[].explore_brief` in `task_pilot.yaml`'s output schema), validated and stored by
`apply_task_pilot_results`. It is produced only for tasks the prepare step marks as requested
(see Configuration).

- **Cost.** The pilot has already paid for the reads (`task.show`, search, selector
  verification at the pinned revision). The brief adds the call-path and pattern reads plus
  about 2k output tokens per task, at the price of the pilot pool (`luna`, `haiku` and
  `muse-spark` in this workspace), not the implementer's. A separate activity would pay again
  for the task read, the search, the inspection checkout and a provider start per task.
- **Latency.** The pilot runs at preparation, off the run's critical path. A separate activity
  is either also off the path, which means a second trigger after apply, or runs at
  admission. At admission it would add minutes to every run and spend exploration on runs
  that never start.
- **Trigger and freshness.** Re-piloting on a material change already exists (fingerprint,
  receipts, `pilot/freshness.rs`). A brief from the same pass inherits it. A separate activity
  needs its own trigger, its own receipt and its own staleness rules, which duplicates
  machinery for the same question.

**Rejected alternative: a separate `explore_brief` activity**, either chained after pilot
apply or run at admission. Its one advantage is isolation: a slow or failing brief cannot
delay or fail the pilot's assessment. The Rules below give that isolation inside the pilot
instead.

Rules:

1. **A brief never fails an assessment.** When the brief alone is invalid (schema, cap, line
   numbers, a missing anchor, a target outside the footprint), apply still applies the
   assessment, writes no brief, and records `explore_brief: {dropped: "<reason>"}` in the
   receipt's audit note. An invalid brief does not by itself mark the task `invalid`, so it
   never spends the single repair pass.
2. **Repair attempt.** When a task is repaired for another reason, the repair pilot returns a
   brief again, at the original `source_revision`, and apply validates it the same way. Either
   way, the brief that is written comes from the pass whose assessment was applied.
3. **No brief for `verified_no_diff`, `host_operational` or `unassessed`.** There is nothing
   to map.
4. **Wall clock.** A deeper per-task pass makes a five-task partition slower against its
   1800 s timeout. Phase 2 measures pilot duration and timeouts with briefs in shadow before
   any brief reaches an implementer. Open question 4 covers what to do if timeouts rise.

### Storage: a system-written task artifact

The brief is stored as the task artifact **`explore-brief.json`**. It is not a new task field.
Apply writes it through the system writer (`update_task_as_system`, `ArtifactWriter::System`).
The name is reserved for that writer, the same way `review-` names are reserved for the review
gate (`update/artifacts.rs::admit_agent_artifact`). An agent therefore cannot overwrite it or
forge one.

- **Revision key.** The brief is keyed by its `inspection_revision` (the pilot's pinned
  `source_revision`, proven by `observed_revision`/`observed_tree`) together with its
  `material_fingerprint` and `receipt_operation_id`. A brief is **current** only when all
  three hold: its `receipt_operation_id` matches the newest task-pilot receipt, its
  `material_fingerprint` equals that receipt's `applied_material.material[0]`, and its
  `material_fingerprint` equals the task's **current** material fingerprint. The current
  fingerprint is recomputed at dispatch under the pilot policy (the task's resolved
  `material_fields`), at the brief's `inspection_revision` — not at `base_sha` — by the
  same comparison `fresh_task_pilot_assessment`
  (`crates/orbit-core/src/application/task/pilot/freshness.rs`) already makes for
  pilot-routine admission: the receipt-certified fingerprint against the consumer's
  current fingerprint, failing closed when either cannot be read or recomputed.
  Receipt/fingerprint self-consistency alone is not currency: a material task edit
  (title, description, criteria, plan or selectors under the default `material_fields`)
  with no intervening re-pilot leaves the receipt and the stored fingerprint mutually
  consistent while the task has moved on. Otherwise (receipt mismatch or superseded
  fingerprint) the brief is ignored as superseded; a current-fingerprint mismatch is
  classified at admission as `dropped: material_changed` (see Staleness).
- **Replacement.** Re-piloting after a task edit upserts the same path. A re-pilot that
  produces no brief leaves the old file in place, but the old file no longer matches the new
  receipt and is never delivered. The store has no artifact delete, and none is needed.
- **Write order.** `AtomicTaskMutationParams` carries no artifacts, so the artifact and the
  receipt are two writes. Apply writes the artifact first and the receipt second. A crash
  between them leaves a brief whose `receipt_operation_id` matches no receipt, so the run
  proceeds cold. Neither order can deliver a brief from an assessment that was not applied.
- **Durable-artifact rule.** The root rule says activities rely on durable artifacts, never
  agent output, except read-only activities whose only product is structured data
  (`require_response_envelope: true`; the root guide names `task_pilot` as one). The brief
  is part of `task_pilot`'s existing structured product. No new step consumes an envelope: the
  existing deterministic apply step validates the brief and writes it as a durable artifact.
  Admission and the implementer read that artifact and never the pilot's message. This is the
  same path the pilot's selectors take into `context_files` today.

### Staleness at admission

The brief is checked when it is delivered to `agent_implement`: against the run's `base_sha`
(`steps.worktree.output.base_sha`) for an owner run, and against the claim base for a claimed
run. **Covered paths** are the anchor files of every brief target. A `dir:` target counts by
prefix and a `symbol:` target through its file, the same semantics as
`source_sensitivity = "context_files"` and `members/stale.rs::stale_tasks`.

**Material currency is checked first, before any code-drift check, for owner and claimed
runs alike.** At dispatch the task's current material fingerprint is recomputed under the
pilot policy at the brief's `inspection_revision` (see Storage). If the brief's stored
`material_fingerprint` differs from that current fingerprint — for example after a material
task edit with no intervening pilot receipt — the brief is not delivered and is recorded as
`dropped: material_changed`. If the current fingerprint cannot be recomputed or read, the
check fails closed with the same outcome. Only a brief that passes this check reaches the
code-drift classification below.

| Condition | Outcome |
|---|---|
| No `explore-brief.json`, or not current by receipt (see Storage) | `absent`: no brief, the run proceeds cold |
| Stored `material_fingerprint` differs from the task's current material fingerprint recomputed at dispatch, or that recomputation fails | `dropped: material_changed`: no brief, the run proceeds cold |
| `inspection_revision` is not an ancestor of `base_sha`, or the diff cannot be read | `dropped: revision_unrelated` |
| No covered path changed in `inspection_revision..base_sha` | `fresh`: delivered as stored |
| 1–3 covered files changed, ≤ 200 changed lines across them, and no `change` target deleted or renamed | `drifted`: delivered with the changed paths, their `--numstat`, and the range to diff |
| More than 3 covered files, more than 200 lines, or a `change` target deleted or renamed | `dropped: stale` |

- **Not the diff itself.** A `drifted` brief names the drifted entries and gives the
  implementer `git diff <inspection_revision> HEAD -- <path>` to run. It does not inline the
  patch, which could exceed the brief's own size cap.
- **No re-pilot at admission.** That would put a pilot run (minutes) on the critical path of
  every stale run. The pilot routine only covers `proposed`/`backlog` tasks, and a workspace
  that wants briefs refreshed before admission can already set
  `source_sensitivity = "context_files"`. Phase 2 reports how often briefs drop. A high drop
  rate is a reason to revisit, not a reason to build re-piloting first. Recomputing the
  current material fingerprint at dispatch is not a re-pilot: it is the deterministic
  fingerprint comparison `fresh_task_pilot_assessment` already performs, with no pilot
  invocation, and it is required even though no re-pilot runs.
- **Constants, not config.** 3 files and 200 lines are constants in phase 3. The trial reports
  `fresh`/`drifted`/`dropped` counts, and a setting is added only if the readout shows a
  threshold matters.
- **Recorded every time.** Each `agent_implement` dispatch records
  `activity.explore_brief_applied` with the outcome above, the brief's SHA-256 and its
  revision. `absent` is recorded too, so the trial can tell "no brief" from "brief dropped".

A resumed implementation on a new base is checked again against that base.

### How the implement prompt presents it

When a brief is delivered, the task context carries it as `explore_brief`, beside
`light_delivery` and `final_validation_commands` (`task_context.rs::task_context_for_agent_input`).
`agent_implement.yaml` gains an `explore_brief_instruction` paragraph, added only when the
field is present, in the same way `explore_instruction` is gated. Its intent:

- This is a starting map from a cheaper read-only pass at `<inspection_revision>`. Start from
  its `change` and `pattern` targets instead of searching the tree.
- It is not authority. The task text, the criteria and the code at your base win. Read each
  file before you edit it or cite it.
- A wrong brief anchors. If an entry does not match the code, discard it and do not bend the
  change to fit it. Treat `drifted` entries as unverified until you have read their diff.
- In the execution summary, list the brief entries that turned out wrong or missing.

Rules:

1. **Absent or invalid brief.** No field and no paragraph: the run proceeds cold, exactly as
   today.
2. **Explorer tools.** Read-only delegation is still offered whenever `[workflow.explore]`
   allows it. The brief does not tell the agent to delegate, and it does not forbid it.
3. **`explore-first`.** Tag instructions render last and are not touched. The trial keeps
   the arms disjoint (see Trial). A task that carries both tags gets both texts, and Orbit
   core does not special-case trial tags. The readout excludes such tasks.
4. **Implement only.** Review, review repair and recovery activities do not receive the
   brief. A reviewer anchored on the pilot's map would stop being an independent check.

### Shared maps vs task briefs

Exploration that every task of a kind repeats belongs in a **checked-in map**, not in each
brief. A map is reviewed in a PR, stays current with the code that changes it (stale docs
block review), and costs nothing per task.

| Belongs in a checked-in map | Belongs in the brief |
|---|---|
| Where a subsystem's pieces live, and how a new one is registered (routes, assets, dispatch) | This task's change targets |
| The canonical pattern for a recurring kind of change | Which precedent fits *this* change |
| Which golden or test family covers a surface | Which tests cover *these* targets |
| Generators and their companions | Gotchas specific to this change |

The pilot reads maps the workspace guide links to. When a map covers part of the task, the
brief cites it in `maps` and does not repeat it. Maps are ordinary workspace docs, and Orbit
core does not know about any particular one.

**The first map** is a new `dashboard-map.md` under `docs/agents/` in this workspace, because dashboard
tasks show the problem most. It covers where the dashboard's views, API routes and assets
live, how a view is registered and served, which pattern a new panel or series copies, and
the screenshot and goldens obligations. It lands before the trial starts, so every arm has
it and the trial measures what the brief adds on top. Further maps come from evidence: in
phase 2 every brief is recorded, so a `pattern` or `call_path` target that recurs across
briefs in one area is a candidate for the next map.

### Generic core

- The schema uses generic selectors and has no UI-, language- or toolchain-specific field.
- Validation uses git object reads and selector parsing, which already exist for
  `context_files_after`.
- Gating is by configured tags. Maps are workspace content that the workspace guide links.
- The UI emphasis in the Problem shows up only as the first map, which is a workspace doc.
- **Non-Git workspaces** get no brief in phase 1. With no revision, staleness cannot be
  decided (open question 3).

### Configuration

| Key | Default | Meaning |
|---|---|---|
| `workflow.explore_brief.enabled` | `false` | Whether the pilot is asked for briefs at all |
| `workflow.explore_brief.tags` | `[]` | When non-empty, only tasks carrying one of these tags get a brief; empty means every piloted task |
| `workflow.explore_brief.deliver` | `true` | `false` keeps briefs in shadow: produced and stored, never given to the implementer (phase 2) |

These keys are layered per workspace like every other `workflow.*` key. The trial sets
`enabled = true` and `tags = ["explore-brief"]`. A tag is read when the pilot runs. Tags are
not in the default `material_fields`, so a task tagged after its pilot gets no brief until it
is piloted again. Trial tags are therefore set at filing.

### Surfaces

- **`orbit run show`** prints an `Explore brief:` line per implement dispatch (`fresh`,
  `drifted`, `dropped: <reason>` or `absent`). Its JSON carries the audit fields.
- **Task detail** lists `explore-brief.json` like any other artifact. `orbit task artifact get`
  reads it.
- **Pilot comment.** The receipt's audit note records `explore_brief: {written, sha256, bytes}`
  or `{dropped: <reason>}`. The CLI pilot-comment presentation shows that one line.

### Trial

The trial runs three arms in parallel over the same period, next to the explore-first trial.

| Arm | Tag | Treatment |
|---|---|---|
| Control | neither tag | Today's prompt, explorer offered but optional |
| Forced explorer | `explore-first` | [ORB-18032] tag instruction, mapped in `ws_orbit` config |
| Explore brief | `explore-brief` | Pilot brief delivered (`workflow.explore_brief.tags = ["explore-brief"]`); explorer offered, not forced |

- **Population.** Tasks filed in `ws_orbit` during the trial with complexity `medium`, `hard`
  or `xhard` and pilot disposition `selectors`.
- **Assignment.** The orchestrator tags tasks at filing by task number modulo 3: 0 is control,
  1 is `explore-first`, 2 is `explore-brief`. This is deterministic, needs no code and leaves
  no room for picking which tasks look promising. Tasks that carry both tags are excluded.
- **Analysis.** The primary analysis is by tag (intention to treat). A secondary cut of the
  brief arm splits it by brief outcome (`fresh`/`drifted` vs `dropped`/`absent`).

**Metrics:**

1. **Normalized tokens per task by complexity.** Source: the [ORB-18364] Analytics › Metrics
   series *average tokens per task over time*. It is one point per done task, the sum of
   `NormalizedTokenSummary::normalized_token_total` over the task's linked managed
   invocations, with tag cohorts. Read it twice. **Implement only** gives the implementer
   saving. **All invocations** gives the net, because it includes the pilot's extra spend. A
   partition's pilot invocation is shared by up to five tasks; the readout states how the
   series attributes it (see Concerns).
2. **Turns to first edit.** The number of tool calls before the first file-mutating tool call
   in a task's first `agent_implement` invocation. Alongside it, the `result_bytes` summed
   over those calls, which measures the dump directly. Both come from the ordered
   `ToolCallTrace` (`seq`, `tool_name`, `result_bytes`) on `InvocationRecord.tool_calls`.
   Orbit records no such metric today, so slice S5 adds the series. A provider whose edits go
   through a generic shell tool has no detectable first edit. Those invocations are excluded
   and counted, never scored as zero.
3. **Delivery pass rate.** The share of the arm's tasks that reach `done` with no review
   repair round and no step or final recovery run on the implement path. Also the share that
   reach `done` at all.
4. **Brief accuracy (brief arm only).** Recall and precision of `change` file targets against
   the files in the delivered diff. Also the footprint-miss rate (`context_files_widened`)
   that `explore_analytics.rs::explore_comparison` already reports for the other arms.

**Minimum sample before a readout:** 30 done tasks per arm, and 15 per arm in each complexity
cell the readout draws a conclusion for. Cells below that are shown and marked, but not
concluded. The trial runs until every arm reaches its minimum or for four weeks, whichever
comes first. If an arm is short at four weeks, the readout says so and concludes nothing for
it. **Adoption bar,** proposed for Daniel to confirm: in the `medium` and `hard` cells, the
brief arm's median implement-only tokens per task is at least 15% below control, its
all-invocation median is lower too, and its delivery pass rate is no more than 5 points below
control. Meeting the bar makes `tags = []` (every task) the recommended setting, which is a
config change only.

## Non-goals

- **Implementing anything here.** This doc and its slice table are the deliverable. Code,
  prompts and config change in the slices.
- **Cross-task caching or replay of exploration.** It goes stale with every merge and misfits
  sibling tasks. Shared knowledge goes into checked-in maps instead.
- **Replacing the explorer tools or the explore-first trial.** Both stay. The trial compares
  all three arms.
- **Changing the lock footprint.** `context_files_after` keeps its meaning and its rules. The
  brief must fit inside it and cannot widen it.
- **Re-piloting at admission.** See Staleness.

## Concerns and honest limitations

- **A wrong brief anchors.** Cheaper models are worse explorers, and a confident wrong map can
  cost more than no map. Mitigations: validation against the revision, shadow phase accuracy
  numbers before delivery, prompt text that says the brief is not authority, and accuracy
  tracked in the trial. None of these makes anchoring impossible.
- **Pilot cost is shared across a partition.** One pilot invocation serves up to five tasks.
  The "all invocations" reading is only honest if the [ORB-18364] series splits a shared
  invocation across its tasks or excludes it and says so. If it does neither, the net reading
  is approximate and the readout must say which.
- **Turns to first edit is provider-dependent.** It depends on how each provider names edit
  tools. Shell-only editing is invisible. The metric compares arms within a provider, not
  across providers.
- **Briefs lag the base.** A task can wait days between its pilot and its run. With the
  default `source_sensitivity = "ignore"`, nothing refreshes the brief, and the admission
  thresholds drop it instead. The trial's drop rate shows what that costs.
- **Tag-at-filing discipline.** Assignment depends on the orchestrator tagging at filing. A
  task tagged late gets no brief until it is piloted again, and it still counts in the brief
  arm under intention to treat.
- **No artifact delete.** Superseded briefs remain stored on the task until the task is
  archived. They are small and never delivered.

## Phasing

1. **Dashboard map.** A new `dashboard-map.md` under `docs/agents/` lands and the workspace guide links it.
   It benefits every run, including control.
2. **Produce in shadow.** The pilot output, apply validation, the `explore-brief.json` artifact
   and the config keys land, with `deliver = false`. Measure pilot duration, timeouts,
   brief-drop reasons and accuracy against delivered diffs.
3. **Deliver.** Admission freshness, the implement prompt paragraph, the audit event and
   `run show` line land for owner runs and for claimed runs (the brief captured into the
   claim like tag instructions).
4. **Trial.** The turns-to-first-edit series lands. The operator enables the arms and the
   orchestrator assigns at filing.
5. **Readout and default.** Read out against the adoption bar. Widen `tags` or retire the
   feature.

## Open questions

1. **Assignment rule?** Task number modulo 3 is mechanical but couples assignment to filing
   order. The alternative is an orchestrator coin flip recorded on the assignment task.
   Recommendation: modulo 3.
2. **Trial population?** The explore-first trial targeted hard tasks on `opus`. The brief is
   generic, so this doc proposes `medium` and above for all crews, read per complexity. Should
   the three-arm comparison instead be limited to `hard` to match [ORB-18033]?
3. **Non-Git workspaces?** No brief in phase 1. Is a filesystem-mtime staleness check worth
   building, or do they stay excluded?
4. **Pilot partition size for brief tasks?** If phase 2 shows timeouts or slow partitions,
   either cap brief-requested tasks at two per partition or raise the pilot timeout for them.
   Which does Daniel prefer?
5. **Adoption bar?** Is 15% fewer implement tokens with no more than a 5-point pass-rate loss
   the right bar?

## Acceptance (feature level)

- Given a task the config requests a brief for and a pilot assessment with disposition
  `selectors`, the applied pilot leaves a current `explore-brief.json` of at most 8 KiB.
  Every target in it is a selector whose anchor exists at the inspection revision, and every
  `change` target lies inside `context_files_after`.
- Given a brief that fails validation, the pilot assessment still applies, no brief is
  written, the drop reason is in the receipt, and no repair pass is spent on it.
- Given an implement dispatch, `orbit run show` reports `fresh`, `drifted`, `dropped: <reason>`
  or `absent`, and the prompt carries the brief only in the first two cases.
- Given a material task edit after the pilot with no intervening pilot receipt, an implement
  dispatch (owner or claimed) does not deliver the brief, records
  `activity.explore_brief_applied` as `dropped: material_changed`, and proceeds cold; given
  no material edit, the same brief is still classified `fresh`/`drifted` by code drift and
  delivered. A regression test at the `agent_implement` brief-delivery (admission/task-context)
  boundary covers both cases.
- Given an agent or operator `orbit.task.artifact.put` to `explore-brief.json`, Orbit refuses
  it.
- Given a claimed run on a follower, the implementer receives the same brief, subject to the
  same staleness rules against the claim base.
- Given the trial, the Analytics › Metrics series give tokens per task by complexity and tag,
  and turns to first edit by tag, with low-count cells marked.

## Implementation slices

Each row is meant to be filed as one `ws_orbit` task, except S7 and S8, which are operator
and orchestrator tasks. Anchors are `path::symbol`, not line numbers.

| # | Outcome | Depends on | Acceptance criteria | Modification anchors |
|---|---|---|---|---|
| S1 | Checked-in dashboard map for implementers | — | A new `dashboard-map.md` under `docs/agents/` names, by path and symbol, where dashboard views, API routes and assets live, how a view is registered and served, the pattern a new panel or series copies, and the screenshot and goldens obligations. It contains no line numbers. The workspace guide links it. The generated docs index is updated if a row changes | new `dashboard-map.md` under `docs/agents/`; `crates/orbit-web/assets/dashboard/`; `crates/orbit-web/src/api/`; root `CLAUDE.md` link |
| S2 | Pilot produces and stores briefs (shadow) | — | Under the three `workflow.explore_brief.*` keys, prepare marks the requested tasks. `task_pilot` returns `tasks[].explore_brief` for them. Apply validates the brief (schema, 8 KiB cap, no line numbers, anchors at the revision, `change` inside the footprint) and writes `explore-brief.json` through the system writer before the receipt. An invalid brief is dropped with its reason in the audit note and does not make the task `invalid`. The repair pass returns a brief. Agent and operator writes to the name are refused. CONFIG.md documents the keys. A regression test at the pilot apply boundary covers one valid brief and one dropped brief | `crates/orbit-core/assets/activities/task_pilot.yaml` (output schema, new "Explore brief" instruction section, Repair attempt); `crates/orbit-core/assets/activities/prepare_task_pilot.yaml`; `crates/orbit-core/src/application/task/pilot/prepare.rs::prepare`; `pilot/apply/task.rs::settle_task`; `pilot/assessment.rs`; `pilot/persist/apply_task.rs::apply_task`; `crates/orbit-core/src/application/task/update/artifacts.rs::admit_agent_artifact`; `crates/orbit-config/src/registry/settings/table/keys_tail.inc`; `crates/orbit-config/src/registry/settings/snapshot.rs`; `crates/orbit-cli/src/command/task/pilot_comment.rs::assessment_summary`; `docs/CONFIG.md` |
| S3 | Owner runs receive a fresh or drifted brief | S2 | An `agent_implement` dispatch reads the current brief, first recomputes the task's current material fingerprint at the brief's `inspection_revision` under the pilot policy and drops the brief as `dropped: material_changed` (recorded in `activity.explore_brief_applied`, no delivery) when it differs from the stored `material_fingerprint` or cannot be recomputed, then classifies a passing brief against `base_sha` with the thresholds in this doc, and injects `explore_brief` and `explore_brief_instruction` only for `fresh`/`drifted`. It records `activity.explore_brief_applied`, including `absent`. `orbit run show` prints the `Explore brief:` line. Review and recovery activities never receive the brief. `deliver = false` suppresses delivery but not the audit record. A regression test at the admission/task-context boundary covers delivery refusal after a material task edit with no intervening pilot receipt, and delivery of an unchanged task's brief | `crates/orbit-core/src/adapter/engine_host/actions/task_context.rs::task_context_for_agent_input`; `crates/orbit-core/assets/activities/agent_implement.yaml`; `crates/orbit-types/src/workflow/activity_job/activity_v2.rs::AgentLoopSpec`; `crates/orbit-engine/src/activity_job/cli_runner/orchestrator/dispatch.rs::run_cli_backend_for_step`; `crates/orbit-cli/src/command/run/show.rs`; `crates/orbit-core/src/application/task/pilot/freshness.rs::fresh_task_pilot_assessment` (pattern to copy) |
| S4 | Claimed runs receive the same brief | S3 | The owner captures the current brief and its revision into the admission claim, alongside the captured tag instructions. The follower recomputes the current material fingerprint and applies the same `dropped: material_changed` check, then classifies it against the claim base and injects it as S3 does, with no owner artifact read at dispatch. The same regression coverage applies to the claimed path (material edit with no new receipt drops, unchanged task delivers) | `crates/orbit-core/src/application/tag_instructions.rs::captured_tag_instructions` (pattern to copy); `crates/orbit-store/src/contracts/task/coordination/admission.rs`; `crates/orbit-store/src/repository/task/coordination/admission.rs` |
| S5 | Turns-to-first-edit series | — | A diagnostics endpoint and an Analytics › Metrics series report, per trend bucket, complexity and tag cohort, the count of tool calls and summed `result_bytes` before the first file-mutating call of each task's first implement invocation. Invocations without a detectable edit are excluded and counted. Low-count buckets are marked. Screenshots are attached | `crates/orbit-types/src/telemetry/invocation.rs::ToolCallTrace`; `crates/orbit-store/src/contracts/invocation.rs::InvocationRecord`; `crates/orbit-web/src/api/diagnostics/trend.rs`; `crates/orbit-web/assets/dashboard/js/metrics.js` |
| S6 | Brief accuracy report | S2 | For done tasks with a brief, a diagnostics endpoint reports recall and precision of `change` file targets against the delivered diff's files, and the brief outcome mix (`fresh`/`drifted`/`dropped`/`absent`) | `crates/orbit-web/src/api/diagnostics/`; `crates/orbit-core/src/application/job/explore_analytics.rs::explore_comparison` (pattern to copy) |
| S7 | Trial started (operator, `ws_constellation`) | S1, S3, S4, S5, [ORB-18364] | Box `ws_orbit` config maps `[workflow.tag_instructions.explore-first]` and sets `workflow.explore_brief = {enabled = true, tags = ["explore-brief"]}`. The orchestration skill records the assignment rule. The start date is commented on the assignment task | box `ws_orbit` `.orbit/config.toml`; `.claude/skills/orchestration-session/SKILL.md` (constellation) |
| S8 | Three-arm readout (`ws_constellation`, with or after [ORB-18033]) | S6, S7, minimum sample | The readout reports each metric per arm and complexity cell, marks cells below 15, applies the adoption bar, and recommends `tags = []`, keeping the trial, or retiring the feature | — |

## Task References

- [ORB-16814] — added read-only explorer delegation (`[workflow.explore]`, `orbit.agent.invoke`).
- [ORB-18032] — added the `explore-first` tag instruction that forces explorer delegation for the trial.
- [ORB-18033] — reads out the explore-first trial (`ws_constellation`).
- [ORB-18363] — designed explore briefs (this doc).
- [ORB-18364] — charts average normalized tokens per task over time by complexity and tag cohort.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
