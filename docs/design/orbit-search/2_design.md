---
summary: "Semantic Search — Design"
type: design
title: "Semantic Search — Design"
owner: claude
last_updated: 2026-08-24
last_validated: 2026-09-05
status: Accepted
feature: orbit-search
doc_role: design
tags: ["orbit-search"]
---

# Semantic Search — Design

This document specifies the semantic-search implementation: the `orbit-search` crate and its optional companion binary, the companion-binary inference model, the SQLite vector storage schema, the per-field embedding strategy, the hybrid (BM25 + cosine) retrieval pipeline, the MCP and CLI surface, cross-workspace federated read, the index-maintenance lifecycle, and the concerns the design deliberately leaves to follow-ups.

---

## 1. Architectural Placement

The `orbit-search` crate contains the library and the optional companion binary target:

- **`orbit-search`** — small client and storage library. Owns the `Embedder` trait, the JSON-RPC request/response types, `SubprocessEmbedder`, vector storage, and the install/index/query commands. Its default feature does not enable fastembed-rs, so the main `orbit` binary remains slim.
- **`orbit-search-companion`** — optional binary target behind the `companion` feature. It depends on fastembed-rs for ONNX inference and is distributed separately per platform. **Not built into `orbit`**; users opt in by running `orbit semantic install`. Per [Companion binary installed on demand, rather than bundled in `orbit`](./4_decisions.md#companion-binary-installed-on-demand-rather-than-bundled-in-orbit).

Updated dependency graph:

```
orbit-common → orbit-search → orbit-core → orbit-cli
                       ↘ orbit-search-companion (optional binary target, fastembed-rs lives here)
```

`orbit-search::vector` owns the `embeddings` table schema, write/upsert/delete API, and the brute-force cosine helper implementation. It opens the workspace-local SQLite database directly and treats the embedder as injected — tests pass a `NoopEmbedder` that returns deterministic vectors so unit tests never need the companion to be installed.

The vector SQLite store is workspace-local at `.orbit/state/semantic.db`, not in the global `~/.orbit/orbit.db` audit/tool database. This preserves the workspace scoping rule: task and doc embeddings and FTS rows do not leak across workspaces. ADRs participate through the docs corpus.

`orbit-tools` exposes `orbit.search` as the MCP query tool, and `orbit-cli` exposes `orbit search` for queries plus `orbit semantic` for companion lifecycle (`install`, `uninstall`, `stats`, `index`). These surfaces are thin shells over the shared search runtime.

---

## 2. Inference Backend

### 2.1 Trait

Defined in `orbit-search`:

```rust
pub trait Embedder: Send + Sync {
    fn model_id(&self) -> &str;        // e.g. "bge-small"
    fn dim(&self) -> usize;            // e.g. 384
    fn max_input_tokens(&self) -> usize; // e.g. 512
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, OrbitError>;
    fn token_count(&self, text: &str) -> Result<usize, OrbitError>;
}
```

Batch input is mandatory — fastembed-rs is meaningfully faster on batches than on single-document calls because of ONNX kernel reuse, and the indexing path naturally batches by task. `token_count` is exposed because the chunker in [§4.2](#42-chunking-long-fields) needs exact token counts to split fields under the model's context limit.

### 2.2 Companion-binary architecture

The library ships a single production trait impl: `SubprocessEmbedder` (in `orbit-search`). It does not perform inference itself — it spawns and talks to the `orbit-search-companion` binary that the user installed with `orbit semantic install`. The arrangement looks like:

```
orbit (main binary)                          orbit-search-companion (installed binary)
├── SubprocessEmbedder                       ├── fastembed-rs
│     ↕ stdio JSON-RPC                       │     ↕ ONNX Runtime
└── orbit-search::vector                     └── BGE-small / MiniLM-L6 / Nomic-v1.5
```

Lifecycle:

- `SubprocessEmbedder::new()` resolves the companion path under `~/.orbit/embed/bin/orbit-search-companion-<platform>` and starts the subprocess. ~100–300ms cold-start latency for ORT init.
- The subprocess stays alive for the duration of the parent process or until explicitly dropped. Callers that retain the embedder, including the background indexing worker, reuse the same subprocess.
- On process exit, the parent sends an `exit` RPC, reads the response, and waits for the child to exit.

### 2.3 RPC protocol

JSON Lines over stdio. Each request and response is a single JSON object on a single line.

```jsonc
// Request
{"id": 1, "method": "info"}
{"id": 2, "method": "embed", "texts": ["hello", "world"]}
{"id": 3, "method": "token_count", "text": "..."}
{"id": 4, "method": "exit"}

// Response
{"id": 1, "result": {"model_id": "bge-small", "dim": 384, "max_input_tokens": 512}}
{"id": 2, "result": {"vectors": [[...384 floats...], [...384 floats...]]}}
{"id": 3, "result": {"tokens": 42}}
{"id": 4, "result": {"ok": true}}

// Error
{"id": 2, "error": {"code": "model_load_failed", "message": "..."}}
```

The protocol is intentionally minimal — four methods, no streaming, no auth. The trust boundary is "this is a binary the user installed under their home directory"; there is no network involvement and no multi-tenant concern.

### 2.4 Default model and install-time model selection

`orbit semantic install` accepts `--model {bge-small | minilm-l6 | nomic-v1.5}`; default is `bge-small`. The install command validates the existing managed companion's integrity metadata and reinstalls it when the metadata is missing or stale; `--force` reinstalls it unconditionally. Model files are downloaded into `~/.orbit/embed/models/<model_id>/`. Switching model means running `orbit semantic install --model OTHER`, which downloads the new model alongside the existing one (so the embeddings under the old `model_id` keep working until reindexed; see [§7.2](#72-backfill-and-migration)).

The three supported models per [3_vision.md §1](./3_vision.md):

| Model | Dim | On-disk | Best for |
|-------|-----|---------|----------|
| `minilm-l6` (all-MiniLM-L6-v2) | 384 | ~23MB | Smallest disk and fastest CPU; older but battle-tested. |
| `bge-small` (BGE-small-en-v1.5) — default | 384 | ~30MB | Strong recall-per-byte for English on MTEB. |
| `nomic-v1.5` (nomic-embed-text-v1.5) | 768 | ~140MB | Best quality; Matryoshka-truncatable; 8192-token context. |

### 2.5 Companion locator and missing-companion behavior

On first use of any embedder-touching path, `SubprocessEmbedder::new()` checks:

1. `~/.orbit/embed/bin/orbit-search-companion-<platform>` → standard managed-install path.
2. `$ORBIT_SEARCH_COMPANION` → developer-only absolute path override when the explicit unsafe override gate is enabled.

If none resolve, the embedder returns `OrbitError::CompanionNotInstalled` with the remediation message: `"Semantic search not enabled. Run \`orbit semantic install\` to download the inference companion."` Indexing-path callers log and skip (semantic search is not on the critical path of task mutation; see [§7.1](#71-on-mutation-indexing)). Query-path callers surface the error directly to the user.

### 2.6 Alternative backends

The trait + RPC protocol make alternative companions viable without changing storage or retrieval. A future Candle-based companion could speak the same protocol and ship as a separate downloadable. None ship today; the protocol exists to keep that door open. The full backend comparison is in [fastembed-rs ONNX backend over Candle, llama.cpp, or external ollama](./4_decisions.md#fastembed-rs-onnx-backend-over-candle-llamacpp-or-external-ollama); the packaging decision is in [Companion binary installed on demand, rather than bundled in `orbit`](./4_decisions.md#companion-binary-installed-on-demand-rather-than-bundled-in-orbit).

---

## 3. Vector Storage

### 3.1 Schema

A new SQLite table in the existing per-workspace store:

```sql
CREATE TABLE embeddings (
    source_kind TEXT NOT NULL,         -- "task" or "doc"
    source_id   TEXT NOT NULL,         -- task ID or corpus-specific source ID
    field       TEXT NOT NULL,         -- "purpose", "plan", "comment_3", "review_1_msg_2", ...
    chunk_idx   INTEGER NOT NULL,      -- 0 for unchunked; >0 for splits of long fields
    content_hash TEXT NOT NULL,        -- BLAKE3 of the embedded text; cheap re-index gate
    model_id    TEXT NOT NULL,         -- "bge-small"
    dim         INTEGER NOT NULL,      -- 384
    embedding   BLOB NOT NULL,         -- dim * 4 bytes, native-endian f32
    created_at  TEXT NOT NULL,
    PRIMARY KEY (source_kind, source_id, field, chunk_idx, model_id)
);

CREATE INDEX embeddings_by_source ON embeddings(source_kind, source_id);
CREATE INDEX embeddings_by_model  ON embeddings(model_id);
```

The composite primary key includes `model_id` so embeddings under multiple models can coexist during a model migration. The `content_hash` lets the indexer skip work when the underlying text hasn't changed since the last embed.

### 3.2 Query path (brute force)

```text
1. embed query under default model_id  → query vector q (dim 384)
2. SELECT embedding, source_kind, source_id, field, chunk_idx
     FROM embeddings WHERE model_id = ?
3. for each row: compute cosine(q, row.embedding); maintain a fixed-size top-k heap
4. return top-k (source_id, field, score)
```

At 30k vectors × 384d, the full scan is ~50ms in pure Rust on a modern laptop and dominated by the SQLite read, not the dot product. The implementation uses `f32` slabs and SIMD-friendly contiguous buffers; it does not need an HNSW index at phase-1 scale. The forward path to HNSW (via `sqlite-vec`) is preserved by the schema — `embedding BLOB` is the same shape `sqlite-vec` expects ([Brute-force cosine over SQLite BLOBs; `sqlite-vec` reserved as phase-2 upgrade](./4_decisions.md#brute-force-cosine-over-sqlite-blobs-sqlite-vec-reserved-as-phase-2-upgrade)).

### 3.3 Write path

A single `upsert_embeddings` API takes `(source_kind, source_id, fields: Vec<(field, text)>)`. For each field:

1. Compute `content_hash = BLAKE3(text)`.
2. If a row already exists with the same `(source_kind, source_id, field, chunk_idx, model_id)` and matching `content_hash`, skip.
3. Otherwise embed and upsert.

This makes "reindex everything" idempotent and cheap when nothing has changed. Re-embedding only happens on real text changes or model changes.

---

## 4. What to Embed for Tasks

### 4.1 Per-field rather than whole-bundle

A task bundle has structured fields with different retrieval value. The design indexes them as separate rows rather than concatenating into a single document:

| Field | Source | Rationale |
|-------|--------|-----------|
| `title` | `Task.title` | High-density signal; what the task is for |
| `description` | `Task.description` | One-line or long-form task context |
| `plan` | `Task.plan` | Implementation intent; long-form |
| `execution_summary` | `Task.execution_summary` | What actually shipped |
| `acceptance` | `Task.acceptance_criteria[*]` joined | Often the most query-relevant text |

A single match in any indexed field surfaces the parent task; the result formatter rolls field-level hits up to task-level results, with the highest-scoring field shown as a snippet ([Per-field embeddings with chunked overflow, not whole-bundle concatenation](./4_decisions.md#per-field-embeddings-with-chunked-overflow-not-whole-bundle-concatenation)).

### 4.2 Chunking long fields

Long task and doc fields can exceed the active model's context. The chunker splits on paragraph boundaries with a target of 400 tokens per chunk and a 50-token overlap, capped by the model's context limit. Each chunk gets its own row with `chunk_idx = 0, 1, 2, ...`. Queries that match multiple chunks of the same field/source collapse to one result with the best-scoring chunk surfaced.

Token counting uses fastembed-rs's tokenizer for the active model — exact, not heuristic — to keep chunks below the model's actual limit.

### 4.3 Fields *not* embedded

- `task.yaml.id`, `created_at`, `updated_at`, `status`, `dependencies`, `external_refs`: identifiers and structured metadata; ordinary task search handles these separately from the embedding fields.
- `artifacts/**` blobs: out of scope phase 1. Most are binary or large generated content; embedding them is expensive and rarely useful for "find the related task" queries.

---

## 5. Hybrid Retrieval

### 5.1 FTS5 virtual table

A virtual table mirrors corpus content for lexical search:

```sql
CREATE VIRTUAL TABLE corpus_fts USING fts5(
    source_kind UNINDEXED,
    source_id UNINDEXED,
    field UNINDEXED,
    content,
    tokenize = 'porter unicode61 remove_diacritics 2'
);
```

Populated from the same per-field text as the embedding indexer. FTS5 ships with BM25 ranking built in — no implementation needed beyond the virtual table.

### 5.2 Reciprocal Rank Fusion

Both retrievers run in parallel for a query. Each returns a ranked list of `(source_id, field, chunk_idx)` candidates. RRF combines them:

```
score(c) = Σ over retrievers r of: 1 / (k + rank_r(c))
```

with `k = 60` (the published-paper default that has held up across many evaluations). The fused ranking determines the final result order. RRF is parameter-light, requires no score calibration between retrievers, and consistently beats either retriever alone in the literature.

### 5.3 Why hybrid

Three queries that motivate the choice:

- **"slow embed inference"** — semantic wins; lexical misses tasks titled "BGE latency degraded after Nomic swap."
- **"ORB-11027"** — lexical wins; semantic returns near-random because the literal token has no semantic neighborhood.
- **"file: orbit-store/src/file/task_store/layout.rs"** — lexical wins; literal path tokens dominate.

Either retriever alone has a failure mode the other doesn't. RRF resolves both at the cost of one extra SQL query per search ([Hybrid retrieval (FTS5 BM25 + cosine, fused via RRF) from day one](./4_decisions.md#hybrid-retrieval-fts5-bm25-cosine-fused-via-rrf-from-day-one)).

---

## 6. CLI and MCP Surface

### 6.1 CLI

```
orbit semantic install   [--model bge-small | minilm-l6 | nomic-v1.5] [--force]
orbit semantic uninstall [--model MODEL] [--all]
orbit search <query> [--hybrid] [--kind task|doc|friction|all] [--limit N]
                     [--workspace SELECTOR]... [--all-workspaces]
orbit search similar <task-id> [--limit N]
orbit search path <path> [--kind task|doc|friction|all] [--limit N]
orbit semantic index     [--force] [--model MODEL] [--kind tasks|docs|all]
orbit docs index         [--force] [--model MODEL]
orbit semantic stats
```

`install` is the gate that enables every other subcommand. It downloads the platform-appropriate `orbit-search-companion` binary from the published release URL and the chosen model through the companion, both into `~/.orbit/embed/`. Default model is `bge-small`; users can override per [§2.4](#24-default-model-and-install-time-model-selection). Re-running `install` with a different `--model` adds that model alongside the existing ones. Re-running `install` after an Orbit upgrade refreshes a companion whose integrity metadata is missing or stale; `--force` is the explicit override for reinstalling regardless of the existing metadata.

`uninstall` removes the companion binary and (by default) the currently active model. `--model M` removes only model M. `--all` removes the companion plus every installed model.

`orbit search` defaults to lexical matching across tasks and docs; ADR content participates through indexed design docs. `--hybrid` blends lexical scoring with cosine over the selected corpus; `orbit search similar <task-id>` embeds the target task and runs cosine-neighbor lookup against other tasks; `orbit search path <path>` performs applicability lookup over path-scoped artifacts. `orbit semantic index` rebuilds the selected corpus (`tasks` by default, or `docs` and `all` via `--kind`); `orbit docs index` is the docs-specific alias and sweeps stale doc paths. `--force` ignores `content_hash` and re-embeds everything. `stats` reports row counts, model distribution, stale-row count, and companion-install status. The retired `learning` kind is rejected.

If the companion is not installed, `orbit search similar <task-id>`, `orbit semantic index`, and `orbit docs index` exit non-zero with: `"Semantic search not enabled. Run \`orbit semantic install\` to download the inference companion."` Hybrid task and doc search are softer: they emit a warning/note and fall back to lexical results.

`--workspace` and `--all-workspaces` select the federated scope described in [§6.4](#64-cross-workspace-federated-search). They apply to the free-text form only; `similar` and `path` are single-workspace by construction.

### 6.2 MCP tools

- `orbit.search` — `(query?, hybrid?, semantic?, kind?, limit?, tag?, all?, status?, path?, workspaces?, all_workspaces?)` → ranked results with snippets.
- `orbit.semantic.install`, `orbit.semantic.uninstall`, `orbit.semantic.stats`, `orbit.semantic.index` — companion lifecycle.
- `orbit.docs.index` — docs-corpus embedding build and stale-source sweep.

`orbit.search` is read-only. Task indexing is implicit (on task mutation) or explicit (`orbit semantic index` / `orbit.semantic.index`); docs indexing is explicit (`orbit docs index` / `orbit.docs.index`).

### 6.3 Result shape

```jsonc
{
  "mode": "hybrid",
  "kind": "task",
  "results": [
    {
      "kind": "task",
      "source": "semantic",
      "id": "ORB-00042",
      "best_field": "plan",
      "snippet": "...",
      "score": 0.87,
      "score_breakdown": { "rrf": 0.87, "bm25_rank": 4, "cosine_rank": 1 }
    }
  ],
  "notes": []
}
```

The global search response exposes the score breakdown when semantic task search contributes a hit: agents can use it to distinguish a lexical-only hit from a semantic neighborhood and adapt downstream behavior. The internal `orbit-search` result also carries the active `model_id`, but the unified CLI/MCP response uses the global shape shown here.

### 6.4 Cross-workspace federated search

Vectors and FTS rows stay workspace-local ([§3](#3-vector-storage)); only the *read* federates. One query may cover several registered checkouts, and Orbit opens each one's `semantic.db`, runs the ordinary single-workspace query there, and fuses the ranked lists. See [Federate the cross-workspace read; deny it inside a managed run](./4_decisions.md#federate-the-cross-workspace-read-deny-it-inside-a-managed-run) for why the index is not centralized instead.

**Scope selector.** `WorkspaceScope` has three states: `Current` (default), `Selectors([..])`, and `AllRegistered`. `Current` takes the untouched single-workspace path, so an existing caller sees identical results and an identical JSON shape — the two federated fields are `skip_serializing_if` empty/`None`.

**Layering.** `orbit-core` may not depend on `orbit-registry`, so the fan-out splits across two crates:

| Owner | Responsibility |
|-------|----------------|
| `orbit-core` (`application/search/federated.rs`) | fan-out, fusion, attribution, notes, caps, the managed-run refusal |
| `orbit-cmd` (`workspace_catalog.rs`) | `WorkspaceCatalog` impl: selector → registered checkout, and opening a runtime for one |

`OrbitRuntime::with_workspace_catalog` attaches the implementation, mirroring `with_coordination_write_owner`. A runtime built without one still works and refuses any scope wider than its own checkout.

**Fusion is by rank, not by score.** Per-workspace hits are interleaved by their position in their own workspace's ranked list, reusing the round-robin merge that already balances kinds. Lexical BM25 scores, blended hybrid scores, and `None` (frictions) are not commensurable across workspaces, so nothing compares them — and one large workspace cannot consume the whole `limit`.

**Model compatibility.** Cosine scores only compare within one `model_id`. A workspace whose index was built under a different model has no rows the query embedder can score, so it degrades to lexical there; the response says so by name rather than emitting a ranking the caller cannot tell apart from a fused one:

```
[polaris] semantic index uses model(s) minilm-l6, not the query model bge-small;
          cosine scores are not fused for this workspace and it contributes
          lexical hits only
```

**Attribution is mandatory.** Every federated hit carries `workspace: {workspace_id, name, repo_root}`. Task IDs are globally unique and resolve through the host registry, but friction and job-run IDs are allocated per workspace, so the same ID names a different record in each. F2026-08-046 records a near-miss write to the wrong record from exactly that ambiguity in a merged result set.

**Partial failure is normal.** A registered checkout can be stale, moved, or owned by another machine. Each such workspace contributes zero hits plus a note and appears in the `workspaces` report with `hits: 0`; the query still succeeds. Per-workspace notes are prefixed `[<name>]` so every note is attributed too.

**No silent caps.** At most `MAX_FEDERATED_WORKSPACES` (16) checkouts are opened per query. Exceeding it adds a note naming both the cap and how many workspaces were dropped.

**Sandbox posture.** A federated scope is refused inside an Orbit-managed run. Rationale in the decision record; the guard lives in `global_search` so CLI, MCP, `orbit tool run`, and the HTTP adapter all reach it through one rule.

```jsonc
{
  "mode": "lexical",
  "kind": "all",
  "results": [
    { "kind": "friction", "id": "F2026-07-013", "workspace":
      { "workspace_id": "ws_orbit", "name": "orbit", "repo_root": "/…/orbit" } }
  ],
  "notes": ["[almanac] skipped: checkout for 'almanac' is gone"],
  "workspaces": [
    { "workspace_id": "ws_orbit", "name": "orbit", "hits": 4 },
    { "workspace_id": "ws_almanac", "name": "almanac", "hits": 0,
      "note": "skipped: checkout for 'almanac' is gone" }
  ]
}
```

---

## 7. Index Maintenance

### 7.1 On-mutation indexing

`task.add` and mutating `task.update` paths emit an `EmbedJob` to a bounded in-process channel after the durable write commits. A worker drains the channel, batches up to 16 jobs at a time, and runs `upsert_embeddings`. Failures log and continue — embedding is not in the critical path of task mutation. Background indexing spawns the companion with stderr suppressed so a best-effort indexing failure cannot make a successful task mutation look failed; direct semantic commands still inherit companion stderr so users see actionable failures. Users without the companion installed (`orbit semantic install` not yet run) see `OrbitError::CompanionNotInstalled` from the worker, which it logs at debug level and skips; core task operations are entirely unaffected.

### 7.2 Backfill and migration

`orbit semantic index` walks the task store and embeds anything not present (or whose `content_hash` differs). A model migration (`--model`) writes new rows under the new `model_id`; the old `model_id` rows can be deleted in a follow-up `orbit semantic prune --model OLD`.

### 7.3 Deletion

`task.delete` cascades to `DELETE FROM embeddings WHERE source_kind = 'task' AND source_id = ?`. Tombstoned tasks (in the v2 task-sync sense, see [docs/design/_archive/task-sync/](../_archive/task-sync/1_overview.md)) are not embedded.

---

## 8. Concerns & Honest Limitations

### 8.1 Two-step install and first-run download

Users who want semantic search must run two commands instead of one: install `orbit`, then run `orbit semantic install [--model X]` to download the companion (~50MB) and the chosen model (~23–140MB). The install command is the friction; the per-model download afterward is the same content cost a bundled design would have charged on first search. For users behind corporate proxies or in airgapped environments the friction multiplies — see [3_vision.md §1.2](./3_vision.md). The cost is explicit in [Companion binary installed on demand, rather than bundled in `orbit`](./4_decisions.md#companion-binary-installed-on-demand-rather-than-bundled-in-orbit); the mitigation is a clear `CompanionNotInstalled` error with the exact remediation command.

### 8.2 Subprocess overhead

The companion lives in a separate process and inference happens via stdio JSON-RPC. Cold-start latency is ~100–300ms (ORT init + model load). The background task-indexing worker reuses the subprocess across its batches, so the cost is amortized for indexing; a fresh `orbit` invocation pays the startup cost again. RPC serialization itself is sub-millisecond at phase-1 batch sizes (≤16 texts × ~512 tokens each); not a measurable contributor.

### 8.3 Default model quality is unmeasured for Orbit specifically

"BGE-small is fine" rests on published benchmarks (MTEB), not Orbit-specific recall numbers. Phase 1 deliberately does not ship an evaluation harness — building one in parallel with the feature is YAGNI before any user has hit a real recall failure. The cost is real: if BGE-small underperforms for Orbit's task corpus, we won't know until someone complains, and at that point we measure then. The `Embedder` trait + `model_id` PK column make swapping the default cheap whenever that day arrives ([3_vision.md §1.1](./3_vision.md)).

### 8.4 Brute-force scaling ceiling

Cosine over a full table scan stays sub-100ms at ~100K vectors. A substantially larger future corpus could push past that; the schema's forward compatibility with `sqlite-vec` is the planned upgrade path, but `sqlite-vec` is itself a loadable extension that may not be available in every distribution. The decision to revisit storage at phase 2 is in [Brute-force cosine over SQLite BLOBs; `sqlite-vec` reserved as phase-2 upgrade](./4_decisions.md#brute-force-cosine-over-sqlite-blobs-sqlite-vec-reserved-as-phase-2-upgrade).

### 8.5 Multilingual content

BGE-small-en is English-tuned. Tasks written primarily in other languages will have weaker semantic recall. fastembed-rs supports multilingual models (e.g. paraphrase-multilingual-MiniLM); the model knob accommodates a swap, but the default ships English-tuned and that's a documented limitation, not a hidden one.

### 8.6 Privacy posture

All embeddings stay local. Task content never leaves the workspace. This is structural — there's no remote API in the loop — but worth stating explicitly because "AI feature" commonly implies "your data is going somewhere," and Orbit's semantic search does not.

---

## 9. Historical Phase-2 Graph Corpus Proposal (Retired)

**Status: Retired by [Retire and delete Orbit's code-graph subsystem](../_archive/orbit-graph/4_decisions.md#retire-and-delete-orbits-code-graph-subsystem) / ORB-10491.** Orbit's code-graph subsystem was
deleted, so this section is retained only as design history and is not an
implementation plan. Any future code-corpus work requires a new design that
does not assume the removed graph types, indexer, or synchronization stream.

Phase 2 extends the existing `embeddings` table to a second `source_kind` covering both code symbols and design-doc sections, with ADRs joining later as a third `source_kind` once ADR vector indexing has its own accepted design. No schema migration; the phase-1 `source_kind` discriminator is the seam this section commits against.

The audience this corpus serves is the **task-creating / task-executing agent**. The five concrete use cases it enables — "find code that does X", duplicate / near-duplicate detection, task-creation grounding, "have we decided this before?", and glossary resolution — all collapse to one primitive: `orbit.search` filtered by `--kind`.

### 9.1 Corpus: knowledge-graph leaves

The corpus is **filtered leaves of the knowledge graph**. The graph is intended to represent code symbols *and* markdown sections as typed leaves, so one indexer can cover both code and design docs uniformly.

Allowlist for the first cut:

| Kind | Source |
|---|---|
| `Function`, `Method`, `Module`, `Struct`, `Enum`, `Trait` | code |
| `Section { depth }` | markdown — `docs/design/**/*.md`, glossaries, READMEs |

Excluded as low-signal pending recall evidence: `Field`, `Property`, `Constant`, `ConfigKey`, `Column`, `Macro`, `Delegate`, `Event`, `Global`, `Namespace`, `Package`, `Object`, `CompanionObject`, `SingletonClass`, `SingletonMethod`, `FunctionDeclaration`, `Record`, `Interface`, `TypeAlias`, `Impl`.

**ADR markdown (`docs/design/*/4_decisions.md`) is explicitly excluded** from the doc-section path. ADR bodies are lifecycle-owned under [.orbit/adrs/](../../../.orbit/adrs/), not docs-owned design sections. Indexing local `4_decisions.md` prose as docs would force a re-index pass if ADRs later join as `source_kind = "adr"`.

### 9.2 Schema reuse

- `source_kind = "symbol"` — the slot already reserved in [§3.1](#31-schema). Used for all graph leaves regardless of `LeafKind`.
- `source_id` = `BaseNodeFields.identity_key` from the leaf. Stable across rebuilds.
- `content_hash` = `LeafNode.source_hash`. The existing upsert gate skips unchanged leaves for free.

`orbit.search` extends `--kind` to accept multiple values (`--kind=task,symbol`). The exact encoding of `LeafKind` into the existing `field` column (or whether kind-filtering goes through a join against the graph's identity-key→kind map) is left for the implementing task — both shapes work without schema changes.

### 9.3 Embedded text per leaf

- **Code leaves** (`Function`, `Method`, `Struct`, `Enum`, `Trait`, `Module`): `LeafNode.source` as the primary input, chunked at paragraph boundaries by the existing 400-token / 50-token-overlap chunker ([§4.2](#42-chunking-long-fields)). Doc-comments and inline `//` comments are inside the span and embedded along with the body. Names and qualified names ride the FTS5 side rather than the embedding side.
- **`Section` leaves**: `LeafNode.source` (heading + body up to the next same-or-higher heading). The parent-heading path is prepended to the first chunk so the section's context survives — a body excerpt under `## Lifecycle and Audit` should still retrieve for queries about lifecycles even when the heading text isn't repeated in the body.

Per-leaf field tuning beyond this sketch is left for the implementing task; the corpus and storage are fixed, the field-level knobs aren't.

### 9.4 Indexer placement: `orbit-embed::graph_indexer`

A new module under `orbit-embed`, consistent with [Semantic-search ownership relocated to `orbit-embed`](./4_decisions.md#semantic-search-ownership-relocated-to-orbit-embed)'s "semantic ownership lives in `orbit-embed`" rule. The indexer consumes a leaf-diff stream from graph synchronization after each *clean* rebuild, batches `EmbedJob`s through the same channel pattern the task path uses ([§7.1](#71-on-mutation-indexing)), and writes to the existing `VectorStore`.

Async by design: graph rebuild commits first, embedding lags behind in a background worker. The graph implementation does not gain a dependency on `orbit-embed` — the indexer pulls the diff via a graph synchronization API. The exact diff-stream contract (push channel vs. pull-after-rebuild, `LeafDiff` shape) is deferred to the implementing task; both shapes are viable.

### 9.5 Freshness and stale-row removal

Three loops at increasing scope:

1. **Per-rebuild diff (primary).** `hash::detect_changes()` in the pipeline already produces the diff the indexer needs:
   - new `identity_key` (within the kind allowlist) → embed → INSERT
   - same key, changed `source_hash` → re-embed → UPDATE (`content_hash` gate skips no-ops)
   - key absent from the new graph → DELETE — the primary stale-row mechanism
2. **Mark-and-sweep (safety net).** After each clean rebuild, anti-join `embeddings` against the current leaf set:
   ```sql
   DELETE FROM embeddings
    WHERE source_kind = 'symbol'
      AND source_id NOT IN (SELECT identity_key FROM <current-leaves>);
   ```
   Catches anything Loop 1 missed (crashed rebuild, branch switch, partial extraction). Single SQL statement, milliseconds at workspace scale.
3. **Explicit reindex (recovery).** `orbit semantic index --kind=symbol [--force]` walks every allowlisted leaf. `--force` ignores `content_hash` — used for model migrations or after a chunker change.

**Dirty-rebuild indexing is deliberately skipped.** `ensure_fresh()` rebuilds the graph on uncommitted edits too (debounced), but the indexer only consumes diffs from *clean* rebuilds. Mid-edit re-embedding would churn for negligible recall gain — the agent's queries are "find prior work like X", not "find code I literally just typed." Cost: a freshly-written symbol isn't searchable until commit.

### 9.6 Scope boundaries

This section deliberately does not commit to:

- **Symbol → ADR back-link as a precomputed edge.** Falls out of a future vector-ranked ADR search path once ADRs are vector-indexed. Precomputing top-k matches per symbol is a phase-3 optimization, not a v1 requirement.
- **Code-aware embedding model.** CodeBERT, voyage-code, and similar outperform general-text models on code retrieval but are larger and weaker on English. v1 ships with the BGE-small default ([fastembed-rs ONNX backend over Candle, llama.cpp, or external ollama](./4_decisions.md#fastembed-rs-onnx-backend-over-candle-llamacpp-or-external-ollama)) and revisits if recall on code queries underperforms.
- **HNSW upgrade.** The graph corpus may cross the brute-force ceiling. Schema is already forward-compatible with `sqlite-vec` per [Brute-force cosine over SQLite BLOBs; `sqlite-vec` reserved as phase-2 upgrade](./4_decisions.md#brute-force-cosine-over-sqlite-blobs-sqlite-vec-reserved-as-phase-2-upgrade); the decision to switch is a separate ADR at the point of operational evidence — see [3_vision.md §1.3](./3_vision.md).
- **Free-floating file-scope comments.** Comments not attached to any leaf's source span (e.g. section dividers between two `fn`s) are not embedded. The project convention is "default to no comments" so this gap is small and low-signal.
- **Multi-workspace ADR scoping.** ADRs would flow in through the ADR store; cross-workspace ADR scoping remains a separate design question.

### 9.7 Sequencing

Phase 2 no longer depends on the removed ADR artifact v2 proposal. Implementing the doc-section indexer should still exclude every `4_decisions.md` so local narrative ADR logs do not churn the doc corpus; ADRs can join as `source_kind = "adr"` through `orbit-embed::vector` when a fresh ADR-vector indexing design exists.

---

## Task References

- [T20260510-3] — Design semantic search over task artifacts and graph (v2). The task that produced this folder.
- [T20260510-9] — Phase-1 semantic search foundation: orbit-embed + orbit-embed-companion + indexing pipeline. Accepted the foundation implementation and workspace-local semantic DB placement.
- [T20260510-26] — Make semantic companion install/update quiet and version-aware. Accepted version-aware companion replacement and quiet background indexing.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
