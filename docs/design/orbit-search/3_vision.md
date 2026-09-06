---
summary: "Semantic Search — Vision"
type: design
title: "Semantic Search — Vision"
owner: claude
last_updated: 2026-08-13
last_validated: 2026-09-05
status: Draft
feature: orbit-search
doc_role: vision
tags: ["orbit-search"]
---

# Semantic Search — Vision

This document captures the questions phase 1 deliberately does not answer, the prior work this design builds on or rejects, what is specific to Orbit's situation, and external references for readers who want to dig deeper. The questions in §1 are the most likely sources of post-phase-1 design pressure.

---

## 1. Open Questions

### 1.1 Model-selection evaluation (deferred)

The design defaults to BGE-small-en-v1.5 because it sits at the favorable end of the MTEB recall/size/latency curve for English sentence embeddings. That justification is published-paper-flavored, not Orbit-specific.

Phase 1 does **not** ship an evaluation harness. Building one in parallel with the first implementation is YAGNI before any user has hit a real recall failure on Orbit content. If complaints surface — "I searched for X and the right task didn't come back" — that's the signal to build a harness then, against real episodes rather than synthesized queries.

If and when the harness lands, the shape that came out of phase-1 design discussion was: ~50 queries paired with the task IDs they should find, hand-curated from real "I was looking for that task" episodes; recall@k computed across BGE-small (default), all-MiniLM-L6 (smaller), and nomic-embed-text-v1.5 (larger, Matryoshka-truncatable); latency and binary-size measurements on the same hardware. The `Embedder` trait + `model_id` PK column already make a default swap mechanically cheap.

The cost of deferring: phase-1 ships with a default chosen on published benchmarks alone. Model quality for the specific corpus of Orbit task content is an open empirical question.

### 1.2 Airgapped install path

The companion-binary architecture ([Companion binary installed on demand, rather than bundled in `orbit`](./4_decisions.md#companion-binary-installed-on-demand-rather-than-bundled-in-orbit)) makes airgapped install harder than a bundled design would have been: operators need to obtain *both* the platform-appropriate `orbit-search-companion` binary *and* the chosen model files, neither of which is in the main `orbit` release. Options:

- **Documented manual placement.** Operator runs `orbit semantic install` on a connected machine, then copies `~/.orbit/embed/` (companion binary + models) onto the airgapped target. Requires documenting the exact file layout. Phase-1 default.
- **`orbit semantic install --from <path>`.** Operator points the install command at a pre-staged tarball of companion + models. Removes the need to document the directory layout. Probably the right phase-2 ergonomic improvement.
- **Per-platform companion + model release tarballs.** Published as GitHub Release artifacts (`orbit-embed-companion-linux-x86_64-bge-small.tar.gz`, etc.). Operator downloads the right one and runs `orbit semantic install --from`. Combines with the previous option.

Phase 1 ships option 1 (manual placement with docs); options 2 and 3 are clean follow-ups that don't require schema or architectural changes.

### 1.3 Historical graph corpus scaling question

Retired by [Retire and delete Orbit's code-graph subsystem](../_archive/orbit-graph/4_decisions.md#retire-and-delete-orbits-code-graph-subsystem) / ORB-10491. This question is preserved as history; no
current Orbit search roadmap depends on a code graph.

A medium repository's graph holds tens of thousands to hundreds of thousands of symbols. Embedding each at 384d puts the corpus past the comfortable brute-force ceiling. Three candidate paths:

- **`sqlite-vec` loadable extension.** HNSW within SQLite. Schema-compatible with phase 1; the `embedding BLOB` column is the format `sqlite-vec` ingests. Drawback: not always available — distributions and managed environments sometimes refuse loadable SQLite extensions.
- **`hnsw_rs` or `instant-distance` in-process.** Pure-Rust HNSW. Independent of SQLite. Adds an index file alongside the DB, complicates atomic mutations.
- **Shard by `source_kind`.** Brute force stays for `task` (small); a separate index handles `symbol`. Hybrid storage.

Decision deferred to the phase-2 design. The phase-1 schema is intentionally compatible with all three.

### 1.4 Embedding-model upgrade migration

Models improve. The day BGE-medium-en-v2 ships and benchmarks better, the team will want to swap. The design supports this — the `model_id` PK column lets two model generations coexist while reindexing — but the operational story is unspecified:

- Does `orbit semantic index --model NEW` block on completion or run in the background?
- Is there a transition period where searches union both models' results, or does the new model become authoritative immediately?
- Who triggers the migration: the user, an upgrade hook, or auto-detection on `orbit upgrade`?

The schema is ready for any answer; the UX is not yet chosen.

### 1.5 Cross-task semantic links

Once embeddings exist, the cheapest possible feature is "show me tasks similar to this one" — already in scope as `orbit search similar <task-id>`. Less obvious:

- **Auto-detect duplicate task creation.** Run the new task's `purpose` through cosine before insert; if a high-similarity match exists, warn the user.
- **Auto-suggest dependencies.** A new task's content is highly similar to an in-progress task — should it depend on it?
- **Cluster the backlog.** k-means or HDBSCAN over backlog embeddings to surface theme clusters for triage.

All three are useful, all three are out of scope phase 1, and all three are downstream of the same vector layer. The design intentionally keeps them as orthogonal follow-ups rather than baking any one in.

### 1.6 Review-thread granularity

The current task index stores title, description, plan, execution summary, and acceptance as separate rows; review-thread messages are not represented in the task embedding fields. If review threads become searchable, the alternative — index each message separately or whole threads as single documents — trades authorship signal against recall when decision context is spread across replies. Which granularity is more useful for "find me the thread where we decided X" is an empirical question that wants the eval harness from §1.1 to settle.

### 1.7 Historical phase-2 graph corpus proposal

Retired by [Retire and delete Orbit's code-graph subsystem](../_archive/orbit-graph/4_decisions.md#retire-and-delete-orbits-code-graph-subsystem) / ORB-10491. The bullets below describe the removed design
and are not a live implementation plan.

Phase 2 extends the embeddings table to graph leaves — code symbols and design-doc sections, with ADRs joining once a fresh ADR-vector indexing design exists. The phase-2 design is sketched in [2_design.md §9](./2_design.md#9-phase-2-graph-corpus-designed-deferred). Highlights:

- **Corpus** is filtered leaves from the knowledge graph (allowlist: `Function`, `Method`, `Module`, `Struct`, `Enum`, `Trait`, `Section`). One indexer is intended to cover code and markdown uniformly because both can be represented as typed graph leaves.
- **Stable IDs** from `BaseNodeFields.identity_key`; **content hashing** reuses `LeafNode.source_hash`; **no schema migration** — the phase-1 `source_kind` discriminator carries the extension.
- **Indexer** is `orbit-embed::graph_indexer`, consuming a diff stream from graph synchronization after clean rebuilds. Async, mirrors the task indexer.
- **Three freshness loops** handle stale-row removal: per-rebuild diff, mark-and-sweep anti-join, explicit `--kind=symbol` reindex. Dirty rebuilds are skipped.
- **Sequencing**: gated on a separate ADR-vector indexing design so the doc-section indexer can cleanly exclude `4_decisions.md` and ADRs flow in as `source_kind = "adr"` through the same machinery.

Open questions kept for the implementing phase:

- **Code-aware vs general embedding model.** CodeBERT / voyage-code outperform general-text models on code retrieval but are larger and weaker on English. Three options: metadata only + general model, metadata only + code model, code bodies + code model. v1 of the corpus ships with the BGE-small default and revisits with eval evidence if recall underperforms.
- **Diff-stream contract.** Push channel vs. pull-after-rebuild between graph synchronization and `orbit-embed::graph_indexer`. Both shapes are viable; the design doesn't change either way.
- **`LeafKind` encoding in the `field` column** vs. a join against the graph's identity-key→kind map at filter time. No schema implications; chosen at implementation.

### 1.8 Privacy of telemetry

If the team eventually wants to share an aggregated query-quality signal (e.g. "this query had no good results — log it for the eval set"), how is that opt-in plumbed and what guarantees does it carry? Out of scope phase 1, but worth flagging as a future interaction with [docs/POSITIONING.md](../../POSITIONING.md)'s self-hosted, no-cloud-dependency stance.

---

## 2. Prior Work

### 2.1 Local sentence-embedding tooling

- **fastembed (Python and Rust).** ONNX-based, ships small models out of the box, batched inference. The Rust port is the chosen backend; the Python original is the reference for behavior.
- **sentence-transformers (Python).** The de facto Python ecosystem. Source of most of the published recall/latency benchmarks the design relies on.
- **Candle (HuggingFace, Rust).** Pure-Rust ML framework. Considered as backend; rejected for phase 1 because fastembed-rs is closer to "drop in and run" for a fixed set of embedding models. Candle becomes attractive once the design needs models not in the fastembed-rs catalog.
- **llama.cpp / llama-cpp-rs.** GGUF, runs anything, optional GPU. Considered; rejected for phase 1 as overkill for the small embedding models that do the job. The `Embedder` trait keeps it as a future swap.

### 2.2 Vector storage in SQLite

- **`sqlite-vec` (Alex Garcia).** Successor to `sqlite-vss`. Loadable extension that adds vector indexing — brute force, IVF, HNSW. Schema-compatible with Orbit's BLOB layout. The phase-2 upgrade path.
- **`sqlite-vss`.** Predecessor; deprecated in favor of `sqlite-vec`. Cited only because most older guides reference it.
- **DuckDB VSS extension.** Comparable approach in DuckDB. Out of scope; Orbit uses SQLite throughout.

### 2.3 Hybrid retrieval

- **Reciprocal Rank Fusion (Cormack, Clarke, Büttcher 2009).** The fusion algorithm. Simple, parameter-light, robust across IR benchmarks.
- **ColBERT and late-interaction models.** Higher recall than RRF in research benchmarks but require per-token storage; expensive on a 30K-vector corpus and very expensive on a 300K-vector graph corpus. Out of scope.
- **Vespa, Weaviate, Qdrant, Elasticsearch hybrid.** Production-grade hybrid retrievers in dedicated services. Cited as evidence that hybrid is industry standard, not exotic. None applicable to a single-binary local tool.

### 2.4 Tasks-on-disk semantic search elsewhere

- **GitHub Copilot Workspace.** Cloud-only, embeds code and tickets centrally. Not a precedent for the local-first constraint.
- **Linear's "duplicate detection"** on issue creation. Server-side, presumably embeddings-based. Same constraint mismatch.
- **[`gh-search`-style local fuzzy finders.](https://github.com/junegunn/fzf)** Lexical only. Solves the typo case but not the vocabulary-mismatch case that motivates this design.

The closest precedent in spirit is [Pieces.app](https://pieces.app)'s local snippet search, which uses local embedding models for personal-knowledge retrieval. Different domain, same constraint stack.

---

## 3. What May Be Distinctive

Three properties separate this design from the prior art it draws on.

### 3.1 Single-binary local-only by construction

Every published "hybrid retrieval" production system above runs as a service. Orbit's constraint inverts that: no daemon, no service, no cloud API, no auth posture to defend. The design is small enough to fit in-process precisely because the corpus is local (tasks and other workspace artifacts, not the whole web). The `Embedder` trait + brute-force cosine + FTS5 + RRF stack adds up to "hybrid retrieval" while shipping as one library crate plus an optional companion binary target rather than four services.

### 3.2 One vector store for multiple local corpora

The schema's `source_kind` discriminator is not future-proofing for its own sake; it lets tasks and docs share one workspace-local vector store while retaining corpus-specific indexing and filters. ADRs participate as design docs. The brute-force implementation stays appropriate for the current corpus size, and the `sqlite-vec` upgrade path remains available if future local corpora make that necessary. Most orbit-search-on-tasks projects assume tasks are the whole story; this one explicitly does not.

### 3.3 Failure-mode honesty in the score breakdown

Semantic task results expose `bm25_rank` and `cosine_rank` separately in their score breakdown. A consumer (especially an agent) can detect "this matched only lexically" or "this matched only semantically" and adjust confidence; lexical and doc results do not necessarily carry both ranks. Most hybrid systems hide the constituent ranks behind a fused score; surfacing both costs nothing and gives downstream tooling a real signal.

---

## 4. References

### 4.1 Orbit-internal

- [docs/design/CONVENTIONS.md](../CONVENTIONS.md) — folder layout, frontmatter, ADR template.
- [docs/design/_archive/knowledge-graph/](../_archive/knowledge-graph/) — the corpus phase 2 will index. Phase-1 schema is designed to accommodate it.
- [docs/design/_archive/task-sync/](../_archive/task-sync/1_overview.md) — relevant for whether embeddings should sync across machines (decision: no, embeddings are derivable from text and stay local). Archived; superseded by [remote-access](../remote-access/1_overview.md).
- [docs/POSITIONING.md](../../POSITIONING.md) — the self-hosted, no-cloud-dependency stance that rules out hosted embedding APIs.
- [README.md](../../../README.md) — single-binary install posture.

### 4.2 External

- **fastembed-rs** — `https://github.com/Anush008/fastembed-rs`. The chosen inference backend.
- **`sqlite-vec`** — `https://github.com/asg017/sqlite-vec`. Phase-2 vector-index upgrade path.
- **BGE embedding family** — `https://huggingface.co/BAAI`. Default model lineage.
- **Reciprocal Rank Fusion paper** — Cormack, Clarke, Büttcher, *Reciprocal Rank Fusion outperforms Condorcet and individual Rank Learning Methods* (SIGIR 2009).
- **Matryoshka Representation Learning** — `https://arxiv.org/abs/2205.13147`. The truncatable-dimension property of nomic-embed and similar models; relevant if dim-vs-quality becomes a phase-2 lever.
- **MTEB (Massive Text Embedding Benchmark)** — `https://huggingface.co/spaces/mteb/leaderboard`. The published recall/latency leaderboard the default-model choice leans on.

---

## Task References

- [T20260510-3] — Design semantic search over task artifacts and graph (v2). The task that produced this folder.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
