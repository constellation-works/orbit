---
title: Experiments — Design
owner: claude
created_date: 2026-10-25
updated_date: 2026-11-28
last_validated: 2026-11-21
status: Draft
feature: experiments
doc_role: design
type: design
summary: "Design: operator-defined experiments whose arms overlay instructions, skills, tools, crews, allowlisted config or crew memory on a task's runs; sticky random assignment at admission, durable SQLite records, per-arm outcome analysis with intervals, and an operator decision that files the promotion"
tags: [experiments, admission, tag-instructions, crew-pools, analytics, decisions]
---

# Experiments — Design

This doc holds the mechanisms behind [1_overview.md](./1_overview.md), which states the problem and the goal.

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
| Skills | Replace a named skill with a bundle directory in the repository | Invocation-local Claude skill root on Unix; control keeps global links, other providers refuse | Bundle exists and parses at the base; provider supports isolated replacement |
| Tools | Add tools to, or remove optional tools from, the activity baseline | Adding reuses `admit_required_tools`; removal applies a per-run denial after `resolve_activity_tool_grant` | Tools exist and are agent-facing; a run-bound tool (`orbit.memory.*`) cannot be added ([ORB-26553]); a tool the activity contract requires cannot be removed |
| Crew | A crew pool (members and weights) for the arm | The admission crew draw, as the run-level `<complexity>_complexity_crews` override does today; the arm's pool skips performance weighting | Crews exist and may run the task's complexity |
| Memory | `roles` and `global`: deliver the drawn crew's model memory space (and the reviewed global memory) as data ([crew memory §5](../crew-memory/2_design.md#5-delivery-the-memory-overlay-surface)) | Resolved after the crew draw into the admission capture; a `crew_memory` envelope field plus one fixed precedence sentence | A running memory arm switches the retained machinery on; [ORB-26501] retired its external surfaces ([crew memory §1](../crew-memory/2_design.md#1-experimental-gate)) |
| Config | Values for allowlisted keys only | `orbit_config::ExperimentConfigKey` admits `workflow.explore.max_calls_per_run` (integer 1–100) and `visual_review.reviewer` (boolean); both are inert and kept so historical experiments still validate | Key is on both the code list and the workspace allowlist |

Each overlay entry names the roles it applies to; the default is `implement` only. Review and
recovery overlays are allowed but are opt-in per entry, because a reviewer that shares the
implementer's treatment stops being an independent check of it. [ORB-19982] added the
`visual_review.reviewer` key, which controlled visual review's mock comparison reviewer
regardless of an entry's declared roles; [ORB-22524] documented that exception. Visual
review was removed in [ORB-22807]: the key is retained only so historical experiments
still validate, and it has been ignored since.

**Explorer removal:** [ORB-20383] removes the explorer agent, invocation tool
and live configuration. The legacy experiment config key has no effect;
historical explorer uptake fields remain readable for concluded experiments.

### Assignment

1. **When.** At a task's first delivery admission on the owner, before the crew draw (an arm
   may set the crew). The population filter (complexity, type, tag include/exclude, task
   origin) is evaluated against the task at that moment.
2. **How.** For each running experiment whose population matches, in creation order:
   a `seeded_crew_ticket`-style ticket from `sha256("<salt>:<experiment_id>:<task_id>")`,
   walked over the arms' cumulative weights in the current segment. The draw is deterministic,
   so any readout can recompute it, and no one can choose a task's arm by timing. (The run
   admission crew draw itself uses unseeded `random_crew_ticket`; that stays as is.)
3. **Historical enrollment and admission authority.** The assignment row is immutable:
   arm, segment, digest, assignment time and forced flag remain available to analysis. Fresh
   admission evaluates whether that historical treatment still has execution authority.
   Concluded and abandoned experiments apply no overlay to a fresh delivery and enroll no
   new tasks. Paused experiments and retired arms retain their existing-assignment behavior.
   Already admitted owner runs, same-task children and owner-issued follower receipts retain
   their captured treatment on resume, even after conclusion. This replaces the earlier rule
   that kept terminal experiment treatment active until the task itself became terminal;
   [ORB-21202] implemented this admission-authority change.
4. **One experiment per surface per task.** A task can be in several experiments only when
   their overlays touch disjoint surfaces. A later experiment that conflicts records the task
   as excluded (`surface_conflict`) rather than assigning it.
5. **Operator authority.** An explicit run crew wins for that fresh run. A later explicit
   task crew supersedes historical crew enrollment, including a forced assignment. A force
   made after a task pin wins until another audited task crew change; assignment and
   `crew_assigned` timestamps determine ordering, with force winning an exact tie. An explicit
   pin with no recoverable timestamp supersedes ordinary enrollment but does not displace
   a timestamped force. The row still records `forced = true` and analysis leaves it out.
   A crew override suppresses the entire crew-bearing arm for that admission, avoiding a
   partial treatment; the frozen capture records `live: false`, and `crew_selection` records
   the intended overlay digest, actual crew and suppression. Historical rows are untouched.
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

Each arm also reports `tasks_with_explorer_call`: the number of analysed
enrolled tasks that recorded at least one explorer-delegation call on an
implement invocation at or after assignment. A call matches when its stored
name is `orbit.agent.invoke` or `orbit_agent_invoke`, compared
case-insensitively as the full name or as the tail after the last `__`, so
an MCP tool such as `mcp__orbit__orbit_agent_invoke` counts. A task counts
once. The readout reads those stored tool-call records and does not make a
delegation call. `orbit experiment readout` JSON and `orbit.experiment.readout`
both serialize the field; the schema version stays 1. An invocation whose
provider output outgrew the bounded capture records only the suffix of its tool
calls and persists `tool_calls_incomplete` ([ORB-20572]). A task whose first
implement invocation is incomplete has no first-edit score, and a task with no
recorded explorer call but an incomplete implement invocation at or after
assignment counts in the arm's `tasks_explorer_unobserved`, not as a confirmed
absence. The readout adds a `tool_trace_incomplete` warning for the first-edit
case and an `explorer_trace_incomplete` warning for the explorer case. A later
truncated invocation leaves a complete first invocation's first edit measured
([ORB-20590]).

`first_pass_acceptance` and `review_repairs` are review-derived: they read a
task's typed review gate settlements in its history, decoding settlement
comments only for records written before that event. `review_repairs` counts
a task's `accept_with_fixes` settlements, and that count (zero included) is
measured only for a task with at least one settlement; a terminal task with no review
gate settlement is `unmeasured` for `review_repairs`, not a measured zero,
because no review happened. A non-terminal task stays open. [ORB-24133] updated
these outcomes for typed settlements; [ORB-23879] added the unmeasured rule and
the warnings below.

When the primary metric is unmeasured on every
settled task in every active arm, the readout carries a
`primary_all_unmeasured` warning naming the metric and the settled-and-
unmeasured count per arm; for a review-derived metric with no settlement on
any enrolled task it also states that no enrolled task has a review gate
settlement. The warning appears in the JSON readout's `warnings` and in the
human readout. It is absent when at least one active arm has a measured
primary outcome, and absent while every enrolled task is still open.
Starting an experiment whose primary metric is review-derived in a workspace whose effective review policy runs no review
gate (`review.timing = "off"` with no before-landing hosts) prints the same
reason as a warning naming the metric and the policy, records it in the
started event's `warnings`, and still starts the experiment: a drain can add
a review layer for its window (`run auto --review`), so the policy at start
does not settle it.

`cost_usd` uses each invocation's valid provider-reported dollars first, then
its token-derived estimate from the shared, versioned configuration data in
`crates/orbit-common/assets/model_prices.yaml`. Rates are keyed by model and
effective date, with provider input/cache semantics handled by shared accounting;
the experiment has no provider-specific prices or pricing branches. A task is
measured only when **every** linked invocation can be priced. No invocations,
an unknown model, an invalid token split, or totals over a prompt-size tier
threshold without per-request usage, each without reported cost, makes its
whole cost unmeasured; partial sums never enter estimates, differences or the
cumulative series. A valid reported zero remains measured.

For the `cost_usd` metric, each arm's `cost_coverage` reports settled task counts:
`reported` when every invocation reports cost, `estimated` when at least one
uses the price table and all are priced, and `unmeasured` otherwise. Reported
plus estimated equals the metric's measured `n`; unmeasured matches its
`unmeasured` count. In-flight tasks stay in `open`, outside these counts.
The CLI prints coverage even with no measured costs, and the dashboard's Trust
tab shows it in per-arm evidence regardless of the primary metric. Models absent
from the shared table remain unmeasured until pricing data is available; they are never assumed free.
Cost and coverage are recomputed on read from the raw invocation records.

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
- **Treatment delivery.** Intention-to-treat counts a task in its arm whether or not the
  treatment reached it, so a treatment whose overlaid step fails on every run would be judged
  on control's work ([ORB-20597]: every explorer call in exp-2b6d910d's treatment arm failed,
  yet its primary interval excluded zero). Each treatment arm's overlay names its dispatch
  roles: the roles of its instructions, skills, tools and memory. Crew and config surfaces act at
  admission or through settings. A crew overlay additionally requires positive evidence from
  the latest delivery's frozen admission selection: the experiment source and overlay digest,
  with no crew deviation or suppression. Choosing the same crew explicitly is not treatment.
  Legacy or remote crew delivery without that evidence is unconfirmed, not counted as delivered.
  Config-only and empty overlays have no delivery check (`delivery: null`). A task's treatment
  is **delivered** when,
  for every dispatch role, a run of the task has an `experiment_overlay_applied` audit row for
  this experiment's arm in that role whose step's last `step_finished` outcome in the same run
  is `success`. The evidence is the durable run audit trail, never agent output, and the rule is
  read from the overlay definition with no per-experiment or per-activity branch. A later
  admission that suppresses the arm cannot borrow an earlier run's dispatch evidence. A task not
  delivered and still in flight is `pending`. Each arm reports `delivery` (`roles`, `assigned`,
  `delivered`, `pending`, `rate` = delivered / (assigned − pending)). When any arm's rate is
  below 0.9 (fixed since [ORB-22808]), or the horizon is reached with none of
  its tasks settled, the verdict is `withheld` with the reason
  ``arm `<arm>` treatment delivered on <n>/<m> runs`` and an `undelivered` warning, from the
  first settled task on and over any would-be `winner` or `no difference`. At or above the
  threshold the verdict rules are unchanged. Delivery proves the overlaid step ran and
  succeeded, not that the agent acted on the instruction; explorer uptake
  (`tasks_with_explorer_call`) remains the check for that.
- **Per-protocol view.** Beside the intention-to-treat metrics, `per_protocol` (labelled
  `per-protocol (delivered runs only)`) reports the primary metric with each checked treatment
  arm reduced to its delivered tasks; control and unchecked arms keep all theirs. When the two
  views disagree, the treatment's effect depends on whether it was delivered. The verdict is
  read on intention-to-treat only.
- **Verdict.** A treatment arm with a delivered rate below 0.9 gets verdict `withheld` before the horizon, even when its primary comparison
  would otherwise be `winner` or `no difference`. If no delivery or sample-ratio check withholds
  the verdict, it remains `collecting` until every arm reaches `min_sample` or `max_days` passes
  (fixed horizon, so repeated looks do not inflate false wins). Then: `winner <arm>` when the primary
  metric's interval excludes zero in the better direction and no guardrail's point estimate is
  past its threshold; `no difference`; or `inconclusive` when a guardrail interval straddles
  its threshold or the primary comparison lacks an interval (no settled outcomes,
  fewer than two values per arm in a used complexity cell, or no common measured
  cell). Missing evidence cannot establish `no difference`; a treatment with a
  complete winning comparison can still win.
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
[human decision channel](../human-decision-channel/1_overview.md) instead of waiting to be
noticed. Decision requests are task-bound today (`orbit_types::task::DecisionRequest` carries
a `task_id`; `DecisionGate::AgentRequest` is the agent-raised human question), so this
needs either a subject that is not a task or a tracking task per
experiment (open question).

### Configuration

Policy only. Experiment definitions are store records.

| Key | Default | Meaning |
|---|---|---|
| `experiments.enabled` | `false` | Off: no new assignments; running experiments keep their rows and in-flight tasks keep their arms |
| `experiments.config_keys` | `[]` | Allowlisted config keys arms may override in this workspace; each must also be on the code list |

At most 3 experiments run at once in a workspace. `experiments.max_running` and
`experiments.min_delivered_rate` set this limit and the 0.9 delivered rate until [ORB-22808]
fixed them at those defaults.

### Surfaces

- **Tools and CLI.** `orbit.experiment.{create, update, clone, start, pause, resume, reweight,
  assign, conclude, abandon, delete, show, list, readout}` and the matching `orbit experiment` verbs.
  `update` works only in `draft`. Creating, starting, reweighting, forcing and concluding are
  operator actions; agents may read. [ORB-20370] added operator-only deletion: a draft,
  concluded or abandoned experiment can be deleted only when it has no concluded arm and
  never assigned a live treatment. Rows, arms, events and assignments are removed together.
- **Task and run.** `orbit.task.show` gains an `experiments` field (arm, segment, digest);
  `orbit run show` prints one line per applied arm.
- **Dashboard: Analytics › Experiments.** A list with status, segment and sample progress.
  Each experiment opens to arm cards (n, primary metric with interval, guardrail status), a
  forest plot of each metric's difference from control, cumulative per-arm series on the
  shared trend ladder, the per-complexity table, health warnings (sample ratio, drift, open
  lagging outcomes), the event timeline, and the conclude action for operators. Operators
  also create a draft, edit a draft and start one there, through the same core lifecycle as
  `orbit.experiment.create`, `update` and `start`; [ORB-24173] added pause and resume. [ORB-22359] organized the detail around a
  decision band and three tabs: Result holds arm cards and primary/guardrail differences;
  Trust holds health, delivery, crew mix, cumulative series and the per-complexity table;
  Setup holds the definition and timeline. [ORB-22445] separated record and readout loading:
  the header, Setup and Conclude remain usable while Result and Trust load or offer Retry.
  A failed refresh retains the previous readout with a stale notice and its own Retry in
  each tab; [ORB-22475] repaired that notice in Result.

## Concerns and honest limitations

- **Small samples.** Most metrics are noisy and a workspace finishes tens of tasks of one
  complexity a week. Two-arm experiments with a large expected effect are the realistic case;
  the readout will often say `inconclusive`, and it should.
- **Interference.** Arms share one codebase. A change landed by one arm (a better map, a
  fixed flake) helps every arm afterwards. Concurrent arms keep this symmetric, but it shrinks
  measured effects.
- **Crew and model mix.** Tasks without an explicit crew draw from the arm's configured
  pool: its overlay pool when set, otherwise the ordinary pool. Different pools can produce
  different mixes by design; within a pool, weighted draws balance only in expectation. The
  readout shows each arm's crew mix and can cut by crew; crew-changing arms bypass
  performance weighting by design.
- **Skill discovery differs by provider.** Control runs retain the shared skill links.
  Treated Claude invocations use a private discovery root; the other provider families
  refuse skill overlays until they have an adapter that can replace a name in isolation.
- **Repository-file drift.** Instructions and skills are read at each base revision, so an
  edit to an arm's file changes the treatment. Digests make this visible; they do not prevent
  it. Freezing small instruction files into the store at start is an open question.
- **Attribution of shared spend.** A pilot partition or explorer call serves several tasks;
  per-task token outcomes inherit whatever attribution the token series uses.
- **Cost of a bad arm.** A harmful arm keeps running until someone looks. Guardrails are read
  at the horizon, not continuously, unless early stopping is added (open question).

## Phasing

1. **Records and shadow assignment.** Done ([ORB-18473] built the records, lifecycle and
   `orbit.experiment.*` tools; [ORB-18474] added sticky shadow assignment at admission,
   captured into claims). An additive migration (`crates/orbit-store/src/driver/sqlite/host/schema/experiments.rs`), lifecycle, events, `orbit.experiment.*`
   and CLI, sticky assignment at admission, claim capture of the arm, `task show` and
   `run show` lines. Overlays are recorded but not applied; the first experiment is an A/A
   test whose sample-ratio check must pass.
2. **Instructions and tools.** Done ([ORB-18475] made instruction and tool overlays live on
   owner and claimed runs). Live mode for the two surfaces that reuse the most existing
   code: the tag-instruction slot and the tool allowlist, on owner and claimed runs, with
   digests.
3. **Skills, crew and config keys.** Done ([ORB-18476] added crew and allowlisted config-key
   overlays; [ORB-18477] added the per-run skill overlay; [ORB-23114] added the memory overlay). The remaining surfaces, each
   validated at start.
4. **Readout.** Done ([ORB-18478] built the per-arm readout; [ORB-18479] built the
   Analytics › Experiments dashboard page; [ORB-19368] added create, edit and start there;
   [ORB-24173] added pause and resume).
   The analysis endpoint and Analytics › Experiments.
5. **Decision and promotion.** Done in part ([ORB-18480] made conclude store the readout
   snapshot and file one promotion task). Conclude stores the readout snapshot and files one promotion
   task. The decision-channel prompt at the horizon remains open: decision requests are
   task-bound today. Live rollout is no longer pending: [ORB-19390] repaired enrollment
   for the first running crew experiments. That rollout does not complete the horizon prompt.

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

## Task References

- [ORB-18364] — charts tokens per task by complexity and tag
- [ORB-18383] — records landed diff stats per delivery
- [ORB-18473] — built experiment records, lifecycle and the `orbit.experiment.*` tools (phase 1)
- [ORB-18474] — added sticky shadow assignment at admission, captured into claims (phase 1)
- [ORB-18475] — made instruction and tool overlays live on owner and claimed runs (phase 2)
- [ORB-18476] — added crew and allowlisted config-key overlays (phase 3)
- [ORB-18477] — added the per-run skill overlay (phase 3)
- [ORB-18478] — built the per-arm readout with intervals and the sample-ratio check (phase 4)
- [ORB-18479] — built the Analytics › Experiments dashboard page (phase 4)
- [ORB-18480] — made conclude store the readout snapshot and file the promotion task (phase 5)
- [ORB-19368] — added create, edit and start for experiments on the dashboard page (phase 4)
- [ORB-19390] — enrolls eligible tasks after experiments are enabled while a drain is already running
- [ORB-19982] — added the live visual-review experiment and its reviewer config overlay
- [ORB-20370] — added guarded deletion of experiments that never applied a live treatment
- [ORB-20383] — removed explorer tooling and reconciled affected experiment surfaces
- [ORB-20572] — records incomplete tool traces and excludes them from first-edit measurements
- [ORB-20590] — separates later retry trace warnings from first-invocation first-edit measurement
- [ORB-20597] — withholds the verdict when a treatment arm's overlay was not delivered, and adds
  the per-protocol view
- [ORB-21202] — stops ended experiments overriding fresh admissions and honors later operator pins
- [ORB-22359] — organizes experiment details around the decision and Result, Trust and Setup tabs
- [ORB-22445] — loads the experiment record independently and reduces readout cost
- [ORB-22475] — preserves a stale-readout notice and Retry in each result-bearing tab
- [ORB-22524] — documents both typed config keys and the visual-review role exception
- [ORB-22808] — fixed the experiment limits at their former defaults.
- [ORB-23114] — added the crew-memory experiment overlay and receipt capture.
- [ORB-23879] — added primary-metric unmeasurability warnings and the review-repairs measurement rule.
- [ORB-24133] — changed experiment outcome documentation to typed review settlements.
- [ORB-24173] — added dashboard pause and resume for experiments.
- [ORB-26553] — refused run-bound memory tools in experiment tool additions.
- [ORB-26501] — retired crew memory's external surfaces while retaining experiment machinery.
- [ORB-22807] — removed visual review: the `visual-review` template, the visual metrics, and the
  effect of `visual_review.reviewer`, which stays allowlisted for historical experiments

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
