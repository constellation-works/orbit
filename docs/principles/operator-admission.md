---
type: context
summary: "Operator admission: agents propose work and the operator controls its admission and authority"
last_validated: 2026-11-27
tags: [principle, admission, authority]
related_features: [auto-tasks, granted-tools, review-gate]
---

# Operator Admission

Agents propose work; the operator admits it and decides what authority it receives.
An agent cannot grant itself the operator's authority.

## Why

Filing and executing work have different consequences. If the agent that proposes a change
could also authorize its own privileges and delivery policy, an automatic queue could carry
that expansion into execution without the operator choosing it. Admission keeps that choice
with the person responsible for the workspace, while allowing routine decisions to be
delegated within boundaries they choose.

## In practice

- A normally filed task starts in `proposed`; promotion is a separate approval. The
  [delivery philosophy](positioning.md#delivery-philosophy) and
  [optimistic merging](optimistic-merging.md#where-the-operator-stays-in-control) explain
  its place in delivery. The [auto-tasks design](../design/auto-tasks/2_design.md) describes
  the exceptions for configured initial statuses and no-diff tasks. The operator can opt
  into a [drain that approves qualifying pilot-assessed proposals](../config/workflow.md#workflowstanding_drain--the-opt-in-drain-keeper),
  subject to holds and opt-outs; this is delegated admission, not self-approval. The config
  layer does not enforce operator-only settings, so that boundary also depends on workspace rules.
- Forced lifecycle transitions belong to the operator; the agent tool surface has no force
  override. See the [lifecycle configuration](../config/crews.md#human-crews) for the distinct
  human-crew path and its limits.
- The [granted-tools design](../design/granted-tools/2_design.md) reserves grants to the
  operator. A grant does not confer operator capability; current mutating grants use reviewed
  proposals rather than live calls. Operators can also put grants in auto-task definitions.
- The operator chooses review layers: [review configuration](../config/review.md#review-and-operation--automatic-review)
  and the [review gate](../design/review-gate/1_overview.md) describe the available modes.
  The [agent gate rules](../agents/gate-rules.md#gates) describe the constrained light-delivery
  exception. Review is one admission choice, not a separate source of agent authority.
- The operator configures [crew pools](../config/crews.md#automatic-crew-pools-by-complexity);
  Orbit draws from those pools under the configured rules.

## What it costs

The operator must configure and sometimes approve work, and a proposal can wait for that
attention. Automatically admitting every agent proposal would be faster, but would surrender
that control.

## Applying it

For a new automation, ask who authorized its admission and whether it can expand its own
scope or privileges. This principle does not require a person to approve each routine
operation after the operator has delegated it within configured limits.
