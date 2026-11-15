---
summary: "Lexical task search using SQLite FTS5 BM25."
type: design
title: "Task Search — Overview"
owner: codex
last_updated: 2026-11-04
last_validated: 2026-11-11
status: Implemented
stage: shipped
next: "—"
feature: orbit-search
doc_role: overview
tags: ["orbit-search"]
---

# Task Search — Overview

Orbit's lexical task search is shipped. [ORB-12742] removed the semantic
stack while retaining SQLite FTS5 BM25; [ORB-11027] delivered cross-workspace
search, and [ORB-14596] added the labelled partial-match fill. No committed
implementation slice remains pending.

## 1. Motivation

Task lookup and duplicate checks need ranked, non-adjacent word matching without
an inference service. The index is disposable: task records remain authoritative,
and a rebuild restores search coverage after an import or restore.

## 2. Core Concepts

`orbit-search` owns a regenerable SQLite index of task text. Task mutations
synchronously replace the title, description, acceptance criteria, plan, and
execution summary chunks. That includes claim-backed evidence writes: a
committed claim mutation (a worker update, an evidence or settlement write, an
accepted handoff, and any deferred branch observation it lands in the same
commit) re-indexes its task from the settled row before returning, so the
recorded sidecar coverage — comment bodies and artifact paths — vouches only
for chunks the index currently holds. A replayed claim mutation commits
nothing and leaves the index untouched, and tasks whose text did not change
retain the indexed fast path with no bundle-document reads. A write that cannot
read the settled row indexes task fields only and records that coverage even when
its chunks are unchanged, so the fast path falls back to bundle reads until a
sidecar-aware write covers the task again. Paragraph-first word chunks are bounded to 256 words;
long paragraphs overlap by 32 words. SQLite triggers maintain the external-content
`corpus_fts` table over `chunks`. Each task replacement and a complete task rebuild
are transactional. Failed incidental writes leave the task authoritative and
emit a repair warning; `orbit search reindex` repairs coverage after imports or
restores and removes stale sources.

Search quotes whitespace-separated query terms individually and joins them with
FTS5 AND, preserving non-adjacent matching. BM25 chunk order rolls up to first-hit
task order. Core reads the ranking in bounded pages and applies status, tag, and
path filters as it goes, so filtered-out chunks cannot starve the result page.
It then appends bundle substring matches for unindexed tasks, comments, external
references, and artifact manifest paths. If fewer than the requested limit survive,
a multi-word query runs an FTS5 OR pass. Partial hits follow all full matches,
ordered by the number of query terms matched in the best chunk, then BM25 and
chunk id. Term counts use FTS5 itself, preserving its token and phrase semantics.
The CLI MATCH column and `orbit.search`'s `matched_by` labels identify partial
hits and their counts (for example `["partial", "terms:3/6"]`). A full page skips
the OR pass. [ORB-14790] bounded that pass to one query of at most 20 ranked
chunks per requested result; partial matches beyond that window are not offered
when filters reject the window. Frictions retain case-insensitive substring
matching, requiring adjacent words. Empty multi-word results and partial task results explain these
per-kind semantics in `notes`.
Federation interleaves per-workspace rankings and attributes each hit.

The index retains `semantic.db` for persisted-path compatibility. The first
writable open migrates earlier FTS layouts and removes the obsolete `embeddings`
and `id_allocations` tables; vacuum runs when either existed. Read-only runtimes can search an
existing index, while an unavailable index uses the bundle substring fallback.
See [upgrade guidance](../../runbooks/upgrades.md#lexical-search-migration).

The public query mode is lexical. There is no inference backend, download,
background worker, or separate executable. Friction retrieval is unchanged.

## 3. At a Glance

All rows describe landed behavior at the current integration checkout.

| Concern | Current source | Task |
|---------|----------------|------|
| Lexical index, transactional replacement/rebuild, chunking and legacy-schema migration | [lexical/](../../../crates/orbit-search/src/lexical/) | [ORB-12742] removed the semantic stack and retained lexical search |
| Ranked task candidates, bundle supplementation and partial-match labels | [search/candidates.rs](../../../crates/orbit-core/src/application/search/candidates.rs) | [ORB-14596] added partial matches; [ORB-14790] bounded their scan |
| Workspace-scoped fan-out and interleaving | [search/federated.rs](../../../crates/orbit-core/src/application/search/federated.rs) | [ORB-11027] delivered federated search |
| Reindex and unified task/friction search entry points | [search/runtime.rs](../../../crates/orbit-core/src/application/search/runtime.rs) | [ORB-12742] retained the lexical query and rebuild surfaces |
| CLI/MCP operation declaration | [governance/search.rs](../../../crates/orbit-common/src/governance/search.rs) | [ORB-15942] migrated search to the operation registry |

## Task References

- [ORB-11027] — delivered cross-workspace federated search with workspace attribution.
- [ORB-12742] — removed semantic inference and retained synchronously maintained FTS5 task search.
- [ORB-14596] — added partial task matches and accurate per-kind query notes.
- [ORB-14790] — bounded the partial-match scan without changing full-match priority.
- [ORB-15942] — derived the search CLI and tool surfaces from the operation registry.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
