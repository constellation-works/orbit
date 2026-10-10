---
title: State Compatibility — Overview
owner: claude
last_updated: 2026-09-13
last_validated: 2026-10-06
status: Draft
feature: state-compatibility
doc_role: overview
type: design
summary: How an Orbit binary decides whether it may open workspace state written by a newer Orbit, instead of refusing every command on a version number.
tags: [state-compatibility, migrations, upgrades]
paths: ["crates/orbit-store/src/contracts/compat.rs", "crates/orbit-store/src/workflow/layout/**", "crates/orbit-store/src/driver/sqlite/migration/**", "crates/orbit-cmd/src/migrate.rs"]
related_features: [state-compatibility, orbit-core]
related_artifacts: [ORB-10003, ORB-10012, ORB-11994, ORB-12078, ORB-12434]
---

# State Compatibility — Overview

Orbit versions two pieces of durable workspace state: the `.orbit/` layout
(a marker file plus a migration registry) and the store database schema (the
`schema_meta` ledger). Both auto-apply pending migrations when a workspace
opens. This feature covers the other direction — an **older binary meeting
newer state** — and defines what it may do with it.

## 1 Motivation

Until [ORB-12434] any state version above the running binary's supported
version failed the open, so every command failed, including write-free ones:

```
{"code":"migration_failed","error":"schema migration failed: workspace '…/.orbit'
 has .orbit layout version 3, newer than the newest version this orbit binary
 supports (2); upgrade orbit to open this workspace"}
```

A host rarely runs one Orbit. A stale `make install` copy on `PATH`, a
Homebrew install, an MCP server pinned to an older release, a pipeline worker
with an inherited `ORBIT_BIN`, and a deploy script probing run history before
it swaps the binary all coexist. One schema bump turned every one of them
into a flag day, and each incident was worked around out of band — copying
binaries, special-casing error text in deploy scripts (2026-08-10:
F2026-08-063/064; 2026-09-13: `update-orbit.sh` on the owner host).

The version number alone cannot answer "is this safe to read?" — a binary
knows nothing about migrations that shipped after it. So the newer binary
answers in advance, and the older binary reads that answer.

## 2 Core Concepts

- **Migration compatibility.** Each migration is `Additive`, `ReadCompatible`,
  or `Breaking`. `Additive` changes let older binaries keep reading and
  writing safely; `ReadCompatible` changes let them read but require writes
  to be blocked; `Breaking` changes require older binaries to refuse.
- **Compatibility record.** When a binary applies a migration it records the
  ledger version and the known `Breaking` and `ReadCompatible` migrations:
  `state/layout.compat` for the layout and the `migration.compat` row in
  `schema_meta` for the database.
- **Forward-compatible open.** With a current, readable record and no newer
  breaking migration, an older binary may open newer state. It may keep
  writing only when all newer migrations are `Additive`. For a newer store
  schema, SQLite can enforce read-only access when a `ReadCompatible`
  migration is present. Workspace-layout writes cannot be gated this way, so
  a newer layout with a `ReadCompatible` migration is refused.
- **Scoped refusal.** A breaking migration the binary lacks, a newer
  read-compatible layout migration, or a missing, stale, unknown-format, or
  unreadable record refuses the open. Store-schema records with
  `ReadCompatible` migrations remain readable in read-only mode.

## 3 At a Glance

| Concern | File | Task |
| --- | --- | --- |
| Compatibility types and the decision | `crates/orbit-store/src/contracts/compat.rs` | [ORB-12434] |
| Layout registry, marker, and `layout.compat` | `crates/orbit-store/src/workflow/layout/registry.rs`, `crates/orbit-store/src/workflow/layout/marker.rs` | [ORB-10012], [ORB-12434] |
| Schema ledger and the `migration.compat` row | `crates/orbit-store/src/driver/sqlite/migration/ledger.rs` | [ORB-10003], [ORB-12434] |
| Read-only enforcement on the store handle | `crates/orbit-store/src/driver/sqlite/connection.rs` | [ORB-12434] |
| Operator reporting (`orbit migrate`) | `crates/orbit-cmd/src/migrate.rs`, `crates/orbit-cli/src/command/migrate.rs` | [ORB-10012], [ORB-12434] |

## Task References

- [ORB-10003] added the versioned SQLite schema ledger.
- [ORB-10012] added the workspace-layout registry and `orbit migrate`.
- [ORB-11994] and [ORB-12078] removed the task checkout projections that
  older binaries recreate, the worked example of a breaking layout change.
- [ORB-12434] replaced the version-number refusal with this contract.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
