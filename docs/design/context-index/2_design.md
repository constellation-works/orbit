---
title: "Context Index — Design"
owner: claude
last_updated: 2026-11-14
last_validated: 2026-11-14
status: Draft
feature: context-index
doc_role: design
type: design
summary: "Design of the task-history inverted index: labels from merged diffs, tokeniser, posting and id-table format, association scoring with a symbol boost and file roll-up, the rebuilt window and its routine, the query surface, the consumers, both evaluations, the build order and the open questions"
tags: [context-index, plugins, evaluation, task-pilot, context-files]
related_features: [plugins, task-pilot, agent-scorecards, symbol-risk-index, routines, path-leases]
related_artifacts: [ORB-24844]
---

# Context Index — Design

This document specifies the context index introduced in [1_overview.md](./1_overview.md): what it indexes, how it scores, where it lives, who reads it, and how it is evaluated before anything is wired. Nothing here is built, and no implementation task is filed. It was written for [ORB-24844]. Sections 1 to 8 are the design; §9 answers the eight recommendations in the task one by one; §10 and §11 are the two evaluations; §13 and §14 are the build order and the questions for Daniel.

Figures marked *measured* were read from the ws_orbit checkout and store on 2026-11-14. Figures marked *estimate* are not measurements and are named as such.

## 1. Placement: a plugin with three generic core seams

**The index, its tokeniser, its scoring and its routine live in a plugin. Orbit's core gains no index, no tokeniser and no knowledge of words or symbols.** This follows the workspace rule that narrow, single-use-case behaviour stays out of core, and one hard constraint: symbol labels need language parsing, and core parses no language (the selector parser checks only that a `symbol:` selector's file exists, [selector.rs](../../../crates/orbit-common/src/fs/selector.rs)).

### 1.1 Which plugin

The recommendation is the existing **orbit-graph** plugin (namespace `graph`, its own repository beside this one), not a new plugin. Its change-recommendation history design, a document in that repository, already provides the parts this index needs:

| Need | What orbit-graph already has |
|---|---|
| Files and symbols a delivery changed | An extractor that diffs two commit trees with rename detection and maps changed lines to the smallest enclosing named symbol, in ten languages plus Markdown headings |
| Task text beside those changes | A history index (SQLite, in the workspace's `.orbit-graph/` directory) that stores each delivery with its tasks' title, description and acceptance criteria, and marks whether the text is known to predate execution |
| Renames and deletions | Per-delivery path lineage, followed at query time; results must exist in the target tree |
| A query tool | `recommend`: free text or a task id in, ranked `file:` or `symbol:` selectors out, with a `cutoff` for historical replay |
| A scheduled job | `history-sync` and `code-sync` routines, each a deterministic `plugin.tool_call` step, seeded disabled |
| A leakage-safe evaluator | `evaluate`: per-case disposable clone, strict cutoff, file and symbol recall@K and precision@K, four ranking variants |

`recommend` already ranks destinations from similar past tasks. It scores each past delivery by token overlap between the query and that delivery's task text, then credits the delivery's files. The word index uses the same evidence in a different shape. When similarity is a sum over shared words, "find similar tasks, then add up their files" and "look up each word's postings, then add up the files" give the same totals; the inverted index is that sum computed once at build time. The two differ in what is practical: the index answers in a few lookups, it makes the per-word evidence inspectable, and it normalises by word and by file in one step (§5). So the index enters orbit-graph as a **fifth ranking variant**, evaluated beside the four that exist, and the offline evaluation decides which ranking the plugin keeps (§10.5). A second plugin would repeat the extractor, the history store and the evaluator for no gain.

A standalone `context-index` plugin stays possible if Daniel wants the experiment kept apart from orbit-graph (§14, question 1). Nothing in §2 to §11 changes except where the code is checked in.

Orbit-graph is not installed on the host this was written on (`orbit plugin list` reports no plugins), so enabling the index is an operator step: install, grant, pin, and turn on the seeded routine.

### 1.2 The three core seams

Each is generic: it names no index, and other consumers want it.

1. **A landed-delivery feed.** A read-only, cursor-paginated list of landed tasks: task id, landed commit, base commit (first parent), landing time, and the task's title, description and acceptance criteria with a flag saying whether any of the three changed after first dispatch. Core holds every part of this already: `landed_diff_stats` rows carry task, `landed_commit`, `base_commit` and `landed_at` ([landed_diff_stats.rs](../../../crates/orbit-store/src/contracts/landed_diff_stats.rs)), with a backfill (`orbit migrate --backfill-diff-stats`), and task history records which fields an update touched. What is missing is a bulk read. Today a plugin must call `orbit.task.list` with a large `limit` (full rows have no cursor and no "since" filter) and then `orbit.task.show` with `field: "delivery"` once per task. The orbit-graph design names this same feed as its missing Orbit-side seam, and the eval-harness design of [ORB-24839] selects its eval sets from the same join. A task with no landed diff has no row, which is how the index skips no-diff tasks.
2. **A selector-candidates hook for the pilot.** A workspace config key naming a plugin tool that, given a task's text, returns ranked selector candidates. When set, pilot preparation calls it and passes the result to the pilot as unverified hints (§8.1). The precedent is `operator_tool_candidates`: a host-computed list in the pilot's partition that the pilot must classify ([pilot/mod.rs](../../../crates/orbit-core/src/application/task/pilot/mod.rs)). Core knows the contract (text in, selectors with scores out), not how the tool ranks.
3. **Selector conditions in replay.** The downstream evaluation needs the eval-harness replay to run one crew over the same tasks with different starting selectors (§11). That belongs to the eval-harness plugin's design, not to core, and is listed here because this design depends on it.

A fourth seam, a predicted-files hint to the implementer, is deliberately not proposed yet (§8.3).

### 1.3 What was considered and rejected: a core index

Core could build a file-only index with no plugin at all. It already caches each task's landed paths (`codebase_task_facts`, [facts/history.rs](../../../crates/orbit-core/src/application/codebase/facts/history.rs)) for `orbit.codebase` action `related`, and it holds the task text. That would be the fewest moving parts for files. It is rejected because symbols could never join it, and because a tokeniser, a stopword list and a scoring formula tuned on one workspace's vocabulary are the kind of narrow behaviour core is meant to leave to a plugin. The feed in §1.2 gives a plugin the same data.

## 2. What is indexed

### 2.1 Tasks

A task enters the window when all of these hold:

- it is `done` and has a landed commit in the feed (§1.2). Tasks that ended with no diff (`no-diff-expected`, a verified no-diff or already-landed delivery) have none and are skipped entirely;
- its landed commit is on the integration branch's first-parent chain and has a first parent;
- after the path exclusions below, its label is not empty and holds at most `max_label_files` files (default 20, the cap `related` uses for the same reason: a 300-file sweep says nothing about any one word, [related.rs](../../../crates/orbit-core/src/application/codebase/related.rs)). *Measured*: over the last 2,000 first-parent commits the median commit changes 4 files and the 90th percentile 17, and 7.5% change more than 20, so the cap drops about one task in thirteen.

One landed commit that names several tasks is skipped: its diff cannot be divided between them.

### 2.2 Words

The indexed text is the task's **title, description and acceptance criteria**, nothing else. The plan, the execution summary, comments, tags and review material are excluded (§9, recommendation 2). The text used is the current record; the feed's changed-after-dispatch flag is kept on the task's row so the evaluation can exclude or separately report tasks whose wording was edited during execution (§10.2).

### 2.3 Labels: files

The label is the diff of the landed commit against its first parent, with rename detection. Squash merges make this one commit per task on ws_orbit. Each changed path contributes the path as it is named *after* the commit; a deleted file contributes its old path. At build time every path is carried forward through recorded renames to its name at the build's target commit, so a file's history under an old name merges into its current one. A path that no longer exists at the target is dropped from the generation.

Excluded paths never enter a label. `exclude_paths` is a glob list in the plugin's config, seeded for ws_orbit with lockfiles, `CHANGELOG.md`, and generated or mirrored outputs: `docs/INDEX.md`, `plugin/skills/**`, the CLI output goldens, `scripts/unit-test-baseline.json` and `scripts/qa-full-sweep-inventory.json`. *Measured*: these are among the most-touched files of the last 2,000 landed commits (`scripts/unit-test-baseline.json` in 84, `tool_list.json` in 74, `docs/INDEX.md` in 68), and none of them is chosen by a task's wording. The pilot already adds such derived companions by rule; the index leaves that to it.

A genuinely hot hand-written file, such as `docs/CONFIG.md` (*measured*: 145 of 2,000 commits), stays in. Scoring discounts it (§5.1) without hiding it from the tasks that are really about it.

### 2.4 Labels: symbols

Symbols come from the diff hunks. For each changed file in a supported language, the extractor maps every added and deleted line to the smallest named symbol that encloses it and emits `symbol:<path>#<qualified name>:<kind>`. Lines outside any named symbol (imports, module-level code), unsupported languages, binary files and files that fail to parse yield the file context only. A symbol the commit *added* is recorded like any other; whether it can ever be predicted is a separate matter (§12).

Every symbol context implies its file context. A task that changed three symbols in one file counts once for the file and once for each symbol.

## 3. Tokeniser

The tokeniser turns text into a set of terms. It is versioned (`tokenizer_version`), and a generation records the version that built it.

1. **Strip** URLs, task ids (`ORB-12345`), hexadecimal runs of seven or more characters, and bare numbers. Code fences and inline code are kept: they hold the identifiers that matter.
2. **Split identifiers.** A token containing `_`, `-`, `.`, `/`, `::` or an internal case change is split at those points, and the parts are kept *together with* the joined whole. `context_files` yields `context`, `files` and `context_files`; `widenContextFiles` yields `widen`, `context`, `files` and `widencontextfiles`; `crates/orbit-core/src/application/task/paths.rs` yields `orbit`, `core`, `application`, `task`, `paths` and the directory-free stem. File extensions are dropped.
3. **Normalise.** Lowercase; fold a trailing plural `s` (`leases` to `lease`). No heavier stemming by default: a Porter stemmer, which `orbit.search` uses through SQLite ([migration.rs](../../../crates/orbit-search/src/lexical/migration.rs)), mangles identifiers, and is one of the swept variants instead (§10.4).
4. **Drop** terms shorter than two or longer than forty characters, a fixed English stopword list, and **window stopwords**: any term present in more than `max_term_share` of the window's tasks (default 30%). The window rule finds the boilerplate no fixed list knows (`task`, `acceptance`, `orbit`, `crates`, `src`) and adapts to each workspace.
5. **Set semantics.** A term counts once per task, however often it appears, so `tasks` is a count of tasks.

The query side runs the identical tokeniser, with one difference: window stopwords are taken from the generation being queried, not recomputed.

Orbit has no reusable tokeniser or stopword list to share; the FTS5 tokeniser is internal to SQLite. The plugin's `recommend` has its own token function, which the new variant replaces for itself and leaves alone for the others.

## 4. Format and storage

### 4.1 Daniel's sketch and what changes

The sketch was `"lease": [{"doc": <context_id>, "field": "path", "tasks": 1}]`. The design keeps the three ideas in it (interned id, a path or symbol distinction, a per-posting task count) and moves one field:

```
terms      term_id → { term, df }                       df = tasks in the window containing the term
contexts   context_id → { kind: file|symbol, path, symbol?, symbol_kind?, file_id?, tasks }
postings   (term_id, context_id) → tasks                tasks = window tasks with the term AND the context
meta       { generation, format, tokenizer_version, extractor_version, config_digest,
             target_commit, window_tasks (N), window_size (n), oldest_landing, newest_landing, built_at }
```

- `field` becomes `kind` on the context row. A context is a file or a symbol for its whole life, so repeating that on every posting is wasted, and one context never has both kinds.
- A symbol row carries `file_id`, the id of its file's row. That one link is what the file roll-up (§5.2) and the check against HEAD (§7.2) use.
- `contexts.tasks` (how many window tasks touched the context) and `terms.df` are stored because scoring needs both.
- Ids are assigned fresh in each generation. Nothing outside a generation ever holds an id, so they need not be stable (§9, recommendation 5).

The export form, for reading and debugging, is the sketch itself: an export operation prints `"lease": [{"doc": 412, "tasks": 18}, …]` beside the id table.

### 4.2 Storage

A generation is one SQLite file with those four tables (`postings` keyed by `(term_id, context_id)`, without a rowid), in the plugin's workspace directory beside the history index: `.orbit-graph/context-index.<format>.<generation>.sqlite3`. SQLite is chosen over one JSON document because the plugin backend starts a process per call: a query should read a few dozen term rows, not parse the whole index. *Estimate*: with n = 4,000 tasks, some tens of distinct terms per task and a handful of contexts per task, a generation holds a few million postings and tens of megabytes; task 2 of §13 measures it.

Publication is atomic: the build writes a temporary file, validates it, renames it into place, then removes generations beyond `retain_generations` (default 2) by exact file-name grammar only. A reader opens the newest complete generation read-only and is never blocked by a build. The index is a derived cache: deleting it loses nothing that the next build does not recreate. It is not replicated between hosts; the owner of a workspace builds and queries its own.

The layer beneath the generation is the plugin's existing history index: one immutable record per landed delivery (task text, files, symbols, lineage). That layer is append-only and is where extraction cost is paid, once per task. Generations are rebuilt from it (§6).

## 5. Scoring

### 5.1 Association of a term with a context

Raw counts are not ranked (§9, recommendation 4). For a query term `w` and a context `c`:

```
N        tasks in the window
df(w)    window tasks containing w
T(c)     window tasks touching c
t(w,c)   window tasks containing w and touching c          (the posting)
E(w,c)   = df(w) · T(c) / N                                (co-occurrences expected if w and c were unrelated)

assoc(w,c) = max(0, ln( (t(w,c) + κ) / (E(w,c) + κ) ))     κ = 1 by default
score(c)   = Σ over query terms w of assoc(w,c)
```

This is a smoothed, positive pointwise mutual information, and it does both of the normalisations the task asks for with one quantity:

- **Rare words count more.** A word in 40 tasks that co-occurs 18 times with a file 25 tasks touched has `E = 0.25` in a 4,000-task window, so `assoc = ln(19 / 1.25) ≈ 2.7`.
- **Hot files do not top every query.** The same word co-occurring 4 times with a file 280 tasks touched has `E = 2.8`, so `assoc = ln(5 / 3.8) ≈ 0.27`. A hot file scores well only for words that pick it out beyond its base rate.
- **Thin evidence is shrunk.** One co-occurrence can score at most `ln 2 ≈ 0.69`, whatever the word. `κ` is the knob; `min_support` (default 2) additionally drops postings below that count at build time, which also cuts storage. Both are swept.

A separate IDF factor is not added: it would count rarity twice. Title terms may carry a weight above body terms (`title_weight`, default 1, swept).

### 5.2 Symbol boost and file roll-up

Files and symbols are scored separately by §5.1, then combined per file:

```
file_score(F) = score(F) + (b − 1) · max over symbols S in F of score(S)        b = symbol boost, default 1.5
```

- `b = 1` means no boost; the sweep covers 1, 1.25, 1.5, 2 and 3.
- **Why a boost at all.** A symbol's evidence is already inside its file's count, since touching the symbol touched the file. A strong symbol score adds something different: the words pick out one place in the file, not just the file. That is the sense of "symbol gets a boost over path".
- **Why the maximum, not the sum.** A sum would rank a file by how many symbols it has, and large files would win by size.

The answer is a ranking of files. Under each file sit its symbols with `score(S)` above the symbol threshold, best first. When the consumer asks for selectors, a file is emitted as `symbol:` selectors if one to five of its symbols pass the threshold and as one `file:` selector otherwise, the same rule the pilot follows when it writes selectors.

### 5.3 Thresholds

`recommend` returns the top `limit` results. A consumer that needs a set, not a ranking, uses `min_score`: results below it are cut. `min_score` values are not chosen here; the offline evaluation publishes the precision and recall reached at each cut (§10.3), and each consumer's threshold is taken from that table.

## 6. Window and routine

**Window.** The last `window_size` (n) eligible tasks by landing time. The default is set by the sweep; the starting value is 4,000. *Measured*: 3,980 of the 4,416 first-parent commits in the 28 days to 2026-11-14 carry a task id, about 1,000 landed tasks a week, so 4,000 tasks is roughly a month of ws_orbit. A quieter workspace reaches back much further for the same n, which is the point of counting tasks and not days.

**Update: rebuild, don't adjust.** Each run does two things:

1. *Sync labels.* Read the feed from the stored cursor and import each new landed task into the history index (text, files, symbols, lineage). This is incremental and append-only. It is the only expensive step, and it runs once per task.
2. *Rebuild the generation.* Take the newest n eligible tasks from the history index, tokenise, count, and publish a new generation (§4.2). No count is ever decremented: a task that fell out of the window is simply absent from the next build.

Rebuilding costs a pass over n short texts and their labels; no Git or parsing work is repeated. It makes the sweep over n free (any n is one more build over the same layer), and it removes the failure Daniel's increment-and-decrement scheme invites: if the tokeniser, the stopwords, the exclusions or the extractor change between runs, subtracting with the new rules what was added with the old ones leaves counts that are wrong and look fine. A generation records the versions that built it, and a query refuses a generation whose tokeniser version it does not implement.

**Routine.** The plugin ships an activity (`type: deterministic`, `action: plugin.tool_call`, calling the plugin's `maintain` tool with a `context_index_build` operation), a one-step job with `max_active_runs: 1`, and a routine targeting that job, seeded `enabled: false` like every plugin routine:

```yaml
schemaVersion: 1
name: context-index-build
enabled: false
trigger: { cron: "17 5 * * 0", missed_run: catch_up_once }   # weekly, as requested
target: job:graph_context_index_build_pipeline
policy: { timeout_minutes: 30, retries: { max: 1, backoff_minutes: 10 }, overlap: forbid }
```

No agent runs. A plugin tool call is capped at 300 seconds, so the tool is bounded and resumable: one call syncs at most a fixed number of feed rows, reports whether more remain, and rebuilds the generation only once the cursor has caught up. A routine run that does not catch up leaves the published generation in place, and the next run continues from the cursor. The first fill of a long history is an operator loop over the same tool, not something the routine is sized for. The existing `history-sync` routine keeps its Git-only role; the two share the history index under its existing file lock.

**Cadence.** Weekly is the requested default. At ws_orbit's rate a weekly build leaves the index up to 1,000 tasks behind, a quarter of a 4,000-task window, so the recommendation for ws_orbit is daily (§14, question 4). The evaluation reports both (§10.2).

## 7. Query surface

### 7.1 Contract

The tool is orbit-graph's read-only `recommend`, with the word index selectable as a variant. Exact field names follow that tool's schema; the content is:

```
request   one of: task_id | text { title, description, acceptance_criteria[] }
          level: file | symbol        limit        min_score?
          revision?   (target commit; default the checkout HEAD)
          cutoff?     (historical replay: only tasks landed strictly before it; excludes the task itself)

response  index   { generation, built_at, target_commit, window_tasks, newest_landing, lag_tasks?, tokenizer_version }
          results [ { selector, kind, score,
                      evidence { terms: [ {term, assoc, tasks} ], supporting_tasks: n },
                      symbols: [ { selector, score } ] } ]
          dropped_stale   count of results removed by the check against HEAD
          empty_reason?   no_generation | generation_stale | no_known_terms | below_min_score
```

Every result names the terms that produced it and how many tasks back each one, so a person or a pilot can see why a file was suggested. Task ids are not returned to agents by default; the count is.

With `cutoff`, the tool does not read the published generation. It builds a throwaway generation from the tasks landed before the cutoff. That is what the evaluations use, and it is why rebuilds must be cheap.

### 7.2 Check against HEAD

Before returning, each result is resolved against the tree of `revision`:

- a file that exists is kept; one that was renamed since the build is followed through the plugin's path lineage; one that is gone is dropped;
- a symbol is kept only if exactly one symbol with the same path, qualified name and kind exists; otherwise the result falls back to its file, marked as a fallback;
- excluded paths are filtered again, so a config change takes effect without a rebuild.

Dropped results are counted in `dropped_stale`, never silently replaced.

### 7.3 Cold start and empty answers

A task about an area with no history gets an empty answer and a reason. The tool never pads the list with the most frequently touched files, because a padded answer cannot be told from a real one. Every consumer treats empty as "no hint" and does what it does today.

## 8. Where the output is used

The answer to "pre-fill at filing, a hint to the implementer, or both" is: **first a hint to the pilot; pre-fill and an implementer hint only behind their own evidence.** The reason is what `context_files` does. A selector takes a lock for as long as the task is in progress or in review, and a `symbol:` selector locks its whole file ([locks/index.rs](../../../crates/orbit-runtime/src/task/locks/index.rs)). Selectors are also the size that risk and effort read, and part of what the review gate digests. A wrong prediction written straight to the task therefore blocks other work, and the path-leases measurements already found 31% of examined lock denials unnecessary ([path-leases design](../path-leases/2_design.md)).

### 8.1 Stage 1: candidates for the task pilot

The pilot is where selectors are set, and it explores with an agent to set them. With the hook of §1.2 configured, pilot preparation asks the tool for candidates for each task in the partition and passes them in as `selector_candidates`: unverified hints, each with its score and evidence. The pilot's rules do not loosen. It still names only what the change will create, modify or delete, verifies every anchor at its inspection revision, and may discard every candidate. The call is bounded; a failure or an empty answer means no candidates and never holds a task.

It runs first in **shadow mode**: candidates are computed and recorded with the pilot's assessment but not shown to the pilot. That costs nothing and gives, for every piloted task, the index's answer beside the pilot's and, once the task lands, beside the truth.

Why this consumer first: its benefit is already measured. The pilot scorecard grades `context_files_after` against the landed files (recall, precision, F1) and reports cost per assessment and median pilot run duration ([agent-scorecards design](../agent-scorecards/2_design.md), [pilots.rs](../../../crates/orbit-core/src/application/agents/roles/pilots.rs)). A useful hint shows up there as higher recall or a cheaper pilot run, with precision no worse.

### 8.2 Stage 2 (gated): pre-fill without the pilot

A task with no footprint is treated as the whole tree: it waits for the pilot or runs alone. A high-confidence prediction could give such a task a footprint at filing time and let a drain place it sooner. This is allowed only if the offline evaluation shows the thresholded answer reaches the pilot's own precision on the same tasks (gate G3, §10.5), and then only as selectors the pilot may still replace. Better footprints reduce collisions in both directions: fewer undeclared files (about 40% of recovered conflicts trace to a file one side never declared, per the path-leases overview) and, if precision holds, no more unnecessary denials than today.

### 8.3 Stage 3 (gated): a hint to the implementer

Not proposed for building now. An implementer-side brief channel existed and was retired before its trial ran (the archived [explore-brief scope](../_archived/explore-brief/1_scope.md)). Rebuilding a channel is justified only by the replay result in §11, which measures exactly this: whether seeded selectors reduce the implementer's exploration without hurting the outcome. Until then the implementer benefits only through better `context_files` from the pilot.

## 9. Positions on the eight recommendations

| # | Recommendation | Position | Reason |
|---|---|---|---|
| 1 | Labels from the merged diff, not stored `context_files` | **Adopt** | Stored selectors are the declared footprint, widened during delivery over the candidate's changed paths ([context_widening.rs](../../../crates/orbit-core/src/application/task/context_widening.rs)). They still contain planned selectors the work never touched (pilot precision is 82–87%), widening adds only `file:` selectors, never symbols, and the candidate's paths are not always the landed ones. Learning from them would teach the index the pilot's guesses and then grade the pilot against itself. Symbols from diff hunks, §2.4. |
| 2 | Leave plan and execution summary out | **Adopt, and make it a rule on the query side** | A new task has neither. On the index side their words would raise `df` and `N`-relative counts for terms no query contains. The execution summary also names the changed files outright, so letting it near a query would make every evaluation look excellent and mean nothing. One swept variant indexes summary words on the *index side only* to see whether they help (§10.4); query text is never anything but title, description and criteria. Comments are out for the same reasons. |
| 3 | Rebuild the window each week, don't add and subtract | **Adopt, with a cached layer beneath** | §6. Per-task labels are extracted once and kept; the counts are rebuilt from them. This keeps the cost Daniel's incremental scheme was after (no repeated Git or parsing work) without counts that drift when rules change. |
| 4 | Don't rank by raw counts; IDF-like weights, divide by context frequency, stopwords, identifier-aware tokens | **Adopt, as one formula** | §5.1's association does the rarity weighting and the hot-file discount together and shrinks single-task evidence. Stopwords are a fixed list plus a per-window share rule (§3). Identifier-aware splitting keeps parts and whole (§3). A separate IDF factor is not added because it would double-count. |
| 5 | Id table with `kind` on the row; symbol rolls up to file; rename as one-row update; reassignable ids | **Adopt the table and roll-up; the rename and append-only clauses become moot** | §4.1. Because generations are rebuilt (recommendation 3), ids are always reassigned and never need to be append-only. A rename is not a row update either: build-time lineage merges the old path's history into the new path (§2.3), and the query-time check covers renames since the build (§7.2). |
| 6 | Check against HEAD at query time; exclude incidental files | **Adopt** | §7.2 for the check. Exclusion happens twice: at build time so lockfiles and generated outputs never dilute postings (§2.3), and again at query time so a config change needs no rebuild. |
| 7 | Co-change expansion along `orbit.codebase` action `related` | **Do not adopt in the first version; evaluate as a variant, from the plugin's own history** | The index already carries co-change: files that land together share the same task's words and rise together. Expansion can only add files the words did not reach, which is where precision is lost. `related` is also the wrong source for this: it reads live facts over a trailing window from now, needs `min_support` 15 by default and cannot honour a historical cutoff, so it would leak the future into the evaluation. The variant in §10.4 computes co-change from the same labels under the same cutoff and is adopted only if recall@10 rises with precision@5 no lower. |
| 8 | Cold start returns empty; the agent explores as today | **Adopt** | §7.3. An empty answer carries a reason and is never padded. |

## 10. Offline evaluation

The evaluation runs before any consumer is wired, as an analysis task whose scripts and results are attached as task artifacts. It extends the orbit-graph evaluator where that fits and is otherwise a script over an exported dataset; it needs no Orbit change.

### 10.1 Dataset

Every landed ws_orbit task with a label (§2.1). *Measured*: the store holds 12,674 `done` tasks. `landed_diff_stats` is recent (the eval-harness design dates its first rows to 2026-10-26) and is backfilled for older tasks whose commits are in the local checkout. For each task: landing time, landed and base commit, first-dispatch time, the three text fields, the changed-after-dispatch flag, file label, symbol label. The dataset's own statistics are part of the result: tasks dropped by each rule in §2.1, the share of label files that are new in the commit, the share of labels with any symbol, and the share of tasks with eligible text.

### 10.2 Split by time

A random split is not allowed: it would train on tasks that landed after the ones being predicted, and neighbouring tasks share files. The split is a rolling origin that mirrors the routine:

- Choose weekly cutoffs `T1 … Tk` (the last eight complete weeks).
- For cutoff `Ti`, **train** on the n eligible tasks that landed strictly before `Ti`. This is the generation the weekly routine would have published at `Ti`.
- **Test** on tasks first dispatched in `[Ti, Ti + 7 days)`, whenever they landed. Each is predicted with the index it would actually have seen, including its staleness late in the week. A daily-cadence run repeats the protocol with daily cutoffs.
- A test task's **query text** must be the text as dispatched. In order: the audit task-scope blob of its first implement invocation (`task_scope_blob_ref`, [audit_envelope.rs](../../../crates/orbit-types/src/workflow/activity_job/audit_envelope.rs)); else the current text when no update after first dispatch touched description or criteria; else the task is excluded and counted. This is the same order the eval-harness design uses for its snapshots. Audit blobs are pruned after `retention.audit_days` (default 60), so the dataset copies the text when it is built.
- Training text is the current record (§2.2). A training task whose wording was edited during execution may carry words a filer would not have written; the result is reported with and without such tasks.
- **Truth** for a test task is its label restricted to contexts that exist at its base commit, minus excluded paths. Files and symbols the task *created* cannot be predicted by any index over history; they are left out of the denominator and their share is reported as the ceiling it is.

### 10.3 Metrics

Per test task, then averaged over tasks (and also pooled):

- file **recall@k** and **precision@k** for k in 1, 3, 5, 10, 20, and mean reciprocal rank;
- the same for symbols, reported separately, never mixed with files;
- **coverage**: the share of test tasks with a non-empty answer, since §7.3 lets the index decline;
- **set quality at a threshold**: precision, recall and F1 of the result set cut at each `min_score`, computed with the pilot scorecard's definitions so the numbers are comparable with the pilot's;
- the stale-result rate before the check against HEAD.

Every number carries a 95% interval from a percentile bootstrap over tasks, as the experiment readout computes its intervals, and every comparison between two rankings is a paired difference over the same tasks. A difference whose interval includes zero is reported as "no difference shown".

### 10.4 Sweep

- **Window size n:** 500, 1,000, 2,000, 4,000, 8,000, and all history.
- **Symbol boost b:** 1, 1.25, 1.5, 2, 3.
- Secondary: `κ` (0.5, 1, 2, 5), `min_support` (1, 2, 3), `max_label_files` (10, 20, 40), `title_weight` (1, 2), `max_term_share` (20%, 30%, 50%), stemming (plural folding or Porter), index-side summary words (off or on, recommendation 2), co-change expansion (off or on, recommendation 7).

The primary grid is n × b at default secondaries; each secondary is then varied alone around the best cell. To keep the sweep from fitting the test weeks, parameters are chosen on the first five cutoffs and the headline numbers are reported on the last three.

### 10.5 Baselines and gates

| Baseline | What it is | Why it is there |
|---|---|---|
| **BM25 over code** | The task text as a query against the files of the tree at the fold's cutoff commit (path and content), ranked by SQLite FTS5 `bm25`; a second run over symbol documents (name, signature, body) | The required baseline: it knows the code and no history, and has no cold start |
| **Frequency** | The most-touched contexts in the window, the same for every query | The floor. A ranking that cannot beat it has learned only which files are hot |
| **Explicit mentions** | Paths and symbol names written in the task text that resolve at the base commit | Filers often name the site; this shows how much of any score is just reading the task |
| **orbit-graph `recommend`** | Its existing combined and task-search-only variants | The ranking the plugin has today over the same evidence |
| **The pilot and the filer** | `context_files_after` and `context_files_before` for the same tasks, graded the same way | The bar a pre-fill has to reach, recomputed on these folds |

BM25 indexes the tree at each fold's cutoff commit, not at each test task's own base; that approximation is disclosed with the result. Two combinations are also scored, because they are the likely best system: word index plus explicit mentions, and word index fused with BM25 by reciprocal rank.

Gates, each read on the held-out cutoffs:

- **G1, keep the index.** Word-index file recall@10 beats both Frequency and BM25 over code, with the paired interval above zero, at coverage of at least half the test tasks. If it fails, nothing is wired; the result says whether BM25 or a combination deserves the slot instead.
- **G2, which ranking the plugin keeps.** The word index and the existing `recommend` variants are compared directly. The loser is removed, not kept beside the winner, unless the combination beats both.
- **G3, pre-fill.** At some `min_score`, the thresholded set reaches the pilot's precision on the same tasks with recall of at least half the pilot's. Otherwise §8.2 is not built.

An unevaluable gate counts as not passed.

## 11. Downstream evaluation

Offline recall says the prediction is right. It does not say an agent that is handed the prediction explores less or delivers as well. That is measured with the replay job of the eval-harness plugin designed by [ORB-24839] (its design is in review as this is written: closed tasks with known merged diffs are re-run at the commit before their merge, off the delivery path, and scored with held-out tests).

Replay's arms are crews. This evaluation holds the crew fixed and varies the selectors in the snapshot each candidate task starts from, which needs one addition to that design: an arm may name a **selector condition** as well as a crew (§1.2, seam 3).

| Condition | Starting `context_files` | Purpose |
|---|---|---|
| **Unseeded** | Empty | Today's worst case: the implementer finds everything itself |
| **Index-seeded** | The index's answer above the G3 threshold (or top 5 files if G3 failed), built with a cutoff before the task's first dispatch and excluding the task | The thing under test |
| **Dispatched** | What the pilot had set when the task was first dispatched | Today's normal case |
| **Oracle** | The files of the reference diff that exist at the base | The most any predictor could give |

Measured per run, from records that exist: tool calls before the first edit and the result bytes of those calls ([first_edit.rs](../../../crates/orbit-runtime/src/engine/first_edit.rs)), implement tokens and derived cost, implement wall time, the held-out test outcome, diff size, and how many changed files fell outside the starting selectors. Orbit records no time-to-first-edit and no tokens-to-first-edit; the two first-edit counts are the proxies for "exploration", and total implement tokens and time bound it from above.

Order and size:

1. **Oracle first.** One crew, the standing eval set (about 25 tasks), one repeat, conditions Unseeded and Oracle: about 50 runs. If perfect selectors do not reduce exploration, no predictor will, and the evaluation stops there with that finding.
2. **Full comparison**, only if step 1 shows a reduction: all four conditions, three repeats, about 300 runs, paired by task with a bootstrap over tasks.

The index-seeded condition passes if exploration is lower than Unseeded with the interval excluding zero and the held-out pass rate is not lower than Dispatched beyond the interval. The readout reports every condition with its interval and prints "no difference shown" where that is the case.

What replay does not measure: the pilot (replay starts after it) and collisions between concurrent tasks. The pilot effect is read from shadow mode and the pilot scorecard (§8.1), which is a before-and-after comparison, not a randomised one; the live experiment mechanism cannot randomise pilot inputs today, because it assigns arms at first delivery admission, after the pilot has run ([experiments scope](../experiments/1_scope.md)). The collision effect is estimated offline from the existing footprint-miss and lock-contention rankings of `orbit.codebase` action `analytics`: for each recorded miss, would the predicted footprint have declared the file.

## 12. Concerns & Honest Limitations

- **It may not work.** No measurement yet shows that task wording predicts files in this repository. The plugin's recorded recommendation cohort is small and claims nothing. That is why the first two tasks build only a dataset and an evaluation.
- **Symbol evidence is thin.** In one recorded orbit-graph evaluation case, 92 of 98 changed symbols were added by the delivery and did not exist beforehand. Symbols that exist and are edited repeatedly are the minority, so the symbol boost may show no effect, and `b = 1` is an acceptable outcome.
- **New files are invisible.** A task that mostly creates files gets little from any history index. The evaluation reports that share instead of hiding it.
- **Vocabulary drift and self-reinforcement.** Filers are mostly agents, and their wording follows prompts and templates that change. Once predictions feed the pilot, the pilot's selectors steer what implementers touch, and the index learns from what they touched. Labels come from merged diffs, not from selectors, which limits the loop but does not remove it. The pilot scorecard's recall against landed files is the watch on it.
- **Training text is not always what the filer wrote.** Only test tasks are held to dispatch-time text. Edited training tasks are measured both ways (§10.2), but the published generation uses current text.
- **Task ids in commit subjects are the fallback for old history.** Tasks without a `landed_diff_stats` row or a landing receipt can be matched only by the id in the squash subject, which the orbit-graph design treats as weaker evidence. The dataset reports how many tasks rest on it.
- **Bundled and oversized commits are dropped**, so areas changed mostly by sweeps are under-represented.
- **Staleness.** A weekly build at ws_orbit's rate is a quarter-window behind by the end of the week. The evaluation measures the cost; the remedy is cadence, not incremental counts.
- **Replay has little power and a narrow task mix.** About 25 bug-fix tasks detect only large differences in pass rate, and bug reports name their site more often than feature requests do. The exploration counts are continuous and paired, which helps, but a null result there is weak evidence.
- **The plugin is not installed anywhere this was checked**, and its manifest still names an Orbit tool that has since been folded into `orbit.task.show`. Bringing the plugin up to date is a precondition, listed in §13.
- **Doc layout.** This folder has an overview and a design, as the task asked, and puts the build order and open questions after this section; the folder convention's `3_vision.md` and `4_decisions.md` are added when the feature has something for them.

## 13. Build order

Not filed. Each task carries the `context-index` tag when it is. Tasks 1 and 2 are analysis; everything after them waits on gate G1.

| # | Task | Where | Depends on | Outcome |
|---|---|---|---|---|
| 1 | Export the labelled dataset | Analysis (scripts as artifacts) | — | One record per landed ws_orbit task: text, dispatch-time text where recoverable, landed and base commit, file and symbol labels; with the dataset statistics of §10.1 |
| 2 | Offline evaluation and prototype | Analysis | 1 | The word-index prototype, the five baselines, the sweep and the gate verdicts G1 to G3, with scripts and results attached; measured generation size and build time |
| — | **Decision point for Daniel** | — | 2 | Stop, continue with the word index, or continue with what beat it |
| 3 | Landed-delivery feed | Orbit core | G1 | A read-only cursor-paginated feed of landed tasks (§1.2, seam 1) |
| 4 | Bring orbit-graph up to the current tool surface; sync labels from the feed | orbit-graph | 3 | The history index fills from the feed with no per-task calls; the stale delivery-tool reference is gone |
| 5 | Generation build and storage | orbit-graph | 4 | `maintain` builds and atomically publishes a generation; `status` reports generation, window and lag |
| 6 | Query variant | orbit-graph | 5 | `recommend` returns word-index results with evidence, the check against HEAD and empty reasons; `cutoff` builds a throwaway generation |
| 7 | Routine, job and activity; install and enable on ws_orbit | orbit-graph, operator | 5 | The seeded routine exists; the operator installs, grants, pins and enables it |
| 8 | Selector-candidates hook, shadow mode | Orbit core | 6 | The pilot records candidates with each assessment without seeing them (§1.2, seam 2) |
| 9 | Shadow readout; show candidates to the pilot | Analysis, then Orbit core | 8, two weeks of shadow data | Index against pilot against truth on live tasks; if it holds up, the pilot receives the candidates |
| 10 | Selector conditions in replay; oracle-first run | Eval-harness plugin | The eval-harness build through its replay readout; 6 | The §11 result, oracle step first |
| 11 | Pre-fill for tasks with no footprint | Orbit core | G3, 9 | High-confidence selectors written at filing, replaceable by the pilot |
| 12 | Implementer hint | Orbit core | 10 passing | Only if replay shows seeded selectors reduce exploration |

## 14. Open questions for Daniel

1. **Home: orbit-graph, or a new plugin?** Orbit-graph already has the extractor, the history store, the lineage, the query tool and the evaluator; a new plugin would copy them. *Recommendation: orbit-graph.*
2. **Is the word index a replacement for `recommend`'s current ranking or a variant beside it?** Both rank from the same evidence. *Recommendation: a variant until the evaluation; then keep one (gate G2), not both.*
3. **Is counting the window in tasks still what you want, given about 1,000 landed tasks a week on ws_orbit?** n = 4,000 is a month there and years elsewhere. *Recommendation: yes, tasks; take the default from the sweep and start at 4,000.*
4. **Weekly or daily?** Weekly is what you asked for; on ws_orbit it leaves a quarter of the window stale by the end of the week, and a build is cheap. *Recommendation: ship the routine with the weekly cron as the default and set ws_orbit's copy to daily if the evaluation shows the weekly staleness costs recall.*
5. **Plan and execution summary.** You were unsure. *Recommendation: out of both sides; one sweep variant tests summary words on the index side only, and query text never includes them.*
6. **First consumer.** Pre-fill at filing is the most direct use but writes locks from a guess. *Recommendation: pilot candidates in shadow mode first; pre-fill only past gate G3; no implementer hint until replay supports it.*
7. **May core gain the landed-delivery feed?** It is generic and three consumers want it (this index, orbit-graph's existing sync, the eval-harness set builder), but it is a new public read surface. *Recommendation: yes, after G1.*
8. **Replay budget.** The full downstream comparison is about 300 candidate runs. *Recommendation: approve the 50-run oracle step first and decide on the rest from its result.*
9. **One index per workspace, or shared across repositories?** Vocabulary and paths are per repository. *Recommendation: one per workspace; no cross-workspace index.*

## Task References

- [ORB-24844] — wrote this design
- [ORB-24839] — designed the eval-harness plugin and its replay job, which §11 uses and extends with selector conditions

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
