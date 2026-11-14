---
title: "Context Index — Overview"
owner: claude
last_updated: 2026-11-14
last_validated: 2026-11-14
status: Draft
stage: designing
next: "Daniel answers 2_design.md §14; the offline evaluation (§13 tasks 1–2) is the first to file"
feature: context-index
doc_role: overview
type: design
summary: "A plugin-built inverted index from the words of landed tasks to the files and symbols their merged diffs changed, queried with a new task's wording to predict its context_files, and shipped only if a time-split evaluation beats BM25 over code"
tags: [context-index, plugins, evaluation, task-pilot, context-files]
related_features: [plugins, task-pilot, agent-scorecards, symbol-risk-index, routines, path-leases]
related_artifacts: [ORB-24844]
---

# Context Index — Overview

The context index is a proposed inverted index built from task history alone. Each word that appeared in a landed task's title, description or acceptance criteria points at the files and symbols that task's merged diff changed, with a count of how many tasks made that link. Given the wording of a new task, the index returns a ranked list of files and symbols the work will probably touch, so a `context_files` footprint can be proposed before any agent explores the code. Nothing here is built. [2_design.md](./2_design.md) is the design, written for [ORB-24844]; it ends with the build order and the questions Daniel has to answer.

## 1. Motivation

A task's `context_files` do three jobs at once: they are the lock footprint a drain schedules around, the size that task risk and effort read before a diff exists, and the implementer's starting reading list. Today they are produced by exploration. The task pilot is a read-only agent run that searches the checkout for each task (up to 1,800 seconds per partition, [task_pilot.yaml](../../../crates/orbit-core/assets/activities/task_pilot.yaml)), and the implementer explores again before its first edit. The result is still incomplete: measured on ws_orbit, pilot footprints cover 59–69% of the files the landed commit changed, at 82–87% precision ([path-leases overview](../path-leases/1_overview.md)).

Daniel's request (2026-11-14) is to predict the footprint from the task's wording, and to try a plain inverted index over task history before any trained model: words from title, description and acceptance criteria; postings to context ids with a `tasks` count; a window of the last n tasks refreshed by a weekly routine; interned ids instead of full paths; and a boost for symbol matches over path matches.

Three findings from the code shape the design:

1. **Most of the machinery exists in the orbit-graph plugin.** It already extracts the files and symbols each delivery changed (tree-sitter, ten languages), keeps them with the task's text in a history index, follows renames, checks results against the target tree, and ranks destinations from similar tasks through its `recommend` tool. It also has a chronological evaluator. The word index is a new ranking over evidence that plugin already holds, not a new subsystem.
2. **Nothing has shown that history-based prediction works here.** The plugin's recorded evaluation cohort is small and, by its own account, supports no superiority claim, because almost no historical task text could be proven to predate execution. The missing piece is a labelled dataset at scale, not a ranking formula.
3. **`context_files` is a strict list with side effects.** A wrong selector takes a lock and changes risk and review scope. A prediction therefore starts as a hint to the pilot, not as a write to the task.

So the design puts measurement first: build the labelled dataset, run the time-split evaluation with the index and its baselines, and wire a consumer only for what passes.

## 2. Core Concepts

- **Context.** One thing a task can touch: a file, or a symbol inside a file. Contexts are interned: an id table maps a small integer to the path (and symbol), and postings carry only the integer.
- **Label.** The set of contexts a landed task's merged diff changed. Labels come from the diff of the landed commit against its first parent, never from the task's stored `context_files`.
- **Term.** A normalised word from a task's title, description or acceptance criteria, after identifier-aware splitting and stopword removal. A term counts once per task.
- **Posting.** `(term, context, tasks)`: the number of tasks in the window whose wording contained the term and whose label contains the context.
- **Window.** The last n landed tasks that have a label. Tasks that landed no diff are never in it.
- **Generation.** One complete index built from one window. A build writes a whole new generation and publishes it atomically; counts are never incremented or decremented in place.
- **Association.** The score one term gives one context: how much more often the two occur together than their separate frequencies predict. This one quantity discounts both common words and files that every task touches.
- **Symbol boost.** The extra weight a file receives when one of its symbols is itself strongly associated with the query.
- **Selector candidate.** One ranked result returned to a consumer: a `file:` or `symbol:` selector with a score and the evidence behind it. A candidate is a hint; it is not a `context_files` entry until the pilot or a gated rule writes it.

## 3. At a Glance

Nothing is implemented. The table maps each concern to where the design settles it and to what it rests on.

| Concern | Settled in | Rests on |
|---------|-----------|----------|
| What is indexed, and where labels come from | [2_design.md §2](./2_design.md#2-what-is-indexed) | [landed.rs](../../../crates/orbit-core/src/application/codebase/landed.rs), [landed_diff_stats.rs](../../../crates/orbit-store/src/contracts/landed_diff_stats.rs), the orbit-graph change extractor |
| Tokeniser and stopwords | [2_design.md §3](./2_design.md#3-tokeniser) | — |
| Posting and id-table format, storage | [2_design.md §4](./2_design.md#4-format-and-storage) | The plugin standard's state rules ([plugins scope](../plugins/1_scope.md)) |
| Scoring, symbol boost, file roll-up | [2_design.md §5](./2_design.md#5-scoring) | — |
| Window and its routine | [2_design.md §6](./2_design.md#6-window-and-routine) | [routines design](../routines/2_design.md), the `plugin.tool_call` action |
| Query surface and the check against HEAD | [2_design.md §7](./2_design.md#7-query-surface) | orbit-graph `recommend` and its path lineage |
| Where the output is used | [2_design.md §8](./2_design.md#8-where-the-output-is-used) | [task pilot](../task-pilot/1_scope.md), [pilot scorecard](../agent-scorecards/2_design.md), [locks/index.rs](../../../crates/orbit-runtime/src/task/locks/index.rs) |
| Core and plugin split | [2_design.md §1](./2_design.md#1-placement-a-plugin-with-three-generic-core-seams) | [plugins scope](../plugins/1_scope.md) |
| The eight recommendations | [2_design.md §9](./2_design.md#9-positions-on-the-eight-recommendations) | — |
| Offline evaluation | [2_design.md §10](./2_design.md#10-offline-evaluation) | [pilots.rs](../../../crates/orbit-core/src/application/agents/roles/pilots.rs), the audit task-scope blob |
| Downstream evaluation | [2_design.md §11](./2_design.md#11-downstream-evaluation) | The eval-harness replay design of [ORB-24839], [first_edit.rs](../../../crates/orbit-runtime/src/engine/first_edit.rs) |
| Build order and open questions | [2_design.md §13](./2_design.md#13-build-order) and [§14](./2_design.md#14-open-questions-for-daniel) | — |

## Task References

- [ORB-24844] — wrote this overview and the design
- [ORB-24839] — designed the eval-harness plugin whose replay job the downstream evaluation uses

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
