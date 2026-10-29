---
title: "Task quality: private shadow consumer"
owner: codex
last_updated: 2026-10-29
last_validated: 2026-10-29
status: Draft
feature: decision-assistance
doc_role: spec
type: design
summary: "Owner task-entry markers, crash reconciliation, bounded semantic assessments and private versioned feedback; no advisory activation."
tags: [decision-assistance]
---

# Task quality: private shadow consumer

The consumer is implemented and defaults off. It has no task-show, CLI, MCP or
dashboard advisory projection. No live effectiveness or cost claim is made;
independent labels, evaluation and any operator enablement remain separate work.
The operator's separate invented-case development screen failed every question
group's predeclared target. This is negative synthetic evidence, not measured
accuracy or production utility; advisory rollout is parked and surfaces remain
archived/deferred. The [scope](../1_scope.md#development-evidence-and-disposition)
records provenance and the improved rubrics/packets, independently adjudicated
permitted data and registered unopened holdout required before any activation.
Code delivery, effectiveness and adoption are recorded separately.
The existing shape validator and complete source/feasibility pilot remain
authoritative. Assistance cannot change task fields, tags, status, crew,
footprint, pilot warnings, readiness or comments.

## Entry points and recovery

`OrbitRuntime::add_task_validated` covers ordinary CLI/MCP creation, automation
and guarded dashboard/desktop creation. After the insert commits, it attempts a
small local marker write, including on idempotent replays. A provenance failure
after insertion does not suppress this marker. Desktop receipt replays repeat
the same idempotent scheduling step. Ordinary semantic edits use
`update_task_locked`; desktop's separate atomic mutation path uses its shared
post-commit refresh. Unrelated edits produce no new semantic snapshot.

Task creation never calls an adapter, resolves its credential or waits for
inference. A marker-write failure leaves creation successful. The committed task
store is the reconciliation source: a subsequent owner `orbit clock tick`
discovers missing markers even if the creation event was never published. This
also catches semantic edits interrupted before their post-commit hook. Clock dry
runs and replicas do not run the consumer. Off returns before scanning or writing
work. Configuration is exclusively the existing host-owned
[`assistance` setting](../../../CONFIG.md#assistance--optional-bounded-model-annotations).
No workspace setting or new outbound destination is introduced.

Each tick examines at most 32 subjects through a durable rotating cursor and
processes at most one service attempt. The scan uses task metadata before loading
selected bodies; later subjects cannot starve behind a large prefix. A new owner
process resumes the cursor and work ledger. Owner clocks must continue running
for automatic recovery; no daemon or task-creation subprocess is started. The
generic service still enforces global concurrency, queue, deadline, retry and
charge limits. Switching off stops new calls, including claimed work's final
pre-send check. Existing task creation stays available during queue, credential,
store and provider outages.

## Questions and freshness

The template consumes the evaluator's `task-quality-questions/1` predicates and
exact rubrics. Two independent predicates cover outcome and scope for the whole task;
`criterion_not_observable/<index>` assesses each numbered criterion and
`requirements_conflict/<first>/<second>` assesses each criterion pair. Each
expanded question carries host-selected index/text-SHA256 references verified by
the generic service. All questions receive the bounded title, description and
numbered criteria. Criterion and conflict flags have exactly one and two
criterion references respectively; no redundant reference-free aggregate flag
can point ambiguously at criteria. Investigations that deliver evidence and coherent changes
across components are explicitly allowed by the rubrics. This checks no source
paths, dependencies, transport, platform feasibility or toolchain semantics.

`task_quality_question_template()` exports the exact grant contract. The host
grant uses `use_case = "task_quality"`, its serialized template's SHA256 as
`contract_digest`, evidence fields `title`, `description`, `criteria` and
`criterion_refs`, and the `task:` reference prefix. There are no selected
confidence thresholds: valid answers remain `uncertain`, including synthetic
positive flags. A host grant in shadow mode persists private results. Advisory
mode alone adds no human-facing projection in this delivery.

One logical snapshot hashes workspace, subject, semantic content and the complete
host policy/model/budget epoch. Only title, description and ordered criteria
enter the semantic digest. Comments, plan, priority and other fields do not.
**Criterion reordering invalidates the snapshot** because references bind an
index as well as a text digest; a previous reference must never silently point
at the new occupant of its index. Multiple edits before a tick coalesce to the
latest content. Service admission coalesces older queued snapshots of the same
subject. A create replay returning the same task reuses its marker and work.

First admission durably fixes the packet collection timestamp before enqueue;
recovery after an interrupted enqueue hashes the identical packet rather than
minting another logical work ID. A marker can wait for owner restart without its
creation timestamp exhausting the service queue age before first collection.
Once admitted, expired work remains an explicit coverage gap under service
policy. At most 27 criteria fit all independent questions within the service's
400-question bound. Larger tasks receive `invalid_request` coverage, and other
host limits or admission failures receive a gap; neither counts as clean.

Task shared locks precede private ledger locks during local admission, current
reads and feedback. They exclude semantic edits and terminal transitions without
writing task state or refusing execution claims. No task, ledger or SQLite lock
is held over inference. The worker rechecks the latest digest and terminal state
immediately before and after adapter I/O; late replies become `stale`. Every
consumer read independently rechecks semantic and policy identity, so a change
after the worker's final check still cannot project an old annotation as current.
Terminal tasks enqueue no new assessments and retain their existing history.

## Private persistence and feedback

Consumer metadata lives under the owner global root at
`state/task-quality/v1/<workspace-digest>/<subject-digest>.json`, independently of
task records. Private directories, bounded file-lock acquisition and durable
atomic replacement protect writes; no new crate edge or database migration is
needed. Files contain schema version 1, immutable content/policy identities,
work IDs, fixed gap codes, typed results and up to 32 retained semantic/policy
snapshots per subject. They contain no task prose, raw provider response,
explanation, arbitrary feedback text or credential. The generic assistance
ledger owns bounded attempts, claims, charge accounting and its retention.

The supported private application boundary is `task_quality_history(id, service)`.
It distinguishes unadmitted pending snapshots, queued work, explicit admission
gaps, complete typed assessments and refused/invalid/unavailable/stale results.
An assessment with no true answers is clean only when its result is complete;
an empty annotation list alone says nothing about coverage. Completed typed
results and their feedback are copied into consumer history and survive terminal
transitions, off switches and service-record retention. The `current_id` is
derived from fresh content/policy identity on each read.

Annotation IDs bind `task-quality-annotation/1`, snapshot, stable rule ID and
criterion references. `dismiss_task_quality_annotation` requires both current
snapshot and annotation IDs and a typed reason: valid investigation, coherent
shared outcome, observable evidence specified, compatible requirements or false
positive. A stale/unknown identity is refused; an exact dismissal replay keeps
its original timestamp. Reasons persist privately and never become adjudicated
ground truth or modify the task. Human-facing feedback transport and advisory
wording belong to the later projection task.

## Reproducible offline verification

The manual executable drives public ordinary and guarded desktop task creation,
updates and the consumer application port in an isolated child with cleared
environment and temporary roots below `.orbit/tmp/`. Synthetic adapters need no
key or HTTP transport. The evaluator schema is checked against the consumed
rubrics, and an altered template expansion is refused.

```sh
mkdir -p .orbit/tmp
python3 scripts/evaluate-decision-assistance.py schema > .orbit/tmp/assistance-eval-schema.json
scripts/build-budget.py -- cargo run -p orbit-core --example task_quality_fixture -- .orbit/tmp/assistance-eval-schema.json
```

The fixture verifies no adapter entry during creation; pending/current and stable
criterion references; replay deduplication; restart and missing-marker recovery;
a deterministic local marker-write outage; unrelated edits, coalesced semantic
edits, criterion reorder and an edit during inference; terminal/off behavior;
missing, malformed, refused and unavailable responses; durable reasoned dismissal;
bounded evidence gaps; and unchanged task/history/comment projections. Synthetic
negative answers for valid investigations/coherent scope verify preservation and
non-authority, not semantic model accuracy. New-feature checks use this manual
fixture under the workspace test-admission policy.

## Task References

- ORB-19723 — task-quality shadow consumer and private feedback.
- ORB-19720 — canonical offline evaluator question/rubric contract.
- ORB-19721 — generic assistance service, bounded durable work and adapter.
- ORB-19724 — later human-facing projections.
