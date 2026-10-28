---
title: Symbol Risk Index — Design
owner: claude
last_updated: 2026-10-28
last_validated: 2026-10-28
status: Draft
feature: symbol-risk-index
doc_role: design
type: design
summary: "Per-symbol record (body lines, LCOV coverage, regression-site history), the indexer plugin contract, core storage, the rebuild routine, staleness and rename lineage, readers and the backtest ship gates"
tags: [symbol-risk-index, riskiness, regression-risk, test-strategy, routines]
paths: ["crates/orbit-core/src/application/codebase/**", "crates/orbit-store/src/**", "crates/orbit-core/assets/**"]
related_features: [backlog-forecast, routines, plugins]
related_artifacts: [ORB-19358, ORB-19361, ORB-19276]
---

# Symbol Risk Index — Design

This doc specifies the mechanisms that [1_scope.md](./1_scope.md) proposes: the per-symbol
record and the source of each field, the boundary between Orbit core and the language-aware
indexer, the core storage shape, the rebuild routine, staleness and rename handling, how site
history is computed, how the two readers use the index, and the backtest protocol behind each
ship gate. Nothing here is built. The task asked for a scope and a design document, so this
folder keeps a scope entry with a separate design file instead of choosing one track
([CONVENTIONS.md §1](../CONVENTIONS.md#1-folder-layout-per-feature-recommended)). The scope
holds the problem, evidence, outputs and gates; this file holds the contracts.

## 1. The per-symbol record

The index is a sequence of **generations**, one per indexed base commit. A generation has a
header and one record per symbol.

**Generation header**

| Field | Meaning |
|---|---|
| `generation_id` | Store-assigned identifier |
| `build_seq` | Store-assigned, monotonically increasing fencing sequence allocated at resolve (§4); unique per build attempt in the workspace |
| `basis_generation_id` | The generation that was `current` when this build resolved, or null for the first build |
| `workspace` | Owning workspace |
| `base_ref`, `base_commit` | The integration branch (the workspace's base, `agent-main` in ws_orbit) and the commit indexed |
| `indexer` | `{tool, plugin_version, extractor_version}` as reported by the indexer |
| `coverage` | `{source, commit, report_digest, has_branches}`, or `null` with a `reason` (`not_configured`, `coverage_unavailable`) |
| `built_at` | Ingest time |
| `counts` | `{symbols, files, unsupported_files, lineage_links, change_events}` |
| `status` | `current` or `superseded` (a failed build writes nothing) |

**Symbol record**

| Field | Source | Meaning |
|---|---|---|
| `selector` | Indexer | Canonical `symbol:<path>#<name>:<kind>`. `kind` is opaque to core: whatever token the indexer uses. |
| `path` | Derived from `selector` | Kept as a column for joins with file-level data |
| `start_line`, `end_line` | Indexer | 1-based inclusive extent at `base_commit` |
| `body_lines` | Core: `end_line - start_line + 1` | The **size** field |
| `parent` | Indexer | Selector of the enclosing symbol (method in an impl, function in a module), or null |
| `is_test` | Indexer | The symbol is a test function or lives in test-only code |
| `body_digest` | Indexer | Digest of the body under the indexer's own normalization. Core only compares digests for equality. |
| `coverage` | Indexer, from the LCOV report | `{state, lines_found, lines_hit, branches_found, branches_hit}` (below) |
| `test_refs` | Indexer | Distinct test symbols that reference this symbol directly; null when the indexer has no reference graph |

The generation also stores every tracked text file the indexer read, as `{path, lines}`. That
lets a `file:` or `dir:` selector resolve to lines even when its language has no extractor.

**Coverage state.** `measured` means the coverage commit is the base commit. `carried` means
the coverage commit differs, but the indexer found this symbol (directly or through lineage) at
the coverage commit with an equal `body_digest`. `stale` means the body changed after the
coverage commit, so its counts are withheld. `not_instrumented` means the report has no
instrumented line inside the extent, such as a constant or a trait declaration. `absent` means
there is no report. Only `measured` and `carried` records have coverage values. Readers treat
every other state as unknown, never as zero coverage.

**Site-history fields** are not stored on the record. They depend on `regression_from` links
that arrive at any time, so they are derived at read time from stored change events and lineage
(§6), then cached with the riskiness model.

## 2. The indexer boundary

Core never parses a language. Everything that needs language knowledge comes from one
configured indexer:

- symbol extents and nesting;
- test detection;
- body normalization for digests;
- mapping LCOV lines onto symbols (nested closures, mangled function records, generated
  lines);
- rename pairing;
- which symbols a commit changed.

LCOV is language-neutral line by line. Turning it into per-symbol counts needs the extents at
the coverage commit and the language's nesting rules, so that mapping belongs to the indexer
too.

**Why a plugin tool.** The indexer runs as a plugin tool, named by
`workflow.symbol_index.indexer_tool`. A plugin backend runs under the plugin sandbox with
manifest-declared filesystem and network permissions, after operator consent
([plugin standard](../plugins/1_scope.md#43-sandboxing)). A bare configured command has no such
path today: the shell activity was removed fail-closed ([ORB-00374]), and
[script steps](../script-steps/1_scope.md) ([ORB-19294]) is still a design. Once script steps
ship, a granted script may implement the same contract, and nothing in core changes.

**Invocation.** The routine's job calls the tool through the existing deterministic
`plugin.tool_call` action, the one orbit-graph's own sync jobs use. Input:

```json
{
  "schemaVersion": 1,
  "operation": "symbol_index",
  "repo_root": "<owner checkout>",
  "base_commit": "<sha>",
  "previous_base_commit": "<sha> | null",
  "coverage": {"commit": "<sha>", "lcov_path": "<run scratch>/lcov.info"},
  "changes": {"commits": ["<landed first-parent commits since previous_base_commit>"]},
  "output_path": "<run scratch>/symbol-index.json"
}
```

`coverage` is null when no report is available. The tool writes the document to
`output_path` and returns `{path, sha256, counts}`, so a 30,000-symbol document never passes
through a tool response. The plugin's manifest must grant write access to the run scratch
directory. The output document:

```json
{
  "schemaVersion": 1,
  "base_commit": "<sha>",
  "extractor_version": "<string>",
  "files": [{"path": "crates/x/src/lib.rs", "lines": 412, "supported": true}],
  "symbols": [{
    "selector": "symbol:crates/x/src/lib.rs#parse:function",
    "start_line": 40, "end_line": 88,
    "parent": null, "is_test": false,
    "body_digest": "<hex>",
    "coverage": {"state": "carried", "lines_found": 31, "lines_hit": 27,
                 "branches_found": 0, "branches_hit": 0},
    "test_refs": 2
  }],
  "lineage": [{"from": "<selector at previous base>", "to": "<selector at base>"}],
  "changes": [{"commit": "<sha>", "selectors": ["<selectors changed, added or removed>"]}]
}
```

**Indexer obligations**

- Read the named commits' trees, never the working tree, so an owner checkout with local
  edits indexes what landed.
- Produce the same output for the same inputs.
- Emit selectors in canonical form, with repo-relative paths that exist at `base_commit`.

**Core validation at ingest**

- The schema version and `base_commit` match the request, and the file digest matches the
  response.
- Every selector parses with the shared selector parser (`orbit-common`'s `fs/selector.rs`)
  and is unique.
- Each record's path equals its selector's path, `start_line <= end_line`, and a `parent`
  names a record in the same document.
- Counts and document size are within fixed bounds.

A violation fails the run. No generation is written, and the previous generation stays
current. The document is untrusted data: core stores numbers and opaque strings from it and
never executes or renders it as markup.

**First implementation.** orbit-graph is the expected indexer, but that work lives in its own
repository, and Orbit core gains no dependency on it. Its tree-sitter extractors already cover
ten languages, `show` returns a symbol's source extent, `refs` gives the reference relations for
`test_refs`, and `changes` maps a revision range to changed symbols with a rename `pairing`.
It needs a `symbol_index` operation, LCOV mapping and an output-path grant.

## 3. Core storage

Four owner-store tables, added by one store migration:

| Table | Key | Rows |
|---|---|---|
| `symbol_index_generations` | `(workspace, generation_id)` | The §1 header |
| `symbol_index_records` | `(generation_id, selector)` | The §1 record, plus `(generation_id, path)` file rows |
| `symbol_lineage` | `(workspace, from_selector, to_selector)` | Rename links with the generation that recorded them and the basis (`indexer`, `digest`) |
| `symbol_change_events` | `(workspace, landed_commit, selector)` | Which selectors a landed commit changed, joined to its task through the landed-delivery rows of `codebase/diff_stats.rs` |
| `symbol_index_build_state` | `(workspace)` | One row per workspace: the next `build_seq` to allocate and the advisory single-flight lease (`lease_run_id`, `lease_expires_at`); see §4 |

The records and generations are a **derived cache**. Dropping them costs one rebuild. Change
events are derived from Git too: when the table is empty, the first run backfills the site
window's landed commits, bounded like the diff-stats backfill ([ORB-18383]). Site rates,
gate results and the test-priority ranking are not stored.

**Size.** At `bc39464a1`, ws_orbit has about 31,000 Rust function definitions (tests included)
in about 1.04 million lines of tracked Rust, by a pattern count. At roughly 200 bytes per row
with its key index, one generation is about 6 MB, and two retained generations about 12 MB.
In the backtest period ws_orbit landed about 4,550 tasks in 11 weeks, about 60 a day. At around
ten changed symbols per delivery over a 180-day window, that is roughly 100,000 change events,
about 10 MB. These are estimates; phase 1 records the measured figures.

**Retention.** Keep `retain_generations` generations (default 2). Deleting a generation deletes
its records. Lineage and change events are kept for `site_window_days` and then pruned.

**Locality.** The index lives in the owner's store, where its readers run: riskiness in the
dashboard and the backlog forecast. Followers and claimed workers neither build nor read it,
so it is not replicated. A new owner rebuilds it from its own checkout.

## 4. The routine

A seeded, disabled routine in the [routines](../routines/2_design.md) grammar:

```yaml
schemaVersion: 1
name: symbol-index
description: Rebuild the symbol risk index when the integration head or the coverage report changes.
enabled: false
trigger:
  cron: "47 */6 * * *"     # every six hours, 30 minutes after the Coverage workflow's :17 slot
  missed_run: catch_up_once
target: job:symbol_index_pipeline
policy:
  timeout_minutes: 60
  retries: {max: 1, backoff_minutes: 10}
  overlap: forbid
```

The trigger is a cadence. Each fire then rebuilds only if the base has advanced or the coverage
report has changed. A delivery trigger is not used: `deliveries_landed` jobs must return
review-coverage evidence for each landed delivery, a contract this job has no business
claiming. The routine is evaluated on each owner checkout with an enabled clock, which in
ws_orbit is dk-server-2. `orbit run job symbol_index_pipeline` rebuilds on demand.

**What the scheduler does and does not guarantee.** `policy.timeout_minutes: 60` is not a
process deadline. In the current scheduler it is the staleness horizon after which a fire
stops blocking `overlap: forbid` and is recorded as timed out (`RoutinePolicy` in
`crates/orbit-types/src/workflow/routine.rs`); the sweep reclassifies the fire record past
that horizon even when the run's owner is still alive or cannot be probed, and the dispatch
seam (`submit`, `run_state`, `run_owner_liveness` in
`crates/orbit-automation/src/routines/sweep.rs`) has no cancellation operation, so nothing
kills or proves stopped a build that passes the horizon. Only a run that itself reaches the
`Cancelled` state is known stopped, because cancellation signals the owner and verifies
termination. A timed-out fire is also no longer a blocking fire (only `intent` and
`dispatched` rows block, in `routine_blocking_fire`), so the next scheduled fire may start
while the earlier build is still running; the existing sweep fixture for an unprobeable
owner being reclaimed at the policy timeout specifies exactly this. A retry re-dispatch
(`retries: {max: 1}`) is a further scheduled start under the same slot. A direct
`orbit run job symbol_index_pipeline` acquires no routine fire slot at all. Correctness
therefore never rests on builds running one at a time: any number of builds may be in
flight, and publication is fenced instead (below). No scheduler change is needed.

**Build fencing.** Every entry point — a scheduled fire, a retry re-dispatch, a fire after
timeout reclamation, and a direct `orbit run job` invocation — runs the same three steps,
and the fence is allocated in the owner store, so it covers all of them:

1. **`symbol_index_resolve`** (core). In one store transaction it allocates the build's
   fencing token: it reads `symbol_index_build_state`, assigns `build_seq` (the stored
   counter, then increments it; sequences are never reused, including by builds that later
   fail, no-op or are rejected), and records the `generation_id` currently marked `current`
   as `basis_generation_id` (null when none exists). It then reads the integration branch's
   head from the owner checkout. It finds the newest successful run of the configured
   coverage workflow on the configured branch through the GitHub integration, the way
   `collect_dependabot_alerts` reads alerts. It downloads that run's artifact into run
   scratch and records the run's head commit as the coverage commit. If the head equals the
   current generation's base and the report's digest is unchanged, the run ends as a no-op
   (its `build_seq` is simply never published; it does not block any other build). A failed
   coverage fetch continues with `coverage: null, reason: coverage_unavailable`, so the size
   data still refreshes. The allocated `build_seq`, `basis_generation_id` and resolved
   `base_commit` are carried in the job run's step outputs to ingest; a build that cannot
   show an allocated `build_seq` is refused at ingest.
2. **`index`**: `plugin.tool_call` to the configured indexer tool with the §2 input, whose
   `base_commit` is the commit resolve read and whose `previous_base_commit` is the basis
   generation's base (null when there is no basis).
3. **`symbol_index_ingest`** (core). It validates the document (§2), including that the
   document's `base_commit` still equals the resolved `base_commit`. It then publishes by
   compare-and-publish in a single transaction:
   - Read the generation currently marked `current` (if any) inside the transaction.
   - If a generation with this `build_seq` is already `current`, the ingest is an
     already-published no-op and succeeds without writing (an ingest retried after its
     transaction committed does not publish twice).
   - Otherwise publish only if both hold: (a) `build_seq` is strictly greater than the
     current generation's `build_seq` (vacuously true when there is no current generation),
     and (b) the build's `base_commit` is not strictly behind the current generation's
     base — that is, it equals it, descends from it, or is incomparable with it after a
     history rewrite (checked with `merge-base --is-ancestor` in the owner checkout; a
     diverged current generation is already unusable for admission under §5, and the newer
     `build_seq` is what orders the replacement). A build whose base is a strict ancestor
     of the current base is stale by construction and fails this check even if its
     sequence were somehow newer.
   - On publication, in the same transaction: write the generation as `current` with its
     `build_seq` and `basis_generation_id`, mark the previous `current` generation
     `superseded`, record lineage and change events, and prune past retention. Pruning
     deletes only `superseded` generations beyond `retain_generations`; it can never
     delete the generation just marked `current` in this transaction.
   - On fence failure, write nothing and prune nothing: no generation rows, no lineage, no
     change events, and the standing `current` generation is untouched. Ingest returns a
     distinct superseded no-op outcome (`superseded_noop`, naming the standing
     generation's `build_seq` and `base_commit`), and the job run ends as a successful
     no-op rather than a failure, so a routine retry is not spent re-running a build that
     can never publish. Operators can still see the rejection in the run's step output.

   A SQL transaction alone would only make each unconditional publication atomic; the
   sequence and ancestry preconditions checked inside that transaction are what order
   concurrent publications. Because sequences are allocated at resolve, a build that
   resolves later always fences out a build that resolved earlier, regardless of which
   finishes first, which entry point started it, or whether its fire was reclaimed.

**Advisory single-flight lease (cost only).** In the same resolve transaction, the build
tries to claim the workspace lease in `symbol_index_build_state` (`lease_run_id`,
`lease_expires_at`, with a TTL comfortably above the index target, refreshed by the index
step while it runs). If an unexpired lease is held by another run, the build ends at
resolve as a skipped no-op (`lease_held`) instead of paying for an index run that would
likely be fenced out; it does not wait. The lease is released at ingest, at the resolve
no-op exit, and on run failure where the runtime records one. The lease may expire while
its holder is still running — that is expected and harmless: a successor then proceeds,
and the fence, not the lease, decides publication. The lease therefore bounds wasted
indexing work in the common case but is never load-bearing for correctness, and its
expiry is never treated as evidence that the earlier build stopped.

**Deterministic overlap trace.** Build A resolves base `C0` with `build_seq` 1 against
basis generation G0, then stalls (its fire passes the 60-minute horizon and is reclaimed,
or it was started directly). Build B — a later scheduled fire, a retry, or a direct run —
resolves newer base `C1` (a descendant of `C0`) with `build_seq` 2, finishes first, and
publishes: `current` becomes B's generation (`C1`, seq 2) and G0 is superseded. Build A
then finishes and reaches ingest with seq 1 at `C0`: check (a) fails (1 is not greater
than 2) and check (b) fails (`C0` is a strict ancestor of `C1`), so A writes nothing,
prunes nothing, and returns `superseded_noop`. B's generation stays `current`, and the
retention pruner never sees a transaction in which A's older generation could supersede
or prune it. The same rejection holds if A and B were both direct runs, if B was a retry
re-dispatch, or if A's owner was alive, stopped or unprobeable at reclamation — the fence
does not consult liveness.

**Cost**

- *Resolve:* a few seconds, plus downloading one LCOV file (tens of MB for this workspace).
- *Index:* unmeasured. It is static parsing with no compilation. orbit-graph syncs
  incrementally by file content, so a rebuild after a day's deliveries re-parses only the
  changed files. Mapping LCOV is linear in the report. Phase 1 records wall time and peak
  memory per build. The target is under 10 minutes for an incremental build. The
  60-minute `timeout_minutes` is not a budget the build is killed at: it is only the
  staleness horizon after which the scheduler may dispatch another fire (above), so a
  slow build may run past it and is then fenced at ingest if a newer build published.
- *Ingest:* about 31,000 row writes in one transaction, expected to take seconds.

The cron fires four slots a day; with `retries: {max: 1}` each slot can dispatch twice, so
at most eight scheduled dispatches a day, plus an unbounded number of direct on-demand
runs. Builds are not guaranteed to run one at a time (see above); the lease keeps
concurrent indexing rare in practice and the fence keeps it safe when it happens. There
is no runner cost: coverage reuses the report the Coverage job already pays for.

## 5. Staleness and renames

**Generation staleness** is measured at read time against the integration head: the number of
commits from `base_commit` to the head, cached with the reader's model. A generation is:

- `fresh` within both `max_staleness_commits` (200) and `max_staleness_days` (7);
- `stale` beyond either limit;
- `diverged` when its base is no longer an ancestor of the head, for example after a force-push.

The admission reader uses only a `fresh` generation. Otherwise it falls back to
`declared_files` with reason `symbol_index_stale`, `symbol_index_diverged` or
`symbol_index_absent`. The read tool and the view still show a stale generation, labelled with
its lag. Coverage freshness is separate and per record: the `measured`, `carried` and `stale`
states of §1.

**Renames.** A selector identifies a symbol only until someone renames or moves it. Lineage
links selectors across generations, in this order of evidence:

1. **Indexer lineage.** The indexer's `lineage` entries, from Git rename detection and its own
   symbol pairing.
2. **Digest match.** A selector present in the previous generation and absent now is linked to
   the one new selector of the same `kind` with an equal `body_digest`. A digest match is not
   trusted for bodies under three lines, where many unrelated symbols share a digest. With
   zero or several candidates, nothing is linked.
3. **Otherwise** history ends. The old selector's site links and change events stay stored and
   are counted as `orphaned_sites` in the read tool.

A rename that also edits the body therefore breaks history unless the indexer pairs it. A split
or merge never carries history. A deleted symbol drops out of the view, and its events age out
of the window.

**Resolving recorded sites.** `regression.sites` are recorded at filing time and never
rewritten on the task. At read time core resolves each `symbol:` site forward through lineage,
at most 16 hops and with a cycle guard, to a current selector. Agents often spell kinds
inconsistently, for example `fn` against `function`. A site with no exact match therefore
resolves by `path#name` when exactly one current symbol in that file has the name; the
resolution basis is `name_match`. Anything else counts as `unresolved_sites`. A `file:` site
counts at file level and is never split among the file's symbols.

## 6. Site history

For symbol `s` over the trailing `site_window_days` (default 180):

- **Exposure `n_s`** is the number of distinct done tasks whose landed commit changed `s`, from
  change events resolved through lineage. A site naming `s` in a task that did not change `s`
  also counts as an exposure, so a rate never exceeds one.
- **Sites `r_s`** is the number of distinct culprit tasks whose `regression_from` links carry a
  `symbol:` site resolving to `s`. Several links or fixes naming the same culprit and symbol
  count once.
- **Base rate.** With `N` the total of (task, symbol) exposures and `R` the number of those that
  were named sites, `p0 = (R + 1) / (N + 2)`.
- **Shrunk rate.** `p_s = (r_s + m·p0) / (n_s + m)`, with `m = workflow.regression_risk.min_support`
  (default 5), the same shrinkage the per-file estimator uses.

The read tool reports `n_s`, `r_s`, `p_s`, counts of `r_s` by `RegressionEscape` (`no_test`,
`test_not_selected` and the rest), `orphaned_sites` and `unresolved_sites`. A symbol with
`n_s = 0` reports `support: none`, never a low rate.

A link enters the history when it is **filed**, not when its culprit landed. A backtest
therefore uses only links filed before a fold starts (§8).

## 7. Readers

### 7.1 Admission: the `declared_lines` basis

This basis is wired only after G1 passes. It extends [ORB-19361]'s estimator with a third
basis beside `declared_files` and `diff_lines`:

1. Resolve each canonical selector of the task's footprint against the current `fresh`
   generation:
   - a `symbol:` selector, through lineage, to its line range;
   - a `file:` selector to the whole file's lines;
   - a `dir:` selector to every indexed file under the prefix.
2. Size is the sum, over files, of the union of the resolved line ranges. A file selector covers
   its whole file, so a symbol inside it adds nothing, and nested symbols are not counted twice.
3. If any path selector is unresolved (a new file, a symbol the index does not have), or the
   generation is not fresh, or the footprint has `module:`/`command:` selectors only, the task is
   scored on `declared_files`. The estimate names the reason, as [ORB-19361] already does for
   tasks without a resolvable diff.
4. Fit `p = 1 - exp(-exp(a + b·log(1 + size)))` by maximum likelihood on the trailing window's
   done tasks that resolve. Their sizes come from the same current generation. That
   generation is available at prediction time, so the fit has no leakage. It only
   approximates each training task's size when it was admitted, and G1 tests exactly this
   approximation.

The estimate adds `symbol_index: {generation_id, base_commit, lag_commits, resolved,
unresolved}`. The change bumps `TASK_RISKINESS_ESTIMATOR_VERSION`, and, under the backlog
forecast's version rule, its `MODEL_VERSION`.

Review-time risk never reads the index: once a diff exists, `diff_lines` is the better size
(AUC 0.737 against 0.704 for declared files).

### 7.2 The test-priority view

Each list below is built only by the implementation task that follows its passed gate.
Before then, the read tool returns records and nothing ranks them.

**Population.** Records with `is_test = false` and no children (no other record names them as
`parent`), so containers such as impl blocks and modules do not dominate by size.

**Risk.** Until G3a passes with site history, risk is the size percentile within the
population. After that, it is the probability fitted in G3a:
`logistic(c0 + c1·log(1 + body_lines) + c2·logit(p_s))`.

**Coverage value.** The branch fraction where `branches_found > 0`, otherwise the line fraction.
It applies only to `measured` and `carried` records.

**Under-tested** (G3a and G3b). Symbols in the top risk decile, with at least five body lines,
whose coverage is below the population's lower quartile, ordered by risk. Each row has the
selector, lines, coverage and state, site counts by escape, and the generation. The `test_refs`
fallback never feeds this list: under boundary-first testing it reads near zero for most
production symbols, which would put almost everything on the list.

**Retirement candidates** (G3a and G3c). Symbols in the bottom risk half with
`test_refs >= 5`, ordered by `test_refs`. Core stores only the count. A caller who wants the
referencing tests asks the indexer plugin's own reference tool, so core stays free of language
detail.

Both lists feed the uses in
[1_scope.md: Reconciling the view with the test rules](./1_scope.md#reconciling-the-view-with-the-test-rules)
and nothing else. No auto-task, prompt mark or gate reads them.

## 8. Backtests and ship gates

**Common protocol.** This reruns the 2026-10-27 setup:

- **Folds:** weekly rolling-origin folds, the original 11 extended to the newest complete week.
- **Labels:** a task regressed when a `regression_from` link names it, with the same label
  maturity cutoff as that run.
- **Features:** only data available at each fold's start.
- **Metrics:** AUC, log loss and Brier score.
- **Uncertainty:** 2,000 paired bootstrap resamples. Task-level gates resample tasks.
  Symbol-level gates resample files as clusters, since symbols in one file are correlated.
- **"Beats":** a higher AUC by the stated margin, the 95% interval of the paired difference
  above zero, and log loss no worse.
- **Evaluability:** a gate with fewer than 30 positive units in its evaluation folds is not
  evaluable, and that counts as not passed.

The scripts and a JSON result per gate (`backtest-g1.json` and so on) are attached to the
analysis task. The outcome is recorded in this doc's Status line in the change that ships or
drops the output.

**Index reconstruction.** For each fold, the indexer builds a generation at the integration
head as of the fold's start. Coverage comes from the newest `coverage-lcov` artifact before that
instant, which limits G3b to folds within the repository's artifact retention (GitHub keeps
artifacts 90 days unless configured otherwise). Declared footprints are taken from the same
pre-execution `context_files` snapshot [ORB-19361]'s declared-files backtest used.

| Gate | Unit and label | Candidate | Baseline | Pass |
|---|---|---|---|---|
| **G1** | Test task; regressed | §7.1 `declared_lines`, falling back to `declared_files` per task, with (a, b) refitted per fold | `declared_files` on the same tasks (0.704 in the 2026-10-27 run) | ΔAUC ≥ 0.01, interval above 0, log loss no worse. Also report the resolved share. |
| **G2** | Test task; regressed | The shipped size basis plus `max logit(p_s)` over the footprint's indexed symbols. Run once for the admission basis and once for the diff basis, using the diff's changed symbols. | The shipped size basis alone | Same as G1, judged for each basis separately. Only a passing basis gains the site term. |
| **G3a** | (symbol, fold); named as a site by a link filed within 28 days of the fold start | Size percentile, then the fitted site model if `p_s` improves it | The symbol's file shrunk regression rate (today's Regressions ranking) | AUC higher, interval above 0 |
| **G3b** | As G3a | G3a's model plus the coverage value | G3a's model | ΔAUC ≥ 0.01, interval above 0 |
| **G3c** | As G3a | Next-window site rate of the bottom risk half | The all-symbol site rate | Upper 95% bound below the all-symbol rate |

G1 can run after phase 2. G2 and G3 run once about 100 `regression_from` links carry `symbol:`
sites. A gate that fails or is not evaluable may be rerun when more history exists, under the
same protocol and margin; the rerun has to clear it again from scratch.

## 9. Concerns & Honest Limitations

- **The indexer is trusted for correctness, not safety.** Core validates shape and bounds, but
  it cannot check that extents or coverage mapping are right. A wrong extractor gives wrong
  sizes silently. `extractor_version` on every generation lets a backtest exclude a bad one.
- **One language has been looked at.** ws_orbit is Rust. Other workspaces depend on the
  indexer's extractor quality for their languages, and no gate has been run on them.
- **Exposure inherits the landed-diff rules.** A rebase merge of several commits counts only
  its last commit, as in `diff_stats.rs`. Hotfixes and commits that no task owns add changes
  without exposure.
- **The production fit uses today's sizes for past tasks.** That is honest at prediction time
  but blurs the fit for symbols that grew or shrank. G1 measures the effect, not an idealized
  variant.
- **Coverage blind spots.** Macro-expanded and generated code map imperfectly to source lines.
  The `stale` state withholds coverage from exactly the recently edited symbols. The pinned
  stable toolchain's LCOV carries no branch records.
- **Recorded sites are agent judgments.** A wrong site misattributes history. A misspelled kind
  or path either resolves through `name_match` or is counted as unresolved, and is never
  guessed further.
- **Statistical power.** At 100 site-bearing links, G3's positives number in the low hundreds
  among about 31,000 symbols. Wide intervals favor "not passed", which is the safe default.
- **Two processes per build.** The plugin boundary costs a sandboxed process and a file handoff
  per build. That is the price of keeping language parsing out of core.

## Task References

- [ORB-19358] — wrote this design and its scope
- [ORB-19361] — replaces the per-file maximum with a size-based riskiness fit (declared files, diff lines)
- [ORB-19276] — made Orbit always record `regression.sites`
- [ORB-18383] — recorded lines added and deleted per landed delivery, with a bounded backfill
- [ORB-00374] — removed the shell activity surface fail-closed
- [ORB-19294] — designed script steps, a granted execution path for workspace scripts

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
