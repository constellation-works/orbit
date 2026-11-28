---
title: Experiments — Overview
owner: claude
created_date: 2026-10-25
updated_date: 2026-11-28
last_validated: 2026-11-21
status: Draft
stage: building
next: "The horizon decision-channel step remains unfiled"
feature: experiments
doc_role: overview
type: design
summary: "Operator-defined experiments whose arms overlay instructions, skills, tools, crews, allowlisted config or crew memory on a task's runs; sticky random assignment at admission, durable SQLite records, per-arm outcome analysis with intervals, and an operator decision that files the promotion"
tags: [experiments, admission, tag-instructions, crew-pools, analytics, decisions]
---

# Experiments — Overview

Status: building. Phase 1 (records and shadow assignment) and the instruction and tool surfaces of phase 2 are implemented: a `live` experiment applies them on owner and claimed runs, and every dispatch it changes records an `activity.experiment_overlay_applied` audit row with the digest of what it received. Removal is refused for the tools the agent handoff contract needs (`orbit.task.show`, `orbit.task.update`, `orbit.task.artifact` action `put`); a `false` instruction entry leaves Orbit's shipped default tag instructions out of that role. The per-arm readout (`orbit experiment readout`, `orbit.experiment.readout` and `GET /api/diagnostics/experiments/{id}/readout`) computes outcomes, intervals, the sample-ratio and drift checks and the verdict on read. The Analytics › Experiments dashboard page lists each experiment's per-arm progress toward `min_sample` (`GET /api/diagnostics/experiments`) and opens one (`GET /api/diagnostics/experiments/{id}`, plus the readout with its cumulative primary-metric series) to arm cards, a forest plot, the per-complexity table, crew mix, health warnings and the event timeline. An operator concludes from that page, the CLI or `orbit.experiment.conclude`: the conclusion event stores the readout JSON at that moment, assignment stops, and a chosen arm files one proposed promotion task. Concluding changes no configuration or repository file. An operator also defines, edits and starts experiments from that page: **New experiment** opens a form over the whole definition (population, arms with their crew, instruction, tool, skill and config overlays, primary metric, guardrails, sample rules, mode and reason), a draft card offers Edit and Start, and **Save and start** saves then starts after a confirmation naming the arms, weights and population. The routes `POST /api/diagnostics/experiments`, `PATCH /api/diagnostics/experiments/{id}`, `POST /api/diagnostics/experiments/{id}/start`, `POST /api/diagnostics/experiments/{id}/pause` and `POST /api/diagnostics/experiments/{id}/resume` are operator-only and call the same `create_experiment`, `update_experiment` and `change_experiment` as `orbit.experiment.create`, `update`, `start`, `pause` and `resume`, so core's validation and refusals apply unchanged and each write records its event with actor `dashboard` and the given reason. A running experiment's page offers **Pause** and a paused one **Resume**, each behind a confirmation with an optional reason; no other state offers either, and a session without operator authority sees neither. Reweight, clone and abandon stay on the CLI and tools.
Skills, crew pools and allowlisted config values are also live. Skill replacement currently supports Claude on Unix through an invocation-local discovery root; other providers refuse the overlay. Bundles are pinned to the run base, carried in claimed captures, and recorded with per-bundle digests (see [configuration](../../config/extensions.md#experiments--experiments-over-agent-treatments)).

Bearing: Orbit already has most of the parts an experiment needs, scattered across
mechanisms that were each built for one purpose. **Tag instructions**
(`[workflow.tag_instructions.<tag>]`,
`crates/orbit-core/src/application/tag_instructions.rs`) give an activity extra instructions
read from a repository file at the run's base revision; `captured_tag_instructions` resolves
them at the claim base into `AdmissionReceipt::tag_instructions` so a follower applies the same
text. **Crew pool draws** (`crates/orbit-core/src/application/job/crew_pools/draw.rs`) pick a
crew by weighted ticket draw and record it as `crew_selection` on the run input and
`crew_source` on the task; `seeded_crew_ticket` already makes keyed draws reproducible (review
admission, keyed task creation). [Crew performance](../_archived/crew-performance/1_scope.md)
scaled those weights until its removal in [ORB-22804]. A task's
`required_tools` widens its activity's tool baseline
(`adapter/engine_host/runtime_host/activity_tools.rs::admit_required_tools`). The analytics side has the shared trend
ladder (`crates/orbit-web/src/api/diagnostics/trend.rs`) behind the tokens-per-task
([ORB-18364]), diff-stats ([ORB-18383]), durations, completions and defects series, and the
[agent scorecards](../agent-scorecards/2_design.md) count review rejects, recoveries and
escaped regressions per run. The former explorer cohort comparison ([ORB-16814]) was observational; it
was removed with explorer delegation.

## Problem

Orbit changes how agents work through many levers: activity prompts, tag instructions,
skills, the tools an activity may call, which crew runs a task, and workflow settings such as
explorer delegation. None of those changes is measured. A prompt edit or a skill rewrite lands,
and whether it helped is read off charts that also moved for a dozen other reasons.

The trials run so far show what is missing:

- **Explorer delegation** was offered in about 300 implement prompts and used zero times
  (2026-10-24). The explore comparison could only split runs by whether the agent *chose* to
  explore, so its cohorts differ in more than the treatment.
- **The explore-first trial** ([ORB-18032] added its tag instruction) assigns by tag at
  filing, by hand. Its readout task [ORB-18033] was rejected.
- **The explore-brief trial** ([ORB-18363]) designs three arms assigned by task number modulo
  3 at filing, arms defined in workspace config, and a readout assembled by hand from several
  Analytics series. The explore brief itself was removed in [ORB-22805].

Each trial re-invents the same four things, and each gets one of them wrong in a way that
weakens the result:

1. **Assignment.** Hand-tagging at filing invites (unintended) picking, and an orchestrator
   that forgets a tag silently moves a task to control. Nothing checks that the arms came out
   the size they should.
2. **Definition history.** Arms live in `.orbit/config.toml`, which is not tracked in git.
   Editing an arm mid-trial rewrites what earlier tasks were measured against, and no record
   says when or why.
3. **Consistency.** A task's arm must hold across retries, step and final recovery, review
   repair and follower claims. Tags mostly give that, but nothing guarantees it, and a tag edit
   mid-run moves the task between arms.
4. **Readout.** Every trial needs per-arm sample counts, intervals, a stratified comparison and
   guardrails. Each readout builds them by hand, so they differ from trial to trial.

## Goal

An operator defines an **experiment**: a population of tasks, two or more **arms** each
carrying an **overlay** (instructions, skills, tools, crew, allowlisted config), a primary
metric, guardrails and a minimum sample. Orbit assigns each eligible task to one arm by a
reproducible random draw when the task is first admitted, keeps that arm for every later run
of the task on any host, measures outcomes from records it already keeps, and shows the arms
side by side with intervals. The operator records the decision; Orbit files the change that
makes the winning overlay the default. Experiments, arms, assignments and every change to
them are durable rows in the workspace store, so a readout can always say exactly what each
task received.

With no running experiment, Orbit behaves exactly as today. Tag instructions, crew pools and
config keep their meaning; an experiment layers on top of them for the tasks it assigns.

### Metric scope

Whole-task primary metrics are `total_tokens`, `cost_usd`, `lead_time`,
`first_pass_acceptance`, `review_repairs`, `step_recoveries`,
`final_recoveries`, `failed_checks`, `merge_conflicts`, `delivery_done_rate`,
`escaped_regressions`, and `lines_changed`. `cost_per_line`
and `lead_time_per_line` are whole-task too (lower is better) and may be
the primary; [ORB-20646] added these per-line metrics and their attributed
denominator. Each is a ratio of sums: the arm's total cost, or its total
lead time in hours, divided by the arm's total reviewable lines. The
interval is a paired bootstrap that resamples tasks. It is never the mean
of per-task ratios. Duration is `lead_time`, not `implement_duration`.

Reviewable lines are the added and deleted lines of that task's own
commits, excluding paths in `workflow.non_reviewable_paths` and pure
renames. That exclusion is workspace configuration, distinct from
`workflow.non_code_paths`, and empty unless the workspace lists lockfiles,
goldens, snapshots or generated files; docs and config stay reviewable.
Rows that landed before the denominator was recorded, or whose commits
cannot be isolated to the task, have an unavailable denominator. They are
not recomputed from a shared landing range. A readout drops them, and
drops runs whose reviewable total is zero, and reports each count per arm
separately.

An experiment whose primary metric is per-line must carry a
`lines_changed` guardrail. An arm that improves the per-line metric while
breaching that guardrail is not a winner.

The step-local
metrics `implement_tokens`, `implement_duration`, `turns_before_first_edit`,
and `result_bytes_before_first_edit` are exploratory. A treatment can move
work between steps without improving the task, so these cannot be primary;
they remain readable and usable as guardrails. A legacy step-local primary
keeps its data but its verdict is withheld.

## Non-goals

- **Adaptive allocation (bandits).** Fixed weights per segment keep the comparison simple to
  read. Shifting traffic to the leader trades inference for regret; revisit once fixed
  experiments are routine.
- **Experimenting on deterministic steps.** Gates, CI, landing and the pilot's validation are
  correctness machinery, not treatments.
- **Per-invocation randomization.** The unit is the task; mixing treatments inside one task's
  runs makes neither arm's outcome attributable.
- **Cross-workspace experiments.** One experiment lives in one workspace store. Running the
  same overlay in two workspaces is two experiments.
- **Automatic promotion.** A verdict suggests; the operator decides, and promotion is an
  ordinary reviewed change.

## Acceptance (feature level)

- Given a running experiment and an eligible task, the task's first admission writes exactly
  one assignment row, and every later run of that task on any host applies the same arm and
  records the overlay digest it received.
- Recomputing assignments from salt, experiment and task ids reproduces every stored unforced
  assignment.
- A running experiment's arms and overlays cannot be edited; reweighting opens a new segment
  and appears in the event log.
- A shadow experiment's readout shows arm counts consistent with the weights and no overlay
  applied to any run.
- The readout reports per-arm n, estimates with intervals, differences from control overall and
  per complexity, sample-ratio and drift warnings, and withholds a verdict before the horizon.
- A treatment arm whose overlaid step did not succeed on at least 0.9
  of its settled tasks gets a `withheld` verdict naming the arm and its delivered count; the
  readout also reports each arm's delivered rate and the primary metric per-protocol.
- Concluding stores the readout snapshot and, with a winner, files one `proposed` promotion
  task; no configuration or repository file is changed by the conclusion itself.
- With `experiments.enabled = false`, admission and runs are unchanged from today.

The mechanisms are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## Task References

- [ORB-16814] — compares implement runs with and without explorer calls (observational)
- [ORB-18032] — adds the `explore-first` tag instruction for a forced-explorer trial
- [ORB-18033] — requested the explore-first readout; the task was rejected
- [ORB-18363] — designs explore briefs and their three-arm trial
- [ORB-18364] — charts tokens per task by complexity and tag
- [ORB-18383] — records landed diff stats per delivery
- [ORB-20646] — adds whole-task per-line cost and lead-time metrics with an attributed denominator
- [ORB-22804] — removed crew-performance weighting.
- [ORB-22805] — removed explore briefs.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
