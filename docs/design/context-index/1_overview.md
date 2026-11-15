---
title: "Context Index — Overview"
owner: claude
last_updated: 2026-11-14
last_validated: 2026-11-14
status: Draft
stage: designing
next: "File 2_design.md §13 tasks 1–5 in ws_orbit-graph (retired-tool fix, word_index variant, baselines and gates, options); Daniel answers the questions still open in §14.2"
feature: context-index
doc_role: overview
type: design
summary: "An inverted index, built in the orbit-graph plugin, from the words of landed tasks to the symbols and files their merged diffs changed, queried with a new task's wording to predict its context_files, and shipped only if a time-split evaluation beats BM25 over code"
tags: [context-index, plugins, evaluation, task-pilot, context-files]
related_features: [plugins, task-pilot, agent-scorecards, symbol-risk-index, routines, path-leases]
related_artifacts: [ORB-24844, ORB-24851, ORB-24861, ORB-24864]
---

# Context Index — Overview

The context index is a proposed inverted index built from task history alone. Each word that appeared in a landed task's title, description or acceptance criteria points at the symbols that task's merged diff changed, and through them at their files, with a count of how many tasks made that link. Given the wording of a new task, the index returns a ranked list of files and symbols the work will probably touch, so a `context_files` footprint can be proposed before any agent explores the code. Nothing here is built. [2_design.md](./2_design.md) is the design, written for [ORB-24844], revised by [ORB-24851] and [ORB-24861] and corrected by [ORB-24864]; it ends with the build order, the decisions Daniel has made, the questions still open and one deferred idea.

Daniel has decided (2026-11-14) that the index is built in the orbit-graph plugin, as its `word_index` ranking variant, and that the dataset and the offline evaluation are built inside that plugin's `recommend` and `evaluate`. Two ideas of his are specified as options of the variant, and the offline evaluation chooses between their settings: how much a task counts for each symbol it changed, and whether symbols a task *added* count like symbols it modified.

The first version is built from words only. It uses no tags and no task type (`feature`, `bug`, `refactor`, `chore`): not as index keys, not as stored fields, not as per-type weights and not as per-type results. Daniel's decision (2026-11-14): "I would say just forget about using tags or types for v1. we will see how a simple version turns out first". An earlier revision specified both; that material is kept as a deferred idea in [2_design.md §15](./2_design.md#15-deferred-tags-and-task-type), with his objections to the type as a key, and is looked at again only after the first version has been evaluated.

## 1. Motivation

A task's `context_files` do three jobs at once: they are the lock footprint a drain schedules around, the size that task risk and effort read before a diff exists, and the implementer's starting reading list. Today they are produced by exploration. The task pilot is a read-only agent run that searches the checkout for each task (up to 1,800 seconds per partition, [task_pilot.yaml](../../../crates/orbit-core/assets/activities/task_pilot.yaml)), and the implementer explores again before its first edit. The result is still incomplete: measured on ws_orbit, pilot footprints cover 59–69% of the files the landed commit changed, at 82–87% precision ([path-leases overview](../path-leases/1_overview.md)).

Daniel's request (2026-11-14) is to predict the footprint from the task's wording, and to try a plain inverted index over task history before any trained model: words from title, description and acceptance criteria; postings to context ids with a `tasks` count; a window of the last n tasks refreshed by a weekly routine; interned ids instead of full paths; and a boost for symbol matches over path matches.

Three findings from the code shape the design:

1. **Most of the machinery exists in the orbit-graph plugin.** It already extracts the files and symbols each delivery changed (tree-sitter, ten languages), keeps them with the task's text in a history index, follows renames, checks results against the target tree, and ranks destinations from similar tasks through its `recommend` tool. It also has a chronological evaluator. The word index is a new ranking over evidence that plugin already holds, not a new subsystem, and that plugin is where it is built.
2. **Nothing has shown that history-based prediction works here.** The plugin's recorded evaluation cohort is small and, by its own account, supports no superiority claim, because almost no historical task text could be proven to predate execution. The missing piece is a labelled dataset at scale, not a ranking formula.
3. **`context_files` is a strict list with side effects.** A wrong selector takes a lock and changes risk and review scope. A prediction therefore starts as a hint to the pilot, not as a write to the task.

So the design puts measurement first: build the labelled dataset, run the time-split evaluation with the index, its options and its baselines, and wire a consumer only for what passes.

## 2. Core Concepts

- **Context.** One thing a task can touch. The unit is the *leaf*: a named symbol, or a file's *remainder*, which holds the changed lines no named symbol encloses. A file is the roll-up of its leaves. Contexts are interned: an id table maps a small integer to the path (and symbol), and postings carry only the integer.
- **Label.** The contexts a landed task's merged diff changed, each with how many lines the task added and deleted in it, its length, and whether the task added, modified or deleted it. Labels come from the diff of the landed commit against its first parent, never from the task's stored `context_files`.
- **Term.** A normalised word from a task's title, description or acceptance criteria, after identifier-aware splitting and stopword removal. A term counts once per task.
- **Posting.** `(term, context, tasks)`: the number of tasks in the window whose wording contained the term and whose label contains the context, with the same tasks also summed by their posting weight.
- **Posting weight.** How much one task counts for one context. Three settings are compared: presence only, the share of the symbol's own lines the task changed, and the context's share of the task's whole diff.
- **Added and modified.** A symbol or file the task created is *added*; one that existed before is *modified*. An option discounts added contexts, down to ignoring them, so that a feature query is answered with the places earlier features plugged into and not with the insides of those features.
- **Window.** The last n landed tasks that have a label. Tasks that landed no diff are never in it.
- **Generation.** One complete index built from one window with one setting of each option. A build writes a whole new generation and publishes it atomically; counts are never incremented or decremented in place.
- **Association.** The score one term gives one context: how much more often the two occur together than their separate frequencies predict. This one quantity discounts both common words and files that every task touches.
- **Symbol boost.** The extra weight a file receives when one of its symbols is itself strongly associated with the query.
- **Selector candidate.** One ranked result returned to a consumer: a `file:` or `symbol:` selector with a score and the evidence behind it. A candidate is a hint; it is not a `context_files` entry until the pilot or a gated rule writes it.

## 3. At a Glance

Nothing is implemented. The table maps each concern to where the design settles it and to what it rests on.

| Concern | Settled in | Rests on |
|---------|-----------|----------|
| What is indexed, and where labels come from | [2_design.md §2](./2_design.md#2-what-is-indexed) | [landed.rs](../../../crates/orbit-core/src/application/codebase/landed.rs), [landed_diff_stats.rs](../../../crates/orbit-store/src/contracts/landed_diff_stats.rs), the orbit-graph change extractor |
| Symbols as the unit, the remainder, file roll-up | [2_design.md §2.4](./2_design.md#24-labels-symbols-are-the-unit) | The per-side symbol byte spans and changed lines in the orbit-graph history index. Its schema extension and backfill add symbol line spans, file line counts and `named_symbol_covered_lines` from the union of all named symbol ranges, so rebuilds can derive the remainder length without re-extraction ([2_design.md §2.4](./2_design.md#24-labels-symbols-are-the-unit), [§13](./2_design.md#13-build-order)) |
| Added and modified symbols | [2_design.md §2.5](./2_design.md#25-added-and-modified-symbols) and [§5.5](./2_design.md#55-option-added-versus-modified) | — |
| Tokeniser and stopwords | [2_design.md §3](./2_design.md#3-tokeniser) | — |
| Posting and id-table format, storage | [2_design.md §4](./2_design.md#4-format-and-storage) | The plugin standard's state rules ([plugins scope](../plugins/1_scope.md)) |
| Scoring, symbol boost, file roll-up | [2_design.md §5](./2_design.md#5-scoring) | — |
| Posting weights and how each enters the score | [2_design.md §5.4](./2_design.md#54-option-posting-weight) | — |
| Window and its routine | [2_design.md §6](./2_design.md#6-window-and-routine) | [routines design](../routines/2_design.md), the `plugin.tool_call` action |
| Query surface and the check against HEAD | [2_design.md §7](./2_design.md#7-query-surface) | orbit-graph `recommend` and its path lineage |
| Where the output is used | [2_design.md §8](./2_design.md#8-where-the-output-is-used) | [task pilot](../task-pilot/1_scope.md), [pilot scorecard](../agent-scorecards/2_design.md), [locks/index.rs](../../../crates/orbit-runtime/src/task/locks/index.rs) |
| Core and plugin split; orbit-graph as the home | [2_design.md §1](./2_design.md#1-placement-a-plugin-with-three-generic-core-seams) | [plugins scope](../plugins/1_scope.md); Daniel's decision, [§14.1](./2_design.md#141-decided-by-daniel-2026-11-14) |
| The eight recommendations | [2_design.md §9](./2_design.md#9-positions-on-the-eight-recommendations) | — |
| Offline evaluation, with the option comparison | [2_design.md §10](./2_design.md#10-offline-evaluation) | orbit-graph `recommend` and `evaluate`, [pilots.rs](../../../crates/orbit-core/src/application/agents/roles/pilots.rs), the audit task-scope blob |
| Downstream evaluation | [2_design.md §11](./2_design.md#11-downstream-evaluation) | The eval-harness replay design of [ORB-24839], [first_edit.rs](../../../crates/orbit-runtime/src/engine/first_edit.rs) |
| Build order, decisions and open questions | [2_design.md §13](./2_design.md#13-build-order) and [§14](./2_design.md#14-decisions-and-open-questions) | — |
| Deferred, not in the first version: tags and task type | [2_design.md §15](./2_design.md#15-deferred-tags-and-task-type) | Daniel's decision, [§14.1](./2_design.md#141-decided-by-daniel-2026-11-14) |

## Task References

- [ORB-24844] — wrote this overview and the design
- [ORB-24851] — recorded the orbit-graph decision and added the posting-weight and added-versus-modified options
- [ORB-24864] — found that the history index holds byte spans, not line lengths, and corrected the design's line-length claims and build scope
- [ORB-24861] — recorded the decision to leave tags and task type out of the first version
- [ORB-24839] — designed the eval-harness plugin whose replay job the downstream evaluation uses

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
