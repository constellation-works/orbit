---
summary: "Lexical task search using SQLite FTS5 BM25."
type: design
title: "Task Search — Overview"
owner: codex
last_updated: 2026-09-21
status: Accepted
feature: orbit-search
doc_role: overview
tags: ["orbit-search"]
---

# Task search

`orbit-search` owns a regenerable SQLite index of task text. Task mutations
synchronously replace the title, description, acceptance criteria, plan, and
execution summary chunks. Paragraph-first word chunks are bounded to 256 words;
long paragraphs overlap by 32 words. SQLite triggers maintain the external-content
`corpus_fts` table over `chunks`. Each task replacement and a complete task rebuild
are transactional. Failed incidental writes leave the task authoritative and
emit a repair warning; `orbit search reindex` repairs coverage after imports or
restores and removes stale sources.

Search quotes whitespace-separated query terms individually and joins them with
FTS5 AND, preserving non-adjacent matching. BM25 chunk order rolls up to first-hit
task order. Core appends bundle substring matches for unindexed tasks, comments,
external references, and artifact manifest paths, then applies existing filters.
Federation interleaves per-workspace rankings and attributes each hit.

The index retains `semantic.db` for persisted-path compatibility. The first
writable open migrates earlier FTS layouts and removes obsolete vector tables,
then vacuums. Read-only/unavailable storage retains the bundle fallback.
See [upgrade guidance](../../runbooks/upgrades.md#lexical-search-migration).

The public query mode is lexical. There is no inference backend, download,
background worker, or separate executable. Friction retrieval is unchanged.
