---
title: Agent Scorecards — Design
owner: claude
last_updated: 2026-11-10
last_validated: 2026-11-08
status: Draft
feature: agent-scorecards
doc_role: design
type: design
summary: Target design for the Agents dashboard view under Analytics; covers problem attribution, the shared score, per-role terms, the API, CLI and MCP contract, scan budget and delivery tracks.
tags: [agent-scorecards, dashboard, analytics]
paths: ["crates/orbit-core/src/application/agents/**", "crates/orbit-web/src/api/agents.rs", "crates/orbit-web/src/runtime_memo.rs", "crates/orbit-common/src/governance/agents.rs", "crates/orbit-cli/src/command/agents.rs", "crates/orbit-core/src/application/codebase/pilot.rs", "crates/orbit-core/src/application/codebase/lock_waits.rs", "crates/orbit-core/src/application/codebase/facts/**", "crates/orbit-web/assets/dashboard/**"]
related_features: [user-interface, review-gate, finding-verification]
related_artifacts: []
---

# Agent Scorecards — Design

This document is the target design for the Agents view under Analytics: one scorecard per agent role, ranked by model and recorded reasoning effort. It is a **Draft** that becomes the description of current behaviour as the delivery tracks in §10 land. It fixes the attribution rules, the shared score, each role's terms, and the API payload first, so that every track builds against a stable contract.

The core, dashboard and role tracks have landed. Sections marked **Current** describe the code: the destination and layout (§1), the ranking unit (§2), attribution (§3), the shared score (§4), the Implementers, Reviewers and Pilots roles (§5.2–5.4), the API (§6), the CLI and MCP (§7) and the scan and cache (§8). Everything else is the target. The view itself is described in [user-interface 2_design.md §5c](../user-interface/2_design.md#5c-analytics--agents).

## 1. Destination and Layout

**Current**.

**Agents** is an Analytics view, listed in the Analytics sub-nav beside Metrics, Reliability, Codebase, Resources, Forecast, Cost and Experiments; there is no standalone Agents rail destination. Its secondary tabs are **Rank** (`#analytics/agents/rank`, the default, the implementer model ranking of the [model ranking design](../model-ranking/2_design.md)) and then the roles: **Implementers** (`#analytics/agents/implementers`), **Reviewers** (`#analytics/agents/reviewers`) and **Pilots** (`#analytics/agents/pilots`). Each role tab shows its count of ranked rows. Reopening Analytics from the rail returns to the remembered Analytics view, including Agents with its remembered role. Navigation changes:

- Old `#agents/<role>` links open `#analytics/agents/<role>` in place, keeping the window, so Back is not trapped. The Orchestrators tab is retired: `#analytics/orchestrators`, `#analytics/agents/orchestrators` and `#agents/orchestrators` open Rank.
- The Task pilot tile and its section leave Analytics › Codebase. The Codebase view keeps its path rankings, change coupling and lock contention.

Every role tab has the same layout:

1. **Ranking.** One row per model and recorded reasoning effort, labelled `model @ effort`: rank, the row label with its contributing crews as secondary detail, support (the role's unit of work), four or five role-specific columns, and the score as a bar and number. Ranks are dense over the ranked rows: an unranked (below-support or unjudged) row never consumes a rank, and tied band ranks on the model-ranking board advance by one band, never by the band's size, so no rank number is skipped. A row is a button that selects it. Rows below the minimum support are greyed, tagged `below 10` and listed last. An unjudged row (§4.3) is greyed, tagged `unjudged`, shows `unjudged` instead of a score and listed after them.
2. **Detail** (right column) for the selected row: rank, score, raw score and the fleet average it was pulled toward, an **Agent profile** radar comparing its rates with the ranked fleet (§1.1), a **Score breakdown** (each term's weight, contribution and bar), and **Shown, not scored** (cost, time and other context).
3. **One role-specific panel**, for the roles that have one. Implementers show **Regression rate by change size**, a display-only view of regression rate by landed change size, one curve per crew reference as dispatched, served by the size-curves route (§6). Models recorded under one crew reference pool into that reference's single curve and appear as metadata, so the `opus@medium` and `opus@high` crew references stay separate curves; the ranking above stays one row per model and recorded effort. The score-against-cost chart and first-reviews tier comparison were removed as a product decision and stay removed, so cost appears only under **Shown, not scored**. Reviewers show how each reviewer's verdicts turned out, and flag an accept rate well above peers. Pilots show assessed tier against observed tier.
4. **How the score works.** A disclosure listing every term with its weight and definition, plus the smoothing formula.
5. **Who each problem counts against.** A collapsed disclosure: one bar across the window's problems, split by owning role plus a **Not scored** segment, with the current role highlighted and each role's legend entry linking to its tab. The retired **Orchestrators** owner has no tab and no link, and no problem counts against it now (§5.1 moves its problems to **Not scored**), so its segment and legend entry appear only if the payload counts a problem against it. This strip is how a reader sees the one-owner rule at work.

Reviewers also carry a **provisional** banner above the ranking while any review in the window is younger than the 30-day miss lookback (§4.4). It counts those reviews against the window's total. A reviewer whose accept rate is well above its peers' carries an `accepts often` tag under its name in the ranking (§5.3). A role module supplies the banner and the row tags through optional hooks beside its panel.

The view opens on a count window, `last 500`, and offers `last 1000` beside the calendar windows 7d and 30d. Other shared windows (1h, 24h and all) fall back to `last 500`, which stays selected while the panel follows its own count. A calendar tab hands the view back to the shared window. The panel header names the count, with the date it reaches back to behind its info hint. It is read-only and scoped to the selected workspace; the aggregate view shows the usual placeholder. It builds HTML with `el()` and the radar with inline SVG. The dashboard is dark-only. The layout keeps its ranking table in an `overflow-x` box and stacks the detail column under the ranking at phone width.

### 1.1 Agent profile radar

Every role's detail column places an inline SVG radar between the score header
and **Score breakdown**. It compares the selected model-and-effort row with the fleet in
the same role and window. Each axis is the row's percentile in the ranked
fleet, so the chart and the ranking tell the same story; the per-axis weight
and signed contribution versus the fleet median (below) explain why a row
with some inward axes still ranks where it does. It does not reconstruct
the smoothed final score or the implementers' tier-adjusted expected counts:
the **Score breakdown** beside it carries those exact contributions.

- Each `scorecard.terms` entry is an axis, using `row.per_100[key]`. The
  denominator is `row.term_units[key]` when present, otherwise `row.units`,
  labelled with the term's `unit` or the role's `unit`. Credit axes point out
  for higher rates; negative-weight axes invert and say **fewer is better**.
- Each axis scales independently by the row's percentile among ranked rows
  with supported evidence for that term: the share of supported rates below
  the row's rate, plus half the share equal to it, inverted for a penalty.
  A rate equal to the fleet median uses radius 0.5. One extreme outlier
  occupies only the outer percentile steps, so it cannot stretch a span and
  squeeze every normal crew toward the good or bad end, and different rate
  magnitudes do not dominate. A constant fleet axis sits at 0.5 for every
  supported value. These are relative positions, not absolute quality
  targets.
- The contrasting dashed polygon is the unweighted median of those same
  supported ranked rates on each axis. With an even population it averages
  the two middle rates. Scaled by the percentile rule it always plots on
  the middle ring (radius 0.5) on every axis, regardless of outliers. An
  agent with more units has no extra weight here. The selected polygon has
  a solid stroke and fill; medians and scales refresh whenever the
  scorecard rows change.
- Roles with fewer than five scored terms add count-valued **Shown, not
  scored** axes, stopping at five. The fixed preference order is unclassified
  rework/regressions, dropped initiative, unknown origin, infrastructure
  rework, no-cause rework, and unclassified regressions. Their rate is `100 × stat.value / row.units`, with fewer
  outward; dashed spokes, italic labels and **unscored** identify them. Hours,
  dollars, percentages, structured stats and already-scored keys are excluded.
- A row below `min_support` has a muted chart and **Too little evidence to
  compare** note. Unsupported terms (`provisional_terms`, zero units or an
  own denominator below `warning_min_support`) are gaps. An implementer
  row whose `reviewed_units` is 0 has no reviewed denominator, so its
  review-derived axes (`first_pass_accepts`, `review_rejects` and
  `reviewer_repairs`) are gaps with the reason **no reviewed deliveries**,
  never a centre (worst) or rim (best) zero. `reviewed_units` is a
  count of the row's tasks with a settled first review and the scoring
  denominator for those three terms. With none, API rates are null and contributions are zero. In the
  ranking, a review column that no row can score leaves the table and one note
  above it names the hidden terms and why; a column some rows can score keeps its
  values and marks the rest with a compact **—** (title **no reviewed
  deliveries**). Measured zeros stay values.
  A positive reviewed count below `review_min_support` (30) is rated and
  shown but provisional, so those axes are gaps too (§5.2); zero is
  unmeasured. Reviewers' pending
  `provisional` reviews make the missed-regressions axis provisional, while
  their other observed terms remain available. An explicitly unmeasured
  missed-scope-block source is also a gap. Unsupported rates never enter the
  fleet reference. If no fleet rate is supported, that axis has no comparison.
  Missing vertices use hollow markers at the outer guide, with the reason in
  their disclosure; these markers have no radial value. Lines connect only
  adjacent available vertices and incomplete polygons have no fill.
- Numbered spokes map to full axis labels beneath the plot at narrow widths.
  Hovering a vertex or its numbered endpoint, focusing or clicking it
  discloses its term, agent rate,
  fleet median rate, its weight, its signed contribution versus the fleet
  median (`weight × (agent rate − fleet median rate)`; unscored axes and
  gaps carry no contribution), raw count, denominator and evidence state.
  The SVG has an accessible name and a visually hidden table carries the
  same numbers, with weight and contribution versus the fleet median as
  columns. No chart library is needed; the implementers' `reviewed_units`
  and `term_units` fields name the review denominator.

## 2. Ranking Unit

**Current.**

A row is a **model and recorded reasoning effort**, labelled `model @ effort`, for implementers, reviewers and pilots. The crews whose units the row groups are secondary detail (`crews`, with `crew` kept as the first of them so a detail selection by crew still resolves; `row_effort` / `--effort` / the `effort` query parameter narrows an ambiguous crew-and-model selection to one row). A crew name never implies an effort: bare crews set their own effort in configuration, that configuration changes over time, and `workflow.auto_effort` bands choose effort per task, so effort is read only from recorded invocation telemetry, never from crew configuration at scan time.

- **Grouping key.** The key is `(model, reasoning_effort)` in exactly the encoding `agent_effort_usage` uses for the model ranking's effort split, so the scorecards and the model ranking (model-ranking design) never disagree on what a model-and-effort population is: a rendered effort is its wire name (`low`, `medium`, `high`, `xhigh`, `max`); a present dispatch identity with no rendered effort is `null` and is labelled **provider default**; and a historical invocation with no dispatch identity is `unknown` unless the backfill below recovers one. Units with unknown effort form a separate **effort unknown** row per model and are never folded into a known effort. Dispatch identities have been recorded at launch since 2026-10-31T01:48Z. The model ranking still pools its board rows by model alone; its `effort_split` and these scorecard rows share this one key and encoding.
- **Per-unit attribution rule.** Effort is attributed per unit of work from that unit's own invocations: the invocations of the unit's job run whose activity matches the role (implementing for an implementation attempt, reviewing for a settled review, piloting for an assessed task), narrowed to the unit's own round by the model it recorded and, when the producing step or worker is known (a pilot receipt's worker, a review settlement's gate), by that exact activity id, and for a later review settlement by ending after the previous settlement on the task, preferring invocations linked to the unit's task. A run carrying several rounds (an initial and a repair pilot, a before-PR and a before-landing review) never lends one round's effort to another round's model. A review-job filing attempt — and its REJECTED-finding charge — is the exception: the filing task runs as a task executor, so its effort comes from the filing task's executor invocation (`implement_one` / `agent_implement`), never from a review (`*review*`) invocation in the same run, matching `MainActivity::ReviewJob` in `effort.rs`. A unit whose matching invocations ran at more than one effort counts under the effort of its **main invocation** — the matching invocation with the longest recorded duration, ties broken by the latest end time then the highest invocation id. When no completed invocation of the unit's run matches — the agent launched but its invocation never completed (the run failed, was interrupted, held or cancelled) — the unit counts under the effort of that run's launch record (`agent_executions`) for the same activity and model, narrowed to the unit's own round by the same activity-id and settlement bounds; when several launch records match, the latest one (by launch time, then launch id) decides. A launch record for another model or another activity in the same run is never used, and a unit with a completed matching invocation is attributed from that invocation exactly as before. A unit with neither a matching invocation nor a matching launch record (no recorded run, or a run that recorded neither of its own round) counts as `unknown`; a unit with no recorded run never borrows a same-task invocation from another attempt or run. Charges and landed credit follow the row of the run they charge or credit, by the same rule.
- **Backfilled identities.** An invocation that completed before dispatch identities were recorded can get one from `orbit migrate --backfill-dispatch-effort`, an operator-run, one-shot store workflow (never run when a store opens) that each host runs against its own store [ORB-23823]. Every launch since about 2026-09-10 is audited as a `cli.invocation.started` event whose redacted argv carries the effort flag Orbit rendered (`--effort X` for Claude, `model_reasoning_effort="X"` for Codex, `--reasoning-effort X` for Grok and Muse); since 2026-10-29 the event also records the resolved effort directly. For an invocation with no identity, the workflow takes the events of the same workspace, run and model that started within a minute of the invocation's end minus its duration; where they all render one effort — from the recorded effort, the argv, or both agreeing — it records that effort (the provider default, `null`, when the argv carries no effort flag). An event with no argv but a recorded effort is evidence; an event with neither is none, never the provider default. The identity's crew is the run's resolved crew only for the run's executor invocation (`implement_one` / `agent_implement`) and is absent otherwise. A backfilled identity reads exactly like a dispatch-time one, so its units join the matching `model @ effort` row; its stored record also carries a `backfill` provenance (source event type, matched event ids, time recorded) that a dispatch-time identity lacks. Invocations that already have an identity are never changed, and a rerun changes nothing. The same workflow also records a launch record for a run whose invocation never completed (no invocation exists to carry an identity): the launch is attributed to the one step whose `step.started`/`step.finished` audit interval contains it, and the scan's launch-record rule above then attributes the unit; a launch no single step provably contains, or whose launches disagree, is left unrecorded. What stays **effort unknown**, by population: (1) invocations with no matching launch event, including launches that predate the 2026-09-10 argv audit and the 2026-10-29 recorded effort, so they carry no readable effort source; (2) invocations or launches whose matching launches disagree on effort or carry an effort this binary cannot read; (3) launches no single step of their run provably contained, and cancelled or interrupted runs with no surviving `cli.invocation.started` event — `cli.invocation.activity` events carry only a provider, a byte count and a message excerpt, never an argv or an effort, so they cannot recover one; (4) attempts a follower ran whose follower reported no effort (the owner holds no invocation, launch or audit for them; see Pulled attempts in §11), including runs absent from this store entirely; and (5) units with no recorded run. A `model unknown` row has the same shape: its model is recorded nowhere local (no local run's `crew_model`, no settlement model), which is why such rows usually share populations (3) and (4). Effort is never taken from crew configuration or a crew name, and never from a crew's current config for a run that predates a config change: no config history is recorded beside a run, so that source can never prove a historical effort.
- Two efforts of one model are two rows, however many crews ran them; one crew that ran a model at two recorded efforts contributes units to both rows and is listed under each.

- An implementation run with no `crew_model` is ranked as `model unknown` under its crew. It is never assigned a model guessed from the crew's configuration, because configuration changes over time and a guess would quietly merge different models into one row. The payload reports, per row, how many of its runs had no model.
- An attempt that no local job run records, such as one another machine pulled, is charged to the crew and model on the settlement record of the claim its `pulled_by` entry names (`claim_id`), under the reasoning effort that record keeps. The follower reports the leaf run's `resolved_crew` and `crew_model` with every settlement, handoff, failure or release, and the owner keeps them on that record. Since [ORB-23825] the follower also reports the leaf's effective reasoning effort: the effort of the leaf run's main implementing invocation, else its latest matching launch record, read from the follower's own store exactly as the per-unit attribution rule reads this host's. The record keeps it in the grouping key's encoding (the wire name, `null` for the provider default, absent when unreported); on the settle call the provider default travels as `provider_default`, because tool input drops a `null` parameter. The owner uses it only when it holds no matching invocation or launch record of its own for the attempt's run, and the attempt is effort-unknown when the follower reported none (it did not know it, or predates the report). The reported effort is never derived from the owner's crew configuration, which can name another effort for the same crew. Without a record, or for a field it lacks, the crew falls back to the one task history had assigned when the attempt started, and the model is `model unknown`. The run is the one the `pulled_by` entry names. An attempt with neither a run, a recorded crew nor an assignment is `crew unknown`.
- Task authorship is not ranked. The orchestrators role was removed [ORB-23582] (§5.1); a task's `orchestrator` and `orchestrator_model` attribution is still recorded on the task, but no scorecard reads it.

## 3. Attribution: Each Problem Has One Owner

**Current** for every problem in §3.1.

Every problem counts against exactly one role, or against none (**Not scored**). Credit is separate: when one role's problem is caught or fixed by another, the catcher may earn credit (a reviewer's held finding), but the problem still counts once. The rule is recorded as [Each problem counts against exactly one role](./4_decisions.md#each-problem-counts-against-exactly-one-role).

### 3.1 Owner by problem

| Problem | Record | Owner | Charged to |
|---|---|---|---|
| Description or criteria edited after the first implementation run started | task-field history naming the document | Not scored (`spec`) | — |
| Agent blocker whose kind names a criterion, spec, requirement, scope or ambiguity | `task_blocked_by_agent` | Not scored (`spec`) | — |
| Final-recovery escalation citing the criteria | `final_recovery_escalated` | Not scored (`spec`) | — |
| Repeat run, `[BLOCKED]` PR, unmerged delivery PR or operator requeue with cause `spec` | history, failure handoff, PR refs | Not scored (`spec`) | — |
| The same rework with cause `crew` | same records | Implementer | the implementation run that ended the attempt |
| Review verdict `reject`, unless it is a false reject | review report | Implementer | the implementation run whose candidate was reviewed |
| Review verdict `accept_with_fixes` | review report | Implementer | the implementation run whose candidate was reviewed |
| Regression with escape `spec_ambiguous` | `regression_from` classification | Not scored (`spec`) | — |
| Regression with escape `review_missed` | `regression_from` classification | Reviewer | the review run that accepted the culprit's landed candidate |
| Regression with escape `no_test` | `regression_from` classification | Implementer | the culprit's final implementation run; scored as escaped regressions (§5.2) unless a pre-landing review already charged the same defect |
| Regression with escape `test_not_selected`, `platform_only`, `nondeterministic` or `unclassified` | `regression_from` classification | Not scored | — |
| Regression with nature `merge_reintroduced` or `contract_drift`, regardless of escape | `regression_from` classification | Not scored | — |
| Finding closed by an authorized decision (`Disposed`) | review report | Reviewer | the review run that raised it |
| False reject (§5.3), counted instead of the implementer's reject | settlement comments, candidate commits | Reviewer | the review run that rejected |
| Reviewer fix that failed owner revalidation | gate escalation | Reviewer | the review run that made the fix |
| Finding filed as a task by a review job and REJECTED by finding verification | `finding_verified` history | Reviewer | the review job run that filed it |
| Review verdict `incomplete` | review report | Reviewer | the review run; Not scored (`infrastructure`) when run failure classification names the block that followed as infrastructure |
| Landed file outside every pilot selector that a base-conflict block or a refused lock request of the task named | codebase facts | Pilot | the pilot run behind the newest pilot receipt |
| Lock wait on a path the holder reserved through a selector that covered none of its landed files | lock-wait episodes, pilot receipt | Pilot | the holder task's pilot run |
| Rework with cause `infrastructure`, `other` or no recorded cause | history | Not scored | — |

A rework's **cause** is the cause of the status change that ended the attempt (an operator's requeue is passed over for the change before it). A run failure is cause `crew` only while nothing judged it otherwise: when final recovery recorded a fault for the run that change names, the fault decides (`implementer` → `crew`, `specification` → `spec`, `infrastructure` → `infrastructure`; `review`, `orbit`, `external` and `unknown` → `other`), and a final-recovery escalation with no recorded fault is `other`, because an escalation hands the task to a person rather than judging the work. So an infrastructure interruption, a recovery refusal and a requeue of the unchanged candidate are never implementer rework, and in a mixed history each restart keeps the cause of its own run [ORB-23642]. Crew memory evidence (`crew_rework`) reads these same charges.

A regression's **escape** decides its owner, because the escape records which safeguard failed. Its **nature** takes precedence: `merge_reintroduced` and `contract_drift` go to Not scored regardless of escape. Every other nature follows the escape's owner. Ownership does not by itself make a problem a scored term; `no_test` regressions are scored for Implementers as escaped regressions (§5.2) [ORB-21789]. Two consequences follow:

- Spec regressions use escape `spec_ambiguous` only, and are Not scored. An incomplete delivery whose escape is not `spec_ambiguous` follows whichever owner its escape names.
- An implementer is not charged for a defect review should have seen (`review_missed`). The reviewer is.

### 3.2 Which run

A problem counts against the run responsible for it, not the task's latest crew, so a task that moved between crews charges each crew only for its own attempt.

- **Implementation run**: the activity job run that produced the candidate. A problem recorded on a task is charged to the implementation run whose span (the run's start to the next status change, as `run_spans` computes it today) contains the problem's timestamp. If none does, it goes to the most recent run before it.
- **Final implementation run**: the run whose candidate landed. Landed credit and `no_test` regressions go to it.
- **Review run**: the review gate's settlement comment for the attempt. Its reviewer line names that review's crew, provider and model — one comment identity, not the row's aggregated contributing crews. The row's recorded effort comes from the matching review invocation (§2's main-invocation rule), never from the comment; a review with no matching invocation takes the effort of its run's matching launch record under §2's launch-record fallback, and stays effort unknown only when no launch record matches either. A re-review round settles through the same gate with the same comment lead, so a settlement matches the gate's own step or any `re_review*` step, narrowed to the invocations that ended after the previous settlement and by its own settlement time. The gate runs inside the implementation pipeline, so the charge names the implementation run whose span contains the settlement. A replayed settlement for the same attempt counts once.
- **Pilot run**: `pilot_run_id` on the newest task-pilot receipt. Older receipts without that field are matched as §5.4 describes (`codebase/pilot.rs`).

### 3.3 The attribution strip

`GET /api/agents/attribution` counts the window's problems by owner: `implementers`, `reviewers`, `pilots` and `not_scored`, with `not_scored` broken down by reason. Each problem contributes once to an owner total and, for a role owner, once to that role's charged rows (model, recorded effort, contributing crews, problems), including roles not yet computed. These are ownership counts, not score contributions: rankings use only their role's scored terms, weights and denominators, so a `no_test` escape that §5.2 counts once as a pre-landing review's verdict still adds one to the Implementers strip. The strip explains that ownership counts include unscored context and rankings use only each role's scored terms [ORB-19644].

The Not scored bucket covers problems without a role owner; it is not a total of every unscored stat. An Implementers-owned `no_test` escape is scored for Implementers as escaped regressions (§5.2) — weight −0.5 at severity weight against the same-tier, expected-size reference, counted once when a pre-landing review already charged the defect — not a Not scored reason (unless attribution is unavailable or the attempt is `no-diff-expected`, as below). Ownership counts and score contributions stay distinct: the strip still counts the escape once for Implementers even when §5.2 counts it once as that review's verdict instead.

The Not scored reasons are:
- `infrastructure`: infrastructure-caused rework or an infrastructure-caused incomplete review;
- `no_cause`: rework with cause `other` or no recorded cause;
- `test_not_selected`, `platform_only`, `nondeterministic` and `unclassified`: regressions with those escapes;
- `merge_reintroduced` and `contract_drift`: regressions with those natures, regardless of escape;
- `spec`: a problem the task's specification caused (the spec rows of §3.1): no role scores task authorship [ORB-23582];
- `no_run`: an implementer problem on a task with no implementation attempt;
- `no_diff_expected`: an implementer problem on an attempt admitted as `no-diff-expected` work, which is not implementation (§5.2);
- `no_review`: a reviewer problem with no settled review to charge, or a REJECTED finding whose filing review job is unknown.

A problem falls in the window of its owner's unit of work: the charged attempt's start for implementers, the settlement for reviewers (the filing attempt's start for a review job), and the problem's own time when Not scored. A regression against a task outside the 90-day scan is not counted.

## 4. The Shared Score

**Current.**

One module computes every role's score. A role supplies its unit of work, its terms and their weights, and optionally the expected counts for its task mix.

### 4.1 Rates and the raw score

Each term's count is converted to a rate per 100 units of the role's work (§5). A role may give a term its own denominator instead: the pilots' calibration term is rated per 100 placed tasks, and warning terms per 100 settled assessments (§5.4). Such a term also has its own minimum support; below it the term is **provisional**, rated and shown but left out of the raw score, so a handful of observations cannot swing a rank. The implementers' three review terms use reviewed deliveries (§5.2) and have the same provisional rule at `review_min_support` (30, matching the smoothing constant K): fewer reviewed deliveries leave them rated and shown but out of the raw score, and zero reviews is unmeasured and neutral. Every other term uses the role's units. For roles without expectations, the **raw score** is `Σ weight × rate` over the role's terms. Positive weights are credit; negative weights are problems. Weights live in one table per role, shown in the view and returned in the payload.

Implementers instead start at 100 and sum weighted actual-minus-expected rates for their tier mix (§5.2).

### 4.2 Smoothing toward the fleet average

The **fleet average** is the mean raw score over every judged row of the role in the window (§4.3), weighted by each row's units. A row's **score** is:

```
score = min(raw, (units × raw + K × fleet_average) ÷ (units + K)),   K = 30
```

Smoothing only lowers a score. A row above the fleet average is pulled down toward it, the more the less work backs it, so three lucky tasks cannot top a ranking: a 40-unit row keeps 57% of its lead over the average and a 450-unit row 94%. A row at or below the average keeps its raw score. Pulling it up would let a small row rise on the rest of the fleet's evidence; under two-sided smoothing, two rows with the same raw score below the average ranked the smaller one first. Now a row never outranks a larger one with the same raw score: the larger one is pulled down less, or both keep their raw score and the tie goes to the row with more units.

Shrinking toward the average can reverse the order of rows with different support: with a fleet average of 100, a 40-unit row with raw 106 scores about 103.43, below a 450-unit row with raw 105, which scores about 104.69. It does not remove a systematic raw advantage from what a row is credited for rather than how well its work turned out; §5.1 removes the orchestrators' complexity weighting for this reason.

### 4.3 Minimum support

Rows with fewer than 10 units (`min_support`, default 10, at least 1) are `below_support`. They are greyed, listed after every supported row, and have no rank. Their scores are still computed and shown.

A row none of whose units has been judged yet is `unjudged` (reviewers only, §5.3). It has no rank and no score (`raw` and `score` are null), is greyed with an `unjudged` tag, is listed after every other row, and is left out of the fleet average, so rows waiting for an outcome neither rank at 0 nor drag the average down. A term with its own denominator (§4.1) has its own support: pilot calibration needs `min_support` placed tasks, while the pilots' warning terms independently need 10 settled assessments (`warning_min_support`).

### 4.3a Score interval

Every ranked row carries `interval`: `p10` and `p90`, the 10th and 90th percentiles of a resample of that row's own units. The row's units are drawn with replacement, 1,000 draws from a fixed seed, so the same facts always give the same interval. Each draw is rescored with the fleet references held as they are: the term expectations (§5), each term's own denominator (§4.1), the smoothing constant and the fleet average (§4.2). Only the row's units vary, the same unit-level resampling the model rank uses within tiers ([model ranking design §4.4](../model-ranking/2_design.md)); the ranking's own tier strata do not apply here. A unit with no tallied term count contributes zero, so the draws use the row's full `units`. The interval is a sampling band for the row's own evidence. Scores, ranks, smoothing and minimum support are unchanged by it.

`interval` is null on a row with no rank: below support, unjudged, or a row with no units. A tier view's row (the dashboard's tier ranking) carries no interval, because the all-tier interval describes the all-tier score.

### 4.4 Windows and late evidence

A role's **unit** decides window membership (§5). Problems count whenever they are recorded, so a regression filed after the window still counts against the work in it. For reviewers this is the expected case: a `review_missed` regression is usually filed days later. A review younger than the 30-day **miss lookback** is **provisional**, measured from the scan. The payload reports each row's provisional count and the window's total (`provisional` beside `rows`), and the view shows the banner from §1. Other roles report `provisional` as null.

### 4.5 Cost is shown, never scored

Dollar cost, tokens and wall time are never terms. Every role shows them under **Shown, not scored**. Implementers previously plotted score against cost in a role panel; that panel was removed as a product decision [ORB-17736], so cost now appears only under **Shown, not scored**. See [Cost is shown, never scored](./4_decisions.md#cost-is-shown-never-scored). An unavailable implementer cost still reports its counted-attempt coverage there (§5.2), so a null is never read as zero spend or as mostly priced work.

Today's per-8-active-hours rate toggle is dropped.

### 4.5a Proposed delivery-quality profile (not a score term)

The [decision-assistance grader proposal](../_archived/decision-assistance/specs/delivery-quality.md)
would have added an independently calibrated advisory profile of delivered code [ORB-20000]:
scope fit, robustness, structure, standards, verification and maintainability.
It never had a grader consumer or dashboard surface, and its shadow runtime was
removed with decision assistance in [ORB-22808]. This document's current terms,
rates, weights, attribution and crew-selection inputs never depended on it.

Keep grade evidence/version, coverage, uncertainty and actual contribution
attribution separate from observed outcomes. A high grade cannot offset a
regression or override a review. Any future crew comparison must account for
task mix, ungraded work and judge reliability; any ranked/composite or routing
use requires its own evaluated proposal and explicit adoption decision.

### 4.6 Severity

Review findings and `regression_from` links share one severity scale with constant weights: critical 8, high 4, medium 2, low 1, nit 0.25 ([ORB-19304]). A held finding adds its weight to **Findings that held**; a missed regression adds its severity weight to the reviewer term. Escaped regressions enter the implementer score at their severity weight (§5.2). Every other problem counts 1. The rubric with examples is in the orbit skill's task-fields reference.

A regression link records `severity_source`: `filer`, `verified`, `operator`, `backfill` or `default`. A new classified link must carry a severity. Finding verification ran as a different crew from the filer until [ORB-22844] removed it. When it confirmed a review-filed finding, its rating replaced the filer's on that finding's links as `verified`, and an `operator` rating stands. So a reviewer cannot earn more by inflating its own findings. Without verification the filer's value scores, and a link stored before severity existed counts as `medium` (`default`). Review gate findings are not verified and count at the reviewer's own severity; a value off the scale counts 1. A review-job finding that lands without a verification verdict (§5.3) counts at the highest severity rated on its own `regression_from` links, the only typed record of its severity at filing, and as `low` when it has none.

## 5. Roles

### 5.1 Orchestrators (removed)

**Current.**

The orchestrators role ranked task authorship by the tasks' `orchestrator` attribution: one point per landed task, less spec defects and spec-caused rework per 100 owned tasks. It was removed on 2026-11-09 because it proved of little use [ORB-23582], together with its CLI, MCP and HTTP surfaces (§6, §7). The problems it scored are now Not scored with reason `spec` (§3.3), and the one-per-task charge it added for a confirmed task-pilot scope hold is no longer produced. Tasks still record `orchestrator` and `orchestrator_model`.

### 5.2 Implementers

**Current.**

Unit: **implemented tasks**, meaning tasks with at least one implementation run by the row's model at the row's recorded effort that started in the window. Tasks tagged `no-diff-expected` are not implementer work and are left out entirely [ORB-18078]: no units, credit, charges or fleet expectation, in the headline rows and the by-type cells alike. The tag is judged per attempt, as of the attempt's start, replayed from the task's `tags_changed` history, so a tag added after work began does not exempt that attempt and removing it does not retroactively count one. The tag alone decides: an untagged task that ends with no diff (a verified no-diff or already-landed handoff) still counts with its normal credits and charges. Implementer problems on an excluded attempt are Not scored with reason `no_diff_expected`, so the attribution strip and the ranking still agree. Tag edits made before writes recorded `tags_changed` entries cannot be reconstructed independently; for an attempt before the first recorded change, replay uses that change's `previous` tags. Current tags apply only when no `tags_changed` entry is recorded. A configured by-type entry that matches only on `no-diff-expected` keeps its cells, all empty. The Pilots role's own `no-diff-expected` exclusion (§5.4) is unchanged, and both roles read the one `is_no_diff_expected` predicate.

**Execution split** [ORB-20391]. Tasks are also worked in interactive sessions (a desktop or terminal agent that moves the task and sets its `crew`) with no implementing job run, so no token, cost or run-time telemetry. Each scanned task's execution is derived from its attempts, never set by an agent: an attempt is a job's when a local implementation run matches it (§3.2) or a claim's `pulled_by` opened it (even without a recorded run id). The task is `managed` when its final attempt (the landed one, else the latest) is a job's, or when history records no attempt but an implementation run names it; `interactive` when it reached `review` or `done` otherwise; and `unclassified` when neither holds (not implemented yet, or interactive work not yet in review). Every implementer count (rows, detail, by-type cells, per-task export and size curves) reads the tasks the selection's `execution` scope admits: `managed` by default, `all` (managed and interactive) or `interactive` alone as a separate series. Unclassified tasks are in no scope. Cost, cost coverage and median run time count managed tasks whatever the scope, so an interactive task never reads as a missing run. The implementers header carries `execution_split`, one entry per row with a counted attempt in the window whatever the scope: `managed_tasks`, `interactive_tasks`, `unclassified_tasks`, and the interactive tasks' `interactive_landed` and `interactive_regressed` (named by any `regression_from` link). The other roles ignore the scope: they score review, authorship and assessment, not how a task was executed. The crew-performance snapshot read scope `all` until its removal in [ORB-22804] ([crew-performance scope](../_archived/crew-performance/1_scope.md#evidence-snapshot)).

| Term | Weight | Definition |
|---|---|---|
| Landed | +1 | Landed tasks whose final implementation run is the row's, one point each at every complexity. |
| First-pass accepts | +1 | Reviewed tasks whose first review verdict on the row's candidate was `accept`. |
| Review rejects | −1 | `reject` verdicts on the row's candidates. |
| Reviewer repairs | −0.5 | `accept_with_fixes` verdicts on the row's candidates. |
| Crew-caused rework | −1 | Repeat runs, `[BLOCKED]` or unmerged PRs, and operator requeues with cause `crew`, charged to the run that ended the attempt. Review verdicts are not counted again here. |
| Escaped regressions | −0.5 | `regression_from` links with escape `no_test` against the row's landed tasks (§3.1), mostly delivery-review findings, each at its severity weight (§4.6), so a `medium` one costs as much as a review reject. Expected counts are size-adjusted (below). A link whose defect site a pre-landing review already charged is not counted again [ORB-21789]. |

Every scored term uses a fleet expectation for the row's **same task mix** [ORB-17779]. First-pass accepts, review rejects and reviewer repairs use only **reviewed deliveries** for both the row and fleet populations. Escaped regressions refine their implemented-task reference to the same tier and expected change size (below):

```
term_units = reviewed tasks for the three review terms, implemented tasks otherwise
expected(row, term) = Σ tier [row_term_units(tier) × fleet_count(term, tier) / fleet_term_units(tier)]
contribution(row, term) = weight(term) × (actual(row, term) − expected(row, term)) × 100 / row_term_units
raw = 100 + Σ term contribution(row, term)
```

Positive credit above expectation raises the score; problems above expectation
lower it. A crew matching every reference rate has raw 100 whatever its tier
mix, including when tiers have different landing or first-pass acceptance
rates. The fleet includes the row itself and below-support and `model unknown`
rows. Its unit is a **row-task observation**: a task implemented by two model-and-effort
rows contributes once to each row and once to each corresponding fleet
observation. For review terms, only observations with a settled first review
enter the denominator or expectation; unreviewed work supplies neither credit
nor penalty. Zero reviewed deliveries gives a zero expected count and neutral
contribution, with a null rate rather than a measured zero.

**Review terms need minimum support [ORB-23838].** The three review terms are rated per 100 reviewed deliveries, so a few reviews could outweigh hundreds of tasks and decide the top of the board (a row with 31 reviewed of 394 tasks scored 144.3 on +42.7 review points; another scored −16.5 from 2 reviewed deliveries). A row with fewer than `review_min_support` reviewed deliveries (default 30, one named value that matches K) lists `first_pass_accepts`, `review_rejects` and `reviewer_repairs` in `provisional_terms`: counts, rates, expectations and `term_units` are still returned and shown, and each contributes 0 to the raw score. At or above the threshold nothing changes. Zero reviewed deliveries stays unmeasured, not provisional. The fleet expectation's population is unchanged: a provisional row's reviews still count in the fleet's rates. By-type and tier cells apply the same rule to their own reviewed count. The implementers header and the by-type payload return `review_min_support`, and the dashboard's "How the score works" states it. Repeated rejects
can exceed one per reviewed task. Other terms include unreviewed and unlanded
implemented tasks, not just tasks with the outcome.

**Sparse tiers:** below 30 fleet row-task observations, a tier retains its own
observations and borrows evidence from the nearest supported assessed tier in
`low < medium < hard < xhard` order. Equal distances prefer the lower tier;
size does not choose the reference. If no assessed tier is supported, or the
sparse tier is unassessed, use the supported tiers together, weighted by their
term denominators. If no tier is supported, use a borrowing-weighted fleet
pool. A supported unassessed tier can participate in the supported pool but
has no position in the assessed order.

Borrowing is a **conservative exchange**, computed separately for each term's
population. Let `n_t` be that tier's denominator, `r_t` its observed count
per unit and `K = 30`. A sparse tier offers exchange mass
`b_t = n_t × K / (n_t + K)`. Against a supported reference tier `u`, its
unscaled exchange mass is `a_tu = b_t`; against a supported pool, distribute
`b_t` over its members in proportion to their denominators. With no supported
tiers, each unordered pair exchanges `a_tu = b_t × b_u / Σ b` instead. Zero
denominators contribute no exchange mass. If a selected reference has no
reviewed observations, review terms retain the sparse tier's observed rate.

Let `e_t = Σ_u a_tu`. Scale all exchanges for a term by
`q = min(1, min_t(b_t / e_t))`, ignoring zero `e_t`. Every tier, including
a supported reference, exchanges at most its own borrowing budget and retains
at least `n_t / (n_t + K)` of its own rate. Starting from observed counts,
each pair adds
`q × a_tu × (r_u − r_t)` to tier `t`'s expected count and subtracts the same
amount from tier `u`. Thus every expected rate is a convex combination of
observed rates, and expected fleet counts are conserved, up to floating-point
roundoff. With one supported reference and no exposure cap, the sparse tier's
rate is exactly `(count_t + K × r_u) / (n_t + K)`. Reference tiers receive the
balancing adjustment. When all tiers are supported there are no exchanges and
all existing rates and scores are unchanged. The escaped-regression term
continues to use its separate size-adjusted, leave-one-row-out expectation.

Each row's JSON `stats` includes `tier_adjustments`, an array for the sparse
tiers present on that row (empty when none). Every entry reports `tier`,
`adjusted: true`, `method: conservative_shrinkage`, `fleet_units`, `reference`
(a tier, `supported_pool` or `fleet_pool`), `reference_tiers`, and `terms`.
Each term reports its `units`, retained `own_weight` after exchange scaling,
and `expected_rate` before display rounding; these exclude escaped regressions.
This makes both the fallback and how much own evidence remains inspectable.

The 30-observation threshold counts all implemented tasks; review exchanges
and rates use only reviewed observations. Even a supported tier can have few
reviews; this adds no separate review-support threshold. Exact per-tier mix
invariance is approximate while borrowing is active. With different review
coverage, the implemented-task-weighted raw average need not be 100;
shrinkage still uses that actual fleet average. An empty fleet has no rows or
fleet average.

This replaces complexity-weighted landed credit and unadjusted problem rates.
Complexity pools choose a crew's tier mix, so the old ranking largely measured
pool membership; rejects and repairs also grow with difficulty. After a month
of data, the tier comparison previously shown only as context now supplies the
scored expectation for **every** term. This extends the unit-landed principle
introduced for the former orchestrators role by [ORB-17118] without changing other roles.
See [the decision](./4_decisions.md#compare-implementers-with-the-fleet-on-their-task-mix).

The public runtime scorecard and detail APIs return `status: ok`. Tasks count
once per model-and-effort row, even if that row ran them more than once in the
window. First-pass credit uses the first settled review of that row's
candidates; a later accept does not erase an earlier reject. Review verdicts
enter their own terms once and are not added to crew rework. The shared
attribution assigns `review_missed` to reviewers and `no_test` to the final
implementer. Shrinkage remains `min(raw, …)` with K = 30; minimum support and
model attribution are unchanged.

The historical payload keys `weighted_landed` and `cost_per_weighted_landed`
remain for compatibility; both now use **unit landings**, with labels **Landed**
and **Cost per landed task (USD)**. Each implementer row adds `reviewed_units`, the count of its tasks with a settled first review (§1.1), `term_units` naming
that denominator for each review term, `expected`, a map
of term keys to expected counts on the same term population, and `score_base: 100`.
The term metadata names `unit: "reviewed deliveries"` for the three review terms;
`per_100` is per 100 of that unit and `expected × 100 / term_units` is its
expected rate. With zero reviewed deliveries, those rates are null and each
contribution is zero. Actual `counts` remain counts; detail term `contribution` is weight times the
actual-minus-expected rate, so `score_base + Σ contribution` equals raw before
rounding. Other roles have neither additive field and retain their formulas.
The former `tier_comparison` stat is removed: scored expectations supersede it.

**Escaped regressions are scored against a size-adjusted expectation [ORB-21789].**
[ORB-19629] left them unscored until roughly 100 linked regressions with sites
and another backtest. The 2026-11-03 rerun had 662 links with sites and found
that change size predicts regressions (cross-validated AUC 0.77) while crew adds
nothing beyond size and tier ([decision](./4_decisions.md#score-escaped-regressions-against-expected-change-size)).
Delivery review covers every landed task, so the term is the only review
evidence for crews whose work no pre-landing review sees. Routing larger work to
a crew must not penalize its score, and dividing regressions by the crew's own
diff size would reward bloat, so the expectation uses expected size, never
actual size:

- The term's **actual** is the severity-weighted count of `no_test` regressions
  against the row's final implementation runs (§4.6), including a measured zero.
- Its **expected** count is size-adjusted, unlike the other terms' tier pools.
  Expected size is the fleet median lines changed among unique landed tasks in
  the window with the same `(tier, type)`; type is classified by the configured
  by-type rules and admitted tags below. Fleet medians include all rows,
  including the row being compared. Tasks enter buckets by that **cell
  median**, never by their own lines changed: `[0, 2)`, `[2, 4)`, `[4, 8)`, and
  so on. For each row and bucket, the expected rate is the severity-weighted
  escapes per row-task observation among the other rows in that bucket; the
  expected count sums those rates over the row's tasks. Missing medians use an
  unknown-size bucket. If a bucket has no peer observations, use the other rows'
  rate across the same tier, then across all tiers if that tier has no peers.
  With no peers anywhere, use a zero reference (an explicit conservative prior,
  not an observed rate). The row's boolean `escaped_regressions_fallback` is true when any task used a fallback. Leave-one-out
  expectations need not conserve fleet totals. By-type cells (§5.2 below) use
  their `(tier, type)` cell rates for this term like every other.
- **Counted once across review layers.** A pre-landing review gate and delivery
  review can both see a task. A `no_test` link whose `sites` name a path that a
  finding of a `reject` (not a false reject) or `accept_with_fixes` on the row's
  candidate cited or changed is the defect that verdict already charged, so it
  adds nothing to escaped regressions. Precise sites require evidence of
  overlap: symbol identities must match, and recorded line ranges must
  intersect (endpoints included). File equality alone cannot match different
  symbols or disjoint ranges. A symbol and a line citation are incomparable
  without a source mapping in the stored facts, so that link still counts;
  a precise site also still counts when the review cites only the file.
  Coarse `file:` sites retain file-level matching, and a `dir:` site covers
  the paths under it on directory boundaries. A link without sites always
  counts. The charge stays in the attribution strip's Implementers ownership
  count.

The former `escaped_regressions` and `regression_residual` stats are removed:
the term's count, expected count and contribution carry them. Each row's
`stats` includes:

- `diff_economy`: the median, over the row's landed tasks with measured diffs
  and a positive fleet cell median, of `lines_changed / fleet_cell_median`.
  Above 1 means larger diffs for comparable work; twice each median gives 2.
  Missing size data and zero reference medians are excluded, and no usable
  ratios returns null (unavailable), never zero. A measured zero diff against a
  positive reference contributes a real zero. Both measures reuse the experiment
  `lines_changed` aggregate: added plus deleted code, test and other lines,
  summed over landed deliveries per task within the scan horizon. No new store
  format or backfill is introduced.

Other unscored context: cost per landed task, cost coverage, median completed
run duration, and tasks by tier. Cost sums the workspace's recorded invocations
for the row's delivery runs (including delivery review and repair activities),
counting each distinct run once, using provider-reported cost when present and
derived pricing otherwise, from the same telemetry as `/cost`. It divides by
this row's landed task count. Missing run telemetry, an unpriced invocation, a
truncated cost scan, or no landed credit returns null, never zero — a valid
zero spend returns zero. The complete-cohort average is strict: it is never
averaged over priced attempts alone. These are recorded costs as of the scan,
not a final bill. Pulled work without local cost records remains unpriced.

Cost coverage makes that null readable. Every row also carries, beside the
cost stat, `cost_coverage` (a summary string) and the counted-attempt counts
`cost_coverage_attempts` (the named denominator), `cost_measured_attempts`,
`cost_unpriced_attempts` and `cost_missing_run_attempts`. The population is
every counted attempt behind the cost calculation — an attempt started in
the window, not admitted as `no-diff-expected`, whose model and recorded effort match
the row — counted per attempt, not per distinct run: a run shared by two
attempts covers both, and the counts sum to the denominator. An attempt is
measured when its run has a priced cost (zero included), unpriced when it
has a run with no priced cost, and missing-run when it has no run at all.
Coverage is display-only: it enters no tally, denominator or score, so
scores and ranks are unchanged by it or by pricing. The detail column shows
the coverage stats under **Shown, not scored** and, while the cost is null,
explains why — how many counted attempts have no run or no priced cost,
measured of the denominator — instead of leaving an unexplained dash.
Complete coverage still shows the existing cost per landed task.

Implementers' role panel is **Regression rate by change size**, display only and
served by the size-curves route (§6); its **Complexity** filter is separate from
**Rank within**. The score-against-cost plot and per-tier review table remain
removed. **Rank within** offers All tiers, low, medium, hard and xhard.
All tiers preserves the existing tier-mix score.
A tier ranks each model-and-effort row by the matrix's `crew_mean`, sums its cell `n` for
support, hides rows with no work there and leaves below-support rows unranked.
Ties use support, crew and model. This tier tie order differs from the general scorecard ordering, which uses support, model, effort and crew; the dashboard tier view follows the tier order, with no effort comparison. The detail explicitly labels the profile,
breakdown and unscored context **All tiers**, and shows the selected tier's score
and support separately. The existing **Score breakdown** shows the 100-point fleet reference
and each term's actual and expected counts beside its contribution. It adds no
new section or per-tier table.

**By-type matrix [ORB-17814].** `orbit agents scorecard --role implementers --by-type`
(and its `--json`) splits the same evidence into one cell per `(model, recorded effort,
tier, type)`, where the type comes from the ordered
`workflow.task_types` list (named `workflow.crew_performance_types` before [ORB-22804]): a task takes the first type whose
`tags` it carries or whose `paths` globs match its `context_files`, else
`other`. Each cell's raw score uses the formula above with the fleet
expectation taken from the same `(tier, type)` fleet cell; a `(tier, type)`
fleet cell under 30 observations falls back to the tier's rates. Review terms
use reviewed deliveries in both the cell and its reference, with neutral
contributions for cells without reviews and for cells with fewer than
`review_min_support` reviewed deliveries, just like headline scores. Cells are
small, so the matrix applies a two-sided, two-level shrinkage with K = 30:
`crew_mean = (n_tier × raw_tier + 30 × 100) / (n_tier + 30)`, then
`score = (n_cell × raw_cell + 30 × crew_mean) / (n_cell + 30)`. Each cell carries the row's `crews`, `model`, `reasoning_effort`, `effort_label` and `label`. A cell with
no evidence scores its row-tier mean (`crew_mean`), and a row with no tier evidence has
mean 100. The matrix is separate from the ranking: the ranking keeps its
one-sided `min(raw, …)` smoothing and is unchanged by `--by-type`, and the
dashboard reads `GET /api/agents/implementers/by-type` from the same cached scan
for its tier selector; it does not render a by-type table. Invalid windows
return 400 as on the other scorecard routes.

### 5.3 Reviewers

**Current.**

Unit: **settled reviews**, meaning review gate attempts by the row's model at the row's recorded effort whose verdict settled in the window. Review jobs that file findings as tasks count too: each attempt of a review job task that filed at least one finding (a bug tagged `code-review`, `delivery-code-review`, `security-review` or `qa-sweep`, linked `spawned_from` the job) is one unit, in the window of the attempt's start, under the attempt's model and recorded effort.

| Term | Weight | Definition |
|---|---|---|
| Findings that held | +1 | Findings repaired by the reviewer and validated, open findings on a `reject` that the next implementation run changed a cited path to address, and review-job findings finding verification CONFIRMED or, with no verdict, whose task landed [ORB-19633]; each weighted by severity (§4.6): critical 8, high 4, medium 2, low 1, nit 0.25. A CONFIRMED finding counts at the verifier's severity, an unverified landed one at the highest severity rated on its own `regression_from` links (else `low`), a review gate finding at the reviewer's own. |
| Fixes that held | +1 | `accept_with_fixes` verdicts whose fixes passed owner revalidation and whose task has no regression with escape `review_missed`. |
| Rejected findings | −1 | Findings closed by an authorized decision (`Disposed`), and findings a review job filed as tasks that finding verification REJECTED. A filed finding dropped without a verdict costs nothing (see below). |
| False rejects | −2 | `reject` verdicts where the next implementation run changed none of the rejected findings' cited paths and the next review accepted. |
| Failed fixes | −1 | Reviewer fixes that failed owner revalidation and were escalated. |
| Missed regressions | −2 | Regressions with escape `review_missed` against tasks the row's review accepted, whenever filed, each counted at its severity weight (§4.6), so one costs −16 (critical) to −0.5 (nit). |
| Incomplete reviews | −0.5 | `incomplete` verdicts not caused by infrastructure. |

An accept rate never enters the score. A reviewer earns only for verdicts that turned out right. See [Score accuracy, never the volume of objections](./4_decisions.md#score-accuracy-never-the-volume-of-objections). The reject rate, the number of findings and cost are not terms either.

How a review job's filed findings are judged:

- A recorded finding verification verdict wins: CONFIRMED holds, REJECTED counts as a rejected finding, whatever became of the task. Since the verification job was removed [ORB-22844], new verdicts are REJECTED ones from the task pilot's `finding_invalid` or an implementer's `finding-invalid.json`; CONFIRMED verdicts come only from history.
- Without a verdict, the finding's task outcome judges it. This is the common case. A task that landed (done, or archived after done) held. A task still open is not judged yet: it earns and costs nothing.
- A task rejected or archived without landing is judged but **neutral**. A drop should count against the reviewer only when the record shows the finding was wrong, not when it was a duplicate, won't-fix or superseded. Task history carries no typed drop reason: a rejection's note is free text and its `decision` field records no such reason. So every drop without a verdict is neutral, and only a REJECTED finding verdict, which carries a typed `rejection.reason`, costs the reviewer.
- A review job unit is judged when any of its findings has a verdict or a landed or dropped task. A row is **unjudged** (§4.3) when none of its units is judged: it has no review gate settlement, no charge, and no judged review job unit.

How a `reject` is judged:

- The settlement comment lists each finding that stays open with a `Cited:` line naming the paths it cites (`path` or `path:line`). The scan reads the rejected candidate (the settlement's final candidate, else its implementation commit) and the candidate the next settlement on the task reviewed.
- It compares each cited path's blob between those two commits in the workspace repository, without fetching. A path whose content differs, or that exists in only one of them, was changed.
- A cited finding whose path changed **held**. When no cited path changed and the next review passed (`accept` or `accept_with_fixes`), the reject is a **false reject**: it counts against the reviewer, and the implementer is not charged a review reject for it.
- A reject with no next settlement yet, no cited path, or a commit the repository cannot read is not judged. It earns nothing and costs the reviewer nothing; the implementer's review reject stands. Settlements written before the gate recorded `Cited:` lines are never judged.

Failed fixes and incomplete reviews:

- A reviewer fix failed when a review gate escalation with verdict `reject` follows the `accept_with_fixes` settlement. Its fixed findings then earn nothing.
- An `incomplete` verdict is the reviewer's unless run failure classification names the first block or status change after it as infrastructure. Then it is Not scored with reason `infrastructure`.

Shown, not scored (each row's `stats`):

- review gate settlements and review job runs that filed findings;
- accept rate, the share of gate settlements that passed (`accept` or `accept_with_fixes`); the accept rate on tiers any other reviewer also judged (`compared_accept_rate`); and the peers' accept rate on those same tiers. Tiers no peer reviewed are left out of both compared rates, so the compared rate can differ from the all-reviews accept rate. A row whose compared rate is 10 or more points above the peers' rate has `accepts_often` true and is tagged `accepts often`. The tag's tooltip quotes those two compared rates.
- the five verdict outcomes below;
- median review minutes, from the settlement's reviewer runtime;
- the row's provisional count (the row's `provisional` field).

Cost per review is not shown: no record ties a review gate attempt's cost to its settlement.

The role panel, **How verdicts turned out**, splits each reviewer's settled reviews into: accepted and held up, caught a real problem (a finding or fix that held, or a review-job finding CONFIRMED or, unverified, landed), false alarm (a false reject, or a review job none of whose findings held and at least one was REJECTED), missed a regression (a passed review a `review_missed` regression came back to), and unresolved (not judged yet, incomplete, fixes that failed, or a review job whose findings were dropped without a verdict). Unjudged rows are greyed there too. "How the score works" adds how filed findings are judged and what an unjudged row is.

### 5.4 Pilots

**Current.**

Unit: **assessed tasks**, meaning done tasks with a landed commit whose newest task-pilot receipt was written in the window. Bulk tasks (more than 12 landed files) and `no-diff-expected` tasks are left out. A row is the model and recorded effort of the pilot run the newest receipt names (`pilot_run_id`), with the contributing crews as detail; an older receipt without that field is matched to the one task-pilot run active when it was written, or among several, the one whose step outputs carry its operation id. The pilot crew comes from the run’s frozen `workflow.purpose_crews.task_pilot` draw, falling back to its `workflow.low_complexity_crews` draw for older runs. This is the same lookup readiness authentication failures and routine provider holds use. A missing draw or a receipt whose run is not found is ranked as `crew unknown`. The model comes from the invocation of the worker paired with the apply report carrying the receipt’s exact operation id. Task links distinguish fan-out invocations; a task-linked retry uses its latest recorded model, including an explicitly missing model. Without a matching report, only one recorded worker can identify the round; legacy invocations without task links must also be unique. Missing or ambiguous evidence is `model unknown`, never a different pilot or repair round. Neither field falls back to the run’s `resolved_crew` or `crew_model`, which describe the wrapper. Crew state is read in batches; invocation evidence is read once per attributed run and cached for its receipts, with a failed or truncated read leaving the model unknown. The task detail’s **Handled by** pilot badges share this resolver, deriving historical identities on read while keeping the receipt’s original run and time. Linked task-detail receipts load only their named runs. No task history, settings or stored actor attribution is rewritten.

The pilot's selectors are the newest receipt's `context_files_after`; the filer's are the oldest receipt's `context_files_before`. Both are graded against the files the landed commit changed. Recall is landed files a selector covers ÷ landed files; precision is selectors that cover a landed file ÷ selectors; F1 is their harmonic mean. A `dir:` selector covers the files beneath it and a wildcard selector the files it matches. A mirrored or generated output counts as covered when a selector covers its source: `plugin/skills/**` through `crates/orbit-core/assets/skills/**`, the CLI help, output and MCP snapshot goldens through any CLI, MCP, tools or governance source, and `docs/INDEX.md` through any `docs/` file. These rules live in `application/agents/roles/pilots.rs`; Analytics › Codebase no longer grades pilots.

| Term | Weight | Definition |
|---|---|---|
| F1 lift | +1 | F1 points gained over the filer's own selectors from before any pilot ran, per task. A task's lift can be negative. |
| Tier calibration | +0.25 | Placed tasks whose assessed complexity matches their observed tier, per 100 placed tasks. |
| Costly misses | −1 | Landed files no pilot selector covered that a base-conflict block of the task, or a refused lock request of the task, named. |
| Confirmed warnings | +3 | Held assessments the records proved right (below), per 100 settled assessments. |
| Dismissed warnings | −1 | Held assessments overridden as harmless (below), per 100 settled assessments. |
| Missed scope blocks | −3 | Clean assessments whose task was promoted, then blocked on a decision block triage proposed, per 100 settled assessments. |
| Lock-wait hours | −1 | Hours other tasks waited, in lock-wait episodes, on paths the holder reserved through a pilot selector that covered none of the holder's landed files. |

A lock-wait episode (Analytics › Codebase's lock contention) knows its holders: each refused request records which task or reservation held each conflicting path, and a reservation resolves to the tasks it named. A wait is charged to a holder's pilot when at least one of the holder's pilot selectors reserves the path and none of those selectors covered a landed file. A wait on a path that a useful selector reserved is not charged, and neither is a wait on a holder no pilot assessed. Both pilot problems count against the pilot run of the holder's newest receipt, in the window that receipt falls in.

The **observed tier** ranks the window's assessed tasks that have an assessed tier (low, medium, hard or xhard) and a total implementation cost (every attempt's run priced) by that cost, cheapest first, and cuts the ranking into tiers in the same proportions the pilots assigned. The observed column totals therefore equal the assessed row totals. Calibration measures whether a pilot orders work correctly, not whether it guesses absolute cost (§11). Tasks without a tier or a priced implementation are not placed. Cost decides only the observed tier and is never a term.

Tier calibration uses only **placed tasks** as its denominator and support (`term_units.tier_calibration`), leaving unplaced tasks out of both the credit and denominator. The tier matrix reports placement coverage as `placed` and `not_placed`. Below the query's `min_support` placed tasks, calibration is provisional and contributes nothing to the score even if the row has enough assessed tasks to rank. Zero placements therefore mean unmeasured calibration: its displayed rate is 0 with support 0 and a provisional marker, never evidence of incorrect assessments. F1 lift, costly misses and lock-wait hours retain all assessed tasks as their denominator; warning terms retain settled assessments and their independent minimum of 10. With full placement coverage and sufficient support, calibration and the score are unchanged.

**Pilot warnings.** F1 lift, tier calibration, costly misses and lock-wait hours grade done tasks; the three warning terms grade every assessment, held or clean, by how it settled. They live in `application/agents/roles/pilot_warnings.rs`. An assessment is one task-pilot receipt: the receipt comment the apply step seals with the assessment artifact it persisted (legacy receipts contain the audit inline). It is **held** when that assessment has at least one finding that holds promotion (`blocked_by`, `adr_conflicts`, `contract_conflicts`, `utility_warnings`, `surface_warnings` or `validation_tool_warnings`), and **clean** otherwise. A finding marked `holds_promotion: false` is advisory and never counts. A `blocked_by`, `utility_warnings` or `surface_warnings` finding is judged against the task's current dependency edges, never against which prerequisites are done now: the done state at assessment time is not stored, and a prerequisite finishing later must not turn a past hold clean. The receipt says only whether the assessment held. How it settled comes from task history (edits and status transitions) and block-triage markers, never from agent output.

A **scope edit** is an `updated` history entry written by someone other than `task-pilot` or `system` whose note names `description` or `acceptance_criteria`. A **bare-note edit** is one whose note names no document. Every envelope-only write leaves that bare note: context files, dependencies and relations, but also tags, title, priority and crew. So history cannot tell a scope fix from bookkeeping there.

Only an assessment written while the task was `proposed` is graded. The **window** of an assessment runs until the next assessment of the same task or until the task leaves `proposed`, whichever comes first.

| Outcome | When |
|---|---|
| Confirmed | Within the window, a scope edit, or the task is rejected or archived. Or the task is approved with no scope edit and no bare-note edit in the window, and its delivery then blocks or the task is rejected. |
| Dismissed | Approved with no scope edit and no bare-note edit, then `done` with no block. |
| Missed scope block (clean assessments only) | Promoted, then a `blocked_task_triaged` marker records disposition `decision` (written only by the retired blocked-task triage, so it appears in old history). |
| Settled clean (clean assessments only) | Promoted, then `done`, rejected or archived with no such marker. Counts in the denominator only. |
| Excluded | Approved after a bare-note edit with no scope edit: neither credit nor charge, since it may be a dependency fix that answered the warning or only a tag change. Superseded by a later assessment with no scope edit in between (the later one is graded). Written outside `proposed`. Held and archived after approval. A clean assessment rejected or archived while still `proposed`. |
| Pending | Still `proposed`, or the delivery has not finished. Reported with the excluded ones. |

An outcome falls in the window in which the entry that settled it was written. The warning terms are rated per 100 **settled assessments**: confirmed, dismissed, missed and settled-clean ones. Excluded and pending assessments are not settled. A row with fewer than 10 settled assessments (`warning_min_support`) shows its three warning terms as provisional, outside the score (§4.1). At about 10 holds a day across crews, the warning terms of a 24-hour window are mostly provisional.

Why these weights: at 3 : 1 a crew's warnings are net positive above 25% precision, and the evidence that motivated the terms measured about 55%. A hold costs one operator read, while a confirmed warning saves a failed delivery run. A missed scope block mirrors the confirmed credit, so staying silent is never free.

Missed scope blocks have one source: block-triage markers, which only the retired blocked-task triage wrote ([ORB-20629]), so they survive in old history alone. When no `blocked_task_triaged` marker of any disposition falls in the window, the term reads 0 and the scorecard's `sources.missed_scope_blocks` reports `available: false`, with the reason. The view shows that as a banner, so the zero is not read as no misses.

Each assessment counts against the model and recorded effort of the pilot run its receipt names, through the same attribution as graded tasks (above), never the task's own crew. A row can therefore have settled assessments but no graded done task (`units` 0).

**Pilot modes** [ORB-21124]. The Pilots payload's `pilot_modes` separates full assessments from re-checks (an explicit re-pilot of a held assessment that re-judges only its holding findings). For each of `full` and `recheck` it reports the window's assessments, the distinct pilot runs behind them, their median and total wall seconds, and their total tokens with `runs_with_tokens` (summed from the run's recorded agent invocations). `rechecks_followed_by_full` counts re-checks a later full assessment of the same task replaced, `rechecks_escalated_to_full` counts re-check runs whose pilot returned a full assessment instead (`mode_reason: recheck_escalated`), and `recheck_full_rate` is their sum over all re-check attempts. These are shown, not scored.

The pilot warning terms are not problems in the attribution strip (§3.3). They grade the pilot's warnings and do not reassign the block or edit, which keeps its own owner.

Shown, not scored (`stats`): recall, precision and F1 pooled over the row's tasks, the filer's F1, landed files no pilot selector covered, unused selectors, selectors per task, the share of placed tasks assessed within one tier, cost per assessment (null when a pilot run is missing or unpriced), median pilot run duration, `tier_matrix`, warning precision (confirmed ÷ (confirmed + dismissed), null with neither), holds per 100 assessments written in the window, and the settled, excluded-or-pending and missed-scope-block counts.

`tier_matrix` is `{tiers, rows, placed, not_placed, on_tier, too_easy, too_hard}`: `tiers` lists the tiers present in the window, cheapest first; each of `rows` is `{assessed, observed}` with one count per tier in `tiers`. Too easy counts tasks assessed below their observed tier and too hard those assessed above it.

The role panel (`js/agents-pilots.js`) is that assessed-by-observed tier matrix for every pilot, the diagonal highlighted, with on-tier, too-easy, too-hard and not-placed counts. The detail column leaves structured stats to the panel. The ranking and detail column render the warning terms like any other. A term with its own unit names it in its header tooltip and detail line (`4 of 20 settled assessments`), and a provisional term is shown in italics and adds nothing to the breakdown. The panel adds the unmeasured-source banner and a "How the score works" paragraph on settlement.

A landed file outside the declared context on a task no pilot assessed is not charged to anyone in v1 (§11).

## 6. API Contract

**Current.**

All routes are GET, scoped by `workspace`, follow the host picker through `/api/on/<host>/…`, and answer only GET.

```
GET /api/agents/{role}/scorecard?window=&min_support=&execution=&until=
GET /api/agents/{role}/detail?window=&min_support=&execution=&crew=&model=&effort=&until=
GET /api/agents/attribution?window=&until=
GET /api/agents/implementers/by-type?window=&min_support=&execution=&until=
GET /api/agents/implementers/size-curves?window=&complexity=&execution=&until=
```

`execution` is `managed` (default), `all` or `interactive` (§5.2 Execution split); only implementer counts read it.

`until` is optional. Set, it is an RFC 3339 instant or a duration ago (`30d`; units `m`/`min`/`minute`, `h`/`hour`, `d`/`day`, `w`/`week`, whitespace ignored, a trailing `s` trimmed). A calendar window is then `[until - window, until)`. A count window takes the newest N delivery attempts with `start < until`. A bare calendar date is not accepted, and a future instant is a 400. Omitted, behaviour is unchanged: nothing is excluded for falling at or after now. Reviewer provisional counts and the other lookbacks stay measured against the scan's now, not against `until`, so a window that ends 30 or more days before now reports `provisional: 0` for reviewers.

`{role}` is `implementers`, `reviewers` or `pilots`; `orchestrators` is refused as an unknown role since [ORB-23582]. `model` is the literal `model unknown` for an unknown model. It may be omitted when the crew has one row, and is required when it has several. `effort` (a wire name, `provider_default` for a null effort, or `unknown`) narrows a crew-and-model selection that names several rows to one model-and-effort row.

**Scorecard** answers:

- `role`, `window`, `requested_window`, `since`, `until`, `min_support`, `execution`, `computed_at`, `cache_ttl_seconds`, `tasks_scanned`. `window` is the window served: `all` is requested as `all` and served as `90d`; `window_attempts` is a count window's size (null for a calendar window). `since` and `until` are the served bounds as RFC 3339 UTC with second precision, after clamping `since` so it is not after a set `until`. When `until` was omitted, `until` is the instant used as now and it is not an exclusive filter;
- `status`: `ok`, or `not_computed` while a role's track has not landed. A `not_computed` role still returns its `terms`, with empty `rows`.
- `unit` (label of the role's unit);
- `fleet_average`, over the judged rows;
- `terms`: `key`, `label`, `weight`, `definition`, `group`, and `unit` on a term rated per 100 of its own unit (the pilots' calibration and warning terms);
- `provisional`: for reviewers, the rows' provisional reviews in total; null for the other roles;
- `rows`, best first; each ranked row has `interval` as `{p10, p90}` (§4.3a), or null when it has no rank;
- pilots only: `warning_min_support`, and `sources.missed_scope_blocks` as `{available, triage_records, detail}`;
- implementers only: `execution_split` (§5.2).

Each row carries `crew` (the first contributing crew), `crews`, `model`, `reasoning_effort` (`null` for the provider default, `"unknown"` for unrecorded work), `effort_label`, `label` (`model @ effort`), `units`, `below_support`, `unjudged` (§4.3; only reviewer rows can be true), `rank` (null below support or unjudged), `counts` (term key to number) and `per_100` (term key to number, or null for an implementer review term with no reviewed deliveries), `raw` and `score` (null on an unjudged row), `runs_without_model`, `provisional` (reviewers), and `stats` (the not-scored context: key, label, value). An implementers row also carries `reviewed_units`, the count of its tasks with a settled first review: the scoring denominator for the three review terms and their expectations (§5.2). A row of a role with per-term denominators (implementers and pilots) also carries `term_units` (term key to its own units) and `provisional_terms` (the keys below their own minimum support). Implementer review terms are provisional below `review_min_support` reviewed deliveries (returned in the implementers header and by-type payload, default 30); zero reviewed deliveries is unmeasured, not provisional. Calibration support counts placed tasks and uses `min_support`; warning support counts settled assessments and uses `warning_min_support`.

**Detail** carries the scorecard header, `fleet_average`, `provisional`, the selected `row`, and `terms`. Each term carries its key, label, weight, group, count, `per_100`, contribution and `tasks`, largest count first, plus `units` and `provisional` when it has its own denominator. Each task carries `task_id`, `title`, `status`, `count` and `occurrences` (`at`, `detail`, `run_id`).

**Size curves** (implementers only, display only) slice the same cached scan into regression rate by landed change size. The cohort is the landed tasks whose final implementation attempt counts for implementers in the window (it started in the window and the task was not `no-diff-expected` work), with a recorded landed diff size and at least 7 days since completion. A task regressed when any `regression_from` link against it, whatever its nature or escape, was observed (its regressing task created) no later than 7 days after completion. Each series is one crew reference as the run dispatched it, so `opus@medium` and `opus@high` are separate. Models recorded under one crew reference pool into that reference's single series and are listed on the series as metadata; a run with no recorded model adds to the crew's curve without adding a model. `complexity` (`low`, `medium`, `hard`, `xhard` or `unassessed`) narrows the cohort to one tier; any other value is a 400. The response carries the selection header plus `role`, `complexity`, `complexities` (each tier's task count before the filter), `follow_up_days` (7), `mature_by`, `min_bucket_tasks` (10), `min_series_tasks` (30), `sparse_outcomes` (5), `excluded` (`immature`, landed too recently; `no_size`, no recorded diff size) and `series`, largest first. Each series carries `crew`, `models`, `tasks`, `regressed`, `default_selected` (at least `min_series_tasks` tasks and a drawn bucket) and `buckets`, each with `bucket` (`floor(log2(1 + lines))`), `lines_from`, `lines_to`, `tasks`, `regressed`, `rate`, the Wilson 95% `low` and `high`, `drawn` (at least `min_bucket_tasks` tasks) and `sparse` (fewer than `sparse_outcomes` regressed tasks).

**Attribution** answers `window`, `requested_window`, `since`, `until`, `computed_at`, `cache_ttl_seconds`, `total`, `owners` and `charged`. `owners` maps each of `implementers`, `reviewers` and `pilots` to a count, plus `not_scored` as `{count, reasons}`. `charged` maps each role to its rows, each with `crew` (the first contributing crew), `crews`, `model`, `reasoning_effort`, `effort_label`, `label` and `problems`.

Every response carries an `Age` header: the seconds since its scan.

The dashboard ages the displayed scan independently of requests: its qualification becomes stale as soon as the reported cache window expires, including while refresh is pending or has failed. The scan status is a polite live region; ranks and selected detail stay in place, and request errors remain visible. The displayed age updates at least once a minute while the view retains that response. A newer successful response replaces the qualification; role, window and workspace changes discard the old timer.

Errors are 400 for:
- an unknown role or window;
- a zero `min_support`;
- an `execution` other than `managed`, `all` or `interactive`;
- a malformed `until`, or one in the future;
- a detail request without `crew`, naming a row the window does not contain, or naming several rows without the `model`/`effort` needed to pick one.

The dashboard reads only these routes. The `/api/orchestrators/*` routes were removed with Analytics › Orchestrators, and `/api/agents/orchestrators/*` with the orchestrators role [ORB-23582].

## 7. CLI and MCP

**Current.**

`orbit agents scorecard --role <role> [--window <w>] [--min-support <n>] [--execution <scope>] [--until <instant-or-duration>] [--json]` and `orbit agents show --role <role> --crew <crew> [--model <model>] [--effort <effort>] [--window <w>] [--min-support <n>] [--execution <scope>] [--until <instant-or-duration>] [--json]` print the same payloads as `/api/agents/<role>/scorecard` and `/api/agents/<role>/detail`. `--until` is the same optional window end as the API's `until`. The table view lists each row's rank (`below` under the minimum support, `unjudged` for an unjudged row, whose score shows `-`), row (`model @ effort`), crews, units, score and term counts. Stdout carries only those rows, so plain and piped output is tab-separated records (with `--header`, the column header first); the selection line, the units footer and the empty-state line go to stderr, and a role with no row prints nothing on stdout. `show` lists each term's weight, count, rate and contribution, then the tasks behind every nonzero count. Implementers also show the 100-point base and an expected-count column; their scorecard footer explains the same-tier reference. The read-only MCP tools `orbit.agents.scorecard` and `orbit.agents.show` take the same arguments as `role`, `window`, `min_support`, `execution`, `until` and, for `show`, `crew`, `row_model` and `row_effort`. The row's model travels as `row_model` because `model` is every tool's caller attribution. A role outside the three is refused, naming the choices. A crew with several rows in the window is refused without a model, naming the selector for each surface (`--model`, `row_model`, or the dashboard's `model` query parameter) and the candidate models. A crew-and-model selection that still names several rows takes an effort on each surface (`--effort`, `row_effort`, or the dashboard's `effort` query parameter) to narrow it to one model-and-effort row.

A missing row names only the caller's recovery controls: the CLI scorecard command and `--window`, the MCP `orbit_agents_scorecard` tool and `window` field, the `orbit.agents.scorecard` tool and `window` field for `orbit tool run`, or `/api/agents/<role>/scorecard` and the `window` query parameter for the dashboard API. Every refusal explains that a crew with no work in the window has no row.

Each of those scans once per call and reports `cache_ttl_seconds: 0`. The definitions live in `orbit-common`'s `governance::agents` registry; the tools call the runtime's role scorecard and detail methods, which scan and slice like the API without keeping the scan.

The deprecated `orbit orchestrator scorecard|show` command and `orbit.orchestrator.scorecard|show` tools were removed with the orchestrators role [ORB-23582], with no replacement; `orbit orchestrator` is now an unknown command.

## 8. Scan, Cache and Budget

**Current.**

One `AgentFacts` scan per workspace serves all three roles and the attribution strip. It reads:
- every task created or updated in the last 90 days, with its history and comments;
- the implementation job runs (owned and claimed PR and local delivery) active in that time, matched to the tasks their input names, read without their steps;
- the review gate's settlement and escalation comments, and, for each judged `reject`, the cited paths' blobs in the rejected and next reviewed candidates, read from the workspace repository without fetching;
- the `finding_verified` history of review findings filed as tasks;
- the `regression_from` links and failed claim settlements;
- task-pilot receipts and the codebase facts the pilot terms need (landed files, base-conflict blocks, lock denials with their holders, lock-wait episodes), from the incremental Analytics › Codebase scan (`codebase_scan`), whose stored per-task facts keep the pilot receipts. A failed codebase scan leaves the pilots role without rows and logs a warning rather than failing the other roles.

It reads at most 90 days back. `all` is served as 90 days, so the scan stays bounded however old the workspace gets.

The scan keeps its invocation accounting records immutable, with an in-memory
index by job run. A unit's effort lookup visits only that run's records, then
applies the same activity, model, task-link and settlement-time filters and
duration/end-time/id tie-break. Aggregate readers retain the store's original
record order. The index belongs to this scan, so refreshing the scan replaces
both records and index; it introduces no additional cache or freshness window.
This also bounds the repeated effort lookups made while rendering each model's
weekly crew-memory evidence pack.

**Count windows.** `last50`, `last200`, `last500` and `last1000` select by delivery attempts rather than time: the window starts at the start of the `N`th newest implementation attempt in the scan (the oldest when the scan holds fewer, the scan's reach when it holds none) and ends now. With `until` set, those attempts are the newest N whose start is before `until`, and the window ends at `until`. Every role slices its own unit from that start as for a calendar window. Window membership, attribution and the shared score are unchanged. They reach no further than the scan's 90 days, so a workspace idle for longer still shows nothing. The implementers `--by-type --shadow` report reads recorded crew draws rather than the scan and refuses a count window.

The server keeps one scan per workspace runtime for five minutes, single-flight. When the cached scan is stale, the next request is answered from it while one background refresh runs. A scan more than an hour past its five minutes is not served; the request waits for a new one. Changing the role, window or minimum support, or opening a detail, only slices the cached facts.

Budget: under 2 s for a warm request and under 15 s for a cold scan on the `ws_orbit` workspace. Measured by the core track on 2026-10-19 with a release build on dk-server-2, over about 6,450 tasks: cold scans of 4.8 s and 5.6 s in two runs, and warm requests under 4 ms. No incremental index is needed. The Analytics › Codebase scan already exceeded 40 s on this workspace, which is why the budget was fixed up front. The interval (§4.3a) adds 1,000 resamples per ranked row. A debug build's warm scorecard call over the synthetic 7,200-task fixture (6 crews × 5 tiers, 6 ranked rows) took 647 ms including fixture construction; no release build was measured on `ws_orbit`.

## 9. Testing

Tests follow the repository rules: behavior at public boundaries, no copy-matching, no pinned weights.

- **Core** (through the public runtime API, in the `orbit-web` HTTP fixture that also drives the routes): a seeded workspace with known tasks, runs, review reports, regressions and lock waits.
  - Each problem lands in exactly one owner or in Not scored.
  - A multi-crew task charges each crew for its own attempt.
  - Smoothing pulls a row above the fleet average down toward it and never raises one, and of two rows with the same raw score the larger ranks first.
  - Below-support rows sort last without a rank.
  - Regressions filed after the window still count.
  - A delivery-review finding lowers the score of an implementer no pre-landing review saw, and a defect both review layers report counts once.
  - Reviewer provisional counts follow the 30-day lookback.
  - A reject the next run proved wrong is the reviewer's false reject, a reject whose cited path changed holds its finding, and rejecting more without being right never raises a reviewer's score.
  - A pilot hold settles from history alone: a description edit before approval confirms it, an unedited approval delivered clean dismisses it, an approval after a bare-note edit is excluded, advisory findings never hold, and a superseded assessment gives way to the next. Warning terms are rated per 100 settled assessments and provisional below 10, and the missed-scope-block source reads unavailable until a block-triage marker is in the window.
- **API** (`orbit-web` HTTP tests): the payload contract, `not_computed`, the 400 cases, and that one scan serves role and window changes.
- **Dashboard**:
  - node DOM-stub tests and a Playwright fixture driven by contract-shaped JSON: ranking selection, detail, disclosure, empty and `not_computed` states, the Reviewers banner, `accepts often` tag and outcome bars, the redirect from `#analytics/orchestrators`, phone width;
  - screenshots of each changed view at desktop and 375 px.
- **CLI and MCP**: goldens regenerated for the new commands, tools and the alias notes.

## 10. Delivery Plan

| Track | Scope | Depends on | Files it owns |
|---|---|---|---|
| A | This design (`docs/design/agent-scorecards/`) | — | `docs/design/agent-scorecards/**`, `docs/design/CONVENTIONS.md` ownership row |
| B | Core and API: `application/agents/` (`facts/`, `attribution.rs`, `score.rs`, the role registry with all four roles, three of them `not_computed`), Orchestrators on the new score, `api/agents.rs`, cache. Landed. | A | `crates/orbit-core/src/application/agents/**`, `agents.rs` in the `orbit-web` API module, routes, `runtime_memo` |
| C | Dashboard shell: Agents destination, `#agents/<role>` routing and redirect, the generic renderer; removes Analytics › Orchestrators and the `/api/orchestrators/*` routes. Landed. | B | `assets/dashboard/js/agents.js`, `js/agents-orchestrators.js`, `css/agents.css`, `index.html`, `app.js`, removal of `js/orchestrators.js` and `api/orchestrators.rs` |
| D | Implementers role. Landed. The cost and tier panel was removed. | C | `application/agents/roles/implementers.rs` |
| E | Reviewers role, its scan additions (false rejects, incomplete reviews, findings rejected by finding verification), provisional banner and outcomes panel. Landed. | C | `application/agents/roles/reviewers.rs`, `js/agents-reviewers.js` |
| F | Pilots role, its scan additions (pilot receipts, footprint misses, lock waits) and calibration panel; moves the pilot ranking out of Codebase. Landed. | C | `application/agents/roles/pilots.rs`, `js/agents-pilots.js`, `application/codebase/pilot.rs`, `js/codebase.js` |
| G | CLI and MCP (`orbit agents`, `orbit.agents.*`, aliases); moves `orbit orchestrator` and `orbit.orchestrator.*` onto the new scorecard and removes `application/orchestrators/`. Landed. | C | `orbit-cli` and `orbit-tools` command and tool files, goldens |

```
A ── B ── C ─┬─ D
             ├─ E
             ├─ F
             └─ G
```

The tracks run in the order B → C → {D, E, F, G}. C waits for B because the dashboard cannot ship against routes that do not exist. D, E, F and G wait for C, because C removes the old orchestrator routes and view and supplies the shell their panels plug into. They then run in parallel: each adds its own files, and none edits the registry, because B registers all four roles. C renders `not_computed` roles as an empty state, so the destination ships before D–F.

Task records: track B is [ORB-16952], C [ORB-16953], D [ORB-16954] (its cost and tier panel removed by [ORB-17736]), E [ORB-16955], F [ORB-16956] and G [ORB-16957]. All six are landed; track A is this design itself.

Each track updates the docs its change makes stale in the same PR. That includes this design (moving the sections it implements from target to current), [user-interface 2_design.md](../user-interface/2_design.md) and the dashboard how-to page.

## 11. Concerns & Honest Limitations

- **Reviewer false alarms are proxies.** The review gate records no verdict on whether its own findings were right. `Disposed` findings and the false-reject rule (§5.3) stand in for that. The rule compares file contents, not intent: a cited file the next run changed for an unrelated reason, or that changed on the base between the two candidates, counts as addressed, and a finding fixed in a file it did not cite is not credited. A dedicated record of an implementer disputing a finding would be exact, but would change the review flow to serve a dashboard. It is deferred until the proxies prove noisy.
- **The observed tier inherits cost differences between crews.** Work routed to a costlier crew looks harder. Ranking by cost within the window reduces this but does not remove it, so calibration should be read as a comparison between pilots on similar work.
- **Footprint misses without a pilot are not charged.** When no pilot assessed a task, a landed file outside the filer's selectors counts against no one in v1. It could be a spec defect, but nothing records whether the file was in scope.
- **Pilot warnings are graded per assessment, not per finding.** An edit cannot be tied to one finding, so a hold with one right and one wrong warning counts once, as confirmed. History names only body documents in a field-edit note, so a context, dependency or relation fix after a hold is excluded rather than credited. Naming envelope fields in that note would let them count as scope edits. Overridden holds that then blocked are confirmed whatever the block's cause, since the block's cause is not tied to the warning either.
- **The escape decides the owner.** An implementer whose visible defects pass a weak reviewer is not charged for them; the reviewer is. Reading the Implementers and Reviewers tabs together is part of the design, not an accident.
- **Weights are judgment.** They are shown with every score and kept in one table per role so they can be changed in one place. Tests check how scores behave, not the weight values.
- **Provisional scores move.** Reviewer scores for the last 30 days can fall when regressions are filed. The banner says so, but a ranking read on a given day is not final.
- **Scores steer crew pools.** Operators will tune complexity pools on these rankings, which rewards whatever the terms measure. The terms favour outcomes recorded by other roles (reviews, regressions, waits) over what an agent says about itself, and nothing reads a self-report.
- **Pulled attempts are attributed from what their follower reported.** A machine that pulls a task runs it outside this host's job runs, so its attempt's crew, model and reasoning effort are the ones its follower reported when it settled the claim, kept on the claim's settlement record (§2). Nothing on the owner checks them, and they change no validation, settlement outcome or review-crew selection. A follower that predates the effort report sends none, and an owner that predates it ignores the field, so in the 30-day compatibility window such attempts stay effort-unknown. A follower that predates the report, or an attempt whose claim has not settled yet, has no model on that record: its attempt is charged to the crew the record or task history names, under `model unknown`, and is `crew unknown` when neither names one. Attempts settled before the report existed are not backfilled.

## Task References

- ORB-20000 — proposed advisory delivery-quality profile; no current score or routing changes.
- ORB-23823 — `orbit migrate --backfill-dispatch-effort` records dispatch effort for invocations that predate dispatch identities.
- ORB-16952 — track B: the core scan, one-owner attribution, shared score, role registry with Orchestrators computed, and the `/api/agents` routes and cache.
- ORB-16953 — track C: the Agents destination, its role tabs, the shared renderer and the orchestrators panel; retired Analytics › Orchestrators and `/api/orchestrators/*`.
- ORB-16954 — track D: the Implementers role (current) and cost/tier panel (removed by ORB-17736).
- ORB-17779 — implementer unit landed credit and same-tier fleet expectations replace complexity weighting and the unscored tier comparison.
- ORB-17736 — removed the Implementers score-against-cost and first-reviews role panel; the tab keeps its ranking, detail and disclosure.
- ORB-16955 — track E: the Reviewers role (current), its scan additions (false rejects judged from cited paths, incomplete reviews, findings rejected by finding verification), the provisional count, the banner, the `accepts often` tag and the outcomes panel.
- ORB-16956 — track F: the Pilots role, its lock-wait holders and calibration panel; removed the Task pilot ranking from Analytics › Codebase (current).
- ORB-16957 — track G: CLI and MCP (current).
- ORB-17118 — Orchestrators credit one point per landed task instead of weighting by self-assessed complexity, drop landed initiative and show unclassified rework and regressions beside the score; smoothing only lowers a score, with K raised from 10 to 30.
- ORB-17291 — orchestrator rows keyed by the task's `orchestrator_model`, recorded at creation from an explicit input or the orchestrator crew's configured model.
- ORB-19304 — one severity scale for findings and regression links (critical to nit, weights 8 to 0.25), verified severity scored, escaped and missed regressions weighted by it.
- ORB-17814 — the Implementers by-type matrix that scores each model-and-effort row per complexity tier and type.
- ORB-18078 — the `no-diff-expected` exclusion from Implementers units, credit, charges and fleet expectations.
- ORB-19629 — unscored escaped regressions with the severity-weighted residual and diff economy shown beside the score (superseded by ORB-21789).
- ORB-21789 — escaped regressions scored against a size-adjusted expectation, counted once when a pre-landing review already charged the defect.
- ORB-19644 — the strip ownership clarification that unscored `no_test` context still counts toward Implementers ownership.
- ORB-19633 — scoring an unverified review-job finding by its task's outcome (landed holds, open unjudged, drops neutral).
- ORB-20391 — derived managed versus interactive execution per task, so token metrics count managed runs only.
- ORB-20629 — made final recovery the single decider for blocked tasks and retired blocked-task triage, the only writer of the block-triage markers.
- ORB-21124 — split pilot modes into full assessments and targeted re-checks, reported under `pilot_modes`.
- ORB-23582 — removed the orchestrators role and its CLI, MCP and `/api/agents/orchestrators/*` surfaces; spec-caused problems became Not scored (`spec`).
- ORB-23642 — a final recovery's recorded fault decides a run exit's rework cause; an unjudged escalation is no longer implementer rework.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
