---
title: Agent Scorecards — Overview
owner: claude
last_updated: 2026-11-15
last_validated: 2026-11-15
status: Draft
stage: shipped
next: "—"
feature: agent-scorecards
doc_role: overview
type: design
summary: One scorecard per agent role (implementers, reviewers, task pilots), ranked by model and recorded effort, where each problem counts against exactly one role.
tags: [agent-scorecards, dashboard, analytics]
paths: ["crates/orbit-core/src/application/agents/**", "crates/orbit-web/src/api/agents.rs", "crates/orbit-cli/src/command/agents.rs", "crates/orbit-core/src/application/codebase/pilot.rs", "crates/orbit-web/assets/dashboard/js/agents.js"]
related_features: [user-interface, review-gate, finding-verification]
related_artifacts: []
---

# Agent Scorecards — Overview

Agent scorecards rank the agents that do each part of Orbit's delivery work: the implementers that write the change, the reviewers that gate it, and the task pilots that predict its footprint and complexity. Each role gets one scorecard of the same shape, ranked by model and recorded reasoning effort, in the Analytics › **Agents** dashboard view (`#analytics/agents/<role>`). Each problem attributed to an agent (a spec edit, a rejected review, a missed regression, a wasted lock) counts against exactly one role, so a single defect never lowers two rankings; problems with no agent at fault are shown as **Not scored**.

## 1. Motivation

Orbit ranked orchestrators (Analytics › Orchestrators, now in Analytics › Agents at `#analytics/agents/<role>`) and task pilots (one tile of Analytics › Codebase). Those two views use different shapes, and neither answers the question operators actually ask when choosing crew pools: which crew and model does this job best?

The orchestrator composite also charges the orchestrator for crew-caused rework. On the 7-day data that motivated this feature, 22 of opus's 28 rework items had cause `crew`, so its score mostly measured its implementers. Separate implementer and reviewer scorecards can only be fair if each attributable problem has one owner.

The orchestrators role this feature first shipped with was removed on 2026-11-09 for lack of use ([ORB-23582]). Problems a task's specification caused are now Not scored, and task authorship is no longer ranked.

## 2. Core Concepts

- **Role**: implementer, reviewer or pilot. A role is defined by the record that shows the work, not by the crew's name.
- **Ranking unit**: the model and recorded reasoning effort attributed to the role's work, with contributing crews as detail. The roles resolve their implementation, review or pilot evidence as described in [the design](./2_design.md#2-ranking-unit). Missing model evidence is ranked as `model unknown`.
- **Problem**: a recorded occurrence that counts against a role: a spec edit, review reject, repeat run, regression, false alarm, wasted lock wait.
- **Term**: one counted component of a role's score, with a fixed weight.
- **Raw score**: the weighted sum of a row's term rates, using each term's denominator. Implementers start at 100 and sum weighted actual-minus-expected rates for their task mix.
- **Score**: the raw score, pulled down toward the fleet average when above it, in proportion to how little work backs it.
- **Not scored**: problems with no agent at fault (infrastructure, nondeterminism, no recorded cause) or a cause no role is ranked for (a spec defect). They are shown so they can be fixed, never charged to anyone.

## 3. At a Glance

| Concern | File | Delivery task |
|---------|------|------|
| Problem attribution and the shared score | [2_design.md §3–§4](./2_design.md#3-attribution-each-problem-has-one-owner) | [ORB-16952] implemented; [ORB-19644] clarified strip ownership |
| Each role's terms and weights | [2_design.md §5](./2_design.md#5-roles) | [ORB-16954] implemented Implementers; [ORB-16955] implemented Reviewers; [ORB-16956] implemented Pilots; [ORB-17779] added implementer tier expectations |
| Recorded model-and-effort rows | [2_design.md §2](./2_design.md#2-ranking-unit) | [ORB-23345] implemented the grouping and labels |
| API, CLI and MCP contract | [2_design.md §6–§7](./2_design.md#6-api-contract) | [ORB-16952] implemented API; [ORB-16957] implemented CLI and MCP |
| Delivery tracks and their order | [2_design.md §10](./2_design.md#10-delivery-plan) | [ORB-16952], [ORB-16953], [ORB-16954], [ORB-16955], [ORB-16956], [ORB-16957] delivered tracks B–G |
| Standing rules | [4_decisions.md](./4_decisions.md) | — |
| The Agents dashboard view | [2_design.md §1](./2_design.md#1-destination-and-layout), [user-interface 2_design.md §5c](../user-interface/2_design.md#5c-analytics--agents) | [ORB-16953] built the view; [ORB-23582] removed Orchestrators |

All committed delivery tracks B–G have landed (the tasks above). `stage: shipped`
records that delivery; the document remains Draft because those implementations
do not themselves record owner approval of the design.

## Task References

- [ORB-16952] — implemented the core scan, attribution, shared score, former Orchestrators role and `/api/agents` routes.
- [ORB-16953] — built the Agents destination and retired Analytics › Orchestrators and `/api/orchestrators/*`.
- [ORB-16954] — implemented the Implementers role.
- [ORB-16955] — implemented the Reviewers role.
- [ORB-16956] — implemented the Pilots role.
- [ORB-23582] — removed the orchestrators role and its CLI, MCP and HTTP surfaces.
- [ORB-16957] — implemented the scorecards CLI and MCP contract.
- [ORB-17779] — introduced same-tier fleet expectations for implementer terms.
- [ORB-19644] — clarified attribution-strip ownership.
- [ORB-23345] — grouped and labelled scorecard rows by recorded model and effort.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
