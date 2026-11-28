---
title: Task Search — Design
owner: codex
created_date: 2026-11-28
updated_date: 2026-11-28
status: Draft
feature: orbit-search
doc_role: design
type: design
summary: "Index of the task-search mechanisms: the regenerable FTS5 index, its synchronous maintenance, ranked candidates and federated fan-out"
tags: [orbit-search]
---

# Task Search — Design

The task-search mechanisms are described in [1_overview.md](./1_overview.md), which was
written before the STD-06@5 scaffold existed. This doc names where each one lives and
records the limits. Decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## 1. Index and maintenance

The regenerable SQLite FTS5 index, the chunks each task mutation replaces synchronously,
claim-backed evidence writes and rebuilds are
[1_overview.md §2](./1_overview.md#2-core-concepts).

## 2. Query surfaces

Ranked candidates, partial-match labels, workspace fan-out and the CLI/MCP declaration are
mapped to their sources in [1_overview.md §3](./1_overview.md#3-at-a-glance).

## 3. Concerns & Honest Limitations

- Task search is lexical. SQLite FTS5 BM25 ranks indexed task chunks containing all query
  terms; when a multi-word query has fewer than the requested full matches, an any-term
  FTS5 pass can add partial matches after them.
- A literal substring supplement follows the BM25-ranked hits. It can match inside a token
  and in comments, external references, or artifact manifest paths. It uses case-insensitive
  Unicode matching and preserves the query's raw whitespace; tasks without indexed sidecar
  fields use the same substring semantics through the bundle matcher.
- There is no semantic inference backend. Tasks are found through token, partial-term, or
  literal substring matches; search does not infer synonyms.
- The index is disposable: after an import or restore it covers only what a rebuild
  re-indexes.

## Task References

- None cited here; the cited sections carry their own task references.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
