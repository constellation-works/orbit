---
summary: "Semantic Search — Overview"
type: design
title: "Semantic Search — Overview"
owner: claude
last_updated: 2026-08-13
last_validated: 2026-09-04
status: Draft
feature: orbit-search
doc_role: overview
tags: ["orbit-search"]
---

# Semantic Search — Overview

Semantic search is a local, offline-first retrieval layer over Orbit's task artifacts and explicitly indexed docs. Agents query it to find prior tasks by topic before adding duplicates; humans query it to recover work they remember by meaning rather than by literal substring. **The shipped feature is opt-in**; the former knowledge-graph integration proposal was retired with the graph subsystem.

This document is the entry point. [2_design.md](./2_design.md) specifies the inference backend, vector storage, embedding strategy, and hybrid-retrieval pipeline; [3_vision.md](./3_vision.md) names open questions and prior work; [4_decisions.md](./4_decisions.md) is the decision log.

---

## 1. Motivation

The task store is already growing past the point where lexical recall is sufficient. Three concrete failure modes exist today:

1. **Duplicate tasks.** Agents create new tasks for problems that have already been worked on because the historical per-domain `task search` subcommand of `orbit` (now retired in favor of `orbit search --kind task`) only matched literal substrings of titles and descriptions. A task titled "embed model latency degraded after Nomic swap" is invisible to a query for "slow inference."
2. **Lost prior work.** A human asks "didn't we have a task about token-counting heuristics?" and gets nothing because the original task used the phrase "context window estimation." The information is on disk, just not findable.
3. **Review-thread context loss.** Long-lived review threads accumulate decisions in comment bodies. Those decisions are unsearchable except by full text scan.

Lexical search via SQLite FTS5 (BM25) is part of the answer — it handles literal identifiers, error codes, and task IDs better than embeddings. But it misses the cases where the user's vocabulary doesn't match the document's. Semantic search via local embeddings handles that. They are complementary: task semantic search fuses lexical and cosine candidates with RRF, while doc hybrid search uses the docs-specific weighted blend ([Hybrid retrieval (FTS5 BM25 + cosine, fused via RRF) from day one](./4_decisions.md#hybrid-retrieval-fts5-bm25-cosine-fused-via-rrf-from-day-one)).

The constraint that shapes every other decision: **the default `orbit` install is single-binary, no-daemon, and no cloud dependency**. That rules out hosted embedding APIs and rules out an always-on inference daemon. The `orbit-search` library keeps the main `orbit` binary slim by making fastembed-rs an optional companion-only dependency; users opt into inference via `orbit semantic install` ([fastembed-rs ONNX backend over Candle, llama.cpp, or external ollama](./4_decisions.md#fastembed-rs-onnx-backend-over-candle-llamacpp-or-external-ollama), [Companion binary installed on demand, rather than bundled in `orbit`](./4_decisions.md#companion-binary-installed-on-demand-rather-than-bundled-in-orbit-1)).

---

## 2. Core Concepts

### 2.1 Embedding backend (companion-binary architecture)

The `orbit-search` crate owns the `Embedder` trait, JSON-RPC types, `SubprocessEmbedder`, vector storage, and command implementations. Its optional `orbit-search-companion` binary target depends on fastembed-rs and runs the actual inference; the main `orbit` binary uses the library without that optional feature and therefore does not link fastembed-rs.

Users opt into semantic search by running `orbit semantic install [--model bge-small | minilm-l6 | nomic-v1.5]`, which downloads the platform-appropriate companion plus the chosen model into `~/.orbit/embed/`. Default model is BGE-small-en-v1.5 (384 dim, ~30MB). The trait abstraction leaves room for a future companion backend without changing storage or retrieval. Airgapped operators have a manual-placement path described in [3_vision.md §1.2](./3_vision.md). The full backend selection rationale is in [fastembed-rs ONNX backend over Candle, llama.cpp, or external ollama](./4_decisions.md#fastembed-rs-onnx-backend-over-candle-llamacpp-or-external-ollama); the packaging decision is in [Companion binary installed on demand, rather than bundled in `orbit`](./4_decisions.md#companion-binary-installed-on-demand-rather-than-bundled-in-orbit).

### 2.2 Vector store

A new SQLite table `embeddings` is stored in the workspace-local semantic database alongside the `chunks` table and the `corpus_fts` virtual table indexing it. Each row holds `(source_kind, source_id, field, chunk_idx, content_hash, model_id, dim, embedding BLOB)`. `source_kind` currently distinguishes task and doc rows; ADRs are indexed through the docs corpus. The forward migration in [ORB-10736] removes rows from the retired native learning corpus.

The implementation uses **brute-force cosine similarity** in Rust over the BLOBs. At the current corpus scale (low thousands of artifacts × a small number of fields per source = tens of thousands of vectors at 384d), brute force is sub-millisecond per query and adds zero new dependencies. The on-disk format remains forward-compatible with `sqlite-vec` should future local corpus growth push past brute-force scaling limits ([Brute-force cosine over SQLite BLOBs; `sqlite-vec` reserved as phase-2 upgrade](./4_decisions.md#brute-force-cosine-over-sqlite-blobs-sqlite-vec-reserved-as-phase-2-upgrade)).

### 2.3 Hybrid retrieval

Queries run two retrievers in parallel: SQLite FTS5 (BM25) over the `corpus_fts` virtual table, and brute-force cosine over the `embeddings` table. The two ranked lists are fused via Reciprocal Rank Fusion (RRF, k=60) to produce the final ordering. RRF is an unweighted, parameter-light fuse that consistently outperforms either retriever alone in the published evaluation literature; it does not require either retriever's score to be calibrated to the other.

This is the single most important quality choice in the design. Pure semantic search loses on literal-identifier queries (function names, error codes, task IDs, file paths); pure lexical search loses on vocabulary-mismatch queries. RRF avoids picking one failure mode over the other ([Hybrid retrieval (FTS5 BM25 + cosine, fused via RRF) from day one](./4_decisions.md#hybrid-retrieval-fts5-bm25-cosine-fused-via-rrf-from-day-one)).

### 2.4 Per-field embeddings

A task is indexed as multiple rows, one per non-empty field: `title`, `description`, `plan`, `execution_summary`, and joined `acceptance` criteria. Search results return the best-matching field, and the result-formatting layer rolls multiple field hits on the same task into a single result with the highest-scoring field surfaced. Long fields are chunked into multiple rows with a `chunk_idx`, giving more precise results than concatenate-and-embed-once ([Per-field embeddings with chunked overflow, not whole-bundle concatenation](./4_decisions.md#per-field-embeddings-with-chunked-overflow-not-whole-bundle-concatenation)).

### 2.5 Phase boundary

The shipped index covers tasks plus explicitly indexed docs, including ADR design records. The old graph-corpus proposal was retired by [Retire and delete Orbit's code-graph subsystem](../_archive/orbit-graph/4_decisions.md#retire-and-delete-orbits-code-graph-subsystem) / ORB-10491; no current implementation or roadmap depends on `source_kind = symbol` rows.

---

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Folder layout, frontmatter, ADR template | [docs/design/CONVENTIONS.md](../CONVENTIONS.md) | — |
| Inference backend choice (fastembed-rs) | [2_design.md §2](./2_design.md), [fastembed-rs ONNX backend over Candle, llama.cpp, or external ollama](./4_decisions.md#fastembed-rs-onnx-backend-over-candle-llamacpp-or-external-ollama) | [T20260510-3] |
| Companion-binary packaging + on-demand install | [2_design.md §2.2–§2.5](./2_design.md), [Companion binary installed on demand, rather than bundled in `orbit`](./4_decisions.md#companion-binary-installed-on-demand-rather-than-bundled-in-orbit) | [T20260510-3] |
| `orbit-search` crate and `orbit-search-companion` binary placement | [2_design.md §1](./2_design.md) | [T20260510-9] |
| Stdio JSON-RPC protocol | [2_design.md §2.3](./2_design.md) | [T20260510-9] |
| `embeddings` SQLite table schema | [2_design.md §3](./2_design.md), [Brute-force cosine over SQLite BLOBs; `sqlite-vec` reserved as phase-2 upgrade](./4_decisions.md#brute-force-cosine-over-sqlite-blobs-sqlite-vec-reserved-as-phase-2-upgrade) | [T20260510-9] |
| Per-field embedding strategy | [2_design.md §4](./2_design.md), [Per-field embeddings with chunked overflow, not whole-bundle concatenation](./4_decisions.md#per-field-embeddings-with-chunked-overflow-not-whole-bundle-concatenation) | [T20260510-9] |
| FTS5 + cosine + RRF hybrid pipeline | [2_design.md §5](./2_design.md), [Hybrid retrieval (FTS5 BM25 + cosine, fused via RRF) from day one](./4_decisions.md#hybrid-retrieval-fts5-bm25-cosine-fused-via-rrf-from-day-one) | [T20260510-10] |
| `orbit semantic install/uninstall` CLI | [2_design.md §6.1](./2_design.md) | [T20260510-9] |
| `orbit search` CLI + MCP | [2_design.md §6](./2_design.md) | [T20260510-10] |
| Cross-workspace federated read + scope selector | [2_design.md §6.4](./2_design.md), [Federate the cross-workspace read; deny it inside a managed run](./4_decisions.md#federate-the-cross-workspace-read-deny-it-inside-a-managed-run) | [ORB-11027] |
| Index-on-mutation + index command | [2_design.md §7](./2_design.md) | [T20260510-9] |
| Existing task store API | [crates/orbit-store/src/repository/task/v2/](../../../crates/orbit-store/src/repository/task/v2/) | — |
| Concerns & honest limitations | [2_design.md §8](./2_design.md) | [T20260510-3] |
| ADR log | [4_decisions.md](./4_decisions.md) | [T20260510-3] |
| Open questions, prior work | [3_vision.md](./3_vision.md) | [T20260510-3] |

---

## Task References

- [T20260510-3] — Design semantic search over task artifacts and graph (v2). The task that produced this folder.
- [T20260510-9] — Phase-1 foundation: `orbit-embed` + `orbit-embed-companion` crates, indexing pipeline, install command.
- [T20260510-10] — Phase-1 retrieval: hybrid query, CLI search/related, MCP tools.
- [ORB-11027] — Cross-workspace federated search with a workspace scope selector.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
