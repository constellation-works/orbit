---
title: Agent Scorecards — Design
owner: claude
last_updated: 2026-10-18
last_validated: 2026-10-18
status: Draft
feature: agent-scorecards
doc_role: design
type: design
summary: Target design for the Agents dashboard destination; covers problem attribution, the shared score, per-role terms, the API, CLI and MCP contract, scan budget and delivery tracks.
tags: [agent-scorecards, dashboard, analytics]
paths: ["crates/orbit-core/src/application/orchestrators/**", "crates/orbit-core/src/application/codebase/pilot.rs", "crates/orbit-core/src/application/codebase/lock_waits.rs", "crates/orbit-web/src/api/orchestrators.rs", "crates/orbit-web/assets/dashboard/**"]
related_features: [user-interface, review-gate, finding-verification]
related_artifacts: []
---

# Agent Scorecards — Design

This document is the target design for the Agents destination: one scorecard per agent role, ranked by crew and model. It is a **Draft**. Nothing here is implemented yet, and it becomes the description of current behaviour as the delivery tracks in §10 land. It fixes the attribution rules, the shared score, each role's terms, and the API payload first, so that the backend and dashboard tracks can be built in parallel against a stable contract. Until then, the live behaviour is the orchestrator scorecard in [user-interface 2_design.md §5c](../user-interface/2_design.md#5c-analytics--orchestrators) and the Task pilot ranking in Analytics › Codebase. Both are replaced here.

## 1. Destination and Layout

**Agents** is a top-level Observe destination beside Health and Analytics. Its secondary tabs are its roles: **Orchestrators** (`#agents/orchestrators`, the default), **Implementers** (`#agents/implementers`), **Reviewers** (`#agents/reviewers`) and **Pilots** (`#agents/pilots`). Each tab shows its count of ranked rows. Navigation changes:

- `#analytics/orchestrators` opens `#agents/orchestrators`, and Analytics loses its Orchestrators view.
- The Task pilot tile and its section leave Analytics › Codebase. The Codebase view keeps its path rankings, change coupling and lock contention.

Every role tab has the same layout:

1. **Who each problem counts against.** One bar across the window's problems, split by owning role plus a **Not scored** segment, with the current role highlighted and each legend entry linking to its role. This strip is how a reader sees the one-owner rule at work.
2. **Ranking.** One row per crew and model: rank, crew and model, support (the role's unit of work), four or five role-specific columns, and the score as a bar and number. A row is a button that selects it. Rows below the minimum support are greyed, tagged `below 10` and listed last.
3. **Detail** (right column) for the selected row: rank, score, raw score and the fleet average it was pulled toward, a **Score breakdown** (each term's weight, contribution and bar), **Biggest penalty** (the tasks behind the largest negative term, each linking to the task), and **Shown, not scored** (cost, time and other context).
4. **How the score works.** A disclosure listing every term with its weight and definition, plus the smoothing formula.
5. **One role-specific panel.** Orchestrators show where each orchestrator's rework went by cause. Implementers show score against cost, with the frontier of crews nothing beats on both, plus first reviews needing changes compared with the tier average. Reviewers show how each reviewer's verdicts turned out, and flag an accept rate well above peers. Pilots show assessed tier against observed tier.

Reviewers also carry a **provisional** banner while any review in the window is younger than the 30-day miss lookback (§4.4).

The view follows the shared dashboard window: 1h, 24h, 7d, 30d or 90d. `all` is accepted and treated as 90d, and labelled that way (§8). It is read-only and scoped to the selected workspace; the aggregate view shows the usual placeholder. It builds every node with `el()`. The dashboard is dark-only. The layout keeps its ranking table in an `overflow-x` box and stacks the detail column under the ranking at phone width.

## 2. Ranking Unit

A row is a **crew and model**: the `resolved_crew` and `crew_model` of the job run that did the work (§3). Two models under one crew name are two rows, and one model under two crews is two rows.

- A run with no `crew_model` is ranked as `model unknown` under its crew. It is never assigned a model guessed from the crew's configuration, because configuration changes over time and a guess would quietly merge different models into one row. The payload reports, per row, how many of its runs had no model.
- **Orchestrators are the exception.** Orchestrator sessions are not job runs, so their row is keyed by the task's `orchestrator` attribution, and the model is `model unknown` unless the attribution comes to carry one (§11).

## 3. Attribution: Each Problem Has One Owner

Every problem counts against exactly one role, or against none (**Not scored**). Credit is separate: when one role's problem is caught or fixed by another, the catcher may earn credit (a reviewer's held finding), but the problem still counts once. The rule is recorded as [Each problem counts against exactly one role](./4_decisions.md#each-problem-counts-against-exactly-one-role).

### 3.1 Owner by problem

| Problem | Record | Owner | Charged to |
|---|---|---|---|
| Description or criteria edited after the first implementation run started | task-field history naming the document | Orchestrator | the task's orchestrator attribution |
| Agent blocker whose kind names a criterion, spec, requirement, scope or ambiguity | `task_blocked_by_agent` | Orchestrator | the task's orchestrator attribution |
| Final-recovery escalation citing the criteria | `final_recovery_escalated` | Orchestrator | the task's orchestrator attribution |
| Repeat run, `[BLOCKED]` PR, unmerged delivery PR or operator requeue with cause `spec` | history, failure handoff, PR refs | Orchestrator | the task's orchestrator attribution |
| The same rework with cause `crew` | same records | Implementer | the implementation run that ended the attempt |
| Review verdict `reject` | review report | Implementer | the implementation run whose candidate was reviewed |
| Review verdict `accept_with_fixes` | review report | Implementer | the implementation run whose candidate was reviewed |
| Regression with escape `spec_ambiguous` | `regression_from` classification | Orchestrator | the culprit task's orchestrator attribution |
| Regression with escape `review_missed` | `regression_from` classification | Reviewer | the review run that accepted the culprit's landed candidate |
| Regression with escape `no_test` | `regression_from` classification | Implementer | the culprit's final implementation run |
| Regression with escape `test_not_selected`, `platform_only`, `nondeterministic` or `unclassified` | `regression_from` classification | Not scored | — |
| Finding closed by an authorized decision (`Disposed`) | review report | Reviewer | the review run that raised it |
| False reject (§5.3) | review reports, run diffs | Reviewer | the review run that rejected |
| Reviewer fix that failed owner revalidation | gate escalation | Reviewer | the review run that made the fix |
| Finding filed as a task by a review job and REJECTED by finding verification | `finding_verified` history | Reviewer | the review job run that filed it |
| Landed file outside every selector that caused a footprint block or lock conflict | codebase facts | Pilot | the pilot run behind the newest pilot receipt |
| Lock wait on a path the holder reserved through a selector that covered none of its landed files | lock-wait episodes, pilot receipt | Pilot | the holder task's pilot run |
| Rework with cause `infrastructure` or `other` | history | Not scored | — |

A regression's **escape** decides its owner, because the escape records which safeguard failed. Its **nature** decides only whether it is a regression the scorecards count at all. Every nature is counted except `merge_reintroduced` and `contract_drift`, which are not scored in v1. Two consequences follow:

- Orchestrator spec regressions become escape `spec_ambiguous` only. Today's scorecard also counts nature `incomplete` against the orchestrator. Under this design, an incomplete delivery whose escape is not `spec_ambiguous` counts against whichever role its escape names.
- An implementer is not charged for a defect review should have seen (`review_missed`). The reviewer is.

### 3.2 Which run

A problem counts against the run responsible for it, not the task's latest crew, so a task that moved between crews charges each crew only for its own attempt.

- **Implementation run**: the activity job run that produced the candidate. A problem recorded on a task is charged to the implementation run whose span (the run's start to the next status change, as `run_spans` computes it today) contains the problem's timestamp. If none does, it goes to the most recent run before it.
- **Final implementation run**: the run whose candidate landed. Landed credit, first-pass accepts, and `no_test` regressions go to it.
- **Review run**: the review job run named by the review report's attempt (`run_id` in the gate's settlement).
- **Pilot run**: `pilot_run_id` on the newest task-pilot receipt. Older receipts without that field are matched the way `codebase/pilot.rs` matches them today.

### 3.3 The attribution strip

`GET /api/agents/attribution` counts the window's problems by owner: `orchestrators`, `implementers`, `reviewers`, `pilots` and `not_scored`, with `not_scored` broken down by reason (`infrastructure`, `no_cause`, and each unscored escape). Its totals are the sums of the problems behind the four scorecards, so the strip and the rankings always agree.

## 4. The Shared Score

One module computes every role's score. A role supplies its unit of work, its terms and their weights. Nothing else is role-specific.

### 4.1 Rates and the raw score

Each term's count is converted to a rate per 100 units of the role's work (§5). The **raw score** is `Σ weight × rate` over the role's terms. Positive weights are credit; negative weights are problems. Weights live in one table per role, shown in the view and returned in the payload.

### 4.2 Smoothing toward the fleet average

The **fleet average** is the mean raw score over every row of the role in the window, weighted by each row's units. A row's **score** is:

```
score = (units × raw + K × fleet_average) ÷ (units + K),   K = 10
```

A row backed by little work is pulled toward the average, so three lucky tasks cannot top a ranking. A row's influence grows with its evidence.

### 4.3 Minimum support

Rows with fewer than 10 units (`min_support`, default 10, at least 1) are `below_support`. They are greyed, listed after every supported row, and have no rank. Their scores are still computed and shown.

### 4.4 Windows and late evidence

A role's **unit** decides window membership (§5). Problems count whenever they are recorded, so a regression filed after the window still counts against the work in it. For reviewers this is the expected case: a `review_missed` regression is usually filed days later. A review younger than the 30-day **miss lookback** is **provisional**. The payload reports each row's provisional count and the window's total, and the view shows the banner from §1.

### 4.5 Cost is shown, never scored

Dollar cost, tokens and wall time are never terms. Implementers show them as a score-against-cost panel, and every role shows them under **Shown, not scored**. See [Cost is shown, never scored](./4_decisions.md#cost-is-shown-never-scored).

Today's per-8-active-hours rate toggle is dropped: active hours mean something only for orchestrators. Orchestrators show active hours under **Shown, not scored**.

## 5. Roles

### 5.1 Orchestrators

Unit: **owned tasks**, meaning tasks whose `orchestrator` attribution names the row, created in the window (unchanged).

| Term | Weight | Definition |
|---|---|---|
| Weighted landed | +1 | Landed tasks weighted by complexity: low 1, medium 2, hard 4, xhard 8, unassessed or unset 1. |
| Landed initiative | +1 | Landed tasks whose typed origin is `orchestrator_initiative`. |
| Spec defects | −1 | Description or criteria edited after the first run started, criteria blocks, criteria escalations, and regressions with escape `spec_ambiguous`. |
| Spec-caused rework | −1 | Repeat runs, `[BLOCKED]` or unmerged PRs, and operator requeues whose cause is `spec`. |

Changes from today's composite:

- Crew-caused rework, review rejects and failed reviewer fixes leave this score; §3 gives their owners.
- Spec regressions no longer count nature `incomplete` by itself.
- No compatibility column keeps the old composite, because a dashboard score is not an external contract. The how-to page says once where crew rework went.

The role panel shows each orchestrator's rework split into spec (counted), crew (moved to Implementers), infrastructure and no cause (both not scored).

### 5.2 Implementers

Unit: **implemented tasks**, meaning tasks with at least one implementation run by the row's crew and model that started in the window.

| Term | Weight | Definition |
|---|---|---|
| Weighted landed | +1 | Landed tasks whose final implementation run is the row's, weighted by complexity as above. |
| First-pass accepts | +1 | Reviewed tasks whose first review verdict on the row's candidate was `accept`. |
| Review rejects | −1 | `reject` verdicts on the row's candidates. |
| Reviewer repairs | −0.5 | `accept_with_fixes` verdicts on the row's candidates. |
| Crew-caused rework | −1 | Repeat runs, `[BLOCKED]` or unmerged PRs, and operator requeues with cause `crew`, charged to the run that ended the attempt. Review verdicts are not counted again here. |
| Escaped regressions | −2 | Regressions with escape `no_test` against tasks whose final implementation run is the row's. |

Shown, not scored:
- cost per weighted landed task;
- median run duration;
- tasks by tier;
- first reviews needing changes compared with the average for the same tier. Rejects and repairs grow with complexity, so this is the fair comparison across pools that get different tiers. It stays unscored until a month of data shows its variance.

The role panel plots each row's score against its cost per weighted landed task. A dashed line joins the rows that no other supported row beats on both.

### 5.3 Reviewers

Unit: **settled reviews**, meaning review gate attempts by the row's crew and model whose verdict settled in the window. Review jobs that file findings as tasks count as reviews too, through their filed findings.

| Term | Weight | Definition |
|---|---|---|
| Findings that held | +1 | Findings repaired by the reviewer and validated, or open findings on a `reject` that the next implementation run changed a cited path to address; each weighted by severity: critical 8, high 4, medium 2, low 1. |
| Fixes that held | +1 | `accept_with_fixes` verdicts whose fixes passed owner revalidation and whose task has no regression with escape `review_missed`. |
| Rejected findings | −1 | Findings closed by an authorized decision (`Disposed`), and findings a review job filed as tasks that finding verification REJECTED. |
| False rejects | −2 | `reject` verdicts where the next implementation run changed none of the rejected findings' cited paths and the next review accepted. |
| Failed fixes | −1 | Reviewer fixes that failed owner revalidation and were escalated. |
| Missed regressions | −4 | Regressions with escape `review_missed` against tasks the row's review accepted, whenever filed. |
| Incomplete reviews | −0.5 | `incomplete` verdicts not caused by infrastructure. |

An accept rate never enters the score. A reviewer earns only for verdicts that turned out right. See [Score accuracy, never the volume of objections](./4_decisions.md#score-accuracy-never-the-volume-of-objections).

Shown, not scored:
- cost per review;
- median review duration;
- accept rate against the other reviewers on the same tiers. A row 10 or more points above its peers is tagged `accepts often`.
- the row's provisional count.

The role panel splits each reviewer's settled reviews into: accepted and held up, caught a real problem, false alarm, and missed a regression.

### 5.4 Pilots

Unit: **assessed tasks**, meaning done tasks whose newest task-pilot receipt was written in the window. Bulk tasks (more than 12 landed files) and `no-diff-expected` tasks are left out, as today. Recall, precision, F1 and the mirrored-file and generated-file coverage rules stay those of `codebase/pilot.rs`.

| Term | Weight | Definition |
|---|---|---|
| F1 lift | +1 | F1 points gained over the filer's own selectors from before any pilot ran. |
| Tier calibration | +0.25 | Percent of tasks whose assessed complexity matches their observed tier. |
| Costly misses | −1 | Landed files no pilot selector covered that caused a footprint block or a lock conflict. |
| Lock-wait hours | −1 | Hours other tasks waited, in lock-wait episodes, on paths the holder reserved through a pilot selector that covered none of the holder's landed files. |

The **observed tier** ranks the window's assessed tasks by total implementation cost and cuts the ranking into tiers in the same proportions the pilots assigned. Calibration therefore measures whether a pilot orders work correctly, not whether it guesses absolute cost (§11).

Shown, not scored:
- cost and median time per assessment;
- selectors per task;
- recall and precision;
- the share of tasks assessed within one tier.

The role panel is the assessed-by-observed tier matrix for every pilot, with counts of work called too easy and too hard.

A landed file outside the declared context on a task no pilot assessed is not charged to anyone in v1 (§11).

## 6. API Contract

All routes are GET, scoped by `workspace`, follow the host picker through `/api/on/<host>/…`, and answer only GET.

```
GET /api/agents/{role}/scorecard?window=&min_support=
GET /api/agents/{role}/detail?window=&min_support=&crew=&model=
GET /api/agents/attribution?window=
```

`{role}` is `orchestrators`, `implementers`, `reviewers` or `pilots`. For orchestrators, `crew` names the orchestrator attribution. `model` is the literal `model unknown` for an unknown model.

**Scorecard** answers:

- `role`, `window`, `since`, `min_support`, `computed_at`, `cache_ttl_seconds`;
- `status`: `ok`, or `not_computed` while a role's track has not landed. A `not_computed` role still returns its `terms`, with empty `rows`.
- `unit` (label of the role's unit);
- `fleet_average`;
- `terms`: `key`, `label`, `weight`, `definition`, `group`;
- `rows`, best first.

Each row carries `crew`, `model`, `units`, `below_support`, `rank` (null below support), `counts` and `per_100` (term key to number), `raw`, `score`, `runs_without_model`, `provisional` (reviewers), and `stats` (the not-scored context: key, label, value).

**Detail** adds `row` and `terms`. Each term carries its key, count, contribution, and `tasks`. Each task carries `task_id`, `title`, `status`, `count` and `occurrences` (`at`, `detail`, `run_id`).

**Attribution** answers `window`, `since`, `total` and `owners`. `owners` maps each of `orchestrators`, `implementers`, `reviewers`, `pilots` to a count, plus `not_scored` with its `reasons`.

Errors are 400 for:
- an unknown role or window;
- a zero `min_support`;
- a detail request without `crew`, or naming a row the window does not contain.

The `/api/orchestrators/*` routes are removed in the same change that moves the dashboard to `/api/agents/orchestrators/*`.

## 7. CLI and MCP

`orbit agents scorecard --role <role> [--window <w>] [--min-support <n>] [--json]` and `orbit agents show --role <role> --crew <crew> [--model <model>] [--window <w>] [--json]` print the same payloads as the API. The read-only MCP tools `orbit.agents.scorecard` and `orbit.agents.show` take the same arguments.

Each of those scans once per call and reports `cache_ttl_seconds: 0`.

`orbit orchestrator scorecard|show` and `orbit.orchestrator.scorecard|show` remain for one release as aliases of `--role orchestrators`, and print a deprecation note naming the replacement. They are removed in the following release.

## 8. Scan, Cache and Budget

One `AgentFacts` scan per workspace serves all four roles and the attribution strip. It reads:
- every task with activity since the window started, with its history and comments;
- the job runs those tasks name;
- their review reports;
- task-pilot receipts;
- the codebase facts the pilot terms need: landed files, footprint blocks, lock-wait episodes.

It reads at most 90 days back. `all` is served as 90 days, so the scan stays bounded however old the workspace gets.

The server keeps one scan per workspace runtime for five minutes, single-flight, like the orchestrator scan today. When the cached scan is stale, the next request is answered from it while one background refresh runs. Changing the role, window or minimum support, or opening a detail, only slices the cached facts.

Budget: under 2 s for a warm request and under 15 s for a cold scan on the `ws_orbit` workspace. The core track records both timings in its PR. If the cold scan is over budget, an incremental index lands before the role tracks start, not after. The Analytics › Codebase scan already exceeded 40 s on this workspace, which is why the budget is fixed up front.

## 9. Testing

Tests follow the repository rules: behavior at public boundaries, no copy-matching, no pinned weights.

- **Core** (`crates/orbit-core/tests/`, through the public runtime API): a seeded workspace with known tasks, runs, review reports, regressions and lock waits.
  - Each problem lands in exactly one owner or in Not scored.
  - A multi-crew task charges each crew for its own attempt.
  - Smoothing pulls a small row toward the fleet average.
  - Below-support rows sort last without a rank.
  - Regressions filed after the window still count.
  - Reviewer provisional counts follow the 30-day lookback.
- **API** (`orbit-web` HTTP tests): the payload contract, `not_computed`, the 400 cases, and that one scan serves role and window changes.
- **Dashboard**:
  - node DOM-stub tests and a Playwright fixture driven by contract-shaped JSON: ranking selection, detail, disclosure, empty and `not_computed` states, the redirect from `#analytics/orchestrators`, phone width;
  - screenshots of each changed view at desktop and 375 px.
- **CLI and MCP**: goldens regenerated for the new commands, tools and the alias notes.

## 10. Delivery Plan

| Track | Scope | Depends on | Files it owns |
|---|---|---|---|
| A | This design (`docs/design/agent-scorecards/`) | — | `docs/design/agent-scorecards/**`, `docs/design/CONVENTIONS.md` ownership row |
| B | Core and API: `application/agents/` (`facts.rs`, `attribution.rs`, `score.rs`, the role registry with all four roles, three of them `not_computed`), Orchestrators ported to the new score, `api/agents.rs`, cache; removes `application/orchestrators/` and `api/orchestrators.rs` | A | `crates/orbit-core/src/application/agents/**`, a new `agents.rs` in the `orbit-web` API module, routes, `runtime_memo` |
| C | Dashboard shell: Agents destination, `#agents/<role>` routing and redirect, the generic renderer, removal of Analytics › Orchestrators | A | `assets/dashboard/js/agents.js`, `css/agents.css`, `index.html`, `app.js`, removal of `js/orchestrators.js` |
| D | Implementers role and its panel | B | `application/agents/roles/implementers.rs`, `js/agents-implementers.js` |
| E | Reviewers role, provisional banner and outcomes panel | B | `application/agents/roles/reviewers.rs`, `js/agents-reviewers.js` |
| F | Pilots role and calibration panel; moves the pilot ranking out of Codebase | B | `application/agents/roles/pilots.rs`, `js/agents-pilots.js`, `application/codebase/pilot.rs`, `js/codebase.js` |
| G | CLI and MCP (`orbit agents`, `orbit.agents.*`, aliases) | B | `orbit-cli` and `orbit-tools` command and tool files, goldens |

```
A ─┬─ B ─┬─ D
   │     ├─ E
   │     ├─ F
   │     └─ G
   └─ C
```

B and C run in parallel against §6. D, E, F and G run in parallel once B lands: each adds its own files, and none edits the registry, because B registers all four roles. C renders `not_computed` roles as an empty state, so the destination can ship before D–F.

Each track updates the docs its change makes stale in the same PR. That includes this design (moving the sections it implements from target to current), [user-interface 2_design.md](../user-interface/2_design.md) and the dashboard how-to page.

## 11. Concerns & Honest Limitations

- **Reviewer false alarms are proxies.** The review gate records no verdict on whether its own findings were right. `Disposed` findings and the false-reject rule (§5.3) stand in for that, and a reject that the implementer satisfied by changing an unrelated file still counts as held. A dedicated record of an implementer disputing a finding would be exact, but would change the review flow to serve a dashboard. It is deferred until the proxies prove noisy.
- **Orchestrator rows have no model.** Orchestrator sessions are not job runs, so orchestrators rank by attribution name with `model unknown` until the attribution records a model.
- **The observed tier inherits cost differences between crews.** Work routed to a costlier crew looks harder. Ranking by cost within the window reduces this but does not remove it, so calibration should be read as a comparison between pilots on similar work.
- **Footprint misses without a pilot are not charged.** When no pilot assessed a task, a landed file outside the filer's selectors counts against no one in v1. It could be an orchestrator spec defect, but nothing records whether the file was in scope.
- **The escape decides the owner.** An implementer whose visible defects pass a weak reviewer is not charged for them; the reviewer is. Reading the Implementers and Reviewers tabs together is part of the design, not an accident.
- **Weights are judgment.** They are shown with every score and kept in one table per role so they can be changed in one place. Tests check how scores behave, not the weight values.
- **Provisional scores move.** Reviewer scores for the last 30 days can fall when regressions are filed. The banner says so, but a ranking read on a given day is not final.
- **Scores steer crew pools.** Operators will tune complexity pools on these rankings, which rewards whatever the terms measure. The terms favour outcomes recorded by other roles (reviews, regressions, waits) over what an agent says about itself, and nothing reads a self-report.
- **The scan budget is a target, not a measurement.** No prototype has been timed. §8 makes the core track measure it before the role tracks build on it.

## Task References

No tasks are cited yet. The implementation tasks will be cited here as they land.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
