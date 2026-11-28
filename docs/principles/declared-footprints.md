---
type: context
summary: "Declared footprints: a task declares the files it will touch, and Orbit runs tasks in parallel only when those footprints do not overlap"
last_validated: 2026-11-27
tags: [principle, scheduling, admission, locks, footprints]
related_features: [distributed-drain, path-leases, conflict-budget, task-pilot, review-gate]
---

# Declared Footprints

A task declares the files it will touch. Orbit runs tasks in parallel only when their declared footprints do not overlap, and holds the reservation until the change lands.

## Why

Parallel agents are the point of Orbit, and parallel edits to one file are where they fail. Two agents that rewrite the same file each produce a diff that is right against the base they saw. The second to land meets a conflict, and a text merge of two rewrites can read cleanly and still contradict itself.

Orbit could have let every task run and repaired conflicts at landing. It repairs them anyway, but measured on one workspace since 2026-10-06, 110 of 2,364 landed tasks (about 4.7%) needed a conflict-recovery agent ([path-leases motivation](../design/path-leases/1_overview.md#1-motivation)). A repair run costs an agent session, a rebase and a second validation. Admitting the overlap later is cheap to build and expensive to run.

So Orbit asks for the cost up front, as a list the pilot and the operator can read before any work starts. The list is a prediction. It is cheaper to check a prediction than to unpick a landing.

## In practice

- **Declaration.** The footprint is the task's `context_files`. The task pilot settles it against the current tree before the task is promoted, and holds a task whose footprint it cannot resolve ([task pilot](../design/task-pilot/1_scope.md), [pilot admission holds](../runbooks/distributed-drain.md)).
- **Overlap defers admission.** A task in `in-progress` or `review` holds its footprint, and admission defers a candidate that overlaps it, naming the holder as `context_lock_conflict` ([distributed-drain footprints](../design/distributed-drain/2_design.md#2-the-ready-queue-and-orbittaskpull)). The same check covers local drain waves, owner pull admission, claims and manual reservations. A remote claim in `claimed`, `running`, `handed_off` or `repair_pending` holds the frozen footprint through review, and the hold ends when the claim lands ([attempt ownership](../design/distributed-drain/2_design.md#31-attempt-ownership-and-recovery)).
- **Held through review, released at landing.** A before-landing review keeps the footprint held while it runs. After-landing review releases it at merge ([optimistic merging](./optimistic-merging.md)).
- **Widening is checked, not free.** An agent may change files outside its declaration where the work needs them, and the owner validates the widening at handoff. Footprint expansion while a claim executes is refused.
- **Unlocked selectors filter the local lock surface.** `workflow.unlocked_context_paths` leaves matching declarations out of local admission when the task also declares a path that remains locked. If every declaration matches, Orbit retains the entire surface: with the built-in `**/*.md` default, two docs-only tasks that declare the same document still conflict. Unlocked declarations remain in the task's context ([unlocked paths](../config/workflow.md#unlocked-context-paths)).
- **Shared mode changes how an overlap is resolved.** `workflow.lock_modes` applies when two footprints overlap: an overlap whose point is `shared` does not block admission, and any resulting conflict is handled at landing; other overlaps remain exclusive by default ([lock modes](../config/workflow.md#footprint-lock-modes)). This policy is separate from local selector filtering. Owner pull checks the claim's full declared footprint, and the work-start and frozen execution-claim guards compare full task context against the claim's frozen footprint, including unlocked paths. These checks apply the same shared-mode policy at the overlap, so an unlocked path may still defer a local mixed task against a remote claim unless that overlap is configured as shared. Orbit ships no shared patterns.
- **Outside work reserves too.** An editor session or manual migration can reserve files with `orbit task locks reserve` ([file reservations](https://orbit-cli.com/concepts/tasks/#file-reservations), [concepts](https://orbit-cli.com/concepts/)).

The limits are part of the rule:

- **A task with no `context_files` holds no file lock.** In a drain with more than one slot, such a task waits for the pilot (`awaiting_footprint`) or runs alone (`awaiting_exclusive_slot`), and holds the tree while it runs. A single-slot drain, an explicit `orbit run ship <id>` and owner pull admission admit it on the next pass ([empty footprints](../design/distributed-drain/2_design.md#2-the-ready-queue-and-orbittaskpull), [optimistic merging](./optimistic-merging.md#where-the-operator-stays-in-control)).
- **A task tagged `no-diff-expected` holds no lock**, because it changes no files ([decision](../design/distributed-drain/4_decisions.md#no-diff-expected-work-does-not-hold-context-locks)).
- **Declarations are incomplete.** On one workspace, pilot footprints covered 59–69% of the files a landed commit changed, and about 40% of recovered conflicts traced to an undeclared file. Landing still rebases and runs conflict recovery, so the footprint reduces conflicts and does not prevent them ([path-leases limitations](../design/path-leases/2_design.md#10-concerns--honest-limitations)).
- **A lock is file-grained.** A `symbol:` selector locks its whole file today. Symbol-level and budget-priced admission is a parked proposal, not behaviour ([conflict budget](../design/conflict-budget/1_scope.md)).
- **Outside sessions are invisible.** Path leases for editors and other clients are built but withdrawn from every user surface, so the lock sees drain tasks and manual reservations only ([path leases](../design/path-leases/1_overview.md)).

## What it costs

- **Throughput.** Some tasks wait that would have run, and 100 of 326 measured lock denials (31%) involved tasks that never changed a common file ([path-leases motivation](../design/path-leases/1_overview.md#1-motivation)). An exclusive lock over-blocks, and an under-declared one under-blocks.
- **Declaration work.** Someone has to name the files. The task author or the pilot pays it before the task can start, and a wrong list either blocks innocent work or lets a collision through.
- **Held idle.** A footprint stays reserved while a task waits on review or a stuck run, though nothing changes. Every overlapping task queues behind it, so review latency becomes queue latency unless review runs after landing.
- **Operator tuning.** Relaxing a path trades prevention for repair, and the operator who does it owns the conflicts that follow.

## Applying it

Ask of a new feature: does it start work that can edit files, and does it know which ones before it starts? If so, it takes the declared footprint through the same overlap check, and does not add a second one with its own rules. The check lives in one place, `FootprintLockModes::blocks` in `orbit-common`, and the admission paths call it.

Where it does not apply: work that edits nothing. A `no-diff-expected` task, a read-only query and an observation of the tree need no footprint, and holding one would stall real work for no gain.
