---
title: "Context Index — Design"
owner: claude
last_updated: 2026-11-14
last_validated: 2026-11-14
status: Draft
feature: context-index
doc_role: design
type: design
summary: "Design of the task-history inverted index built in orbit-graph: symbol-first labels from merged diffs, tokeniser, posting and id-table format, association scoring with posting-weight, added-versus-modified and type-and-tag options, the rebuilt window and its routine, the query surface, the consumers, both evaluations, the build order and the open questions"
tags: [context-index, plugins, evaluation, task-pilot, context-files]
related_features: [plugins, task-pilot, agent-scorecards, symbol-risk-index, routines, path-leases]
related_artifacts: [ORB-24844, ORB-24851]
---

# Context Index — Design

This document specifies the context index introduced in [1_overview.md](./1_overview.md): what it indexes, how it scores, where it lives, who reads it, and how it is evaluated before anything is wired. Nothing here is built, and this document files no task. It was written for [ORB-24844] and revised by [ORB-24851]. Sections 1 to 8 are the design; §9 answers the eight recommendations of [ORB-24844] one by one; §10 and §11 are the two evaluations; §13 and §14 are the build order, the decisions Daniel has made and the questions still open.

The revision records one decision and specifies three options. The decision: the index is built in the orbit-graph plugin (§1.1, §14). The options, each built as an option of the plugin's `word_index` ranking variant:

| Option | Settings | Specified in |
|---|---|---|
| `posting_weight` | `presence`, `symbol_fraction`, `diff_share` | §2.4 (what is recorded), §5.4 (how it scores) |
| `added_weight` | 1 (added symbols count in full) down to 0 (modified symbols only) | §2.5, §5.5 |
| `tag_mode` | `off`, `terms`, `nested` | §2.6, §5.6 |

The design does not pick a setting by argument. It states what each one does to the scoring of §5, and the offline evaluation chooses (§10.4, gate G4).

Figures marked *measured* were read from the ws_orbit checkout and store on 2026-11-14. Figures marked *estimate* are not measurements and are named as such.

## 1. Placement: a plugin with three generic core seams

**The index, its tokeniser, its scoring and its routine live in a plugin. Orbit's core gains no index, no tokeniser and no knowledge of words or symbols.** This follows the workspace rule that narrow, single-use-case behaviour stays out of core, and one hard constraint: symbol labels need language parsing, and core parses no language (the selector parser checks only that a `symbol:` selector's file exists, [selector.rs](../../../crates/orbit-common/src/fs/selector.rs)).

### 1.1 Which plugin

**Decided (Daniel, 2026-11-14): the index is built in the existing orbit-graph plugin** (namespace `graph`, its own repository beside this one), not in a new plugin. Its change-recommendation history design, a document in that repository, already provides the parts this index needs:

| Need | What orbit-graph already has |
|---|---|
| Files and symbols a delivery changed | An extractor that diffs two commit trees with rename detection and maps changed lines to the smallest enclosing named symbol, in ten languages plus Markdown headings. Its history index keeps each changed symbol once per side of the diff (before, after) with its line span and its changed lines, and each file with its change kind and added and deleted line counts |
| Task text beside those changes | A history index (SQLite, in the workspace's `.orbit-graph/` directory) that stores each delivery with its tasks' title, description and acceptance criteria, and marks whether the text is known to predate execution. It stores no task type and no tags (§2.6 adds them) |
| Renames and deletions | Per-delivery path lineage, followed at query time; results must exist in the target tree |
| A query tool | `recommend`: free text or a task id in, ranked `file:` or `symbol:` selectors out, with a `cutoff` for historical replay |
| A scheduled job | `history-sync` and `code-sync` routines, each a deterministic `plugin.tool_call` step, seeded disabled |
| A leakage-safe evaluator | `evaluate`: per-case disposable clone, strict cutoff, file and symbol recall@K and precision@K, four ranking variants |

`recommend` already ranks destinations from similar past tasks. It scores each past delivery by token overlap between the query and that delivery's task text, then credits the delivery's files. The word index uses the same evidence in a different shape. When similarity is a sum over shared words, "find similar tasks, then add up their files" and "look up each word's postings, then add up the files" give the same totals; the inverted index is that sum computed once at build time. The two differ in what is practical: the index answers in a few lookups, it makes the per-word evidence inspectable, and it normalises by word and by file in one step (§5). So the index enters orbit-graph as a **fifth ranking variant, `word_index`**, evaluated beside the four that exist (`combined`, `task_search_only`, `graph_only`, `frequency`), and the offline evaluation decides which ranking the plugin keeps (§10.5). A second plugin would repeat the extractor, the history store and the evaluator for no gain.

Build tasks are therefore filed in `ws_orbit-graph` under the `context-index` tag: the retired-tool fix, the `word_index` variant, the baselines and gates, and the options of §5.4 to §5.6 (§13). The dataset and the offline evaluation are built inside the plugin's `recommend` and `evaluate`, not as separate analysis scripts.

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

The indexed text is the task's **title, description and acceptance criteria**, nothing else. The plan, the execution summary, comments and review material are excluded (§9, recommendation 2). The task's type and tags are not text and are never tokenised into words; whether they enter the index at all is the option of §5.6. The text used is the current record; the feed's changed-after-dispatch flag is kept on the task's row so the evaluation can exclude or separately report tasks whose wording was edited during execution (§10.2).

### 2.3 Labels: files

The label is the diff of the landed commit against its first parent, with rename detection. Squash merges make this one commit per task on ws_orbit. Each changed path contributes the path as it is named *after* the commit; a deleted file contributes its old path. At build time every path is carried forward through recorded renames to its name at the build's target commit, so a file's history under an old name merges into its current one. A path that no longer exists at the target is dropped from the generation.

Excluded paths never enter a label. `exclude_paths` is a glob list in the plugin's config, seeded for ws_orbit with lockfiles, `CHANGELOG.md`, and generated or mirrored outputs: `docs/INDEX.md`, `plugin/skills/**`, the CLI output goldens, `scripts/unit-test-baseline.json` and `scripts/qa-full-sweep-inventory.json`. *Measured*: these are among the most-touched files of the last 2,000 landed commits (`scripts/unit-test-baseline.json` in 84, `tool_list.json` in 74, `docs/INDEX.md` in 68), and none of them is chosen by a task's wording. The pilot already adds such derived companions by rule; the index leaves that to it.

A genuinely hot hand-written file, such as `docs/CONFIG.md` (*measured*: 145 of 2,000 commits), stays in. Scoring discounts it (§5.1) without hiding it from the tasks that are really about it.

### 2.4 Labels: symbols are the unit

Daniel's direction (2026-11-14): since labels come from the actual diff, go to full symbol granularity, measure how much of each symbol the task changed, and let that measure serve as a scoring weight. So the symbol is the unit of the index, and a file is a roll-up of what is inside it.

**Leaves.** Every changed line belongs to exactly one *leaf* context: an added line is placed by its position in the file after the commit, a deleted line by its position before it.

- A line inside a named symbol belongs to the **smallest** named symbol that encloses it, `symbol:<path>#<qualified name>:<kind>`. A line in a method counts for the method and not also for its `impl` block or class; the container keeps only the lines that sit directly in it.
- A line with no enclosing named symbol belongs to the file's **remainder**, one leaf per file. That covers imports, module-level statements and the space between symbols. For a file in an unsupported language, or one that fails to parse, every changed line is in the remainder, so the index behaves there as a plain file index. A binary file has no lines: its remainder is recorded as one changed line of a one-line file.

The remainder is where changes with no enclosing symbol go. It is scored like a symbol but is never returned as a selector and never earns the symbol boost; its uses are in §5.2.

**What is recorded for each task and leaf.** `added` and `deleted` line counts, `len_before` and `len_after` (the leaf's line count on each side, 0 on a side where it does not exist) and `status` (§2.5). For a remainder, the length on a side is the file's line count minus the lines inside its named symbols. The plugin's history index already holds each changed symbol's span and changed lines per side, so these are read from it and no second extraction is needed.

**File roll-up.** A file context is not a leaf. For each task, the file's record is the sum of its leaves' line counts, with the file's own lengths and status. A task that changed three symbols in one file therefore counts once for the file and once for each symbol. The file's postings are computed from these per-task records at build time and stored; they cannot be derived by adding up leaf postings, which would count that task three times.

A symbol is matched between the two sides by path (through the commit's renames), qualified name and kind. One that the commit renamed, or moved to another file, appears as a deleted symbol and an added one (§12).

### 2.5 Added and modified symbols

Every task-and-context record carries a `status`:

| Status | Symbol | File |
|---|---|---|
| `added` | Exists only after the commit | The diff reports the file as added or copied |
| `modified` | Exists on both sides | Modified, renamed or type-changed; a renamed file counts under its new path |
| `deleted` | Exists only before the commit | Deleted |

A remainder has the status of its file. Status is always recorded, whatever option is chosen, and each posting keeps a separate count of the tasks that *added* its context (§4.1), so the evidence behind a result can say "modified by 14 tasks, created by 1".

Why it matters: in one recorded orbit-graph evaluation case, 92 of 98 changed symbols were added by the delivery. For a query that describes a new feature, the symbols that past features created are the wrong answer: they belong to those features. The useful answer is where a feature plugs in, which is the set of existing symbols and files that past feature tasks modified. How status changes the scoring is §5.5.

### 2.6 Type and tags

Each task's record also carries its **type** (`feature`, `bug`, `refactor` or `chore`, the task's existing `type` field, [status.rs](../../../crates/orbit-types/src/task/model/status.rs)) and its **tags**. The plugin's history index stores neither today; its task rows gain both, with a flag saying whether they are known as of first dispatch. Together they are the task's *keys*.

- Tags are normalised: lowercased, trimmed, with `_` folded to `-`.
- `exclude_tags` is a glob list in the plugin's config for tags that mark handling, not subject. It is seeded with `no-diff-expected`, `skip-review`, `skip-validation` and `breaking-change`.
- **A query uses only the type and tags the task had when it was first dispatched.** Some tags are set during delivery; `breaking-change` is set by the implementer. Such a tag on the query side would be an answer leaked into the question, exactly as the execution summary would be (§9, recommendation 2). The evaluation takes test tasks' keys from the same dispatch-time source as their text (§10.2).

Whether keys are used, and how, is the option of §5.6. Recording them is unconditional, because the evaluation reports its results per task type in every case (§10.3).

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
contexts   context_id → { kind: file|symbol|remainder, path, symbol?, symbol_kind?, file_id?,
                          tasks, mass }
postings   (term_id, context_id) → { tasks, tasks_added, mass }
                                                        tasks = window tasks with the term AND the context
                                                        tasks_added = those of them that added the context
                                                        mass = the same tasks, each counted by its posting weight
meta       { generation, format, tokenizer_version, extractor_version, config_digest,
             options { posting_weight, smoothing_unit, added_weight, tag_mode, … },
             target_commit, window_tasks (N), window_size (n), oldest_landing, newest_landing, built_at }

only with tag_mode nested (§5.6):
keys          key_id → { kind: type|tag, name, tasks }  tasks = window tasks carrying the key
key_terms     (key_id, term_id) → df
key_contexts  (key_id, context_id) → { tasks, mass }
key_postings  (key_id, term_id, context_id) → { tasks, mass }
```

- `field` becomes `kind` on the context row. A context is a file, a symbol or a file's remainder (§2.4) for its whole life, so repeating that on every posting is wasted, and one context never has two kinds.
- A symbol or remainder row carries `file_id`, the id of its file's row. That one link is what the file roll-up (§5.2) and the check against HEAD (§7.2) use.
- `contexts.tasks` (how many window tasks touched the context) and `terms.df` are stored because scoring needs both.
- `mass` is the weighted form of `tasks` (§5.4). Under the `presence` weight with `added_weight` 1 the two are equal. `tasks` stays an integer count under every option: `min_support` and the evidence shown to a reader use it.
- A generation is built with one setting of each option and records them in `meta.options` and in `config_digest`. Comparing settings means building one generation per setting, which §6 makes cheap.
- The `key_*` tables are Daniel's nested sketch, `"tag_1": {"word_1": [ … ]}`, in table form: the same three counts as the global index, restricted to the tasks that carry one key.
- Ids are assigned fresh in each generation. Nothing outside a generation ever holds an id, so they need not be stable (§9, recommendation 5).

The export form, for reading and debugging, is the sketch itself: an export operation prints `"lease": [{"doc": 412, "tasks": 18}, …]` beside the id table, and with `tag_mode: nested` also `"dashboard": {"lease": [{"doc": 412, "tasks": 6}, …]}` for each key.

### 4.2 Storage

A generation is one SQLite file with the tables of §4.1 (`postings` keyed by `(term_id, context_id)`, without a rowid), in the plugin's workspace directory beside the history index: `.orbit-graph/context-index.<format>.<generation>.sqlite3`. SQLite is chosen over one JSON document because the plugin backend starts a process per call: a query should read a few dozen term rows, not parse the whole index. *Estimate*: with n = 4,000 tasks, some tens of distinct terms per task and a handful of contexts per task, a generation holds a few million postings and tens of megabytes. Symbol-first labels add leaf rows in proportion to the symbols changed per task, and `tag_mode: nested` repeats a task's postings once for each key it carries. Task 4 of §13 measures the size under each option.

Publication is atomic: the build writes a temporary file, validates it, renames it into place, then removes generations beyond `retain_generations` (default 2) by exact file-name grammar only. A reader opens the newest complete generation read-only and is never blocked by a build. The index is a derived cache: deleting it loses nothing that the next build does not recreate. It is not replicated between hosts; the owner of a workspace builds and queries its own.

The layer beneath the generation is the plugin's existing history index: one immutable record per landed delivery (task text, type and tags, files, symbols with their line counts and status, lineage). That layer is append-only and is where extraction cost is paid, once per task. Generations are rebuilt from it (§6).

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

This is the formula under the default options: `presence` weight, `added_weight` 1, `tag_mode: off`. Each option of §5.4 to §5.6 changes one input of it and keeps its shape.

### 5.2 Symbol boost and file roll-up

Leaves (symbols and remainders) are scored by §5.1 on their own postings, and a file is scored by §5.1 on its rolled-up postings (§2.4). The two are then combined per file:

```
file_score(F) = score(F) + (b − 1) · max over symbols S in F of score(S)        b = symbol boost, default 1.5
```

- `b = 1` means no boost; the sweep covers 1, 1.25, 1.5, 2 and 3.
- **Why a boost at all.** A symbol's evidence is already inside its file's count, since touching the symbol touched the file. A strong symbol score adds something different: the words pick out one place in the file, not just the file. That is the sense of "symbol gets a boost over path".
- **Why the maximum, not the sum.** A sum would rank a file by how many symbols it has, and large files would win by size.
- **The remainder earns no boost.** The maximum runs over named symbols only. A remainder is not one place in the file, so a strong remainder score says only what the file's own score already says.

The answer is a ranking of files. Under each file sit its symbols with `score(S)` above the symbol threshold, best first. When the consumer asks for selectors, a file is emitted as `symbol:` selectors if one to five of its symbols pass the threshold and as one `file:` selector otherwise, the same rule the pilot follows when it writes selectors. A file whose remainder passes the symbol threshold is always emitted as `file:`: past tasks with these words changed it outside any named symbol, so no list of symbols covers the work.

At `level: symbol` the result is the ranking of named symbols by `score(S)`, without the roll-up.

### 5.3 Thresholds

`recommend` returns the top `limit` results. A consumer that needs a set, not a ranking, uses `min_score`: results below it are cut. `min_score` values are not chosen here; the offline evaluation publishes the precision and recall reached at each cut (§10.3), and each consumer's threshold is taken from that table.

### 5.4 Option: posting weight

With symbols as the unit, a task no longer has to count the same for every context it touched. Let `x(d,c)` be the weight of context `c` in task `d`, 0 when `d` did not touch `c`. The counts of §5.1 become sums of weights, and the formula is otherwise the same:

```
n(w,c)   window tasks containing w and touching c          (integer; min_support applies to it)
t(w,c)   = Σ over tasks d containing w of x(d,c)           (the posting's mass)
T(c)     = Σ over window tasks d of x(d,c)                 (the context's mass)
E(w,c)   = df(w) · T(c) / N
assoc(w,c) = max(0, ln( (t(w,c) + κ) / (E(w,c) + κ) ))
```

`posting_weight` has three settings. `changed(d,c)` is `added + deleted` for `c` in `d` (§2.4).

| Setting | `x(d,c)` | Reading |
|---|---|---|
| `presence` | 1 | The task touched the context. This is §5.1 unchanged |
| `symbol_fraction` (Daniel's) | `changed(d,c) / (len_before + len_after)` | The share of the symbol's own lines the task changed: "10% of `function_b`". A symbol the task added, deleted or rewrote entirely scores 1 |
| `diff_share` | `changed(d,c) / changed lines of the whole task` | The share of the task's diff that fell in this context. A task's leaves sum to 1 |

A file's weight is the same expression over its rolled-up record: for `symbol_fraction` the file's changed lines over the file's two lengths, for `diff_share` the sum of its leaves' shares. Excluded paths (§2.3) are outside every total.

**How `symbol_fraction` enters, and the concern raised against it.** A two-line fix in a 200-line function changes 1% of it (4 changed lines over 400), although that function is the whole task. A one-line tweak to a three-line helper changes 33% (2 over 6). Daniel's reply was that the formula's normalisation may offset this. It does so in part, and the part can be stated exactly:

- *What the normalisation offsets.* `t(w,c)` and `E(w,c)` are both sums of the same context's weights. Multiply every weight of one context by a constant and both scale together, so their ratio does not move. A function that is always changed by about 1% is not ranked below a helper that is always changed by about 33%: the context's usual fraction cancels.
- *What it does not offset.* `κ` is counted in tasks. Take the example of §5.1 (a word in 40 tasks, 18 of them touching a context that 25 tasks touched). Under `presence`, `assoc = 2.7`. If the context is the 200-line function at 1% each time, then `t = 0.18` and `E = 0.0025`, and `assoc = ln(1.18 / 1.0025) ≈ 0.16`. If it is the three-line helper at 33%, then `t = 6` and `E = 0.083`, and `assoc = ln(7 / 1.083) ≈ 1.87`. The evidence is the same 18 tasks; the smoothing treats the large function as if it had almost none.
- *What remains after both.* Within one context, a task that changed more of it counts for more. The words of the task that rewrote half of a function outweigh the words of the tasks that fixed two lines in it. Whether that is signal (a rewrite is more about the function) or noise (most tasks are small fixes) is for the evaluation.

So `symbol_fraction` and `diff_share` are each built with two settings of **`smoothing_unit`**:

| `smoothing_unit` | Rule | Effect |
|---|---|---|
| `tasks` | The formula above as written, `κ` constant | Daniel's proposal as stated. Contexts whose weights are usually small are shrunk toward zero |
| `relative` | Each context's weights are divided by its mean weight, `x̄(c) = T(c) / n(c)` with `n(c)` the tasks touching `c`, before the sums are taken | `T(c)` becomes the plain task count again, so `κ` means the same for every context and the cancellation above is complete. A task counts for more or less than one task according to whether it changed more or less of the context than is usual for that context |

Under `relative` the function and the helper of the example both score 2.7. One consequence is disclosed here and measured in §10.4: a single unusually large change can outweigh many small ones. If 24 tasks each changed 1% of a function and one changed 50%, that one task counts as 17 tasks and each of the others as a third of one. Under `presence` the two settings are identical.

The symbol's line count appears only as the denominator of `symbol_fraction`. It is not used as a document length in a BM25-style normalisation; that suggestion was dropped (Daniel, 2026-11-14) and nothing here depends on it.

**How `diff_share` enters.** The two-line fix that is the whole task scores 1, which answers the concern above directly, and a task spread over forty symbols gives each a fortieth, which discounts sweeps beyond what `max_label_files` does. Its weakness is the opposite case: a feature task that adds a 300-line file and edits three lines of a registration function gives the registration function 1% of its weight, and that function is the plug-in point §2.5 is after. `added_weight` (§5.5) changes this, because it removes added lines from the total. `smoothing_unit` applies to `diff_share` as it does to `symbol_fraction`.

**How `presence` enters.** As §5.1. It ignores how much changed, cannot be skewed by formatting churn or one large rewrite, and is the setting the other two have to beat.

### 5.5 Option: added versus modified

`added_weight` (α, from 0 to 1) multiplies the weight of every task-and-context record whose status is `added` (§2.5):

```
x′(d,c) = α · x(d,c)    when d added c
x′(d,c) = x(d,c)        when d modified or deleted c
```

| Setting | Meaning | Gains | Gives up |
|---|---|---|---|
| α = 1 | Added symbols count like any other. The design of [ORB-24844] | The task that created a symbol is usually the fullest description of what it is for, and later fixes to that symbol are found through those words | A feature query is answered with the insides of earlier features |
| α = 0 | Only modified and deleted contexts earn postings. A created symbol or file enters the index the first time a later task modifies it | Results for a feature query are plug-in points: registries, dispatch tables, config schemas, the files that gain a symbol each time | Until a second task touches it, a new symbol is invisible, including to the bug filed against it the next day |
| 0 < α < 1 | Added contexts count at a discount | A middle ground, swept at 0.25 and 0.5 | One more parameter |

Three rules make the option precise:

- **Status belongs to the context, not to its lines.** A new function added to an existing file is an `added` symbol inside a `modified` file. The symbol's posting is scaled by α; the file's is not, and the file's rolled-up record keeps the new function's lines. The file that gains a symbol is itself a plug-in point.
- **Under `diff_share`, the total is scaled too.** The share is taken over the task's lines after each context's α is applied, at each level (leaves, files). With α = 0 the 300-line new file of §5.4 leaves the total, and the registration function's three lines become the whole of what is left.
- **`tasks_added` is counted whatever α is.** At α = 0 a posting has `mass` only from modifying tasks, but the stored count still shows how many tasks created the context.

α is a build option with one value per generation. The evaluation reports the best α for each task type (§10.3). A per-type α is added only if the types disagree by more than their intervals; the likely case is α near 0 for `feature` and near 1 for `bug`. With `tag_mode: nested` (§5.6) and α = 0, the cell of the `feature` type is literally "the contexts past feature tasks modified", which is the answer §2.5 describes.

Added contexts never count as truth in the evaluation (§10.2), under any α: they did not exist when the task was written. α changes only what the index learns from them.

### 5.6 Option: task type and tags

Daniel's concern (2026-11-14): an index over words alone will be noisy for new features, so the task's tags should enter, with a "double index" of the form `"tag_1": {"word_1": [ … ]}`. A task's *keys* are its type and its tags (§2.6). `tag_mode` has three settings.

**`off`.** Keys are recorded but not used in scoring. This is §5.1.

**`terms`: keys as boosted terms in the same index.** Each key becomes one extra term of the task, in its own namespace so it cannot collide with a word: `type:feature`, `tag:dashboard`. Key terms are not split, and the stopword and `max_term_share` rules do not drop them (a type that half the window carries is kept; its associations are simply near zero where it picks nothing out). A query's key terms score like its words, multiplied by `tag_weight` (default 2; swept at 1, 2 and 3):

```
score(c) = Σ over query words w of assoc(w,c)  +  tag_weight · Σ over query keys g of assoc(g,c)
```

**`nested`: Daniel's per-key index, blended with the global one.** For each key `g`, the counts of §5.1 (or §5.4) are kept a second time over only the window tasks that carry `g`: `N_g`, `df_g(w)`, `T_g(c)`, `t_g(w,c)`. A cell is not read on its own. It adjusts the global answer, by as much as the cell has evidence:

```
E_g(w,c)     = df_g(w) · T_g(c) / N_g
ratio(w,c)   = (t(w,c) + κ) / (E(w,c) + κ)                        the global ratio of §5.1
ratio_g(w,c) = (t_g(w,c) + μ · ratio(w,c)) / (E_g(w,c) + μ)       μ = key prior, default 2
assoc_K(w,c) = max(0, mean over g in K of ln ratio_g(w,c))        K = the query's keys that have a cell
score(c)     = Σ over query words w of assoc_K(w,c)               K empty: assoc(w,c), exactly §5.1
```

- **Fallback.** A key has a cell only if at least `min_key_tasks` window tasks carry it (default 30). A query whose keys are all unseen or too rare has an empty `K` and gets the global answer unchanged. Within a cell, a word and context the cell has never seen together or apart have `t_g = E_g = 0`, and `ratio_g` is the global ratio. As a cell's counts grow, its own ratio takes over. μ sets how fast: it is the number of expected co-occurrences the global answer is worth.
- **What it can say that the global index cannot.** That a word means something different under a key. If `lease` points at the lock index in general but, in `dashboard` tasks, at a panel, the `dashboard` cell holds `t_g` well above `E_g` for the panel and `t_g = 0` against a real `E_g` for the lock index, and the blend moves both.

The three concerns with the nested form, and what the design does about each:

| Concern | Treatment | What is left |
|---|---|---|
| **Sparse cells.** A key carried by 2% of the window holds 2% of the evidence, spread over the same vocabulary | The blend above: a thin cell changes little, an empty one nothing; `min_key_tasks` removes the thinnest | For most keys the nested answer equals the global one. The evaluation reports the share of test tasks whose answer it changed at all |
| **Free-form tags applied inconsistently** | Normalisation and `exclude_tags` (§2.6); dispatch-time keys only on the query side; `min_key_tasks`. A training task that lacks a tag it should have had is missing from that cell but still in the global index. A query that lacks the tag gets the global answer | Synonyms (`dashboard`, `ui`) split one subject across two cells. No alias table is proposed: curating one is the narrow, per-workspace upkeep an index over history is meant to avoid. The dataset statistics list the keys and their sizes (§10.1) |
| **A multi-tag task counted once per tag** | Inside one cell a task counts once. Across cells the query takes the *mean* over its keys, not the sum, so a past task that carries two of the query's keys does not count twice | Storage does repeat it: a task's postings are written once per key it carries (§4.2) |

Trade-offs of the three settings:

| | `off` | `terms` | `nested` |
|---|---|---|---|
| What a key can do | Nothing | Add its own evidence: "files that `feature` tasks touch more than others" | Change what each word points at under that key |
| Answers Daniel's noise concern | No | Only partly. Word postings from every type of task still score in full; the key adds to them and cannot subtract | Yes, where the cell has evidence |
| Storage and build | Baseline | A handful of extra terms | The `key_*` tables, roughly one more copy of the postings per key a task carries |
| New parameters | — | `tag_weight` | `μ`, `min_key_tasks` |
| Main risk | Feature queries stay noisy | A key carried by many tasks has weak associations and changes little | Sparse cells; more settings to fit on eight weeks of data |

The type alone (four values, set on every task) and the tags can be switched separately in either mode (`keys: type`, `tags` or `both`), because the type has none of the consistency problems tags have and may carry most of the gain.

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

**Cadence.** Weekly is the requested default. At ws_orbit's rate a weekly build leaves the index up to 1,000 tasks behind, a quarter of a 4,000-task window, so the recommendation for ws_orbit is daily (§14, question 3). The evaluation reports both (§10.2).

## 7. Query surface

### 7.1 Contract

The tool is orbit-graph's read-only `recommend`, with the word index selectable as the `word_index` variant. Exact field names follow that tool's schema; the content is:

```
request   one of: task_id | text { title, description, acceptance_criteria[], type?, tags[]? }
          level: file | symbol        limit        min_score?
          revision?   (target commit; default the checkout HEAD)
          cutoff?     (historical replay: only tasks landed strictly before it; excludes the task itself)
          options?    (only with cutoff: posting_weight, smoothing_unit, added_weight, tag_mode and the
                       other build parameters of §10.4)

response  index   { generation, built_at, target_commit, window_tasks, newest_landing, lag_tasks?,
                    tokenizer_version, options }
          results [ { selector, kind, score,
                      evidence { terms: [ {term, assoc, tasks, tasks_added} ], supporting_tasks: n },
                      symbols: [ { selector, score } ] } ]
          keys            { used: [ … ], no_cell: [ … ] }        with tag_mode terms or nested
          dropped_stale   count of results removed by the check against HEAD
          empty_reason?   no_generation | generation_stale | no_known_terms | below_min_score
```

Every result names the terms that produced it and how many tasks back each one, so a person or a pilot can see why a file was suggested. Task ids are not returned to agents by default; the count is. `keys` says which of the query's type and tags took part and which had no cell, so a reader can tell a keyed answer from a fallback to the global index.

With the `task_id` form the tool reads the task's type and tags itself. A live query is made before or at dispatch, so the keys it reads are dispatch-time keys; the evaluation has to reconstruct them (§10.2).

With `cutoff`, the tool does not read the published generation. It builds a throwaway generation from the tasks landed before the cutoff, with the requested `options`. That is what the evaluations use, and it is why rebuilds must be cheap. A published generation has one setting of each option (§4.1), and a live query cannot change it.

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
| 2 | Leave plan and execution summary out | **Adopt, and make it a rule on the query side** | A new task has neither. On the index side their words would raise `df` and `N`-relative counts for terms no query contains. The execution summary also names the changed files outright, so letting it near a query would make every evaluation look excellent and mean nothing. One swept variant indexes summary words on the *index side only* to see whether they help (§10.4); query text is never anything but title, description and criteria. Comments are out for the same reasons. Type and tags are not text; they enter only through the option of §5.6, and only as they stood at dispatch (§2.6). |
| 3 | Rebuild the window each week, don't add and subtract | **Adopt, with a cached layer beneath** | §6. Per-task labels are extracted once and kept; the counts are rebuilt from them. This keeps the cost Daniel's incremental scheme was after (no repeated Git or parsing work) without counts that drift when rules change. |
| 4 | Don't rank by raw counts; IDF-like weights, divide by context frequency, stopwords, identifier-aware tokens | **Adopt, as one formula** | §5.1's association does the rarity weighting and the hot-file discount together and shrinks single-task evidence. Stopwords are a fixed list plus a per-window share rule (§3). Identifier-aware splitting keeps parts and whole (§3). A separate IDF factor is not added because it would double-count. |
| 5 | Id table with `kind` on the row; symbol rolls up to file; rename as one-row update; reassignable ids | **Adopt the table and roll-up; the rename and append-only clauses become moot** | §4.1, with the symbol as the unit and the file as its roll-up (§2.4). Because generations are rebuilt (recommendation 3), ids are always reassigned and never need to be append-only. A rename is not a row update either: build-time lineage merges the old path's history into the new path (§2.3), and the query-time check covers renames since the build (§7.2). |
| 6 | Check against HEAD at query time; exclude incidental files | **Adopt** | §7.2 for the check. Exclusion happens twice: at build time so lockfiles and generated outputs never dilute postings (§2.3), and again at query time so a config change needs no rebuild. |
| 7 | Co-change expansion along `orbit.codebase` action `related` | **Do not adopt in the first version; evaluate as a variant, from the plugin's own history** | The index already carries co-change: files that land together share the same task's words and rise together. Expansion can only add files the words did not reach, which is where precision is lost. `related` is also the wrong source for this: it reads live facts over a trailing window from now, needs `min_support` 15 by default and cannot honour a historical cutoff, so it would leak the future into the evaluation. The variant in §10.4 computes co-change from the same labels under the same cutoff and is adopted only if recall@10 rises with precision@5 no lower. |
| 8 | Cold start returns empty; the agent explores as today | **Adopt** | §7.3. An empty answer carries a reason and is never padded. |

## 10. Offline evaluation

The evaluation runs before any consumer is wired, and it is built inside orbit-graph (decided, §14), not as analysis scripts beside it. The dataset is the plugin's history index. The word index is the `word_index` variant of `recommend`, queried with `cutoff`. The protocol, the baselines, the sweep and the gates are `evaluate`, whose reports are attached to the build tasks as artifacts. What is built for the evaluation is therefore what ships if the gates pass.

It needs no Orbit core change. Until the landed-delivery feed exists (§1.2), the history index is filled once through `orbit.task.list` and one `orbit.task.show` call per task, which is slow and acceptable for a one-time fill.

### 10.1 Dataset

Every landed ws_orbit task with a label (§2.1). *Measured*: the store holds 12,674 `done` tasks. `landed_diff_stats` is recent (the eval-harness design dates its first rows to 2026-10-26) and is backfilled for older tasks whose commits are in the local checkout. For each task: landing time, landed and base commit, first-dispatch time, the three text fields, the changed-after-dispatch flag, the type and tags with their dispatch-time flag (§2.6), and the label: every leaf and file with its line counts, lengths and status (§2.4, §2.5).

The dataset's own statistics are part of the result, each also broken down by task type:

- tasks dropped by each rule in §2.1, and the share of tasks with eligible text;
- the share of changed lines that fall in a named symbol, in a remainder, and in files with no symbol support;
- the share of changed symbols and files that are added, modified and deleted (the 92-of-98 figure of §2.5, measured over the whole dataset instead of one case);
- the distribution of `symbol_fraction` and `diff_share` weights, since §5.4's concern is about their spread;
- the keys: how many tasks carry each type and each tag, how many tags pass `min_key_tasks`, and the share of test tasks with at least one key that has a cell.

### 10.2 Split by time

A random split is not allowed: it would train on tasks that landed after the ones being predicted, and neighbouring tasks share files. The split is a rolling origin that mirrors the routine:

- Choose weekly cutoffs `T1 … Tk` (the last eight complete weeks).
- For cutoff `Ti`, **train** on the n eligible tasks that landed strictly before `Ti`. This is the generation the weekly routine would have published at `Ti`.
- **Test** on tasks first dispatched in `[Ti, Ti + 7 days)`, whenever they landed. Each is predicted with the index it would actually have seen, including its staleness late in the week. A daily-cadence run repeats the protocol with daily cutoffs.
- A test task's **query text** must be the text as dispatched. In order: the audit task-scope blob of its first implement invocation (`task_scope_blob_ref`, [audit_envelope.rs](../../../crates/orbit-types/src/workflow/activity_job/audit_envelope.rs)); else the current text when no update after first dispatch touched description or criteria; else the task is excluded and counted. This is the same order the eval-harness design uses for its snapshots. Audit blobs are pruned after `retention.audit_days` (default 60), so the dataset copies the text when it is built. A test task's **type and tags** follow the same order: those in the dispatch-time snapshot; else the current ones when no update after first dispatch touched them; else the task is queried with no keys and counted.
- Training text is the current record (§2.2). A training task whose wording was edited during execution may carry words a filer would not have written; the result is reported with and without such tasks.
- **Truth** for a test task is its label restricted to contexts that exist at its base commit, minus excluded paths. Files and symbols the task *created* cannot be predicted by any index over history; they are left out of the denominator and their share is reported as the ceiling it is.

### 10.3 Metrics

Per test task, then averaged over tasks (and also pooled):

- file **recall@k** and **precision@k** for k in 1, 3, 5, 10, 20, and mean reciprocal rank;
- the same for symbols, reported separately, never mixed with files;
- **coverage**: the share of test tasks with a non-empty answer, since §7.3 lets the index decline;
- **set quality at a threshold**: precision, recall and F1 of the result set cut at each `min_score`, computed with the pilot scorecard's definitions so the numbers are comparable with the pilot's;
- the stale-result rate before the check against HEAD.

**Results per task type.** Every number above is reported for all test tasks and separately for `feature`, `bug`, `refactor` and `chore` tasks, by the test task's dispatch-time type. This is where the options are expected to differ: a setting that helps feature tasks and costs bug tasks shows as no change in the total. Each type's row carries its task count and its own interval, and a type with fewer than 30 test tasks on the held-out cutoffs is reported as "too few to read" and not compared. Each type's row also states its ceiling: the share of its truth that the task created and no index could predict (§10.2).

Every number carries a 95% interval from a percentile bootstrap over tasks, as the experiment readout computes its intervals, and every comparison between two rankings is a paired difference over the same tasks. A difference whose interval includes zero is reported as "no difference shown".

### 10.4 Sweep

- **Window size n:** 500, 1,000, 2,000, 4,000, 8,000, and all history.
- **Symbol boost b:** 1, 1.25, 1.5, 2, 3.
- Secondary: `κ` (0.5, 1, 2, 5), `min_support` (1, 2, 3), `max_label_files` (10, 20, 40), `title_weight` (1, 2), `max_term_share` (20%, 30%, 50%), stemming (plural folding or Porter), index-side summary words (off or on, recommendation 2), co-change expansion (off or on, recommendation 7).
- **Options** (§5.4 to §5.6):

| Option | Settings compared | Count |
|---|---|---|
| Posting weight | `presence`; `symbol_fraction` and `diff_share`, each with `smoothing_unit` `tasks` and `relative` | 5 |
| Added versus modified | `added_weight` 1, 0.5, 0.25, 0 | 4 |
| Type and tags | `off`; `terms` at `tag_weight` 2; `nested` at μ 2 | 3 |

The **baseline configuration** is `presence`, `added_weight` 1 and `tag_mode: off`, which is the index as [ORB-24844] designed it. The order of the sweep:

1. The primary grid, n × b, at the baseline configuration and default secondaries.
2. The option grid at the best (n, b) cell. The three options are crossed in full, 60 configurations, because they act on each other: `added_weight` changes the totals `diff_share` divides by (§5.5), and a `nested` type cell means something different at α = 0. Each configuration is one more build per cutoff over the same history layer.
3. Around the best configuration: `tag_weight` (1, 2, 3), μ (1, 2, 5, 10), `min_key_tasks` (15, 30, 60), keys from the type only, the tags only or both, and each secondary varied alone. Then n × b once more, since symbol weights may move the best boost.

To keep the sweep from fitting the test weeks, parameters and options are chosen on the first five cutoffs and the headline numbers are reported on the last three. Sixty configurations chosen on five weeks will show a winner by chance alone, so an option's setting is not adopted on its selection-fold result; gate G4 (§10.5) reads it on the held-out cutoffs.

### 10.5 Baselines and gates

| Baseline | What it is | Why it is there |
|---|---|---|
| **BM25 over code** | The task text as a query against the files of the tree at the fold's cutoff commit (path and content), ranked by SQLite FTS5 `bm25`; a second run over symbol documents (name, signature, body) | The required baseline: it knows the code and no history, and has no cold start |
| **Frequency** | The most-touched contexts in the window, the same for every query (the plugin's existing `frequency` variant) | The floor. A ranking that cannot beat it has learned only which files are hot |
| **Explicit mentions** | Paths and symbol names written in the task text that resolve at the base commit | Filers often name the site; this shows how much of any score is just reading the task |
| **orbit-graph `recommend`** | Its existing combined and task-search-only variants | The ranking the plugin has today over the same evidence |
| **The pilot and the filer** | `context_files_after` and `context_files_before` for the same tasks, graded the same way | The bar a pre-fill has to reach, recomputed on these folds |

BM25 indexes the tree at each fold's cutoff commit, not at each test task's own base; that approximation is disclosed with the result. Two combinations are also scored, because they are the likely best system: word index plus explicit mentions, and word index fused with BM25 by reciprocal rank.

Gates, each read on the held-out cutoffs:

- **G1, keep the index.** Word-index file recall@10 beats both Frequency and BM25 over code, with the paired interval above zero, at coverage of at least half the test tasks. If it fails, nothing is wired; the result says whether BM25 or a combination deserves the slot instead.
- **G2, which ranking the plugin keeps.** The word index and the existing `recommend` variants are compared directly. The loser is removed, not kept beside the winner, unless the combination beats both.
- **G3, pre-fill.** At some `min_score`, the thresholded set reaches the pilot's precision on the same tasks with recall of at least half the pilot's. Otherwise §8.2 is not built.
- **G4, which option settings the variant keeps.** Each option is read separately, as the paired difference between the chosen configuration and the same configuration with that one option set back to its baseline (`presence`, `added_weight` 1, `off`). A non-baseline setting is kept only if file recall@10 or symbol recall@10 is higher with the interval above zero, and the other of the two is not lower beyond its interval. Otherwise the baseline setting stays and the code for the losing settings is removed, as under G2. The verdict is given for all tasks and per task type; a setting that passes for one type only is kept only as a per-type setting (§5.5), and only if that type has enough test tasks to be read (§10.3).

G1 to G3 are read on the configuration G4 leaves standing.

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

- **It may not work.** No measurement yet shows that task wording predicts files in this repository. The plugin's recorded recommendation cohort is small and claims nothing. That is why tasks 2 to 5 of §13 build only what the evaluation needs.
- **Symbol evidence is thin.** In one recorded orbit-graph evaluation case, 92 of 98 changed symbols were added by the delivery and did not exist beforehand. Symbols that exist and are edited repeatedly are the minority, so symbol granularity, the posting weights and the symbol boost may all show no effect. `presence` with `b = 1` is an acceptable outcome, and §10.1 measures the added share over the whole dataset before anything is concluded from one case.
- **Line counts are a rough measure of change.** A reformatting, a moved block or a renamed variable changes many lines and little meaning, and both line-based weights read it as a large change. A symbol the commit renamed or moved to another file is recorded as one deleted symbol and one added one (§2.4), so at `added_weight` 0 its new name starts with no history. Neither is corrected; `presence` is immune to the first and is the comparison.
- **The options multiply what is fitted.** Three options, their parameters and four task types are chosen on eight weeks of one workspace. G4 keeps a setting only on held-out evidence and returns to the baseline otherwise, but a per-type result rests on a quarter of the tasks or fewer and will often be unreadable.
- **Tags are what filers made them.** Filers are mostly agents, tags are free-form, and nothing enforces that two tasks about one subject share one. The nested index can be no more consistent than its keys, and the treatment in §5.6 limits the damage without fixing the cause. Reconstructing dispatch-time tags depends on the same audit records as dispatch-time text.
- **New files are invisible.** A task that mostly creates files gets little from any history index. The evaluation reports that share instead of hiding it.
- **Vocabulary drift and self-reinforcement.** Filers are mostly agents, and their wording follows prompts and templates that change. Once predictions feed the pilot, the pilot's selectors steer what implementers touch, and the index learns from what they touched. Labels come from merged diffs, not from selectors, which limits the loop but does not remove it. The pilot scorecard's recall against landed files is the watch on it.
- **Training text is not always what the filer wrote.** Only test tasks are held to dispatch-time text. Edited training tasks are measured both ways (§10.2), but the published generation uses current text.
- **Task ids in commit subjects are the fallback for old history.** Tasks without a `landed_diff_stats` row or a landing receipt can be matched only by the id in the squash subject, which the orbit-graph design treats as weaker evidence. The dataset reports how many tasks rest on it.
- **Bundled and oversized commits are dropped**, so areas changed mostly by sweeps are under-represented.
- **Staleness.** A weekly build at ws_orbit's rate is a quarter-window behind by the end of the week. The evaluation measures the cost; the remedy is cadence, not incremental counts.
- **Replay has little power and a narrow task mix.** About 25 bug-fix tasks detect only large differences in pass rate, and bug reports name their site more often than feature requests do. The exploration counts are continuous and paired, which helps, but a null result there is weak evidence.
- **The plugin is not installed anywhere this was checked**, and its manifest still names an Orbit tool that has since been folded into `orbit.task.show`. Bringing the plugin up to date is the first build task (§13, task 1).
- **Doc layout.** This folder has an overview and a design, as the task asked, and puts the build order, Daniel's decisions and the open questions after this section (§14.1 holds the decisions until the folder gains the convention's `4_decisions.md`); `3_vision.md` is added when the feature has something for it.

## 13. Build order

This document files nothing. The orbit-graph tasks are filed in `ws_orbit-graph`, the Orbit core tasks in `ws_orbit`, each with the `context-index` tag.

Tasks 1 to 5 are the dataset and the offline evaluation, built inside orbit-graph's history sync, `recommend` and `evaluate` (decided, §14). They replace the two analysis tasks of the first version of this design, need no Orbit core change, and are what Daniel named for filing: the retired-tool fix, the `word_index` variant, the baselines and gates, and the options. Everything after them waits on gate G1.

| # | Task | Where | Depends on | Outcome |
|---|---|---|---|---|
| 1 | Retired-tool fix | orbit-graph | — | The manifest and the history sync read deliveries through `orbit.task.show`; the plugin installs and syncs against the current Orbit tool surface |
| 2 | Dataset in the history index | orbit-graph history sync, `evaluate` | 1 | One record per landed ws_orbit task: text, dispatch-time text where recoverable, type and tags, landed and base commit, and the label with per-leaf line counts and status (§2.4 to §2.6); `evaluate` reports the dataset statistics of §10.1 |
| 3 | `word_index` variant | orbit-graph `recommend` | 2 | The tokeniser, the generation built from a `cutoff`, the scoring of §5.1 to §5.3 at the baseline configuration, evidence, the check against HEAD and empty reasons (§7) |
| 4 | Baselines, protocol and gates | orbit-graph `evaluate` | 3 | The rolling-origin protocol, the five baselines and two combinations, the n × b and secondary sweeps, results per task type, and the verdicts of G1 to G3; measured generation size and build time |
| 5 | Options | orbit-graph `recommend`, `evaluate` | 3, 4 | `posting_weight` with `smoothing_unit`, `added_weight` and `tag_mode` as options of `word_index`; the option grid and the G4 verdict, overall and per task type; G1 to G3 re-read on the configuration G4 keeps |
| — | **Decision point for Daniel** | — | 4, 5 | Stop, continue with the word index in the configuration G4 kept, or continue with what beat it |
| 6 | Landed-delivery feed | Orbit core | G1 | A read-only cursor-paginated feed of landed tasks (§1.2, seam 1) |
| 7 | Sync labels from the feed | orbit-graph | 6 | The history index fills from the feed with no per-task calls |
| 8 | Published generation | orbit-graph | 3, G1 | `maintain` builds and atomically publishes a generation in the kept configuration; `recommend` reads it when no `cutoff` is given; `status` reports generation, window, options and lag |
| 9 | Routine, job and activity; install and enable on ws_orbit | orbit-graph, operator | 7, 8 | The seeded routine exists; the operator installs, grants, pins and enables it |
| 10 | Selector-candidates hook, shadow mode | Orbit core | 8 | The pilot records candidates with each assessment without seeing them (§1.2, seam 2) |
| 11 | Shadow readout; show candidates to the pilot | orbit-graph `evaluate`, then Orbit core | 10, two weeks of shadow data | Index against pilot against truth on live tasks; if it holds up, the pilot receives the candidates |
| 12 | Selector conditions in replay; oracle-first run | Eval-harness plugin | The eval-harness build through its replay readout; 8 | The §11 result, oracle step first |
| 13 | Pre-fill for tasks with no footprint | Orbit core | G3, 11 | High-confidence selectors written at filing, replaceable by the pilot |
| 14 | Implementer hint | Orbit core | 12 passing | Only if replay shows seeded selectors reduce exploration |

Task 5 is separate from task 3 so that the baseline configuration is measured first and every option is a measured difference from it. If G1 fails at the baseline configuration and no option configuration passes it either, tasks 6 to 14 are not filed.

## 14. Decisions and open questions

### 14.1 Decided by Daniel (2026-11-14)

- **Home.** The index is built in the orbit-graph plugin, not a new plugin. This was open question 1 of the first version (§1.1).
- **Where the build is filed.** Build tasks go to `ws_orbit-graph` under the `context-index` tag: the retired-tool fix, the `word_index` variant, the baselines and gates, and the options (§13, tasks 1 to 5).
- **No throwaway analysis.** The dataset and the offline evaluation are built inside orbit-graph's `recommend` and `evaluate` (§10, §13).
- **Symbol granularity.** Labels come from the actual diff at full symbol granularity, with the share of each symbol changed available as a scoring weight (§2.4, §5.4). Using the symbol's line count as a BM25 document length was considered and dropped.
- **The options are settled by measurement.** Posting weight, added versus modified, and type and tags are each built as an option of `word_index`, and the offline evaluation chooses (§10.4, G4).

### 14.2 Open

1. **Is the word index a replacement for `recommend`'s current ranking or a variant beside it?** Both rank from the same evidence. *Recommendation: a variant until the evaluation; then keep one (gate G2), not both.*
2. **Is counting the window in tasks still what you want, given about 1,000 landed tasks a week on ws_orbit?** n = 4,000 is a month there and years elsewhere. *Recommendation: yes, tasks; take the default from the sweep and start at 4,000.*
3. **Weekly or daily?** Weekly is what you asked for; on ws_orbit it leaves a quarter of the window stale by the end of the week, and a build is cheap. *Recommendation: ship the routine with the weekly cron as the default and set ws_orbit's copy to daily if the evaluation shows the weekly staleness costs recall.*
4. **Plan and execution summary.** You were unsure. *Recommendation: out of both sides; one sweep variant tests summary words on the index side only, and query text never includes them.*
5. **First consumer.** Pre-fill at filing is the most direct use but writes locks from a guess. *Recommendation: pilot candidates in shadow mode first; pre-fill only past gate G3; no implementer hint until replay supports it.*
6. **May core gain the landed-delivery feed?** It is generic and three consumers want it (this index, orbit-graph's existing sync, the eval-harness set builder), but it is a new public read surface. *Recommendation: yes, after G1.*
7. **Replay budget.** The full downstream comparison is about 300 candidate runs. *Recommendation: approve the 50-run oracle step first and decide on the rest from its result.*
8. **One index per workspace, or shared across repositories?** Vocabulary and paths are per repository. *Recommendation: one per workspace; no cross-workspace index.*
9. **Should a `feature` tag vocabulary be asked of filers?** The nested index of §5.6 is only as good as the tags, and your example (`new_feature`, `feature_x`) assumes a tag that names the feature. The type field already covers "new feature"; nothing today makes filers name the feature the same way twice. *Recommendation: change nothing in filing until G4 shows whether tags help as they are; if the type alone carries the gain, no vocabulary is needed.*

## Task References

- [ORB-24844] — wrote this design
- [ORB-24851] — recorded the orbit-graph decision and specified the posting-weight, added-versus-modified and type-and-tag options
- [ORB-24839] — designed the eval-harness plugin and its replay job, which §11 uses and extends with selector conditions

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
