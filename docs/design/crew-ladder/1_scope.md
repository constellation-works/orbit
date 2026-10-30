---
title: "Scope: crew ladder"
owner: claude
last_updated: 2026-10-30
last_validated: 2026-10-30
status: Draft
stage: designing
next: "Daniel reviews this scope; then file phase 1 (ladder config, shadow trials and the ladder report)"
feature: crew-ladder
doc_role: scope
type: design
summary: "Scope: an operator-ordered ladder of crews (model and effort) per workspace; each complexity tier continuously trials the next cheaper rung on a small share of its tasks as a system-run experiment, promotes it on a sequential non-inferiority test and keeps going until a rung fails, so each tier settles at the cheapest crew that holds its quality, with demotion on evidence"
tags: [crew-ladder, crew-pools, experiments, crew-performance, agent-scorecards, effort]
---

# Scope: crew ladder

Status: design only, nothing built. Daniel, 2026-10-30: "crew complexity assignment is based
on a feel. we have experiment feature, but that's manual. I think it should be auto-adjusted",
with a "randomness" rate at which a crew is tried above its level, its level moving up when
it does well "and eventually settle to its optimal level. Or perhaps we should add this for
'effort' level instead."

Bearing: which crews may run a tier is the operator's pool configuration
(`workflow.<tier>_complexity_crews`, `workflow.type_complexity_crews.<type>.<tier>`,
[CONFIG.md › pools](../../CONFIG.md#automatic-crew-pools-by-complexity)). Inside a pool,
[usage pressure](../usage-pressure/1_scope.md) and
[crew performance](../crew-performance/1_scope.md) scale member weights, and admission writes
the drawn crew and its `crew_source` back to the task. A crew is a model, a provider and an
optional reasoning `effort`. The
[agent scorecards](../agent-scorecards/2_design.md) score implementers per tier.
[Experiments](../experiments/1_scope.md) randomise tasks between arms, one of which can be a
crew pool, read outcomes per arm (done rate, first-pass acceptance, repairs, recoveries,
escaped regressions, `cost_usd`) and file a proposed promotion task on an operator's
conclusion. `exp-4e3e0399` (half of medium to luna, haiku and muse-spark) is the first crew
experiment, run by hand, with a verdict expected around 2026-11-18.

## Problem

- **Pool membership is set by feel.** "Haiku is a low crew", "hard needs opus" are
  judgements made once and revisited when someone notices. Crew performance re-weights
  within a pool but never changes who is in it, so a cheaper crew that could handle a tier
  never gets the chance to show it, and a member that got worse stays.
- **Experiments answer one question at a time, by hand.** Each one needs a definition, a
  start, someone to read the verdict and a promotion. The question "what is the cheapest crew
  that can do medium?" is really a sequence of experiments (can luna? then can haiku?), and
  the answer changes whenever a model is updated, which is monthly.
- **The real unit is model plus effort.** `opus·xhigh` and `opus·high` differ in cost and
  latency much as two models do. Pools name crews, so effort is already part of a crew, but
  nobody tests whether a tier needs the higher effort.

The current code shapes the fix: the experiment machinery already does the hard parts
(deterministic sticky assignment, claim capture, surface conflicts, per-arm outcomes with
cost, sample-ratio checks), and crews already carry effort. What is missing is something that
chooses the next question, decides it without an operator reading a forest plot, and acts on
the answer.

## Goal

The operator orders the workspace's crews into a ladder, cheapest to most capable. For each
complexity tier, Orbit keeps one trial running: a small share of the tier's tasks (the
exploration rate) goes to the next rung below the tier's current floor. A sequential
non-inferiority test on task quality decides the trial. A rung that holds the tier's quality
within a margin is promoted into the tier, and the next rung down is trialled. A rung that
falls short ends the descent, and the tier has settled. Promotions are proposed to the
operator by default, or applied automatically when the workspace opts in. Evidence keeps
being read after promotion, and a promoted rung that starts doing worse is demoted. Failed
rungs are retried after a while, because models change.

Unchanged: the configured pools stay the operator's and are never rewritten. Explicit crews,
purpose pools, run overrides, usage pressure, crew performance and manual experiments work as
today. A workspace without a ladder behaves exactly as now.

## Shape

### The ladder

```toml
[workflow.crew_ladder]
mode = "shadow"             # off | shadow | propose | auto
rungs = ["haiku", "luna", "sol-medium", "sol", "opus-high", "opus"]   # cheapest first
tiers = ["low", "medium", "hard"]
explore_rate = 0.10
```

- **A rung is a crew.** Effort rungs are crews that differ only in `effort`, for example
  `opus-high` (`model = "claude-opus-5-5", effort = "high"`) beside `opus` (`effort =
  "xhigh"`). One ladder covers models and effort levels together, so there is no second
  mechanism for effort.
- **The order is the operator's.** It is the cost and capability order the operator believes;
  Orbit does not infer it. A rung missing from the ladder is never trialled.
- **A tier's floor** is its cheapest rung in the effective pool (configured members plus
  learned ones, below). A tier whose pool has no ladder rung is not trialled.

Exploring "a crew above its level" and "a tier below its floor" are the same act seen from
the two ends. This design takes the tier's view because the decision (is this tier's quality
held?) and the evidence (that tier's outcomes) belong to the tier.

### Trials

One trial per tier at a time, for the rung directly below the tier's floor:

- **A trial is a standing system experiment.** Daniel, 2026-10-30: "standing experiment is
  a good idea." It is created, started and concluded by
  `system:crew-ladder` through the experiment lifecycle, so assignment, claim capture,
  `surface_conflict` exclusion, outcomes, cost and the dashboard readout are reused
  unchanged. Arms: `control` with no overlay (the tier's effective pool) and `candidate` with
  a crew overlay of the single rung, at weights `1 − explore_rate` and `explore_rate`.
  Population: the tier, minus `exclude_types` and anything an operator experiment already
  covers on the crew surface.
- **Operator experiments come first.** A tier with a running operator crew experiment gets no
  trial; its tasks are excluded through the existing one-experiment-per-surface rule. Until
  `exp-4e3e0399` concludes, medium is not trialled.
- **One rung, one tier.** A trial never skips a rung and never runs on two tiers for the same
  rung at once, so a bad rung costs at most `explore_rate` of one tier.

### The decision

A fixed-horizon verdict, which manual experiments use so that repeated looks do not inflate
false wins, would leave every trial waiting for an operator-chosen sample. A trial instead
uses a **sequential non-inferiority test**:

- **Quality per task.** A task scores 1 when it reached `done` with first-pass acceptance and
  no escaped regression of severity `medium` or worse within 14 days of landing; 0.5 when it
  reached `done` after review repairs or recoveries with no such regression; 0 otherwise.
  Tasks are counted only once their regression window has closed. The score is computed from
  the experiment's existing outcome records.
- **Margin.** `margin` (default 5 points) is how much quality the operator will trade for
  cost. The candidate is non-inferior when the control's quality exceeds the candidate's by
  less than the margin.
- **Looks.** The test looks after every `look_every` (default 10) settled candidate tasks,
  with alpha spent across looks (O'Brien–Fleming bounds, overall one-sided α = 0.05), up to
  `max_sample` (default 60) candidate tasks:
  - **promote** when the upper confidence bound of (control − candidate) is below the
    margin;
  - **stop** when the lower bound is above the margin, or at `max_sample` without promotion;
  - otherwise keep collecting.
- **Guardrails.** Any confirmed `critical` escaped regression on a candidate task, or a
  candidate done rate more than 15 points under control at any look, stops the trial at once,
  whatever the test says.
- **Cost is recorded, not tested.** The ladder's order already says the candidate is cheaper;
  the readout reports the measured cost difference so the operator can see it was.

Quality is compared within the tier, post-stratified by type where the tier mixes types, as
the experiment readout does by complexity.

### Acting on a decision

| Mode | On promote | On stop |
|---|---|---|
| `shadow` | Nothing is assigned: the system experiment runs in experiment `shadow` mode, recording which tasks would have gone to the candidate. The ladder report shows what would be trialled. | — |
| `propose` | The trial concludes with the candidate as winner, which files the experiment's usual proposed promotion task (add the rung to the tier's pool) and a decision request. The next rung is trialled only after the operator accepts. | The trial concludes with no winner; the rung is marked `failed` for the tier. |
| `auto` | The rung joins the tier's **learned pool** at once, and the next rung down is trialled. | As `propose`. |

- **The learned pool is a store record, not configuration.** It lists, per tier, the rungs
  the ladder promoted, each with the trial that promoted it. Draws read the configured pool
  plus the learned members. Orbit still never rewrites configuration or repository files.
  An operator who wants a promotion permanent adds the rung to the configured pool, and the
  learned entry becomes redundant.
- **Weight.** A learned member's weight is the weight of the tier's cheapest configured
  member, so promotion adds a peer, not a takeover. Crew performance and usage pressure then
  scale it like any member.
- **Provenance.** A draw that picks a learned member writes `crew_source:
  ladder:<tier>/<rung>`, and a trial arm writes the experiment's
  `experiment:<id>/candidate`, so the task's crew fields always say why a crew ran.
- **Reversing the experiments non-goal.** Experiments list automatic promotion and adaptive
  allocation as non-goals. `auto` reverses the first for this one case: the candidates are
  rungs the operator ordered, the change is an additive pool member held in the store, and
  it is reversible by demotion or by removing the rung. Allocation stays fixed per trial.

### Demotion

- **Learned members** are watched after promotion: their quality over the last
  `watch_window` (default 30) settled tasks in the tier, against the configured members',
  with the same margin. A member that falls below it, or has a confirmed `high` or worse
  escaped regression, is demoted: removed from the learned pool in `auto`, proposed for
  removal in `propose`. A demotion marks the rung `failed` for the tier.
- **Configured members are never removed.** The ladder reports a configured member that
  fails the same check and files a decision request; changing the configuration stays the
  operator's.

### Re-trial

A `failed` rung is trialled again after `retry_after_days` (default 30), or at once when its
crew's model or effort changes (the crew's configuration digest moves). Evidence from an
earlier model never carries over, as in crew performance.

### Configuration

| Key | Default | Meaning |
|---|---|---|
| `workflow.crew_ladder.mode` | `"off"` | `off`, `shadow`, `propose` or `auto`. |
| `workflow.crew_ladder.rungs` | `[]` | Crew names, cheapest first. Unknown or disabled crews fail at load. |
| `workflow.crew_ladder.tiers` | `["low", "medium", "hard"]` | Tiers that run trials. `xhard` is opt-in. |
| `workflow.crew_ladder.explore_rate` | `0.10` | Candidate arm weight, between 0.02 and 0.25. |
| `workflow.crew_ladder.margin` | `5` | Non-inferiority margin in quality points. |
| `workflow.crew_ladder.look_every` | `10` | Settled candidate tasks between looks. |
| `workflow.crew_ladder.max_sample` | `60` | Candidate tasks before a trial stops without promotion. |
| `workflow.crew_ladder.exclude_types` | `[]` | Task types never trialled, for example `["security", "ui"]`. |
| `workflow.crew_ladder.watch_window` | `30` | Settled tasks per learned member for the demotion check. |
| `workflow.crew_ladder.retry_after_days` | `30` | Days before a failed rung is trialled again. |

Ladder trials count toward `experiments.max_running`, and need `experiments.enabled`.

### Surfaces

- **`orbit crew ladder [--json]`** and `orbit.crew.ladder`: per tier, the configured pool, the
  learned members with the trials that promoted them, the floor, the active trial with its
  progress to the next look and its bounds, failed rungs with retry dates, and in `shadow`
  what would be trialled.
- **Experiments page.** Trials appear as experiments owned by `system:crew-ladder`, with the
  usual readout plus the sequential bounds.
- **Analytics › Crews.** A ladder panel: rungs down the side, tiers across, each cell
  configured, learned, trialling, failed or untried.
- **Decision channel.** Promotions in `propose`, demotions, and configured members failing the
  check.
- **Run and task.** `crew_source` as above; `orbit run show` names the trial for a candidate
  run.

## Non-goals

- **Adaptive allocation within a trial.** Weights are fixed per trial; the ladder adapts by
  choosing the next trial, not by shifting traffic mid-trial.
- **Upward strengthening.** Raising a tier's floor because its quality is poor (trialling a
  stronger rung) is not covered; demotion only removes learned members. A later phase may
  add it.
- **Learning the ladder order.** Orbit does not infer which crew is cheaper or stronger; the
  operator orders the rungs.
- **Rewriting configuration.** Learned members live in the store; configured pools never
  change.
- **Purpose pools.** Pilot, review, recovery and auto-fix pools are not trialled.

## Concerns and honest limitations

- **Hard tiers learn slowly.** At 10% of a tier with tens of tasks a week, a hard trial
  collects a few candidate tasks a week, plus the 14-day regression lag. A hard promotion
  takes one to three months; low and medium settle in weeks. Raising `explore_rate` trades
  risk for speed.
- **Exploration costs real work.** A candidate that cannot do the tier produces failed or
  repaired tasks until the test stops it. The one-rung limit, `exclude_types`, the guardrail
  stop and the rate cap bound the damage; they do not remove it.
- **The quality score is a choice.** Weighting repairs at 0.5 and counting only `medium`+
  regressions are judgement calls, and the margin is the operator's price for cost. Both are
  explicit so they can be argued with.
- **Reviewer effects.** First-pass acceptance is another crew's verdict, as in crew
  performance; a strict reviewer drawn more often for one arm reads as worse implementation.
  Randomisation makes this noise, not bias, but it widens the bounds.
- **Model changes.** A provider updating a model mid-trial invalidates the evidence. The
  crew's configuration digest catches a changed model name; a silent provider-side change is
  not detected.
- **Interaction with crew performance.** A newly learned member starts at neutral
  performance, so it gets its configured-peer weight until evidence moves it.

## Phasing

1. **Ladder config, report and shadow.** The `workflow.crew_ladder` keys and validation,
   `orbit crew ladder`, and `shadow` mode: system experiments in experiment shadow mode, so
   the report shows which rung each tier would trial and which tasks would have gone to it.
   Nothing changes for any task.
2. **Trials with `propose`.** Live system experiments, the quality score, the sequential test
   with its looks and guardrail stops, promotion and stop handling, failed rungs and re-trial,
   and decision requests. Promotion goes through the existing promotion task.
3. **`auto` and demotion.** The learned pool record, draws that include learned members with
   `ladder:` provenance, the demotion watch, and the Analytics › Crews ladder panel.
4. **Later.** Each filed only when needed: upward strengthening, per-type ladders (trials per
   type pool), `xhard` trials by default, and purpose pools.

## Open questions

1. **Quality score.** Is 1 / 0.5 / 0 the right scale, and should a `low` regression count?
2. **First slice: effort or models?** Starting with effort rungs only (`opus-high` under
   `opus` in hard, `sol-medium` under `sol` in medium) is the safest first trial: same model,
   lower cost, quick evidence. Should phase 2 start there before cross-model rungs?
3. **Learned weight.** Should a learned member start at its peers' weight, or ramp up over
   its first watch window?
4. **`xhard`.** The pilot never demotes out of `xhard`. Should the ladder ever trial there,
   or is `xhard` always the operator's?

## Acceptance (feature level)

- A ladder naming an unknown or disabled crew fails to load naming it; a workspace without a
  ladder draws exactly as before.
- In `shadow`, `orbit crew ladder` shows for each tier its floor and the rung it would trial,
  and no task's crew changes.
- In `propose` or `auto`, each listed tier runs at most one trial, for the rung directly below
  its floor, at `explore_rate`, and excludes tasks of `exclude_types` and tasks an operator
  crew experiment covers.
- A trial promotes only when the sequential bound is inside the margin at a look, and stops on
  the bound, at `max_sample`, or at once on a guardrail breach; every look's bounds are
  recorded.
- In `propose`, a promotion files a proposed promotion task and a decision request and does
  not change any draw; in `auto`, it adds the rung to the tier's learned pool, later draws can
  pick it with `crew_source: ladder:<tier>/<rung>`, and the next rung is trialled.
- A learned member that falls below the margin over its watch window, or has a confirmed
  high-severity escaped regression, is demoted and its rung marked failed; a configured member
  is only reported.
- A failed rung is trialled again after `retry_after_days`, or when its crew's model or effort
  changes.

## Task References

No tasks yet. The reused machinery is in the [experiments](../experiments/1_scope.md) and
[crew performance](../crew-performance/1_scope.md) designs.
