---
title: Granted Tools — Overview
owner: claude
created_date: 2026-10-25
updated_date: 2026-11-28
last_validated: 2026-11-20
status: Accepted
stage: shipped
next: "—"
feature: granted-tools
doc_role: overview
type: design
summary: "An operator-only `granted_tools` task field that lifts the implementing activity's deny list for named tools, without granting capability, placement or live side effects"
tags: [granted-tools]
---

# Granted Tools — Overview

Status: accepted and shipped — all six implementation phases are shipped, as scoped by [ORB-18317]. Grants are
operator-only, owner-host-only and frozen for a dispatched attempt. Every
currently denied implementer tool is either mutating or conservatively treated
as mutating by grant classification: grants authorize reviewed proposals,
never live mutations. See the shipped
[operator runbook](../../../crates/orbit-core/assets/skills/orbit/references/task-fields-tools.md#operator-runbook-granting-tools).
Bearing: tasks already carry `required_tools`
([`artifacts.rs`](../../../crates/orbit-types/src/task/artifacts.rs));
`agent_implement` is a deny-list activity
([`agent_implement.yaml`](../../../crates/orbit-core/assets/activities/agent_implement.yaml))
whose requirements never override a deny [ORB-15162]
([`activity_tools.rs`](../../../crates/orbit-core/src/adapter/engine_host/runtime_host/activity_tools.rs));
field-level operator authorization already exists in
[`tool_exec.rs`](../../../crates/orbit-runtime/src/host/exec/tool_exec.rs); and claimed
(follower) implementers already refuse coordination requirements in
[`serve/admission.rs`](../../../crates/orbit-core/src/application/distributed/serve/admission.rs).

## Problem

An operator wants a task whose implementing agent can call a tool the
activity normally denies — the motivating example is `orbit.auto_task` action `update`,
so a task can revise an auto-task definition as part of its work.

Before this feature that was impossible by design.
`agent_implement`'s `tool_disallow_list`
names `orbit.auto_task` action `add`, `orbit.auto_task` action `mint`, `orbit.auto_task` action `update`,
`orbit.command.exec`, `orbit.pipeline.invoke`, `orbit.run` actions `resume` and
`show`, and `orbit.workflow` actions `land`, `refresh` and `ship`. Listing one in
`required_tools` does not help:
`admit_required_tools` in
[`activity_tools.rs`](../../../crates/orbit-core/src/adapter/engine_host/runtime_host/activity_tools.rs)
fails implementer admission when the deny policy covers a requirement, and
only non-implementer activities (review, recovery, pilot — see
[`activity_tool_requirements.rs`](../../../crates/orbit-core/src/application/job/activity_tool_requirements.rs))
drop a covered requirement with a note instead. [ORB-15162] made "a
requirement never overrides a disallow list" the rule; the reason is
[operator admission](../../principles/operator-admission.md).

So the mechanism needed is not a stronger `required_tools`. It is a grant
whose *authority* is different from the requirement's, and whose effect is
narrower than "the agent may now do this."

## Goal

A task may carry operator-granted tools that the implementing activity —
and only that activity — may use despite its deny list, while every other
gate (capability, filesystem/process policy, external authentication,
placement, and the review gate for side effects) keeps applying exactly as
it does today. An agent can request tools; only an operator can grant the
denied ones.

## Non-goals

- **No capability grants.** A grant never confers `Operator` or `Runner`,
  satisfies an inner field-level gate, or makes an always-operator-only
  tool callable ([2_design.md §2](./2_design.md#2-what-a-grant-means--list-membership-only-never-capability)).
- **No grants for non-implementers.** Review, recovery, pilot and
  exploration activities are unchanged; this doc does not revisit the
  non-implementer drop-with-a-note rule [ORB-15162].
- **No claimed-mode bridge widening.** `CLAIMED_OWNER_TOOLS` stays a
  closed list; grants pin placement instead ([2_design.md §3](./2_design.md#3-placement--a-grant-pins-the-task-to-the-owner-host)).
- **No live pre-review mutations.** The artifact path is the only route
  for granted mutations ([2_design.md §4](./2_design.md#4-side-effects-outside-the-diff--mutations-travel-as-artifacts-central-question)); mid-run operator operations remain
  operator work.

## Acceptance (feature level)

- An agent-capability `orbit.task.add` / `orbit.task.update` carrying
  `granted_tools` is refused naming the missing operator authority; the
  same write from an operator session succeeds and is recorded in task
  history and the authorization audit.
- A task granting `orbit.command.exec` (always operator-only) is refused
  at grant time; a task granting `orbit.auto_task` action `update` is admitted,
  and its implementer can propose `orbit.auto_task` action `update` without
  `expected_enabled`. A proposal carrying `expected_enabled` is still
  refused for want of `orbit.routine.control`; a direct live mutation remains
  denied by the implementing activity.
- Review, recovery and pilot runs of a granted task receive none of the
  granted tools and behave exactly as today.
- A granted task is never claimed by a follower; the hold names the
  grant, and with [ORB-18316] labels present the placement is the
  owner-host ∩ label intersection or a hold naming both constraints.
- A mutating grant is exercised by attaching a proposal artifact, with no
  live change; after review approval the change is applied once, a receipt
  artifact exists, and a retried run re-emitting the same proposal does
  not apply it twice.
- Changing `granted_tools` after dispatch is refused for the live
  attempt; the change takes effect only on a new attempt.

The mechanisms are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## Task References

- [ORB-18317] — this scope doc: operator-granted tools for the implementing activity
- [ORB-15162] — established that `required_tools` never overrides an activity disallow list
- [ORB-18316] — label-based host placement, whose placement resolution grants compose with

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
