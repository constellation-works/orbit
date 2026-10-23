---
title: Agent Scorecards — Overview
owner: claude
last_updated: 2026-10-19
last_validated: 2026-10-23
status: Draft
feature: agent-scorecards
doc_role: overview
type: design
summary: One scorecard per agent role (orchestrators, implementers, reviewers, task pilots), ranked by crew and model, where each problem counts against exactly one role.
tags: [agent-scorecards, dashboard, analytics]
paths: ["crates/orbit-core/src/application/agents/**", "crates/orbit-web/src/api/agents.rs", "crates/orbit-cli/src/command/agents.rs", "crates/orbit-core/src/application/codebase/pilot.rs", "crates/orbit-web/assets/dashboard/js/agents.js"]
related_features: [user-interface, review-gate, finding-verification]
related_artifacts: []
---

# Agent Scorecards — Overview

Agent scorecards rank the agents that do each part of Orbit's delivery work: the orchestrators that file and supervise tasks, the implementers that write the change, the reviewers that gate it, and the task pilots that predict its footprint and complexity. Each role gets one scorecard of the same shape, ranked by crew and model, on a new Observe › **Agents** dashboard destination. Every problem Orbit records (a spec edit, a rejected review, a missed regression, a wasted lock) counts against exactly one role, so a single defect never lowers two rankings.

## 1. Motivation

Orbit ranked orchestrators (Analytics › Orchestrators, now Observe › Agents) and task pilots (one tile of Analytics › Codebase). Those two views use different shapes, and neither answers the question operators actually ask when choosing crew pools: which crew and model does this job best?

The orchestrator composite also charges the orchestrator for crew-caused rework. On the 7-day data that motivated this feature, 22 of opus's 28 rework items had cause `crew`, so its score mostly measured its implementers. Separate implementer and reviewer scorecards can only be fair if every problem has one owner.

## 2. Core Concepts

- **Role**: orchestrator, implementer, reviewer or pilot. A role is defined by the record that shows the work, not by the crew's name.
- **Ranking unit**: the crew and model that did the work, read from the job run (`resolved_crew`, `crew_model`). A run with no recorded model is ranked as `model unknown`.
- **Problem**: a recorded occurrence that counts against a role: a spec edit, review reject, repeat run, regression, false alarm, wasted lock wait.
- **Term**: one counted component of a role's score, with a fixed weight.
- **Raw score**: the weighted sum of a row's terms, each per 100 units of the role's work.
- **Score**: the raw score, pulled down toward the fleet average when above it, in proportion to how little work backs it.
- **Not scored**: problems with no agent at fault (infrastructure, nondeterminism, no recorded cause). They are shown so they can be fixed, never charged to anyone.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Problem attribution and the shared score | [2_design.md §3–§4](./2_design.md#3-attribution-each-problem-has-one-owner) | ORB-16952 |
| Each role's terms and weights | [2_design.md §5](./2_design.md#5-roles) | ORB-16952 (orchestrators) |
| API, CLI and MCP contract | [2_design.md §6–§7](./2_design.md#6-api-contract) | ORB-16952 (API) |
| Delivery tracks and their order | [2_design.md §10](./2_design.md#10-delivery-plan) | — |
| Standing rules | [4_decisions.md](./4_decisions.md) | — |
| The Agents dashboard view | [2_design.md §1](./2_design.md#1-destination-and-layout), [user-interface 2_design.md §5c](../user-interface/2_design.md#5c-observe--agents) | ORB-16953 |

## Task References

- ORB-16952 — the core scan, attribution, shared score, Orchestrators role and `/api/agents` routes.
- ORB-16953 — the Agents dashboard destination, which retired Analytics › Orchestrators and `/api/orchestrators/*`.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
