---
title: Model Ranking — Overview
owner: claude
last_updated: 2026-11-11
last_validated: 2026-11-11
status: Accepted
stage: building
next: "R4 has no open task (ORB-23305 rejected): file the freeze and 4-week shadow run"
feature: model-ranking
doc_role: overview
type: design
summary: "Per-model ranking of implementer models, built through R3: quality and reliability measured against the fleet's expectation for the same task mix, noise-scaled, with 95% tie bands ordered by score. Cost breaks exact score ties. CI and recovery measures are not yet scored."
tags: [model-ranking, agent-scorecards]
paths: ["crates/orbit-core/src/application/agents/**", "crates/orbit-web/src/api/agents.rs", "crates/orbit-cli/src/command/agents.rs"]
related_features: [agent-scorecards, task-pilot, granted-tools]
related_artifacts: [ORB-23297, ORB-23066, ORB-23277, ORB-21789]
---

# Model Ranking — Overview

## 1. Motivation

Orbit's [agent scorecards](../agent-scorecards/1_overview.md) rank each crew and model on a
few role-specific terms. The weekly MVP note then turned four of those boards into one
winner with podium points. Four model reviews of that method (2026-11-07,
`constellation/docs/discussion/`) agreed that it was transparent but not a fair claim that
one model is better than another:

- taking each model's best crew rewards models that have more crews;
- podium points ignore how far apart the scores are;
- one week of a few dozen tasks per model is mostly noise.

The implementer scorecard also leaves out evidence that operators already watch:

- CI failures the implementer caused;
- final recoveries that were the implementer's fault;
- failed tool calls.

Daniel asked for one holistic ranking inside Orbit itself.

## 2. Core Concepts

- **Model, not crew.** A row is a model. Its crews and effort levels (`opus`, `opus@medium`)
  are pooled, and the split is shown underneath it.
- **Expected for the same work.** Every measure is the model's result minus what the fleet
  achieves on the same tier and, where it matters, the same expected change size. The tier
  is the task pilot's assessment. Expected size is the fleet's median lines changed for the
  task's tier and type, never the lines the model actually wrote.
- **Two pillars.** *Quality*: escaped regressions and review outcomes. *Reliability*: CI
  failures, final recoveries, rework and tool-call errors. Each counts only what the
  implementer caused.
- **Noise units.** Each measure is divided by a frozen estimate of its sampling noise, so
  measures with different units add up fairly.
- **Tie bands.** Models whose scores aren't distinguishable share a band. Within a band,
  rows follow descending score; cost and model name break exact score ties without moving
  models across the boundary.
- **Cost is never scored.** Cost, tokens and time are shown, and a score-against-cost chart
  per tier makes the trade-off visible for pool decisions.

## 3. At a Glance

| | Agent scorecards (today) | Model ranking (built through R3) |
|---|---|---|
| Row | crew × model | model (crews pooled) |
| Roles | orchestrator, implementer, reviewer, pilot | implementer only; the other roles keep their boards |
| Terms | landed, first-pass accepts, rejects, repairs, rework, escaped regressions | escaped regressions, review outcome, CI failures, final recoveries, rework, tool-call errors |
| Comparison | actual − expected per tier | the same, then divided by each measure's noise scale |
| Output | strict rank with one-sided smoothing | score, interval, tie band; score orders within a band, with cost used for exact score ties |
| Window | 30 days ending now (or `--until`) | 30 days ending at a settled cutoff (default 14 days before `until`) |

**Built so far.** R1 records implementer-caused CI failures by cause and the fault of each
final recovery ([ORB-23300], [ORB-23301]). R2 computes the ranking, `orbit agents rank` and
`GET /api/agents/rank` ([ORB-23302]), and the Analytics › Agents Rank tab shows it
([ORB-23303]). R3's replay recommended a wider band threshold, adopted at 95%
([ORB-23304], [ORB-23839]). CI failures, final recoveries and tool-call errors are not yet
scored. The four-week shadow run (R4) has no open task: [ORB-23305] was rejected on
2026-11-09.

The ranking reads the scorecard's attribution and expectation machinery. It adds two
recorded facts that Orbit didn't keep before R1: which CI failures the implementer caused, and
whose fault each final recovery was. See [2_design.md](2_design.md) and the reasoning in
[4_decisions.md](4_decisions.md).

## Task References

- [ORB-23297] — designs the model ranking (this folder)
- [ORB-23066] — `--until` on the scorecards, used for the settled window
- [ORB-23277] — server-derived model on every MCP tool audit event, needed for tool-call errors by model
- [ORB-21789] — escaped regressions scored for implementers, deduplicated against pre-landing reviews
- [ORB-23300] — R1: records implementer-caused CI failures by cause
- [ORB-23301] — R1: records fault and evidence on final-recovery decisions
- [ORB-23302] — R2: computes the ranking, `orbit agents rank` and `GET /api/agents/rank`
- [ORB-23303] — the dashboard Rank tab and per-tier score-against-cost chart
- [ORB-23304] — R3: season-0 calibration and the component replay
- [ORB-23305] — R4 freeze and snapshot task, rejected 2026-11-09
- [ORB-23839] — adopts the 95% tie-band threshold

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
