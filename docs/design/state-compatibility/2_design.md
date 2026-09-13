---
title: State Compatibility — Design
owner: claude
last_updated: 2026-09-13
last_validated: 2026-09-13
status: Draft
feature: state-compatibility
doc_role: design
type: design
summary: The implemented forward-compatibility contract — how migrations declare additive vs breaking, where the record lives, and how an older binary is held read-only.
tags: [state-compatibility, migrations, upgrades]
paths: ["crates/orbit-store/src/contracts/compat.rs", "crates/orbit-store/src/workflow/layout/**", "crates/orbit-store/src/driver/sqlite/migration/**", "crates/orbit-store/src/driver/sqlite/connection.rs", "crates/orbit-cmd/src/migrate.rs"]
related_features: [state-compatibility, orbit-core]
related_artifacts: [ORB-10003, ORB-10012, ORB-12434]
---

# State Compatibility — Design

Scope: what an Orbit binary does when the `.orbit/` layout marker or the
store-database schema ledger records a version newer than it supports. The
auto-migration path (a newer binary applying pending migrations on open) is
unchanged and is not restated here.

## 1 Migrations declare their own compatibility

Both registries carry a `compat: MigrationCompatibility` field per entry:

- **`Additive`** — the migration only adds state (tables, columns, indexes,
  files), or rewrites state into a shape older binaries already understand.
  A binary without the migration reads the result correctly and ignores what
  it does not know.
- **`Breaking`** — the migration removes, renames, or reinterprets state that
  an older binary reads *or writes*. Layout v3
  (`remove-task-checkout-projections`) is the canonical example: binaries
  without it read tasks through the removed projections and recreate them
  when they write ([ORB-11994], [ORB-12078]).

The declaration is a claim about *readers and writers that do not have the
migration*, not about the SQL statements it runs. When in doubt, declare
`Breaking`: the cost is the old refusal, while a wrong `Additive` claim hands
an old binary state it misreads.

## 2 The compatibility record

An older binary cannot classify migrations it has never seen, so the binary
that applies them records the classification:

| State | Record location | Written when |
| --- | --- | --- |
| `.orbit/` layout | `<orbit_dir>/state/layout.compat` | after each migration advances `state/layout.version` |
| Store database | `schema_meta` row `migration.compat` | inside the same transaction that commits a migration and its ledger row |

The record (`contracts::CompatibilityRecord`) is JSON: a `format`, the
`version` it describes, and every `Breaking` migration at or below that
version. It is written only when a migration is applied — the up-to-date
fast path stays free of writes, and a record is only ever needed once some
binary has advanced the state past another.

The layout record is a separate file rather than extra fields in
`layout.version` because shipped binaries parse that whole marker as one
integer; a marker they cannot parse would fail worse than the refusal this
contract replaces.

## 3 The decision

`contracts::compat::evaluate_newer_state` runs only when the recorded version
exceeds the binary's supported version:

1. No record → refuse (state written before this contract, or by a binary
   that never applied a migration to it).
2. `format` above the one this binary knows → refuse.
3. `record.version` below the recorded state version → refuse: the
   migrations in between are unclassified (a crash between the marker write
   and the record write leaves exactly this, which is why the order is marker
   first).
4. A `Breaking` entry above the supported version → refuse, naming the
   **first** such entry, so the diagnostic names a migration the operator can
   look up rather than the newest version number.
5. Otherwise → `ForwardCompatibleOpen`: read-only.

Every refusal keeps the previous message shape (`… newer than the newest
version this orbit binary supports (N); …; upgrade orbit …`) with the reason
inserted, so existing operator habits and log greps still match.

## 4 Read-only is enforced, not promised

For the store database, a forward-compatible open pins the writer connection
with `PRAGMA query_only=ON` before `Store::open` returns, and `Store` refuses
its write surfaces (`with_transaction*`, store-metadata writes) with a scoped
`OrbitError::Migration` naming the operation. The pragma is the guarantee;
the early refusals only make the error actionable instead of
`attempt to write a readonly database`. Reader-pool connections are already
`query_only`, so reads are unaffected.

For the layout, the pre-flight applies no migration and does not rewrite the
marker or the record. There is no single choke point for arbitrary `.orbit/`
file writes, so the layout guarantee is the declaration itself: a layout
change that an older *writer* could damage must be declared `Breaking`, which
returns that release to the old refusal.

## 5 Surfaces

Every entry point — CLI, MCP server, dashboard, and `ORBIT_BIN`-inherited
agent subprocesses — reaches this state through `Store::open` and the
runtime's layout pre-flight, so all of them gain the same behaviour without
their own compatibility logic.

`orbit migrate` reports it: `MigrateStatus` carries
`layout_forward_compatible` / `schema_forward_compatible`, and
`forward_compatible_only()` is true when everything newer is additive. The
dry-run path then reports a read-only workspace as a successful inspection
instead of erroring, while a breaking-newer workspace keeps the previous
refusal and exit code. Pending listings, auto-apply on open by a newer
binary, and the apply path are otherwise unchanged.

## 6 Concerns & Honest Limitations

- **Forward compatibility starts from the first release that writes records.**
  A binary that predates [ORB-12434] still refuses any newer state, and state
  advanced only by such binaries carries no record, so it is refused too.
  This contract pays off from the next schema bump onward.
- **`Additive` is a human claim.** Nothing verifies that a migration marked
  additive is one. The registries are append-only, so a mistaken marker
  cannot be corrected in place for state already stamped — only a later
  breaking migration raises the floor again.
- **The layout half is not write-gated.** See §4: an additive-newer layout
  permits ordinary operation, so the classification carries the whole weight.
- **Audit rows are not written.** A read-only open still tries to record its
  audit event; that write is refused, and the CLI reports it as a warning
  without failing the command. Read-only commands therefore succeed but leave
  no audit trail on a newer store.
- **Task-registry compatibility is separate.** The global task registry keeps
  its own `PRAGMA user_version` reader floor (see
  [upgrades runbook](../../runbooks/upgrades.md)); this contract does not
  change it.

## Task References

- [ORB-10003] versioned schema ledger; [ORB-10012] layout registry and
  `orbit migrate`.
- [ORB-11994] / [ORB-12078] removed the task checkout projections, the
  worked example of a breaking layout migration.
- [ORB-12434] added the compatibility declaration, records, decision, and
  read-only enforcement.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
