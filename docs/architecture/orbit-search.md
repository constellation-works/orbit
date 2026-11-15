---
type: context
summary: Architecture map for orbit-search: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-search]
---

# orbit-search

## What it owns

Lexical retrieval: SQLite chunk + FTS5 schema, chunking, task-field extraction, BM25 ranking. Index file keeps `semantic.db` name.

## Main modules and types

`lexical` (`LexicalIndex` in `crates/orbit-search/src/lib.rs`). See ``crates/orbit-search/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-types`.
- Depended on by: `orbit-core`, `orbit-runtime`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-search/src/lib.rs`.

Design intent: [docs/design/orbit-search/](../../docs/design/orbit-search/) — this page describes the code as it is; the design folder holds intent.
