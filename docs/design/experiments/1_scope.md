---
title: "Scope: randomized experiments over agent treatments"
owner: claude
last_updated: 2026-10-26
last_validated: 2026-10-25
status: Draft
stage: building
next: "Slice 4: skills, crew and config surfaces (phase 3)"
feature: experiments
doc_role: scope
type: design
summary: "Scope: operator-defined experiments whose arms overlay instructions, skills, tools, crews or allowlisted config on a task's runs; sticky random assignment at admission, durable SQLite records, per-arm outcome analysis with intervals, and an operator decision that files the promotion"
tags: [experiments, admission, tag-instructions, crew-pools, analytics, decisions]
---

# Scope: randomized experiments over agent treatments

Status: building. Phase 1 (records and shadow assignment) and the instruction and tool surfaces of phase 2 are implemented: a `live` experiment applies them on owner and claimed runs, and every dispatch it changes records an `activity.experiment_overlay_applied` audit row with the digest of what it received. Removal is refused for the tools the agent handoff contract needs (`orbit.task.show`, `orbit.task.update`, `orbit.task.artifact.put`); a `false` instruction entry leaves Orbit's shipped default tag instructions out of that role. The per-arm readout (`orbit experiment readout`, `orbit.experiment.readout` and `GET /api/diagnostics/experiments/{id}/readout`) computes outcomes, intervals, the sample-ratio and drift checks and the verdict on read.
Bearing: Orbit already has most of the parts an experiment needs, scattered across
mechanisms that were each built for one purpose. **Tag instructions**
(`[workflow.tag_instructions.<tag>]`,
`crates/orbit-core/src/application/tag_instructions.rs`) give an activity extra instructions
read from a repository file at the run's base revision; `captured_tag_instructions` resolves
them at the claim base into `AdmissionReceipt::tag_instructions` so a follower applies the same
text. **Crew pool draws** (`crates/orbit-core/src/application/job/crew_pools/draw.rs`) pick a
crew by weighted ticket draw and record it as `crew_selection` on the run input and
`crew_source` on the task; `seeded_crew_ticket` already makes keyed draws reproducible (review
admission, keyed task creation). [Crew performance](../crew-performance/1_scope.md) scales
those weights, with a shadow mode that records a factor without applying it. A task's
`required_tools` widens its activity's tool baseline
(`adapter/engine_host/runtime_host/activity_tools.rs::admit_required_tools`). The analytics side has the shared trend
ladder (`crates/orbit-web/src/api/diagnostics/trend.rs`) behind the tokens-per-task
([ORB-18364]), diff-stats ([ORB-18383]), durations, completions and defects series, and the
[agent scorecards](../agent-scorecards/2_design.md) count review rejects, recoveries and
escaped regressions per run. The one cohort comparison Orbit ships,
`application/job/explore_analytics.rs::explore_comparison` ([ORB-16814]), is observational.

## Problem

Orbit changes how agents work through many levers: activity prompts, tag instructions,
skills, the tools an activity may call, which crew runs a task, and workflow settings such as
explorer delegation. None of those changes is measured. A prompt edit or a skill rewrite lands,
and whether it helped is read off charts that also moved for a dozen other reasons.

The trials run so far show what is missing:

- **Explorer delegation** was offered in about 300 implement prompts and used zero times
  (2026-10-24). The explore comparison could only split runs by whether the agent *chose* to
  explore, so its cohorts differ in more than the treatment.
- **The explore-first trial** ([ORB-18032], readout [ORB-18033]) assigns by tag at filing,
  by hand.
- **The explore-brief trial** ([ORB-18363]) designs three arms assigned by task number modulo
  3 at filing, arms defined in workspace config, and a readout assembled by hand from several
  Analytics series.

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

## Shape

### Nouns

| Noun | Meaning |
|---|---|
| Experiment | A named test in one workspace: hypothesis, population, arms, metrics, sample rules, status |
| Arm | One treatment of an experiment, with a weight. One arm is the control |
| Overlay | What an arm changes, per surface (below). The control's overlay is usually empty |
| Assignment | One task's arm in one experiment, drawn once and kept |
| Segment | A stretch of an experiment with fixed weights and arms; reweighting starts a new one |
| Outcome | The metrics read for one assigned task, from records Orbit already keeps |

### Storage

Experiments are operational records, not configuration. They live in the workspace store
(SQLite), behind one additive migration, because they need what config cannot give: an
append-only history, per-task rows, and a definition that cannot be edited out from under the
tasks already measured.

| Table | Holds |
|---|---|
| `experiments` | id, name, hypothesis, population filter, primary metric, guardrails, min sample per arm, max duration, salt, mode (`shadow` or `live`), status, created by/at |
| `experiment_arms` | experiment id, arm name, `control` flag, weight, overlay (typed JSON), overlay digest |
| `experiment_events` | append-only: every status change, reweight, arm retirement and conclusion, with actor, time and reason; each reweight opens a new segment |
| `experiment_assignments` | task id, experiment id, arm, segment, overlay digest as delivered, assigned at, `forced` flag, exclusion reason if not enrolled |

Rules:

1. **Frozen once running.** Arms, overlays, population and metrics are editable only in
   `draft`. A running experiment changes only through events: a reweight or an arm retirement
   (both open a new segment), pause, resume, conclude, abandon. Anything else is a new
   experiment, which `clone` makes cheap.
2. **Overlays pin content.** An overlay that names a repository file (instructions, a skill
   bundle) is read at each delivery's base revision, like tag instructions today. Every
   assignment and every run records the digest of what it actually received. When the digest
   changes mid-experiment the arm is marked **drifted** on the readout and the runs before and
   after are distinguishable; they are never merged silently.
3. **The owner's store is the only source.** Followers hold no experiment tables. A claimed
   run carries its arm and resolved overlay in the admission claim (next section).

Workspace config keeps only policy: whether experiments may run at all, how many may run at
once, and which config keys arms may override (Configuration).

### Overlay surfaces

Each surface reuses an existing seam, so core gains no experiment-specific behaviour in the
activities themselves.

| Surface | What an arm can set | Seam | Validated at start |
|---|---|---|---|
| Instructions | A repository file per role (`implement`, `review`, `recovery`), or `false` to suppress a shipped default | The tag-instruction slot; an experiment's text follows tag-instruction text | File exists at the current base |
| Skills | Replace a named skill with a bundle directory in the repository | **New seam.** Skills reach agents today only as global symlinks (`application/skill.rs::seed_default_skills`, `bootstrap/init.rs::ensure_skill_links`); a per-run skill root is needed | Bundle exists and parses |
| Tools | Add tools to, or remove optional tools from, the activity baseline | Adding reuses `admit_required_tools`. Removing is **new**: a per-run denial applied after `resolve_activity_tool_grant` | Tools exist and are agent-facing; a tool the activity contract requires cannot be removed |
| Crew | A crew pool (members and weights) for the arm | The admission crew draw, as the run-level `<complexity>_complexity_crews` override does today; the arm's pool skips performance weighting | Crews exist and may run the task's complexity |
| Config | Values for allowlisted keys only | **New:** no typed list of per-run overridable `workflow.*` keys exists; `orbit-config` gains one, each key opted in by code | Key is on both the code list and the workspace allowlist |

Each overlay entry names the roles it applies to; the default is `implement` only. Review and
recovery overlays are allowed but are opt-in per entry, because a reviewer that shares the
implementer's treatment stops being an independent check of it.

### Assignment

1. **When.** At a task's first delivery admission on the owner, before the crew draw (an arm
   may set the crew). The population filter (complexity, type, tag include/exclude, task
   origin) is evaluated against the task at that moment.
2. **How.** For each running experiment whose population matches, in creation order:
   a `seeded_crew_ticket`-style ticket from `sha256("<salt>:<experiment_id>:<task_id>")`,
   walked over the arms' cumulative weights in the current segment. The draw is deterministic,
   so any readout can recompute it, and no one can choose a task's arm by timing. (The run
   admission crew draw itself uses unseeded `random_crew_ticket`; that stays as is.)
3. **Sticky.** The assignment row is written before the run starts and is read, never
   redrawn, for every later run of the task: retries, step and final recovery, review repair,
   resume, re-admission after a block. A task keeps its arm until it is terminal, even if the
   experiment concludes meanwhile.
4. **One experiment per surface per task.** A task can be in several experiments only when
   their overlays touch disjoint surfaces. A later experiment that conflicts records the task
   as excluded (`surface_conflict`) rather than assigning it.
5. **Forced assignment.** An operator can assign an arm by hand. The row records
   `forced = true` and the analysis leaves it out.
6. **Claims.** The owner resolves the overlay (reads instruction files and skill bundles at
   the claim base, computes digests) and captures arm, segment and resolved overlay into the
   admission claim as a new `AdmissionReceipt` field, next to the captured tag instructions,
   under the same byte budget discipline. The follower applies what the claim carries and reads
   nothing from the owner's store at dispatch. Unlike tag instructions, which fall back to the
   replica's config when the receipt lacks them, a missing or unreadable experiment capture
   makes the follower refuse the claim: there is no replica copy to fall back to. Hosts
   already run one build in lockstep.
7. **Shadow mode.** A `shadow` experiment assigns and records but applies no overlay. Its
   readout is an A/A test: the arms should not differ and should match their weights. Every new
   assignment path ships in shadow first.

### Outcomes and analysis

The unit is the task and the analysis is intention-to-treat: every enrolled, unforced task
counts in its arm, including tasks that end blocked or rejected. Outcomes are read from
existing records joined on the assignment row; nothing new is recorded per run except the
overlay digest.

| Metric | Source |
|---|---|
| Implement tokens, all-invocation tokens, cost | Invocation records linked to the task's runs (the [ORB-18364] series' attribution) |
| Lead time, implement step duration | Task history, step records |
| Turns and result bytes before first edit | `InvocationRecord.tool_calls` (the explore-brief S5 series) |
| First-pass acceptance, review repairs | Review gate records, scorecard terms |
| Step recoveries, final recoveries, failed CI checks, merge conflicts | Run events (`step_recovery_attempted`, `final_recovery_*`, `failed_checks`, `pr_conflict_recovery`) |
| Done rate | Terminal status |
| Escaped regressions | `regression_from` relations within 14 days of landing, any escape class (the scorecards charge only some classes; the experiment counts them all) |
| Lines changed | Landed diff stats ([ORB-18383]) |

The experiment names one primary metric and any number of guardrails
(`delivery_done_rate: -5pt`, `escaped_regressions: +0`). The readout computes, per arm and
segment:

- **Estimates.** n, mean and median, with 95% bootstrap intervals for continuous metrics
  (fixed seed, resampled by task) and Wilson intervals for rates.
- **Difference from control.** With its interval, overall and per complexity cell; the overall
  figure is post-stratified by complexity so an arm that drew more hard tasks is not penalised.
- **Sample-ratio check.** A chi-square test of observed arm counts against the weights; below
  p = 0.001 the readout shows a warning and gives no verdict, because the assignment itself is
  suspect.
- **Verdict.** Withheld until every arm reaches `min_sample` or `max_days` passes (fixed
  horizon, so repeated looks do not inflate false wins). Then: `winner <arm>` when the primary
  metric's interval excludes zero in the better direction and no guardrail's point estimate is
  past its threshold; `no difference`; or `inconclusive` when a guardrail interval straddles
  its threshold. Before the horizon the readout says `collecting`.
- **Lagging metrics.** Escaped regressions settle 14 days after landing; the readout shows how
  many outcomes are still open.

The readout is computed on read. At conclusion its full JSON is stored with the conclusion
event, so the decision can always be reproduced against the numbers it was made on.

### Decision and promotion

Concluding is an operator act: pick the winning arm (or none) and give a reason. The
conclusion is an `experiment_events` row carrying the readout snapshot. When a winner is chosen, Orbit files one
`proposed` task in the workspace that makes the winning overlay the default: an instruction
file becomes a tag-instruction mapping or a workspace-guide change, a skill bundle replaces the
canonical skill, a tool change moves into the baseline, a crew pool into the complexity pool
weights, a config value into config. Orbit never rewrites its own configuration or repository
as a side effect of a decision.

When an experiment reaches its horizon, Orbit should raise it for decision through the
[human decision channel](../human-decision-channel/1_scope.md) instead of waiting to be
noticed. Decision requests are task-bound today (`orbit_types::task::DecisionRequest` carries
a `task_id` and a `DecisionGate` of block triage, pilot admission, proposed approval or review
approval), so this needs either a subject that is not a task or a tracking task per
experiment (open question).

### Configuration

Policy only. Experiment definitions are store records.

| Key | Default | Meaning |
|---|---|---|
| `experiments.enabled` | `false` | Off: no new assignments; running experiments keep their rows and in-flight tasks keep their arms |
| `experiments.max_running` | `3` | Running experiments per workspace |
| `experiments.config_keys` | `[]` | Allowlisted config keys arms may override in this workspace; each must also be on the code list |

### Surfaces

- **Tools and CLI.** `orbit.experiment.{create, update, clone, start, pause, resume, reweight,
  assign, conclude, abandon, show, list, readout}` and the matching `orbit experiment` verbs.
  `update` works only in `draft`. Creating, starting, reweighting, forcing and concluding are
  operator actions; agents may read.
- **Task and run.** `orbit.task.show` gains an `experiments` field (arm, segment, digest);
  `orbit run show` prints one line per applied arm.
- **Dashboard: Analytics › Experiments.** A list with status, segment and sample progress.
  Each experiment opens to arm cards (n, primary metric with interval, guardrail status), a
  forest plot of each metric's difference from control, cumulative per-arm series on the
  shared trend ladder, the per-complexity table, health warnings (sample ratio, drift, open
  lagging outcomes), the event timeline, and the conclude action for operators.

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

## Concerns and honest limitations

- **Small samples.** Most metrics are noisy and a workspace finishes tens of tasks of one
  complexity a week. Two-arm experiments with a large expected effect are the realistic case;
  the readout will often say `inconclusive`, and it should.
- **Interference.** Arms share one codebase. A change landed by one arm (a better map, a
  fixed flake) helps every arm afterwards. Concurrent arms keep this symmetric, but it shrinks
  measured effects.
- **Crew and model mix.** The crew draw is independent of the arm, so the mix balances in
  expectation, not in a small sample. The readout shows each arm's crew mix and can cut by
  crew; crew-changing arms bypass performance weighting by design.
- **Skills are global today.** Every run on a host sees the same skill symlinks. A per-run
  skill root is real new plumbing across providers, which is why skills come after
  instructions and tools.
- **Repository-file drift.** Instructions and skills are read at each base revision, so an
  edit to an arm's file changes the treatment. Digests make this visible; they do not prevent
  it. Freezing small instruction files into the store at start is an open question.
- **Attribution of shared spend.** A pilot partition or explorer call serves several tasks;
  per-task token outcomes inherit whatever attribution the token series uses.
- **Cost of a bad arm.** A harmful arm keeps running until someone looks. Guardrails are read
  at the horizon, not continuously, unless early stopping is added (open question).

## Phasing

1. **Records and shadow assignment.** An additive migration (the store is at schema 62), lifecycle, events, `orbit.experiment.*`
   and CLI, sticky assignment at admission, claim capture of the arm, `task show` and
   `run show` lines. Overlays are recorded but not applied; the first experiment is an A/A
   test whose sample-ratio check must pass.
2. **Instructions and tools.** Live mode for the two surfaces that reuse the most existing
   code: the tag-instruction slot and the tool allowlist, on owner and claimed runs, with
   digests.
3. **Skills, crew and config keys.** The remaining surfaces, each validated at start.
4. **Readout.** The analysis endpoint and Analytics › Experiments.
5. **Decision and promotion.** Conclude with snapshot, the decision-channel prompt at the
   horizon, and the promotion task. The explore-brief trial becomes the first live experiment.

## Open questions

1. **Assignment point?** Admission is the latest point every surface can still act and the
   earliest at which a task's shape is settled. A treatment that needs earlier preparation (the
   explore brief is produced by the pilot) must then prepare for the whole population and gate
   only delivery by arm. Assigning at filing instead lets the pilot see the arm, at the cost of
   assigning tasks that are never admitted.
2. **Who may start experiments?** Operator only, or may an orchestrator create a `draft` for
   the operator to start?
3. **Early stopping?** Should a guardrail breach past a hard threshold pause the experiment
   automatically, or only alert?
4. **Decision subject?** Generalize decision requests beyond tasks, or open one tracking task
   per experiment that carries the conclusion request?
5. **Freeze instruction files?** Copy small instruction files into the store at start, which
   removes drift for them but makes the store a second home for prompt text.
6. **Explore-brief timing?** Start the explore-brief trial on tags now and migrate, or wait
   for phase 2 so its first readout uses this machinery?

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
- Concluding stores the readout snapshot and, with a winner, files one `proposed` promotion
  task; no configuration or repository file is changed by the conclusion itself.
- With `experiments.enabled = false`, admission and runs are unchanged from today.

## Task References

- [ORB-16814] — compares implement runs with and without explorer calls (observational)
- [ORB-18032] — adds the `explore-first` tag instruction for a forced-explorer trial
- [ORB-18033] — reads out the explore-first trial
- [ORB-18363] — designs explore briefs and their three-arm trial
- [ORB-18364] — charts tokens per task by complexity and tag
- [ORB-18383] — records landed diff stats per delivery

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
