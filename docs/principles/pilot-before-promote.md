---
type: context
summary: "Pilot before promote: a task is checked against the current code before it enters the backlog, and the pilot holds it when the brief contradicts the tree"
last_validated: 2026-11-27
tags: [principle, task-pilot, admission, promotion]
related_features: [task-pilot, automation-triggers, routines, resident-orchestrator, distributed-drain]
---

# Pilot Before Promote

A task is checked against the current code before it enters the backlog. The pilot settles its footprint, complexity and crew, and holds the task when the brief contradicts the tree. Skipping the pilot is the operator's choice.

## Why

A task is written from memory of the code, and the code moves. By the time an agent picks the task up, the work may already have landed, a named file may be gone, or the acceptance criteria may ask for something no lane can check. An implementer that starts from such a brief spends a whole session finding out, and the finding arrives as a failed run.

Orbit could have admitted every proposal as written and let the implementer discover the mismatch. It asks a cheaper reader first. The pilot reads the brief against the tree once, before any delivery slot or lock is spent, and writes down what it found. Parallel scheduling needs the same read: the lock set comes from the footprint the pilot settles ([declared footprints](./declared-footprints.md)), and the crew pool comes from the complexity it assesses.

## In practice

- **What the pilot settles.** It writes the task's `context_files`, assesses complexity, and leaves a sealed assessment as a task artifact with a one-line receipt comment ([assessment persistence](../design/task-pilot/1_scope.md)). The implementation crew is drawn from the pool for the assessed complexity, and an explicit crew assignment is kept ([crew pools](../config/crews.md#automatic-crew-pools-by-complexity)).
- **It runs by itself.** The seeded `task_pilot` routine pilots eligible new tasks, and an `--approve-proposed` drain pilots a stale or unassessed proposal the same way ([routines](../design/routines/1_overview.md), [pilot eligibility](../design/automation-triggers/2_design.md#6-pilot-eligibility-and-freshness)). Preparing a task never approves it.
- **It holds what contradicts the tree.** A duplicate, an already-landed task, a footprint the pilot could not resolve, work needing an operator's validation, a host or OS the owner lacks, and a holding warning finding each withhold the task. Each hold has its own classification: `pilot_duplicate`, `pilot_already_landed`, `pilot_footprint_unresolved`, `operator_validation`, `host_operational`, `native_os` ([distributed-drain runbook](../runbooks/distributed-drain.md)). A `proposed` task with a holding warning waits there until an operator decides it, and the attention items and the dashboard's Pilot warnings callout say which edits clear it ([pilot warning holds](../design/automation-triggers/2_design.md#61-pilot-warning-holds-pilot_warnings_held)). Once a task is in the backlog, admission no longer holds it on warnings ([decision](../design/automation-triggers/4_decisions.md#no-pilot-is-an-operator-skip-of-pilot-admission)).
- **An edit makes the assessment stale.** Changing the description or acceptance criteria after the pilot holds dispatch as `pilot_assessment_stale`. The hold names its two remedies, a new pilot (`orbit task repilot <id>`, and a drain or the routine queue one themselves when no claim holds the task) or the operator's skip. Other edits, such as tags, comments or priority, do not stale it ([freshness settings](../config/workflow.md#workflowtask_pilot_freshness--when-a-task-is-piloted-again)).
- **Promotion reads the assessment.** A drain that approves proposals, the CI sweep and a resident with the `promote_held` right approve only a task whose latest assessment is current and clean, and refuse one without it ([mandate and authority](../design/resident-orchestrator/specs/mandate-and-authority.md#rights--tools), [resident re-pilot](../design/resident-orchestrator/4_decisions.md#residents-re-pilot-and-requeue-under-dispatch)).
- **A task with no footprint waits.** Until the pilot gives it `context_files`, a task runs only alone in a multi-slot drain, as `awaiting_footprint`, and a task with unassessed complexity is withheld ([readiness waiver](../design/automation-triggers/4_decisions.md#readiness-waived-is-an-operator-waiver-of-readiness-gaps)).
- **Skipping is the operator's.** The `no-pilot` tag admits a task past a stale assessment and the ordinary holds. Adding it to a task a pilot has assessed or is preparing is refused for any writer but an operator, and the operator's write records a `pilot_skipped` history entry with the requirements it dispatched unassessed ([decision](../design/automation-triggers/4_decisions.md#no-pilot-is-an-operator-skip-of-pilot-admission), [operator admission](./operator-admission.md)). The tag leaves a current host-operational hold in place, which only an operator decision, a re-scope or a newer assessment resolves ([narrowing](../design/automation-triggers/4_decisions.md#no-pilot-leaves-a-current-host-operational-hold)).

## What it costs

- **Latency.** A task waits in `proposed` for a pilot run, and an edit sends it back for another. An operator who has just settled the footprint and criteria pays for a second assessment that adds only delay, which is why the skip exists.
- **Agent sessions.** Every pilot is a provider run, and the drain and routine spend them on tasks that turn out clean.
- **A read, not a proof.** The pilot reads the tree at one revision. A task can still be wrong about a file it did not read, and a held task can be held for a finding the operator judges wrong.
- **Unassessed dispatch.** A skipped task can run on requirements no pilot has read, including ones an earlier assessment flagged. The `pilot_skipped` entry is the only record, so audits read history, not the receipt.

## Applying it

Ask of a new path into the backlog: has the brief been read against the tree it will change, and does that read decide the footprint, the complexity and whether to hold? If not, send the task through the pilot or its assessment, and do not add a second check with its own rules.

Where it does not apply: the operator's own decision on a held task. The operator can approve a proposal over a pilot warning ([tasks](https://orbit-cli.com/concepts/tasks/)) or skip the pilot with `no-pilot`, and the tag is the visible form of that choice. The principle stops an agent skipping the check, not the operator.
