---
title: Agent Scorecards — Design
owner: claude
last_updated: 2026-10-24
last_validated: 2026-10-23
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

This document is the target design for the Agents view under Analytics: one scorecard per agent role, ranked by crew and model. It is a **Draft** that becomes the description of current behaviour as the delivery tracks in §10 land. It fixes the attribution rules, the shared score, each role's terms, and the API payload first, so that every track builds against a stable contract.

The core, dashboard and role tracks have landed. Sections marked **Current** describe the code: the destination and layout (§1), the ranking unit (§2), attribution (§3), the shared score (§4), the Orchestrators, Implementers, Reviewers and Pilots roles (§5.1–5.4), the API (§6), the CLI and MCP (§7) and the scan and cache (§8). Everything else is the target. The view itself is described in [user-interface 2_design.md §5c](../user-interface/2_design.md#5c-analytics--agents).

## 1. Destination and Layout

**Current**.

**Agents** is an Analytics view, listed in the Analytics sub-nav beside Metrics, Reliability, Codebase, Resources and Cost; there is no standalone Agents rail destination. Its secondary tabs are its roles: **Orchestrators** (`#analytics/agents/orchestrators`, the default), **Implementers** (`#analytics/agents/implementers`), **Reviewers** (`#analytics/agents/reviewers`) and **Pilots** (`#analytics/agents/pilots`). Each tab shows its count of ranked rows. Reopening Analytics from the rail returns to the remembered Analytics view, including Agents with its remembered role. Navigation changes:

- `#analytics/orchestrators` and old `#agents/<role>` links open `#analytics/agents/<role>` in place, keeping the window, so Back is not trapped.
- The Task pilot tile and its section leave Analytics › Codebase. The Codebase view keeps its path rankings, change coupling and lock contention.

Every role tab has the same layout:

1. **Who each problem counts against.** One bar across the window's problems, split by owning role plus a **Not scored** segment, with the current role highlighted and each legend entry linking to its role. This strip is how a reader sees the one-owner rule at work.
2. **Ranking.** One row per crew and model: rank, crew and model, support (the role's unit of work), four or five role-specific columns, and the score as a bar and number. A row is a button that selects it. Rows below the minimum support are greyed, tagged `below 10` and listed last.
3. **Detail** (right column) for the selected row: rank, score, raw score and the fleet average it was pulled toward, a **Score breakdown** (each term's weight, contribution and bar), **Biggest penalty** (the tasks behind the largest negative term, each linking to the task), and **Shown, not scored** (cost, time and other context).
4. **How the score works.** A disclosure listing every term with its weight and definition, plus the smoothing formula.
5. **One role-specific panel**, for the roles that have one. Orchestrators show where each orchestrator's rework went by cause. Implementers show no panel: the score-against-cost chart and first-reviews tier comparison were removed as a product decision, so the ranking, detail column and disclosure are the whole tab. Reviewers show how each reviewer's verdicts turned out, and flag an accept rate well above peers. Pilots show assessed tier against observed tier.

Reviewers also carry a **provisional** banner under the strip while any review in the window is younger than the 30-day miss lookback (§4.4). It counts those reviews against the window's total. A reviewer whose accept rate is well above its peers' carries an `accepts often` tag under its name in the ranking (§5.3). A role module supplies the banner and the row tags through optional hooks beside its panel.

The view follows the shared dashboard window: 1h, 24h, 7d, 30d or 90d. `all` is accepted and treated as 90d, and labelled `all (90d)` (§8). It is read-only and scoped to the selected workspace; the aggregate view shows the usual placeholder. It builds every node with `el()`. The dashboard is dark-only. The layout keeps its ranking table in an `overflow-x` box and stacks the detail column under the ranking at phone width.

## 2. Ranking Unit

**Current.**

A row is a **crew and model**: for implementers, the `resolved_crew` and `crew_model` of the job run that did the work (§3). Pilots use the activity’s frozen crew draw and recorded invocation model (§5.4); reviewer and orchestrator attribution is described in their role sections. Two models under one crew name are two rows, and one model under two crews is two rows.

- An implementation run with no `crew_model` is ranked as `model unknown` under its crew. It is never assigned a model guessed from the crew's configuration, because configuration changes over time and a guess would quietly merge different models into one row. The payload reports, per row, how many of its runs had no model.
- An attempt that no local job run records, such as one another machine pulled, is charged to the crew and model on the settlement record of the claim its `pulled_by` entry names (`claim_id`). The follower reports the leaf run's `resolved_crew` and `crew_model` with every settlement, handoff, failure or release, and the owner keeps them on that record. Without a record, or for a field it lacks, the crew falls back to the one task history had assigned when the attempt started, and the model is `model unknown`. The run is the one the `pulled_by` entry names. An attempt with neither a run, a recorded crew nor an assignment is `crew unknown`.
- **Orchestrators are the exception.** Orchestrator sessions are not job runs, so their row is keyed by the task's `orchestrator` attribution and its `orchestrator_model`. That model is recorded once, when the task is created with an orchestrator: an explicit `orchestrator_model` input, else the orchestrator crew's configured model snapshotted at that moment (`orchestrator_model_source` is `explicit` or `crew_config`). It is never read from configuration at scan time, so a later crew-config change does not move old tasks between rows. A task without one, including every task filed before the field existed, ranks under `model unknown` (§5.1).

## 3. Attribution: Each Problem Has One Owner

**Current** for every problem in §3.1.

Every problem counts against exactly one role, or against none (**Not scored**). Credit is separate: when one role's problem is caught or fixed by another, the catcher may earn credit (a reviewer's held finding), but the problem still counts once. The rule is recorded as [Each problem counts against exactly one role](./4_decisions.md#each-problem-counts-against-exactly-one-role).

### 3.1 Owner by problem

| Problem | Record | Owner | Charged to |
|---|---|---|---|
| Description or criteria edited after the first implementation run started | task-field history naming the document | Orchestrator | the task's orchestrator attribution |
| Agent blocker whose kind names a criterion, spec, requirement, scope or ambiguity | `task_blocked_by_agent` | Orchestrator | the task's orchestrator attribution |
| Final-recovery escalation citing the criteria | `final_recovery_escalated` | Orchestrator | the task's orchestrator attribution |
| Repeat run, `[BLOCKED]` PR, unmerged delivery PR or operator requeue with cause `spec` | history, failure handoff, PR refs | Orchestrator | the task's orchestrator attribution |
| The same rework with cause `crew` | same records | Implementer | the implementation run that ended the attempt |
| Review verdict `reject`, unless it is a false reject | review report | Implementer | the implementation run whose candidate was reviewed |
| Review verdict `accept_with_fixes` | review report | Implementer | the implementation run whose candidate was reviewed |
| Regression with escape `spec_ambiguous` | `regression_from` classification | Orchestrator | the culprit task's orchestrator attribution |
| Regression with escape `review_missed` | `regression_from` classification | Reviewer | the review run that accepted the culprit's landed candidate |
| Regression with escape `no_test` | `regression_from` classification | Implementer | the culprit's final implementation run |
| Regression with escape `test_not_selected`, `platform_only`, `nondeterministic` or `unclassified` | `regression_from` classification | Not scored | — |
| Finding closed by an authorized decision (`Disposed`) | review report | Reviewer | the review run that raised it |
| False reject (§5.3), counted instead of the implementer's reject | settlement comments, candidate commits | Reviewer | the review run that rejected |
| Reviewer fix that failed owner revalidation | gate escalation | Reviewer | the review run that made the fix |
| Finding filed as a task by a review job and REJECTED by finding verification | `finding_verified` history | Reviewer | the review job run that filed it |
| Review verdict `incomplete` | review report | Reviewer | the review run; Not scored (`infrastructure`) when run failure classification names the block that followed as infrastructure |
| Landed file outside every pilot selector that a base-conflict block or a refused lock request of the task named | codebase facts | Pilot | the pilot run behind the newest pilot receipt |
| Lock wait on a path the holder reserved through a selector that covered none of its landed files | lock-wait episodes, pilot receipt | Pilot | the holder task's pilot run |
| Rework with cause `infrastructure` or `other` | history | Not scored | — |

A regression's **escape** decides its owner, because the escape records which safeguard failed. Its **nature** decides only whether it is a regression the scorecards count at all. Every nature is counted except `merge_reintroduced` and `contract_drift`, which are not scored in v1. Two consequences follow:

- Orchestrator spec regressions become escape `spec_ambiguous` only. Today's scorecard also counts nature `incomplete` against the orchestrator. Under this design, an incomplete delivery whose escape is not `spec_ambiguous` counts against whichever role its escape names.
- An implementer is not charged for a defect review should have seen (`review_missed`). The reviewer is.

### 3.2 Which run

A problem counts against the run responsible for it, not the task's latest crew, so a task that moved between crews charges each crew only for its own attempt.

- **Implementation run**: the activity job run that produced the candidate. A problem recorded on a task is charged to the implementation run whose span (the run's start to the next status change, as `run_spans` computes it today) contains the problem's timestamp. If none does, it goes to the most recent run before it.
- **Final implementation run**: the run whose candidate landed. Landed credit, first-pass accepts, and `no_test` regressions go to it.
- **Review run**: the review gate's settlement comment for the attempt. Its reviewer line names the row's crew and model. The gate runs inside the implementation pipeline, so the charge names the implementation run whose span contains the settlement. A replayed settlement for the same attempt counts once.
- **Pilot run**: `pilot_run_id` on the newest task-pilot receipt. Older receipts without that field are matched as §5.4 describes (`codebase/pilot.rs`).

### 3.3 The attribution strip

`GET /api/agents/attribution` counts the window's problems by owner: `orchestrators`, `implementers`, `reviewers`, `pilots` and `not_scored`, with `not_scored` broken down by reason. Its totals are the sums of the problems behind the four scorecards, so the strip and the rankings always agree. It also lists each role's charged rows (crew, model, problems), including roles not yet computed.

The Not scored reasons are:
- `infrastructure` and `no_cause`: rework with those causes;
- each unscored escape, and the unscored natures `merge_reintroduced` and `contract_drift`;
- `unattributed`: an orchestrator problem on a task with no orchestrator attribution;
- `no_run`: an implementer problem on a task with no implementation attempt;
- `no_diff_expected`: an implementer problem on an attempt admitted as `no-diff-expected` work, which is not implementation (§5.2);
- `no_review`: a reviewer problem with no settled review to charge, or a REJECTED finding whose filing review job is unknown.

A problem falls in the window of its owner's unit of work: the task's creation for orchestrators, the charged attempt's start for implementers, the settlement for reviewers (the filing attempt's start for a review job), and the problem's own time when Not scored. A regression against a task outside the 90-day scan is not counted.

## 4. The Shared Score

**Current.**

One module computes every role's score. A role supplies its unit of work, its terms and their weights, and optionally the expected counts for its task mix.

### 4.1 Rates and the raw score

Each term's count is converted to a rate per 100 units of the role's work (§5). A role may give a term its own denominator instead: the pilots' warning terms are rated per 100 settled assessments, not per 100 assessed tasks (§5.4). Such a term also has its own minimum support; below it the term is **provisional**, rated and shown but left out of the raw score, so a handful of settlements cannot swing a rank. Every other term, and every other role, uses the role's units. For roles without expectations, the **raw score** is `Σ weight × rate` over the role's terms. Positive weights are credit; negative weights are problems. Weights live in one table per role, shown in the view and returned in the payload.

Implementers instead start at 100 and sum weighted actual-minus-expected rates for their tier mix (§5.2).

### 4.2 Smoothing toward the fleet average

The **fleet average** is the mean raw score over every row of the role in the window, weighted by each row's units. A row's **score** is:

```
score = min(raw, (units × raw + K × fleet_average) ÷ (units + K)),   K = 30
```

Smoothing only lowers a score. A row above the fleet average is pulled down toward it, the more the less work backs it, so three lucky tasks cannot top a ranking: a 40-unit row keeps 57% of its lead over the average and a 450-unit row 94%. A row at or below the average keeps its raw score. Pulling it up would let a small row rise on the rest of the fleet's evidence; under two-sided smoothing, two rows with the same raw score below the average ranked the smaller one first. Now a row never outranks a larger one with the same raw score: the larger one is pulled down less, or both keep their raw score and the tie goes to the row with more units.

Shrinking toward the average narrows a small row's lead but cannot reverse it: when a 40-unit row's raw score is above a 450-unit row's, it still ranks first. A raw advantage from what a row is credited for, rather than how well its work turned out, has to be removed in the terms; §5.1 removes the orchestrators' complexity weighting for this reason.

### 4.3 Minimum support

Rows with fewer than 10 units (`min_support`, default 10, at least 1) are `below_support`. They are greyed, listed after every supported row, and have no rank. Their scores are still computed and shown. A term with its own denominator (§4.1) has its own minimum support, which `min_support` does not change: the pilots' warning terms need 10 settled assessments (`warning_min_support`).

### 4.4 Windows and late evidence

A role's **unit** decides window membership (§5). Problems count whenever they are recorded, so a regression filed after the window still counts against the work in it. For reviewers this is the expected case: a `review_missed` regression is usually filed days later. A review younger than the 30-day **miss lookback** is **provisional**, measured from the scan. The payload reports each row's provisional count and the window's total (`provisional` beside `rows`), and the view shows the banner from §1. Other roles report `provisional` as null.

### 4.5 Cost is shown, never scored

Dollar cost, tokens and wall time are never terms. Every role shows them under **Shown, not scored**. Implementers previously plotted score against cost in a role panel; that panel was removed as a product decision, so cost now appears only under **Shown, not scored**. See [Cost is shown, never scored](./4_decisions.md#cost-is-shown-never-scored).

Today's per-8-active-hours rate toggle is dropped: active hours mean something only for orchestrators. Orchestrators show active hours under **Shown, not scored**.

## 5. Roles

### 5.1 Orchestrators

**Current.**

Unit: **owned tasks**, meaning tasks whose `orchestrator` attribution names the row, created in the window (unchanged).

Rows are keyed by `(orchestrator, orchestrator_model)`: the crew the task names and the model recorded when it was created (§2). A task with no recorded model counts in the orchestrator's `model unknown` row, so an orchestrator whose older tasks predate the field has two rows until those tasks leave the window. Orchestrator problems are charged to the same row as the task they fall on.

| Term | Weight | Definition |
|---|---|---|
| Landed | +1 | Landed tasks, one each whatever their complexity. |
| Spec defects | −1 | Description or criteria edited after the first run started, criteria blocks, criteria escalations, and regressions with escape `spec_ambiguous`. |
| Spec-caused rework | −1 | Repeat runs, `[BLOCKED]` or unmerged PRs, and operator requeues whose cause is `spec`. |

A row's raw score is the percent of its owned tasks that landed, less its spec problems per 100 owned tasks. A spec problem costs as much as a landing earns, so a task whose spec needed fixing earns nothing, and spec defects on a tenth of a row's tasks cost it 10 points on a scale where every row lands most of its work.

Why the credit ignores complexity:

- Complexity is assessed when a task is filed or piloted, so the orchestrator being ranked largely sets it. Weighting landed tasks 1/2/4/8 by complexity made the credit the mean complexity of a row's tasks × 100. On the 30-day `ws_orbit` data of 2026-10-20, that difference in task mix was the whole ranking, and rating more tasks `hard` raised a score with no better orchestration.
- The spec penalties counted on the same per-100 scale against a credit of about 260, so a row could double its spec problems without changing rank.
- Landed initiative was dropped. Origin is unknown on nearly every task (40 of 40 for one row), so the term was 0 for every row. Dropped initiative and unknown origin stay under **Shown, not scored**.

Rework with no recorded cause and regressions with escape `unclassified` count against no one (§3). They are not scored here either, because no cause covers base conflicts, manual blocks and blockers on outside dependencies, which are not the orchestrator's, and scoring them would charge problems the attribution strip lists as Not scored. They are shown beside the score as its confidence instead: the `unclassified` stat counts both on the row's owned tasks, the ranking prints `N unclassified` under the score, and the detail gives the rate per 100 owned tasks. A row with many of them is scored on less evidence than it appears.

Changes from the composite before the agent scorecards:

- Crew-caused rework, review rejects and failed reviewer fixes leave this score; §3 gives their owners.
- Spec regressions no longer count nature `incomplete` by itself.
- No compatibility column keeps the old composite, because a dashboard score is not an external contract. The how-to page says once where crew rework went.

The role panel shows each orchestrator's rework split into spec (counted), crew (moved to Implementers), infrastructure and no cause (both not scored). The payload's shown-not-scored `stats` carry `unclassified`, those four rework counts, unclassified regressions (`regressions_unclassified`), dropped initiative, unknown origin and active hours.

### 5.2 Implementers

**Current.**

Unit: **implemented tasks**, meaning tasks with at least one implementation run by the row's crew and model that started in the window. Tasks tagged `no-diff-expected` are not implementer work and are left out entirely: no units, credit, charges or fleet expectation, in the headline rows and the by-type cells alike. The tag is judged per attempt, as of the attempt's start, replayed from the task's `tags_changed` history, so a tag added after work began does not exempt that attempt and removing it does not retroactively count one. The tag alone decides: an untagged task that ends with no diff (a verified no-diff or already-landed handoff) still counts with its normal credits and charges. Implementer problems on an excluded attempt are Not scored with reason `no_diff_expected`, so the attribution strip and the ranking still agree. Tag edits made before writes recorded `tags_changed` entries cannot be reconstructed; those attempts read the task's current tags. A configured by-type entry that matches only on `no-diff-expected` keeps its cells, all empty. The Pilots role's own `no-diff-expected` exclusion (§5.4) is unchanged, and both roles read the one `is_no_diff_expected` predicate.

| Term | Weight | Definition |
|---|---|---|
| Landed | +1 | Landed tasks whose final implementation run is the row's, one point each at every complexity. |
| First-pass accepts | +1 | Reviewed tasks whose first review verdict on the row's candidate was `accept`. |
| Review rejects | −1 | `reject` verdicts on the row's candidates. |
| Reviewer repairs | −0.5 | `accept_with_fixes` verdicts on the row's candidates. |
| Crew-caused rework | −1 | Repeat runs, `[BLOCKED]` or unmerged PRs, and operator requeues with cause `crew`, charged to the run that ended the attempt. Review verdicts are not counted again here. |
| Escaped regressions | −2 | Regressions with escape `no_test` against tasks whose final implementation run is the row's. |

Every scored term uses a fleet expectation for the row's **same task mix**:

```
expected(row, term) = Σ tier [row_tasks(tier) × fleet_count(term, tier) / fleet_tasks(tier)]
raw = 100 + Σ term [weight(term) × (actual(row, term) − expected(row, term)) × 100 / row_tasks]
```

Positive credit above expectation raises the score; problems above expectation
lower it. A crew matching every reference rate has raw 100 whatever its tier
mix, including when tiers have different landing or first-pass acceptance
rates. The fleet includes the row itself and below-support and `model unknown`
rows. Its unit is a **row-task observation**: a task implemented by two crew/model
rows contributes once to each row and once to each corresponding fleet
observation. The denominator includes unreviewed and unlanded implemented tasks,
not just tasks with the outcome. Repeated rejects can exceed one per task.

**Sparse tiers:** a tier with fewer than 30 fleet observations joins the largest
tier's pool; ties for largest use the lexically first tier name. All sparse
tiers join that same pool, including unassessed. Supported tiers otherwise keep
their own rates. If all tiers are sparse, the pool is the entire fleet. Both
counts and denominators are pooled before division, so a rare tier cannot set
an extreme reference from one task, and no empty denominator is used. Within
that pool the comparison is approximate by design: exact tier-mix invariance
applies to the reference pools, not to distinct rates inside a sparse pool.
Pooling conserves expected counts across the fleet, so its units-weighted raw
average is 100. An empty fleet has no rows or fleet average.

This replaces complexity-weighted landed credit and unadjusted problem rates.
Complexity pools choose a crew's tier mix, so the old ranking largely measured
pool membership; rejects and repairs also grow with difficulty. After a month
of data, the tier comparison previously shown only as context now supplies the
scored expectation for **every** term. This extends the unit-landed principle
introduced for orchestrators by [ORB-17118] without changing other roles.
See [the decision](./4_decisions.md#compare-implementers-with-the-fleet-on-their-task-mix).

The public runtime scorecard and detail APIs return `status: ok`. Tasks count
once per crew and model, even if that row ran them more than once in the
window. First-pass credit uses the first settled review of that row's
candidates; a later accept does not erase an earlier reject. Review verdicts
enter their own terms once and are not added to crew rework. The shared
attribution assigns `review_missed` to reviewers and `no_test` to the final
implementer. Shrinkage remains `min(raw, …)` with K = 30; minimum support and
model attribution are unchanged.

The historical payload keys `weighted_landed` and `cost_per_weighted_landed`
remain for compatibility; both now use **unit landings**, with labels **Landed**
and **Cost per landed task (USD)**. Each implementer row adds `expected`, a map
of term keys to expected counts, and `score_base: 100`. Actual `counts` and
`per_100` remain actuals; detail term `contribution` is weight times the
actual-minus-expected rate, so `score_base + Σ contribution` equals raw before
rounding. Other roles have neither additive field and retain their formulas.
The former `tier_comparison` stat is removed: scored expectations supersede it.

Shown, not scored: cost per landed task, median completed run duration, and
tasks by tier. Cost sums the workspace's recorded invocations for the row's
delivery runs (including delivery review and repair activities), counting each
run once, using provider-reported cost when present and derived pricing
otherwise, from the same telemetry as `/cost`. It divides by this row's landed
task count. Missing run telemetry, an unpriced invocation, a truncated cost
scan, or no landed credit returns null, never zero. These are recorded costs as
of the scan, not a final bill. Pulled work without local cost records remains
unpriced.

Implementers have no role panel. The score-against-cost plot and per-tier
review table remain removed; the ranking, detail column and disclosure are the
whole tab. The existing **Score breakdown** shows the 100-point fleet reference
and each term's actual and expected counts beside its contribution. It adds no
new section or per-tier table.

**By-type matrix.** `orbit agents scorecard --role implementers --by-type`
(and its `--json`) splits the same evidence into one cell per `(crew, model,
tier, type)`, where the type comes from the ordered
`workflow.crew_performance_types` list: a task takes the first type whose
`tags` it carries or whose `paths` globs match its `context_files`, else
`other`. Each cell's raw score uses the formula above with the fleet
expectation taken from the same `(tier, type)` fleet cell; a `(tier, type)`
fleet cell under 30 observations falls back to the tier's rates. Cells are
small, so the matrix applies a two-sided, two-level shrinkage with K = 30:
`crew_mean = (n_tier × raw_tier + 30 × 100) / (n_tier + 30)`, then
`score = (n_cell × raw_cell + 30 × crew_mean) / (n_cell + 30)`. A cell with
no evidence scores its crew mean, and a crew with no tier evidence has crew
mean 100. The matrix is separate from the ranking: the ranking keeps its
one-sided `min(raw, …)` smoothing and is unchanged by `--by-type`, and the
dashboard shows no by-type section.

### 5.3 Reviewers

**Current.**

Unit: **settled reviews**, meaning review gate attempts by the row's crew and model whose verdict settled in the window. Review jobs that file findings as tasks count too: each attempt of a review job task that filed at least one finding (a bug tagged `code-review`, `delivery-code-review`, `security-review` or `qa-sweep`, linked `spawned_from` the job) is one unit, in the window of the attempt's start, under the attempt's crew and model.

| Term | Weight | Definition |
|---|---|---|
| Findings that held | +1 | Findings repaired by the reviewer and validated, open findings on a `reject` that the next implementation run changed a cited path to address, and review-job findings finding verification CONFIRMED; each weighted by severity: critical 8, high 4, medium 2, low 1. |
| Fixes that held | +1 | `accept_with_fixes` verdicts whose fixes passed owner revalidation and whose task has no regression with escape `review_missed`. |
| Rejected findings | −1 | Findings closed by an authorized decision (`Disposed`), and findings a review job filed as tasks that finding verification REJECTED. |
| False rejects | −2 | `reject` verdicts where the next implementation run changed none of the rejected findings' cited paths and the next review accepted. |
| Failed fixes | −1 | Reviewer fixes that failed owner revalidation and were escalated. |
| Missed regressions | −4 | Regressions with escape `review_missed` against tasks the row's review accepted, whenever filed. |
| Incomplete reviews | −0.5 | `incomplete` verdicts not caused by infrastructure. |

An accept rate never enters the score. A reviewer earns only for verdicts that turned out right. See [Score accuracy, never the volume of objections](./4_decisions.md#score-accuracy-never-the-volume-of-objections). The reject rate, the number of findings and cost are not terms either.

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
- accept rate, the share of gate settlements that passed (`accept` or `accept_with_fixes`), and the peers' accept rate on the same tiers. The peers' rate is the rate the other reviewers had on each of the row's task tiers (complexity, or unassessed), averaged over the row's reviews. A row 10 or more points above it has `accepts_often` true and is tagged `accepts often`.
- the five verdict outcomes below;
- median review minutes, from the settlement's reviewer runtime;
- the row's provisional count (the row's `provisional` field).

Cost per review is not shown: no record ties a review gate attempt's cost to its settlement.

The role panel, **How verdicts turned out**, splits each reviewer's settled reviews into: accepted and held up, caught a real problem (a finding or fix that held, or a CONFIRMED review-job finding), false alarm (a false reject, or a review job whose verified findings were all REJECTED), missed a regression (a passed review a `review_missed` regression came back to), and unresolved (not judged yet, incomplete, or fixes that failed).

### 5.4 Pilots

**Current.**

Unit: **assessed tasks**, meaning done tasks with a landed commit whose newest task-pilot receipt was written in the window. Bulk tasks (more than 12 landed files) and `no-diff-expected` tasks are left out. A row is the crew and model of the pilot run the newest receipt names (`pilot_run_id`); an older receipt without that field is matched to the one task-pilot run active when it was written, or among several, the one whose step outputs carry its operation id. The pilot crew comes from the run’s frozen `workflow.purpose_crews.task_pilot` draw, falling back to its `workflow.low_complexity_crews` draw for older runs. This is the same lookup readiness authentication failures and routine provider holds use. A missing draw or a receipt whose run is not found is ranked as `crew unknown`. The model is the latest recorded `task_pilot` activity invocation’s model, or `model unknown` when unavailable. Neither field falls back to the run’s `resolved_crew` or `crew_model`, which describe the piloted tasks. Crew state and invocation models are read in batches without loading run steps.

The pilot's selectors are the newest receipt's `context_files_after`; the filer's are the oldest receipt's `context_files_before`. Both are graded against the files the landed commit changed. Recall is landed files a selector covers ÷ landed files; precision is selectors that cover a landed file ÷ selectors; F1 is their harmonic mean. A `dir:` selector covers the files beneath it and a wildcard selector the files it matches. A mirrored or generated output counts as covered when a selector covers its source: `plugin/skills/**` through `crates/orbit-core/assets/skills/**`, the CLI help, output and MCP snapshot goldens through any CLI, MCP, tools or governance source, and `docs/INDEX.md` through any `docs/` file. These rules live in `application/agents/roles/pilots.rs`; Analytics › Codebase no longer grades pilots.

| Term | Weight | Definition |
|---|---|---|
| F1 lift | +1 | F1 points gained over the filer's own selectors from before any pilot ran, per task. A task's lift can be negative. |
| Tier calibration | +0.25 | Tasks whose assessed complexity matches their observed tier, so per 100 tasks the percent on the tier. |
| Costly misses | −1 | Landed files no pilot selector covered that a base-conflict block of the task, or a refused lock request of the task, named. |
| Confirmed warnings | +3 | Held assessments the records proved right (below), per 100 settled assessments. |
| Dismissed warnings | −1 | Held assessments overridden as harmless (below), per 100 settled assessments. |
| Missed scope blocks | −3 | Clean assessments whose task was promoted, then blocked on a decision block triage proposed, per 100 settled assessments. |
| Lock-wait hours | −1 | Hours other tasks waited, in lock-wait episodes, on paths the holder reserved through a pilot selector that covered none of the holder's landed files. |

A lock-wait episode (Analytics › Codebase's lock contention) knows its holders: each refused request records which task or reservation held each conflicting path, and a reservation resolves to the tasks it named. A wait is charged to a holder's pilot when at least one of the holder's pilot selectors reserves the path and none of those selectors covered a landed file. A wait on a path that a useful selector reserved is not charged, and neither is a wait on a holder no pilot assessed. Both pilot problems count against the pilot run of the holder's newest receipt, in the window that receipt falls in.

The **observed tier** ranks the window's assessed tasks that have an assessed tier (low, medium, hard or xhard) and a total implementation cost (every attempt's run priced) by that cost, cheapest first, and cuts the ranking into tiers in the same proportions the pilots assigned. The observed column totals therefore equal the assessed row totals. Calibration measures whether a pilot orders work correctly, not whether it guesses absolute cost (§11). Tasks without a tier or a priced implementation are not placed. Cost decides only the observed tier and is never a term.

**Pilot warnings.** F1 lift, tier calibration, costly misses and lock-wait hours grade done tasks; the three warning terms grade every assessment, held or clean, by how it settled. They live in `application/agents/roles/pilot_warnings.rs`. An assessment is one task-pilot receipt: the audit comment the apply step seals with the assessment it persisted. It is **held** when that assessment has at least one finding that holds promotion (`blocked_by`, `adr_conflicts`, `utility_warnings`, `surface_warnings` or `validation_tool_warnings`), and **clean** otherwise. A finding marked `holds_promotion: false` is advisory and never counts. A `blocked_by` finding is judged against the task's current dependency edges, never against which prerequisites are done now: the done state at assessment time is not stored, and a prerequisite finishing later must not turn a past hold clean. The receipt says only whether the assessment held. How it settled comes from task history (edits and status transitions) and block-triage markers, never from agent output.

A **scope edit** is an `updated` history entry written by someone other than `task-pilot` or `system` whose note names `description` or `acceptance_criteria`. A **bare-note edit** is one whose note names no document. Every envelope-only write leaves that bare note: context files, dependencies and relations, but also tags, title, priority and crew. So history cannot tell a scope fix from bookkeeping there.

Only an assessment written while the task was `proposed` is graded. The **window** of an assessment runs until the next assessment of the same task or until the task leaves `proposed`, whichever comes first.

| Outcome | When |
|---|---|
| Confirmed | Within the window, a scope edit, or the task is rejected or archived. Or the task is approved with no scope edit and no bare-note edit in the window, and its delivery then blocks or the task is rejected. |
| Dismissed | Approved with no scope edit and no bare-note edit, then `done` with no block. |
| Missed scope block (clean assessments only) | Promoted, then a `blocked_task_triaged` marker records disposition `decision` (a `propose` from `orbit.task.block_triage` or the blocked-task triage routine). |
| Settled clean (clean assessments only) | Promoted, then `done`, rejected or archived with no such marker. Counts in the denominator only. |
| Excluded | Approved after a bare-note edit with no scope edit: neither credit nor charge, since it may be a dependency fix that answered the warning or only a tag change. Superseded by a later assessment with no scope edit in between (the later one is graded). Written outside `proposed`. Held and archived after approval. A clean assessment rejected or archived while still `proposed`. |
| Pending | Still `proposed`, or the delivery has not finished. Reported with the excluded ones. |

An outcome falls in the window in which the entry that settled it was written. The warning terms are rated per 100 **settled assessments**: confirmed, dismissed, missed and settled-clean ones. Excluded and pending assessments are not settled. A row with fewer than 10 settled assessments (`warning_min_support`) shows its three warning terms as provisional, outside the score (§4.1). At about 10 holds a day across crews, the warning terms of a 24-hour window are mostly provisional.

Why these weights: at 3 : 1 a crew's warnings are net positive above 25% precision, and the evidence that motivated the terms measured about 55%. A hold costs one operator read, while a confirmed warning saves a failed delivery run. A missed scope block mirrors the confirmed credit, so staying silent is never free.

Missed scope blocks have one source: block-triage markers. The blocked-task triage routine is disabled by default. When no `blocked_task_triaged` marker of any disposition falls in the window, the term reads 0 and the scorecard's `sources.missed_scope_blocks` reports `available: false`, with the reason. The view shows that as a banner, so the zero is not read as no misses.

Each assessment counts against the crew and model of the pilot run its receipt names, through the same attribution as graded tasks (above), never the task's own crew. A row can therefore have settled assessments but no graded done task (`units` 0).

The warning terms are not problems in the attribution strip (§3.3). They grade the pilot's warnings and do not reassign the block or edit, which keeps its own owner.

Shown, not scored (`stats`): recall, precision and F1 pooled over the row's tasks, the filer's F1, landed files no pilot selector covered, unused selectors, selectors per task, the share of placed tasks assessed within one tier, cost per assessment (null when a pilot run is missing or unpriced), median pilot run duration, `tier_matrix`, warning precision (confirmed ÷ (confirmed + dismissed), null with neither), holds per 100 assessments written in the window, and the settled, excluded-or-pending and missed-scope-block counts.

`tier_matrix` is `{tiers, rows, placed, not_placed, on_tier, too_easy, too_hard}`: `tiers` lists the tiers present in the window, cheapest first; each of `rows` is `{assessed, observed}` with one count per tier in `tiers`. Too easy counts tasks assessed below their observed tier and too hard those assessed above it.

The role panel (`js/agents-pilots.js`) is that assessed-by-observed tier matrix for every pilot, the diagonal highlighted, with on-tier, too-easy, too-hard and not-placed counts. The detail column leaves structured stats to the panel. The ranking and detail column render the warning terms like any other. A term with its own unit names it in its header tooltip and detail line (`4 of 20 settled assessments`), and a provisional term is shown in italics and adds nothing to the breakdown. The panel adds the unmeasured-source banner and a "How the score works" paragraph on settlement.

A landed file outside the declared context on a task no pilot assessed is not charged to anyone in v1 (§11).

## 6. API Contract

**Current.**

All routes are GET, scoped by `workspace`, follow the host picker through `/api/on/<host>/…`, and answer only GET.

```
GET /api/agents/{role}/scorecard?window=&min_support=
GET /api/agents/{role}/detail?window=&min_support=&crew=&model=
GET /api/agents/attribution?window=
```

`{role}` is `orchestrators`, `implementers`, `reviewers` or `pilots`. For orchestrators, `crew` names the orchestrator attribution and `model` the task's recorded `orchestrator_model`. `model` is the literal `model unknown` for an unknown model. It may be omitted when the crew has one row, and is required when it has several.

**Scorecard** answers:

- `role`, `window`, `requested_window`, `since`, `min_support`, `computed_at`, `cache_ttl_seconds`, `tasks_scanned`. `window` is the window served: `all` is requested as `all` and served as `90d`;
- `status`: `ok`, or `not_computed` while a role's track has not landed. A `not_computed` role still returns its `terms`, with empty `rows`.
- `unit` (label of the role's unit);
- `fleet_average`;
- `terms`: `key`, `label`, `weight`, `definition`, `group`, and `unit` on a term rated per 100 of its own unit (the pilots' warning terms);
- `provisional`: for reviewers, the rows' provisional reviews in total; null for the other roles;
- `rows`, best first;
- pilots only: `warning_min_support`, and `sources.missed_scope_blocks` as `{available, triage_records, detail}`.

Each row carries `crew`, `model`, `units`, `below_support`, `rank` (null below support), `counts` and `per_100` (term key to number), `raw`, `score`, `runs_without_model`, `provisional` (reviewers), and `stats` (the not-scored context: key, label, value). A row of a role with per-term denominators (pilots) also carries `term_units` (term key to its own units) and `provisional_terms` (the keys below their own minimum support).

**Detail** carries the scorecard header, `fleet_average`, `provisional`, the selected `row`, and `terms`. Each term carries its key, label, weight, group, count, `per_100`, contribution and `tasks`, largest count first, plus `units` and `provisional` when it has its own denominator. Each task carries `task_id`, `title`, `status`, `count` and `occurrences` (`at`, `detail`, `run_id`).

**Attribution** answers `window`, `requested_window`, `since`, `computed_at`, `cache_ttl_seconds`, `total`, `owners` and `charged`. `owners` maps each of `orchestrators`, `implementers`, `reviewers` and `pilots` to a count, plus `not_scored` as `{count, reasons}`. `charged` maps each role to its rows, each with `crew`, `model` and `problems`.

Every response carries an `Age` header: the seconds since its scan.

Errors are 400 for:
- an unknown role or window;
- a zero `min_support`;
- a detail request without `crew`, or naming a row the window does not contain.

The dashboard reads only these routes. The `/api/orchestrators/*` routes were removed with Analytics › Orchestrators.

## 7. CLI and MCP

**Current.**

`orbit agents scorecard --role <role> [--window <w>] [--min-support <n>] [--json]` and `orbit agents show --role <role> --crew <crew> [--model <model>] [--window <w>] [--min-support <n>] [--json]` print the same payloads as `/api/agents/<role>/scorecard` and `/api/agents/<role>/detail`. The table view lists each row's rank (`below` under the minimum support), crew, model, units, score and term counts. Stdout carries only those rows, so plain and piped output is tab-separated records (with `--header`, the column header first); the selection line, the units footer and the empty-state line go to stderr, and a role with no row prints nothing on stdout. `show` lists each term's weight, count, rate and contribution, then the tasks behind every nonzero count. Implementers also show the 100-point base and an expected-count column; their scorecard footer explains the same-tier reference. The read-only MCP tools `orbit.agents.scorecard` and `orbit.agents.show` take the same arguments as `role`, `window`, `min_support` and, for `show`, `crew` and `row_model`. The row's model travels as `row_model` because `model` is every tool's caller attribution. A role outside the four is refused, naming the choices. A crew with several rows in the window is refused without a model, naming the selector for each surface (`--model`, `row_model`, or the dashboard's `model` query parameter) and the candidate models.

Each of those scans once per call and reports `cache_ttl_seconds: 0`. The definitions live in `orbit-common`'s `governance::agents` registry; the tools call `OrbitRuntime::agent_role_scorecard` and `agent_role_detail`, which scan and slice like the API without keeping the scan.

`orbit orchestrator scorecard|show` and `orbit.orchestrator.scorecard|show` remain for one release as aliases of `--role orchestrators` (`show <name>` is `--crew <name>`, and `--model` or `row_model` names the row's model as on `orbit agents show`, so a crew with several recorded models stays selectable). They answer the agents payload with a `deprecation` field naming the replacement, and the command prints that note to stderr, so `--json` stays parseable. They are hidden from `orbit --help` and are removed in the following release.

## 8. Scan, Cache and Budget

**Current.**

One `AgentFacts` scan per workspace serves all four roles and the attribution strip. It reads:
- every task created or updated in the last 90 days, with its history and comments;
- the implementation job runs (owned and claimed PR and local delivery) active in that time, matched to the tasks their input names, read without their steps;
- the review gate's settlement and escalation comments, and, for each judged `reject`, the cited paths' blobs in the rejected and next reviewed candidates, read from the workspace repository without fetching;
- the `finding_verified` history of review findings filed as tasks;
- the `regression_from` links and failed claim settlements;
- task-pilot receipts and the codebase facts the pilot terms need (landed files, base-conflict blocks, lock denials with their holders, lock-wait episodes), from the incremental Analytics › Codebase scan (`codebase_scan`), whose stored per-task facts keep the pilot receipts. A failed codebase scan leaves the pilots role without rows and logs a warning rather than failing the other roles.

It reads at most 90 days back. `all` is served as 90 days, so the scan stays bounded however old the workspace gets.

The server keeps one scan per workspace runtime for five minutes, single-flight, like the orchestrator scan. When the cached scan is stale, the next request is answered from it while one background refresh runs. A scan more than an hour past its five minutes is not served; the request waits for a new one. Changing the role, window or minimum support, or opening a detail, only slices the cached facts.

Budget: under 2 s for a warm request and under 15 s for a cold scan on the `ws_orbit` workspace. Measured by the core track on 2026-10-19 with a release build on dk-server-2, over about 6,450 tasks: cold scans of 4.8 s and 5.6 s in two runs, and warm requests under 4 ms. No incremental index is needed. The Analytics › Codebase scan already exceeded 40 s on this workspace, which is why the budget was fixed up front.

## 9. Testing

Tests follow the repository rules: behavior at public boundaries, no copy-matching, no pinned weights.

- **Core** (through the public runtime API, in the `orbit-web` HTTP fixture that also drives the routes): a seeded workspace with known tasks, runs, review reports, regressions and lock waits.
  - Each problem lands in exactly one owner or in Not scored.
  - A multi-crew task charges each crew for its own attempt.
  - Smoothing pulls a row above the fleet average down toward it and never raises one, and of two rows with the same raw score the larger ranks first.
  - Orchestrator credit does not grow with assessed complexity, spec defects on a tenth of a row's tasks lower its rank, and unclassified regressions are shown, not scored.
  - Below-support rows sort last without a rank.
  - Regressions filed after the window still count.
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
| D | Implementers role and its panel | C | `application/agents/roles/implementers.rs`, `js/agents-implementers.js` |
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

Each track updates the docs its change makes stale in the same PR. That includes this design (moving the sections it implements from target to current), [user-interface 2_design.md](../user-interface/2_design.md) and the dashboard how-to page.

## 11. Concerns & Honest Limitations

- **Reviewer false alarms are proxies.** The review gate records no verdict on whether its own findings were right. `Disposed` findings and the false-reject rule (§5.3) stand in for that. The rule compares file contents, not intent: a cited file the next run changed for an unrelated reason, or that changed on the base between the two candidates, counts as addressed, and a finding fixed in a file it did not cite is not credited. A dedicated record of an implementer disputing a finding would be exact, but would change the review flow to serve a dashboard. It is deferred until the proxies prove noisy.
- **Open tasks lower an orchestrator's land rate.** A task created in the window earns nothing until it lands, so an orchestrator that filed many tasks recently reads lower until they land. Every row is measured the same way, and a longer window reduces the effect.
- **Orchestrator credit ignores how hard the work was.** One landing counts the same at every complexity, because the ranked orchestrator sets complexity. A complexity assessed independently of the orchestrator could weigh landings again.
- **Orchestrator models are recorded, not observed.** Orchestrator sessions are not job runs, so a row's model is what the task recorded at creation: the orchestrator crew's configured model unless the filer passed `orchestrator_model`. A session that runs another model without saying so is ranked under its crew's model. The field is set once, so changing a task's `orchestrator` later keeps the model recorded for the original crew, and a task that gains its orchestrator after creation stays `model unknown`. Tasks filed before the field existed are never backfilled.
- **The observed tier inherits cost differences between crews.** Work routed to a costlier crew looks harder. Ranking by cost within the window reduces this but does not remove it, so calibration should be read as a comparison between pilots on similar work.
- **Footprint misses without a pilot are not charged.** When no pilot assessed a task, a landed file outside the filer's selectors counts against no one in v1. It could be an orchestrator spec defect, but nothing records whether the file was in scope.
- **Pilot warnings are graded per assessment, not per finding.** An edit cannot be tied to one finding, so a hold with one right and one wrong warning counts once, as confirmed. History names only body documents in a field-edit note, so a context, dependency or relation fix after a hold is excluded rather than credited. Naming envelope fields in that note would let them count as scope edits. Overridden holds that then blocked are confirmed whatever the block's cause, since the block's cause is not tied to the warning either.
- **The escape decides the owner.** An implementer whose visible defects pass a weak reviewer is not charged for them; the reviewer is. Reading the Implementers and Reviewers tabs together is part of the design, not an accident.
- **Weights are judgment.** They are shown with every score and kept in one table per role so they can be changed in one place. Tests check how scores behave, not the weight values.
- **Provisional scores move.** Reviewer scores for the last 30 days can fall when regressions are filed. The banner says so, but a ranking read on a given day is not final.
- **Scores steer crew pools.** Operators will tune complexity pools on these rankings, which rewards whatever the terms measure. The terms favour outcomes recorded by other roles (reviews, regressions, waits) over what an agent says about itself, and nothing reads a self-report.
- **Pulled attempts are attributed from what their follower reported.** A machine that pulls a task runs it outside this host's job runs, so its attempt's crew and model are the ones its follower reported when it settled the claim, kept on the claim's settlement record. Nothing on the owner checks them. A follower that predates the report, or an attempt whose claim has not settled yet, has no model on that record: its attempt is charged to the crew the record or task history names, under `model unknown`, and is `crew unknown` when neither names one. Attempts settled before the report existed are not backfilled.

## Task References

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

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
