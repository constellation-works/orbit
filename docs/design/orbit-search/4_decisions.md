---
summary: "Semantic Search — Decisions"
type: design
title: "Semantic Search — Decisions"
owner: claude
last_updated: 2026-08-24
last_validated: 2026-09-05
status: Accepted
feature: orbit-search
doc_role: decisions
tags: ["orbit-search"]
---

# Semantic Search — Decisions

> **Retired learning clauses:** [ORB-10736] / [Remove the native project-learning subsystem](../project-learnings/4_decisions.md#remove-the-native-project-learning-subsystem) removed the native
> project-learning corpus. Learning-specific statements in earlier entries are
> historical records only and do not describe a supported search kind or index.

Decision log of non-obvious orbit-search decisions. Each entry names the pressure, the choice, and the tradeoff. Entries stay in historical order; title-based supersession links preserve changes in direction without deleting the earlier reasoning.

Format for each entry: **Recorded date and task provenance**, then *Context → Decision → Consequences*. Every decision names at least one cost.

---

## fastembed-rs ONNX backend over Candle, llama.cpp, or external ollama

**Recorded:** 2026-07-26 21:51:25.816199Z · [T20260510-3], [T20260510-9], [ORB-10458]

### Context

Local embedding inference has four plausible backends:

| Backend | Profile |
|---------|---------|
| **fastembed-rs** | Pure Rust crate wrapping ONNX Runtime; ships a small set of well-known sentence-embedding models (BGE, MiniLM, Nomic, mxbai); CPU-only fine; batch-friendly. |
| **Candle** | Pure-Rust ML framework from HuggingFace; broader model support; more code to integrate; less plug-and-play for embeddings specifically. |
| **llama-cpp-rs** | Bindings to llama.cpp; GGUF format; runs anything from tiny embedding models to large LLMs; optional GPU; C++ build dependency. |
| **External ollama or similar always-on daemon** | Outsources inference but requires the user to install and run a separate long-lived process. |

This ADR addresses *which* backend to use. The orthogonal decision of *how* the backend is delivered to the user (in-process vs. companion binary vs. feature flag) is in [Companion binary installed on demand, rather than bundled in `orbit`](#companion-binary-installed-on-demand-rather-than-bundled-in-orbit). Within in-process or in-companion options, fastembed-rs covers the embedding-model use case directly; Candle is more general but requires more Orbit-side code; llama-cpp-rs is overkill and adds a C++ build dependency that complicates Orbit's release pipeline. An always-on ollama-style daemon contradicts Orbit's no-daemon posture regardless of binary placement.

### Decision

Phase 1 uses fastembed-rs as the inference backend, exposed through an `Embedder` trait that lives in a new `orbit-embed` library crate. Per [Companion binary installed on demand, rather than bundled in `orbit`](#companion-binary-installed-on-demand-rather-than-bundled-in-orbit), fastembed-rs is linked into a separate `orbit-embed-companion` binary, not into the main `orbit` binary; the trait abstraction means an alternative backend can later swap in without touching `orbit-store` or `orbit-tools`. The user-facing default model is BGE-small-en-v1.5 (384 dim, ~30MB), with `--model {bge-small | minilm-l6 | nomic-v1.5}` selected at install time. Reject external always-on ollama: contradicts the no-daemon posture. Reject llama-cpp-rs: C++ build dependency outweighs its flexibility for embedding-only work. Reject Candle as default: more integration work for less out-of-the-box behavior; remains a viable trait-impl swap.

### Consequences


- The `Embedder` trait isolates the choice of backend from storage and retrieval; later-arriving backends (Candle, code-tuned models) plug in without schema or query changes.
- The fastembed-rs model catalog (BGE, MiniLM, Nomic, mxbai) is the menu phase-1 users pick from. Other model families require a new `Embedder` impl, not a config change.
- Model output is well-characterized by published benchmarks (MTEB) so the default is defensible without an Orbit-specific eval ([3_vision.md §1.1](./3_vision.md)).
- Cost: locking in to the fastembed-rs catalog means models outside that catalog (e.g., voyage-code, code-tuned models in [3_vision.md §1.7](./3_vision.md)) need a different `Embedder` impl in a future task. The trait abstraction makes that mechanical, but it does mean the phase-1 menu is bounded by what fastembed-rs ships.


## Brute-force cosine over SQLite BLOBs; `sqlite-vec` reserved as phase-2 upgrade

**Recorded:** 2026-07-26 21:51:26.296186Z · [T20260510-3], [T20260510-9], [ORB-10458]

### Context

Vector storage and retrieval has three plausible shapes:

1. **Brute-force cosine in Rust over SQLite BLOBs.** No new dependency. Linear scan per query.
2. **`sqlite-vec` loadable extension.** HNSW-indexed nearest-neighbor inside SQLite. Same on-disk format as (1). Adds a runtime extension load.
3. **Standalone vector DB** (Qdrant, LanceDB, ChromaDB). Production-grade. Adds a binary dependency or sidecar.

At phase-1 scale — tasks-only, low thousands of artifacts × small number of fields per task = tens of thousands of vectors at 384d — brute-force cosine is sub-100ms on a modern laptop and zero new dependencies. `sqlite-vec` is the right answer once the corpus crosses ~100K vectors; that crossing happens with phase-2 graph integration, not phase 1. Standalone vector DBs are inappropriate for an embedded local tool.

A subtle point: the choice of `embedding BLOB` storage format in (1) is forward-compatible with `sqlite-vec`. Upgrading is a CREATE VIRTUAL TABLE plus an INSERT … SELECT, not a schema rewrite.

### Decision

Phase 1 implements brute-force cosine in Rust over `embeddings.embedding` BLOBs. The schema preserves forward compatibility with `sqlite-vec` (same BLOB layout, same `dim` and `model_id` columns). Phase 2's graph corpus revisits storage as a separate ADR; if `sqlite-vec` is the right call at that point, it's an additive change, not a migration.

### Consequences


- Zero new runtime dependencies in phase 1.
- Schema and on-disk layout are stable across the phase-1/phase-2 boundary.
- Query latency is acceptable until the corpus crosses ~100K vectors.
- Cost: brute force scans every row every query. For a stable phase-1 corpus that's fine, but it means we can't ship "semantic search across the entire repository graph" without revisiting storage. The decision deliberately scopes phase 1 to where brute force is comfortable, and pays the upgrade cost later when there is operational evidence to size against.


## Per-field embeddings with chunked overflow, not whole-bundle concatenation

**Recorded:** 2026-07-26 21:51:26.815084Z · [T20260510-3], [T20260510-9], [ORB-10458]

### Context

A task bundle has structurally distinct fields (purpose, summary, plan, acceptance criteria, comments, review threads) of widely varying length. Two embedding strategies exist:

- **Concatenate everything into one document and embed once.** Simplest; one row per task. Loses precision because a strong match in `purpose` is averaged with weak signal from twenty unrelated comments. Long bundles routinely exceed BGE-small's 512-token context, forcing arbitrary truncation.
- **Per-field embeddings, with long fields chunked at paragraph boundaries.** Multiple rows per task. Best-matching field surfaces in the result. Chunking handles the context-window limit cleanly.

The cost of per-field is mostly storage (~5–20× rows per task) and indexing CPU. At BGE-small's 384d, even a generous 20 rows × 10K tasks = 200K rows × 1.5KB = 300MB. Fits comfortably in SQLite, comfortable for brute force at this scale.

### Decision

Phase 1 indexes one row per `(task_id, field, chunk_idx)`. Result formatting collapses multiple field hits on the same task to a single result with the highest-scoring field surfaced as the snippet. Long fields (`plan.md`, `execution-summary.md`) are split at paragraph boundaries with a target of 400 tokens per chunk and 50-token overlap.

### Consequences


- Result snippets point to the actual field that matched, which makes the answer interpretable to users and agents.
- Comments and review messages become independently findable, which directly addresses the "decisions buried in long threads" failure mode in [1_overview.md §1](./1_overview.md).
- Schema's `field` column carries the discriminator without a separate table.
- Cost: 5–20× more rows per task, more storage, more indexing CPU. At phase-1 scale the cost is unproblematic; at much larger scales the per-field strategy may need revisiting alongside the storage upgrade in [Brute-force cosine over SQLite BLOBs; `sqlite-vec` reserved as phase-2 upgrade](#brute-force-cosine-over-sqlite-blobs-sqlite-vec-reserved-as-phase-2-upgrade).


## Hybrid retrieval (FTS5 BM25 + cosine, fused via RRF) from day one

**Recorded:** 2026-07-26 21:51:27.306164Z · [T20260510-3], [T20260510-9], [ORB-10458]

### Context

Three retrieval strategies were on the table:

- **Semantic only.** Strong on vocabulary mismatch; weak on literal-identifier queries (function names, error codes, task IDs, file paths). Ignores SQLite's already-shipped FTS5 BM25 capability.
- **Lexical only.** The status quo without this design. Fast, free, well-understood. Cannot find tasks whose vocabulary doesn't match the query.
- **Hybrid: BM25 + cosine, fused via Reciprocal Rank Fusion.** Both retrievers run in parallel; ranks combine without score calibration. Published research consistently shows hybrid beats either alone across information-retrieval benchmarks.

The third option costs one extra SQL query per search and ~30 lines of fusion code. SQLite ships FTS5 with BM25 built in, so the lexical side is essentially free — the implementation is `CREATE VIRTUAL TABLE tasks_fts USING fts5(...)`. Picking semantic-only would be a deliberate choice to fail on literal-identifier queries, which agents query frequently.

A weighted combination (e.g. `0.6 * cosine_score + 0.4 * bm25_score`) was considered as an alternative fusion. Rejected because BM25 and cosine produce scores on incommensurable scales, weights become a tuning knob with no obvious right answer, and RRF demonstrates equal or better quality without the calibration burden.

### Decision

Phase 1 ships hybrid retrieval. Both retrievers run on every `search` query. RRF (k=60) fuses the rankings. Score breakdown (`bm25_rank`, `cosine_rank`) is exposed in result payloads so consumers can detect which retriever drove a given hit. `related` (similar-task discovery) is cosine-only because lexical similarity adds noise for that use case.

### Consequences


- Literal-identifier queries (task IDs, function names, file paths) match correctly.
- Vocabulary-mismatch queries match correctly.
- Score breakdown gives agents a real signal for confidence calibration without exposing raw incommensurable scores.
- Cost: every `search` runs two SQL queries instead of one and computes one extra fusion pass. At phase-1 latency budgets (target <200ms p95) this is unproblematic, but it doubles the per-query work versus a single-retriever design and that overhead is paid even on queries where one retriever would have been enough.


## Companion binary installed on demand, rather than bundled in `orbit`

**Recorded:** 2026-07-26 21:51:27.766417Z · [T20260510-3], [T20260510-9], [ORB-10458]

### Context

Once fastembed-rs is the chosen backend ([fastembed-rs ONNX backend over Candle, llama.cpp, or external ollama](#fastembed-rs-onnx-backend-over-candle-llamacpp-or-external-ollama)), the question of where it lives matters. Linking ONNX Runtime + fastembed-rs into the main `orbit` binary adds ~50MB and pays that cost for every user — including users who never invoke semantic search. Three packaging shapes are plausible:

| Option | Default install size | Opt-in mechanism | Inference latency |
|--------|----------------------|------------------|-------------------|
| **A. Bundled in `orbit`** | Large (~50MB+) | None (always available) | In-process; instant after warm cache |
| **B. Cargo feature flag, two release artifacts** | Small or large depending on which artifact you download | Choose `orbit-full` at install time; replace the binary to swap | In-process; instant |
| **C. Companion binary downloaded on demand** | Small | `orbit semantic install [--model X]` | Subprocess; ~100–300ms ORT cold start, amortized across batches |

Option A is what the design originally called "single binary install posture preserved." It does preserve that, but it also means the always-pay binary cost is a permanent tax on users who don't want semantic search. Option B requires users to swap their main binary, which is gross UX (in-flight processes, partially-applied upgrades, surprising behavior changes). Option C keeps the default install slim and gives the user explicit control over which model — and how much disk — they're committing to, at the cost of subprocess overhead.

### Decision

Phase 1 ships option C. Two new crates:

- `orbit-embed` — small library holding the `Embedder` trait, JSON-RPC types, and `SubprocessEmbedder` (the trait impl that locates and talks to the companion). No fastembed-rs dependency. Linked into the main `orbit` binary.
- `orbit-embed-companion` — binary crate. Depends on `orbit-embed` + fastembed-rs. Produces a standalone `orbit-embed-companion` binary distributed via GitHub Releases per platform.

`orbit semantic install [--model bge-small | minilm-l6 | nomic-v1.5]` downloads the platform-appropriate companion binary plus the chosen model files into `~/.orbit/embed/`. Inference happens via stdio JSON-RPC; the subprocess is kept alive across a batch (`reindex`, multi-query session) and shut down at process exit. `orbit semantic uninstall` removes both the companion and the model. When semantic search is invoked without the companion installed, all read/write paths fail with a clear, actionable error pointing at `orbit semantic install`.

### Consequences


- Default `orbit` install stays slim — no ORT, no fastembed-rs in the main binary. Users who don't want semantic search pay no cost.
- The model menu is exposed at install time, not as a runtime config knob the user has to discover. Users actively choose between MiniLM-L6 (smallest, ~23MB), BGE-small (default, ~30MB), and Nomic-v1.5 (largest, ~140MB) at the moment they're committing to the feature.
- The subprocess-RPC boundary makes the companion swappable: a future `orbit-embed-companion-candle` could reuse the same RPC protocol with a different inference engine.
- Cost: install becomes a two-step user action (`orbit` install, then `orbit semantic install`). Users hitting `orbit search` without the companion installed need a clean, helpful error. The subprocess introduces ~100–300ms ORT cold-start latency per process; mitigated by reusing the subprocess across batches but still visible on first interactive query. Additionally, the companion binary requires a per-platform release pipeline (Linux x86_64, Linux arm64, macOS x86_64, macOS arm64, Windows x86_64), which is real release-engineering work for follow-up tasks.


## Workspace-local semantic DB separate from global audit/tool DB

**Recorded:** 2026-07-26 21:51:38.541365Z · [T20260510-9], [ORB-10458]

### Context

Orbit already has a global SQLite database at `~/.orbit/orbit.db` for command audit, tool registry, and task-lock bookkeeping. Task bundles themselves are workspace-scoped under `.orbit/tasks`, and the scoping rules treat task data as workspace-only. Semantic rows are derived from task text, so putting embeddings in the global DB would create cross-project leakage and make stale-row accounting depend on which workspace happened to be active.

### Decision

Store phase-1 semantic tables in a workspace-local SQLite database at `.orbit/state/semantic.db`. The semantic feature crate (`orbit-embed`, see [Semantic-search ownership relocated to `orbit-embed`](#semantic-search-ownership-relocated-to-orbit-embed)) opens and owns this file end-to-end: `VectorStore::open(path)` and `VectorStore::open_in_memory()` apply the WAL + busy_timeout pragmas, run `ensure_vector_schema(conn)` (CREATE TABLE IF NOT EXISTS for `embeddings` + `tasks_fts`), and return a `VectorStore` whose `Arc<Mutex<Connection>>` is the only handle into the database.

### Consequences


- Task-derived vectors and FTS rows follow task scoping: one workspace cannot see another workspace's semantic index.
- `orbit semantic index` can rebuild only the active workspace without filtering a global table by workspace ID.
- Tests use `VectorStore::open_in_memory()` directly — no orbit-store handle to plumb through.
- `semantic.db` carries only the embeddings/FTS5 schema. Earlier phase-1 implementations co-located the generic `orbit-store` migration bundle in the same file (audit, tools, reservations, etc.); that collateral was removed when [Semantic-search ownership relocated to `orbit-embed`](#semantic-search-ownership-relocated-to-orbit-embed) cut the `orbit-embed → orbit-store` dependency.


## Semantic-search ownership relocated to `orbit-embed`

**Recorded:** 2026-07-26 21:51:39.228648Z · [T20260510-20], [ORB-10458]

### Context

[T20260510-9] landed phase-1 with logic split across the wrong crate boundary:

- The former `orbit-store::vector` module hosted `VectorStore`, `EmbedWorker`, the BLAKE3 dedup, the paragraph chunker, and the cosine helper. It imported `orbit_embed::{Embedder, SubprocessEmbedder}` directly, so the dep arrow was `orbit-store → orbit-embed` and `orbit-store` leaked knowledge of the embedding feature.
- `crates/orbit-core/src/command/semantic.rs` (328 lines) hosted install / uninstall / reindex / stats logic plus HTTP companion download, model-dir wrangling, and companion version probe. None of that is `OrbitRuntime` orchestration.

Compare to the analogous knowledge-graph crate: `orbit-knowledge` owns its data + commands in `commands/{search,show,overview,…}.rs` and `orbit-core/src/command/graph.rs` is a six-line re-export. Phase-1 semantic search was three-crate-spread; the graph feature is one-crate-plus-thin-re-export.

### Decision

Relocate orbit-search ownership into `orbit-embed` and make it self-contained:

- Move the vector storage module to `crates/orbit-embed/src/vector/mod.rs`. `orbit-store` drops its dependency on `orbit-embed`. **`orbit-embed` does not depend on `orbit-store`**: it owns its own SQLite handle directly via `rusqlite::Connection` wrapped in `Arc<Mutex<_>>`, applies WAL + busy_timeout pragmas, and runs `ensure_vector_schema` on `VectorStore::open(path)` / `VectorStore::open_in_memory()`. This mirrors how `orbit-knowledge` owns `graph_index.sqlite` end-to-end without going through `orbit-store`.
- Move the per-command logic to `crates/orbit-embed/src/commands/{install,uninstall,reindex,stats}.rs`. Each command file owns its `*Params` and `*Result` types and one public `run` function. `crates/orbit-embed/src/commands/mod.rs` aggregates the surface and holds shared helpers (`parse_model`, `active_model`, `remove_file_if_exists`, `DEFAULT_RELEASE_BASE_URL`).
- Reduce `crates/orbit-core/src/command/semantic.rs` to a thin `OrbitRuntime` delegate (≤45 lines) that re-exports the param/result types and forwards each method to `orbit_embed::commands::*::run`. CLI ergonomics are preserved: `orbit-cli` still calls `runtime.semantic_install(params)` etc., and `Execute` impls are unchanged.

### Consequences


- Crate dependency direction matches the graph feature exactly: `orbit-embed` is a near-leaf feature crate that depends only on `orbit-common` (and its workspace-standard libs: rusqlite, blake3, chrono, reqwest). `orbit-store` is storage-only; both crates are independent.
- `semantic.db` carries only the `embeddings` and `tasks_fts` tables — no audit/tools/reservations/task_tags collateral. Pre-T-20 implementations had `orbit-store::Store::open` apply the full migration bundle to `semantic.db` as a side effect; that's gone now.
- `orbit-embed` gains `reqwest` (blocking) for the install download and inlines a small WAL helper. Neither violates [Companion binary installed on demand, rather than bundled in `orbit`](#companion-binary-installed-on-demand-rather-than-bundled-in-orbit)'s slim-client constraint: the prohibition is on linking ML inference (fastembed-rs / ONNX Runtime) into the main `orbit` binary; storage and HTTP client are fine. `orbit-embed-companion` remains the only crate that links fastembed.
- The phase-2 graph corpus (per [Brute-force cosine over SQLite BLOBs; `sqlite-vec` reserved as phase-2 upgrade](#brute-force-cosine-over-sqlite-blobs-sqlite-vec-reserved-as-phase-2-upgrade), [Per-field embeddings with chunked overflow, not whole-bundle concatenation](#per-field-embeddings-with-chunked-overflow-not-whole-bundle-concatenation)) can land in `orbit-embed::vector` directly without crossing another crate boundary.
- The phase-1 CLI surface is preserved exactly (install / uninstall / reindex / stats produce identical observable output). `VectorStore::new(store)` is replaced by `VectorStore::open(path)` and `VectorStore::open_in_memory()`. Only one in-tree call site (`crates/orbit-core/src/runtime/builder.rs`) is affected: it stops opening `Store::open(&persistence.semantic_db)` and instead calls `VectorStore::open(&persistence.semantic_db)` directly.
- Cost: a small amount of `rusqlite::Connection` plumbing duplicates what `orbit-store::Store` does (WAL pragma helper, parent-dir creation, mutex wrapping). The duplication is small (≈30 lines) and isolates the semantic feature's schema from migrations to other store domains, which is the whole point.


## Version-aware companion refresh and quiet background indexing

**Recorded:** 2026-07-26 21:51:39.751563Z · [T20260510-26], [ORB-10458]

### Context

The companion binary is installed outside the main `orbit` executable, so upgrading Orbit does not automatically replace an already-present `~/.orbit/embed/bin/orbit-embed-companion-<platform>`. A stale companion can therefore keep old subprocess behavior after the main binary has moved on. The concrete failure was a stale companion writing `execution failed: Broken pipe (os error 32)` to stderr during best-effort background task indexing, after the durable task update had already succeeded. Direct semantic commands should still surface companion stderr because users explicitly invoked the semantic subsystem and need useful failure detail.

### Decision

`orbit semantic install` probes an existing installed companion with `--version-info` and compares the returned version to the current Orbit package version. Missing, stale, unprobeable, or explicitly forced companions are replaced through a temporary sibling file before being moved into place; successful install output reports `companion_changed`. The CLI exposes `--force` for intentional replacement even when the probe says the companion is current. `SubprocessEmbedder` keeps inherited stderr as the default for direct semantic commands, while the background task-mutation worker uses a quiet spawn mode.

### Consequences


- Re-running `orbit semantic install` after upgrading Orbit naturally refreshes stale companions without requiring users to uninstall first.
- Task mutation output stays trustworthy: background indexing remains best-effort and cannot leak companion stderr into successful `task.add` / `task.update` command output.
- Direct commands such as `orbit search <query> --hybrid`, `orbit search similar <task-id>`, and `orbit semantic index` still show actionable companion stderr because they use the inherited-stderr path.
- Cost: install now trusts the companion's `--version-info` protocol. If a broken companion cannot answer the probe, Orbit conservatively replaces it, which can redownload or recopy the binary even when the file might have been usable for embeddings.


## Split lifecycle and query search namespaces

**Recorded:** 2026-05-20 05:19:25.925165Z · [ORB-00196]

### Context
`orbit semantic` mixed embedding-companion lifecycle (`install`, `uninstall`, `stats`, `index`) with user query verbs (`search`, `related`). The phase-1 search engine now owns both lexical and vector ranking, so leaving queries under `semantic` would make users choose an implementation detail before they search.

### Decision
`orbit semantic` is only the lifecycle namespace for the local embedding companion. `orbit search` is the unified query surface; lexical ranking is the default, `--semantic` opts into hybrid BM25 plus cosine for task vectors, and `--related <id>` performs cosine-neighbor lookup for indexed tasks.

### Consequences
- Establishes a precedent that lifecycle namespaces manage local subsystems while query namespaces describe what users are trying to do.
- `orbit semantic search`, `orbit semantic related`, and `orbit semantic reindex` are hard breaks with no shim because there are no known external consumers yet.
- Per-domain search commands stay untouched for phase 1; a later task decides whether they thin-wrap `orbit search`, demote to filters, or retire.
- Vector index coverage remains task-only today; docs, learnings, and ADRs continue to use lexical matching even when `--semantic` is set.
- Cost: historical audit event names `semantic.search` and `semantic.related` become orphaned event types, accepted because no external audit-history consumers exist yet.

## Rename search mode and neighbor flags

**Superseded by:** [Split orbit search modes and require per-kind statuses](#split-orbit-search-modes-and-require-per-kind-statuses)
**Recorded:** 2026-08-01 19:15:27.082047Z · [ORB-00204], [ORB-10479]

**Context.** Phase 1 used the semantic name for the hybrid BM25 plus cosine mode toggle and a separate related-task flag for cosine-neighbor lookup. That inverted the intuitive reading of semantic search: users expect semantic plus an ID to mean nearest neighbors, while hybrid is the honest name for the ranking algorithm.

**Decision.** Rename the free-text ranking toggle to `--hybrid` / `hybrid: true` and rename task-neighbor lookup to `--semantic <id>` / `semantic: "<id>"`. Keep lexical search as the default and report JSON mode `hybrid` for hybrid free-text search and `neighbor` for cosine-only task-neighbor lookup.

**Consequences.**
- The CLI and MCP surfaces match user vocabulary before external consumers depend on the phase-1 names.
- Historical phase-1 audit payloads that carried `semantic: true` are orphaned by the hard break, matching the no-shim policy for this young surface.
- Documentation and packaged skills must distinguish the `orbit semantic` lifecycle command from the MCP `semantic: "<id>"` search parameter. [Split orbit search modes and require per-kind statuses](#split-orbit-search-modes-and-require-per-kind-statuses) replaces the CLI flag form with `orbit search similar <id>`.
- Cost: Agents and docs written against phase 1 need a one-time rename sweep, and ORB-00202 may need a rebase because it edits adjacent search surfaces.
- Cost: historical audit event names `semantic.search` and `semantic.related` become orphaned event types, accepted because no external audit-history consumers exist yet.

## Consolidate per-domain search; cross-kind --path and --tag filters; learning list --path semantics flip

**Recorded:** 2026-05-21 01:14:54.176322Z · [ORB-00202]

### Context

After [Split lifecycle and query search namespaces](#split-lifecycle-and-query-search-namespaces) and [Rename search mode and neighbor flags](#rename-search-mode-and-neighbor-flags) consolidated `orbit search` as the unified query surface, the per-domain `search` subcommands (`orbit task search`, `orbit docs search`, `orbit learning search`) became redundant for content-similarity queries. Worse, `orbit learning search` was bundling three unrelated operations under one verb: substring search (content), path-glob applicability lookup (structural), and tag filter (structural). Two of those are filters dressed up as search.

At the same time, agents pre-edit need a single command that answers *given this file path, what tasks / learnings / ADRs apply here?* — the context-pack assembly query. In the final CLI shape after [Split orbit search modes and require per-kind statuses](#split-orbit-search-modes-and-require-per-kind-statuses), that is the `orbit search path <path>` form; MCP keeps a `path` parameter. The same logic applies to `--tag`: one cross-kind label bridge.

The phase-1 search engine already supported `--kind {task,doc,learning,adr,all}`. Phase 2 finishes the consolidation: removes the redundant verbs, re-homes their filters under the unified search surface, and fixes one observed semantics bug in `learning list --path`.

### Decision

Five threads decided together because they share a single mental model — *search = content-similarity, list = structural filter*:

1. **Deletion verdicts.** Hard-remove `orbit task search`, `orbit docs search`, `orbit learning search` (CLI + MCP). No deprecation shims — phase 1 set the precedent that no external consumers depend on these surfaces. Replacement: `orbit search <query> --kind <X>` for content-similarity queries; `orbit <kind> list --filter` for structural filters.

2. **Structural-vs-content split.** `search` carries free-text or neighbor queries against indexed content. `list` carries structural filters (status, tags, paths, owners). `orbit learning search --path` and `--tag` cases re-home onto `orbit learning list --path` / `--tag`. The substring case re-homes onto `orbit search <query> --kind learning`.

3. **Universal status wideners replace per-kind flags.** Introduce `--all` (kind-aware widener) and `--status <kind:value,...>` on `orbit search`. Per-kind defaults: task = `proposed,backlog,in-progress,review` (+ `done,rejected,archived` on `--all`); learning = `active` (+ `superseded` on `--all`); adr = `proposed,accepted` (+ `superseded` on `--all`); doc = no-op. The old `orbit docs search --include-superseded` mental model is replaced by `orbit search --kind adr --all`. One vocabulary, three kinds covered.

   *Implementation note:* `AdrStatus` does not currently carry a `Deprecated` variant, so `--all` adds `Superseded` only. If a deprecated state is added later, the widener will pick it up without a flag change.

4. **Path lookup and `--tag` as cross-kind filters.** Both compose with `--kind` and with each other. The CLI spells path lookup as `orbit search path <path>`; the MCP tool keeps a `path` parameter. Per-kind semantics:
   - **`--tag`**: AND semantics for repeated values; case-insensitive. Applies to task, doc, learning, and the union (`all`). For `--kind adr` the filter returns empty and `--help` documents the deferral; the underlying constraint is that ADRs have no free-form `tags` field today (`related_features` is structural).
   - **Path lookup**: applies to task and learning. For task, selector-mapping against `context_files` (`file:` exact, `dir:` containment in either direction, `symbol:` matches on file component). For learning, glob-containment against `scope.paths`. ADR and doc return empty; help text states the deferral.
   - Cross-kind ADR tag and path matching is deferred to phase 3 (ORB-00203) which adds the necessary frontmatter fields.

5. **`orbit learning list --path` semantics flip.** From exact-match (`scope.paths.iter().any(|p| p == path)`) to glob-containment (compile each rule as a glob regex, match the normalized query path). This aligns `learning list --path` with what the deleted `learning search --path` did, which is what the pre-edit context-pack use case needs. This is the only observable behavior change in phase 2; everything else is surface consolidation.

### Consequences

- One mental model for search: `orbit search` queries indexed content; `orbit <kind> list` filters structural metadata. The boundary is enforced by the flag layout, not by convention.
- The agent context-pack query collapses to a single command: `orbit search path <file> --kind all`. Previously this was three separate calls plus client-side merging.
- `--all` and `--status kind:value` give every kind the same widening vocabulary; reviewers reading a script with `--all` know what it does without checking per-kind flag tables.
- Phase-3 (ORB-00203) gets a clean specification for ADR `paths` and `tags`: `orbit search path X --kind adr` and `orbit search <query> --tag X --kind adr` already exist as no-op branches; phase 3 fills them in without changing the public surface.
- `learning list --path` now matches the intuition that *a learning with `scope.paths: [src/auth/**]` applies to `src/auth/login.rs`*. The behavior flip is called out in the CHANGELOG; the previous exact-match semantics were never documented as load-bearing for any agent flow.
- Audit middleware sheds the `Search` arms on Task/Docs/Learning subcommands. Audit event names `orbit.task.search`, `orbit.docs.search`, `orbit.learning.search` are orphaned by the hard break, matching the no-shim policy.
- Cost: the `learning list --path` semantics flip is a real behavior change. Any script or skill calling `orbit learning list --path src/auth/**` expecting exact-match behavior will now also see paths inside that glob. Mitigated by: (a) `learning list` returned no matches before the flip when called with a concrete file path under a glob scope, so almost all real-world calls were broken anyway; (b) the new behavior is what the deleted `learning search --path` already did, so the migration target for ex-`learning search --path` users is unchanged.
- Cost: ADR carries tag and path no-ops until phase 3 lights them up. Documented in `--help` so users do not construct queries that silently return empty.
- Cost: `AdrStatus` lacks a `Deprecated` variant; `--all` widening on ADRs is asymmetric with the task widener (which gets multiple terminal states). A separate task can extend `AdrStatus` if a deprecated state ever becomes load-bearing.

## Split orbit search modes and require per-kind statuses

**Recorded:** 2026-05-21 01:31:03.329990Z · [ORB-00205]
**Supersedes:** [Rename search mode and neighbor flags](#rename-search-mode-and-neighbor-flags)
**Paths:** `crates/orbit-cli/src/command/search.rs`, `crates/orbit-core/src/command/search.rs`, `crates/orbit-tools/src/builtin/orbit/search.rs`, `crates/orbit-core/src/runtime/orbit_tool_host/search_tools.rs`

### Context
[Rename search mode and neighbor flags](#rename-search-mode-and-neighbor-flags) corrected the search flag names after phase 1, but the resulting CLI still mixed a positional query with mode flags and allowed flat status tokens whose meaning changed by corpus kind. The real alternatives were to keep extending that single-command flag matrix, or split the user-facing CLI modes before more corpora grow vector support.

### Decision
Use three explicit CLI forms: `orbit search <query>` for free-text search, `orbit search similar <id>` for cosine-neighbor lookup, and `orbit search path <path>` for applicability lookup. Require `--status` values to use `kind:value` tokens such as `task:open`, `doc:active`, and `adr:proposed`. Remove the CLI field-selection and embedding-model flags, and remove the parallel MCP `field` and `embedding_model` parameters while keeping MCP `model` only as provenance.

### Consequences
- The CLI no longer has a top-level `<query | --semantic | --path>` trichotomy; each primary search operation has its own visible form.
- Status filters are unambiguous across task, doc, learning, and ADR corpora.
- MCP remains a parameterized tool surface, but it mirrors the reduced public parameter set and the same per-kind status parser.
- Cost: `similar` and `path` become reserved words immediately after `orbit search`; searching those literal words requires passing a quoted/free-text query with additional context.
- Cost: callers using the young mode flags, flat `--status`, the retired CLI field/model flags, MCP `field`, or MCP `embedding_model` surfaces must migrate with no compatibility shim.

## Doc corpus embeddings use `docs index` and opt-in hybrid search

**Recorded:** 2026-05-21 · [ORB-00206]

**Context.** Doc search was lexical-only after [ORB-00202] unified the query surface, while the orbit-search store already had a `source_kind` discriminator that could hold docs. The alternatives were to keep semantic ranking deferred, add a separate docs search verb, or reuse the existing vector store behind the unified `orbit search --kind doc --hybrid` path.

**Decision.** Use `orbit docs index` as the explicit admin verb that embeds configured docs roots into `source_kind = "doc"` rows, and keep retrieval opt-in through `orbit search <query> --kind doc --hybrid`. Lexical doc search remains the default, while the same companion and vector store now also support explicitly indexed learning and ADR corpora.

**Consequences.**
- `orbit docs index` shares the semantic companion, model catalog, and `embeddings` table with task vectors rather than creating a doc-specific store.
- The search crate owns doc field extraction and stale-source sweeping, but does not depend on orbit-core; core passes a small `DocEmbeddingSource`.
- Hybrid doc search falls back to lexical when the companion or doc rows are unavailable, preserving read-path ergonomics while making the admin indexing verb fail clearly.
- Cost: docs now have a manual freshness loop separate from task mutation indexing. Background docs indexing remains a future task.

---

## Expose unified search through a thin HTTP adapter

**Recorded:** 2026-07-20 02:13:41.746781Z · [ORB-10304]
**Paths:** `crates/orbit-dashboard/src/api/**`, `crates/orbit-core/src/command/search/**`

### Context
Bridge needs hybrid Orbit search but can only proxy the dashboard HTTP surface. The alternatives were to keep reconstructing lexical results in Bridge, expose a generic tool-execution HTTP endpoint, or add a narrow search endpoint backed by the same runtime pipeline as the CLI.

### Decision
Expose GET /api/search as a thin transport adapter over OrbitRuntime::global_search. The endpoint accepts the unified query, kind, status, tag, path, hybrid, and semantic parameters and returns the runtime response unchanged, including the effective mode and per-hit retriever rank breakdown. If hybrid infrastructure is unavailable, the shared runtime pipeline degrades to lexical so CLI and HTTP callers observe the same behavior.

### Consequences
- Bridge can proxy one authoritative endpoint instead of owning a second search implementation.
- CLI, tool, and HTTP search share filtering, ranking, result ordering, and fallback semantics.
- Cost: the unified search parameter names and serialized result shape become an HTTP compatibility contract; future search changes must preserve or deliberately version that surface.

## Companion binary installed on demand, rather than bundled in `orbit`

**Recorded:** 2026-05-11 02:06:39.422729Z · [T20260510-3], [T20260510-9]

### Context
Once fastembed-rs is the chosen backend ([fastembed-rs ONNX backend over Candle, llama.cpp, or external ollama](#fastembed-rs-onnx-backend-over-candle-llamacpp-or-external-ollama)), the question of where it lives matters. Linking ONNX Runtime + fastembed-rs into the main `orbit` binary adds ~50MB and pays that cost for every user — including users who never invoke semantic search. Three packaging shapes are plausible:

| Option | Default install size | Opt-in mechanism | Inference latency |
|--------|----------------------|------------------|-------------------|
| **A. Bundled in `orbit`** | Large (~50MB+) | None (always available) | In-process; instant after warm cache |
| **B. Cargo feature flag, two release artifacts** | Small or large depending on which artifact you download | Choose `orbit-full` at install time; replace the binary to swap | In-process; instant |
| **C. Companion binary downloaded on demand** | Small | `orbit semantic install [--model X]` | Subprocess; ~100–300ms ORT cold start, amortized across batches |

Option A is what the design originally called "single binary install posture preserved." It does preserve that, but it also means the always-pay binary cost is a permanent tax on users who don't want semantic search. Option B requires users to swap their main binary, which is gross UX (in-flight processes, partially-applied upgrades, surprising behavior changes). Option C keeps the default install slim and gives the user explicit control over which model — and how much disk — they're committing to, at the cost of subprocess overhead.

### Decision
Phase 1 ships option C. Two new crates:

- `orbit-embed` — small library holding the `Embedder` trait, JSON-RPC types, and `SubprocessEmbedder` (the trait impl that locates and talks to the companion). No fastembed-rs dependency. Linked into the main `orbit` binary.
- `orbit-embed-companion` — binary crate. Depends on `orbit-embed` + fastembed-rs. Produces a standalone `orbit-embed-companion` binary distributed via GitHub Releases per platform.

`orbit semantic install [--model bge-small | minilm-l6 | nomic-v1.5]` downloads the platform-appropriate companion binary plus the chosen model files into `~/.orbit/embed/`. Inference happens via stdio JSON-RPC; the subprocess is kept alive across a batch (`reindex`, multi-query session) and shut down at process exit. `orbit semantic uninstall` removes both the companion and the model. When semantic search is invoked without the companion installed, all read/write paths fail with a clear, actionable error pointing at `orbit semantic install`.

### Consequences
- Default `orbit` install stays slim — no ORT, no fastembed-rs in the main binary. Users who don't want semantic search pay no cost.
- The model menu is exposed at install time, not as a runtime config knob the user has to discover. Users actively choose between MiniLM-L6 (smallest, ~23MB), BGE-small (default, ~30MB), and Nomic-v1.5 (largest, ~140MB) at the moment they're committing to the feature.
- The subprocess-RPC boundary makes the companion swappable: a future `orbit-embed-companion-candle` could reuse the same RPC protocol with a different inference engine.
- Cost: install becomes a two-step user action (`orbit` install, then `orbit semantic install`). Users hitting `orbit semantic search` without the companion installed need a clean, helpful error. The subprocess introduces ~100–300ms ORT cold-start latency per process; mitigated by reusing the subprocess across batches but still visible on first interactive query. Additionally, the companion binary requires a per-platform release pipeline (Linux x86_64, Linux arm64, macOS x86_64, macOS arm64, Windows x86_64), which is real release-engineering work for follow-up tasks.

---

## Federate the cross-workspace read; deny it inside a managed run

**Recorded:** 2026-08-24 · [ORB-11027]
**Paths:** `crates/orbit-core/src/application/search/federated.rs`, `crates/orbit-core/src/runtime/workspace_catalog.rs`, `crates/orbit-cmd/src/workspace_catalog.rs`

### Context

`orbit search` was single-workspace: `GlobalSearchKind` is global across *kinds*, not across workspaces, and vectors plus FTS live in one `<repo>/.orbit/state/semantic.db` per workspace ([Workspace-local semantic DB separate from global audit/tool DB](#workspace-local-semantic-db-separate-from-global-audittool-db)). Answering "have we thought about X before" across polaris, almanac, constellation, and the code workspaces meant running the same query N times by hand and merging by eye.

Three shapes were available:

1. **Centralize the store** — move the index to a host-global `~/.orbit/semantic.db`.
2. **Federate the read** — keep per-workspace indexes; open the registered checkouts and fuse.
3. **Leave it to the caller** — keep N calls, document the merge.

Option 1 was assessed and rejected in ORB-11001. It breaks the sandbox boundary: the v2 host allow-lists `{workspace}/state/semantic.db*` per run, so a host-global index would hand every sandboxed agent run — including one scoped to an unrelated code workspace — a handle on a file containing the personal vault, degrading a filesystem-level guarantee into a query-time filter. It also does not replicate: a per-workspace index travels with its checkout, a host-global DB does not. Option 3 is what F2026-08-046 already shows failing — merged cross-workspace results without attribution led to a near-miss write to the wrong friction record, because friction IDs are allocated per workspace and the same ID names a different record in each.

Federating the read raises the same sandbox question in a different place, because the fan-out is in-process rather than through the filesystem allow-list: should a sandboxed agent run be able to ask for a multi-workspace scope at all?

### Decision

Federate the read. Each workspace keeps owning and writing its own `semantic.db`; a `WorkspaceScope` selector (`Current` | `Selectors` | `AllRegistered`) widens only the query.

1. **Layering.** `orbit-core` owns fan-out, fusion, attribution, notes, and caps; it takes a registry-neutral `WorkspaceCatalog` seam supplied by `orbit-cmd`, which already joins Core to Registry. No new crate edge.
2. **Fuse by rank, never by raw score.** Per-workspace hits interleave by position in their own workspace's ranked list, reusing the round-robin merge that already balances kinds. Lexical, hybrid, and absent scores are not commensurable across workspaces.
3. **Attribution is part of the hit.** Every federated hit carries `workspace: {workspace_id, name, repo_root}`, so the F2026-08-046 defect does not move in-tree along with the merge logic.
4. **Model mismatch is a named note.** A workspace indexed under a different `model_id` contributes lexical hits only, and the response says so instead of emitting a ranking the caller cannot tell apart from a fused one.
5. **Sandbox posture: deny.** A federated scope is refused inside an Orbit-managed run. Denying the *scope* rather than widening the per-run allow-list keeps the in-process read and the filesystem grant consistent: a run may read exactly the index it may write. The traded capability — an agent asking one question of the whole corpus mid-run — stays available to the human and operator surfaces that are not inside a run. The alternative, an enumerated allow-list of "safe" workspaces per run, was rejected as a second authorization system whose default would be wrong the first time a new personal workspace was registered.
6. **Default is untouched.** `Current` takes the existing single-workspace path; both new response fields are omitted from JSON when empty.

### Consequences

- One `orbit search --all-workspaces` answers across the whole corpus, and each hit names the workspace a follow-up read or write should target.
- Rank-interleaved fusion means a large workspace cannot consume the budget, and no cosine score is ever compared across a model boundary.
- A missing, stale, or unreadable index degrades one workspace to zero hits with a note instead of failing the query — the normal case for a machine with several registered checkouts.
- The `WorkspaceCatalog` seam gives any future cross-workspace read the same registry-neutral shape without pulling Registry under Core.
- Cost: an agent inside a managed run cannot search other workspaces, so cross-workspace recall stays a human/orchestrator step. If that becomes load-bearing, the follow-up is an explicit per-run scope grant carried in the run's own record — not a widened default.
- Cost: at most 16 workspaces are opened per query. The cap is announced in the response, so a machine that outgrows it sees the truncation rather than silently narrowed recall.
- Cost: opening a target goes through the ordinary `RegisteredRuntimeFactory::open_registered_checkout`, which runs the same layout pre-flight any `orbit --workspace X` invocation does. A federated *read* can therefore apply a pending layout upgrade to another checkout. That is the established open path rather than a new one, and the managed-run denial keeps it out of agent runs; a genuinely read-only runtime open would be the fix if it ever matters.
- Cost: `workspaces` and `all_workspaces` join the `orbit.search` parameter surface and the search response shape that [Expose unified search through a thin HTTP adapter](#expose-unified-search-through-a-thin-http-adapter) made a compatibility contract. `workspaces` is deliberately distinct from the reserved `workspace` routing selector; the two are easy to confuse and are documented against each other in both help texts.

## Task References

- [T20260510-3] — Design semantic search over task artifacts and graph (v2). The task that produced this folder.
- [T20260510-9] — Phase-1 semantic search foundation: orbit-embed + orbit-embed-companion + indexing pipeline. The task that accepted and implemented [fastembed-rs ONNX backend over Candle, llama.cpp, or external ollama](#fastembed-rs-onnx-backend-over-candle-llamacpp-or-external-ollama) through [Workspace-local semantic DB separate from global audit/tool DB](#workspace-local-semantic-db-separate-from-global-audittool-db).
- [T20260510-20] — Refactor: relocate orbit-search ownership to orbit-embed (vector store + commands). The task that accepted and implemented [Semantic-search ownership relocated to `orbit-embed`](#semantic-search-ownership-relocated-to-orbit-embed).
- [T20260510-26] — Make semantic companion install/update quiet and version-aware. The task that accepted and implemented [Version-aware companion refresh and quiet background indexing](#version-aware-companion-refresh-and-quiet-background-indexing).
- [ORB-00196] — Split `orbit semantic` lifecycle from the unified `orbit search` query surface. The task that accepted and implemented [Split lifecycle and query search namespaces](#split-lifecycle-and-query-search-namespaces).
- [ORB-00204] — Rename `orbit search` flags to `--hybrid` for free-text vector ranking and `--semantic <id>` for task-neighbor lookup. The task that accepted and implemented [Rename search mode and neighbor flags](#rename-search-mode-and-neighbor-flags).
- [ORB-00202] — Consolidate per-domain search subcommands and add cross-kind `--path` / `--tag` filters. The task that proposed and implemented [Consolidate per-domain search; cross-kind --path and --tag filters; learning list --path semantics flip](#consolidate-per-domain-search-cross-kind---path-and---tag-filters-learning-list---path-semantics-flip).
- [ORB-00203] — Add ADR envelope `tags` and `paths` so ADRs participate in cross-kind `--tag` / `--path` search filters.
- [ORB-00205] — Split `orbit search` into query / similar / path forms and require per-kind `--status` syntax. The task that accepted and implemented [Split orbit search modes and require per-kind statuses](#split-orbit-search-modes-and-require-per-kind-statuses).
- [ORB-00206] — Add doc-corpus embeddings through `orbit docs index` and `orbit search --kind doc --hybrid`. The task that accepted and implemented [Doc corpus embeddings use `docs index` and opt-in hybrid search](#doc-corpus-embeddings-use-docs-index-and-opt-in-hybrid-search).
- [ORB-10304] — Expose unified lexical, hybrid, and neighbor search through `GET /api/search`.
- [ORB-11027] — Cross-workspace federated search with a workspace scope selector. The task that accepted and implemented [Federate the cross-workspace read; deny it inside a managed run](#federate-the-cross-workspace-read-deny-it-inside-a-managed-run).

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
