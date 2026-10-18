---
title: State Compatibility — Design
owner: claude
last_updated: 2026-10-13
last_validated: 2026-10-18
status: Draft
feature: state-compatibility
doc_role: design
type: design
summary: The implemented forward-compatibility contract — how migrations declare additive, read-compatible or breaking, where the record lives, and when an older binary keeps writing, is held read-only, or is refused.
tags: [state-compatibility, migrations, upgrades]
paths: ["crates/orbit-store/src/contracts/compat.rs", "crates/orbit-store/src/workflow/layout/**", "crates/orbit-store/src/driver/sqlite/migration/**", "crates/orbit-store/src/driver/sqlite/connection.rs", "crates/orbit-cmd/src/migrate.rs"]
related_features: [state-compatibility, orbit-core]
related_artifacts: [ORB-10003, ORB-10012, ORB-12434, ORB-13631]
---

# State Compatibility — Design

Scope: what an Orbit binary does when the `.orbit/` layout marker or the
store-database schema ledger records a version newer than it supports. The
auto-migration path (a newer binary applying pending migrations on open) is
unchanged and is not restated here.

## 1 Migrations declare their own compatibility

Both registries carry a `compat: MigrationCompatibility` field per entry:

- **`Additive`** — the migration only adds state an older binary can both
  read *and keep writing through its own code paths*: new tables no existing
  row depends on, nullable or defaulted columns, indexes an older writer
  cannot violate, files it never touches. Rows the older binary writes
  afterwards stay correct for the newer one, which reads a missing value as
  its documented default.
- **`ReadCompatible`** — an older binary reads the result correctly, but its
  writes would not be: a `NOT NULL` column without a default, a constraint or
  trigger its statements could trip, a projection or journal kept in step with
  rows an older writer would not update, or a backfill an older writer would
  write back in the old shape.
- **`Breaking`** — the migration removes, renames, or reinterprets state that
  an older binary reads *or writes*. Layout v3
  (`remove-task-checkout-projections`) is the canonical example: binaries
  without it read tasks through the removed projections and recreate them
  when they write ([ORB-11994], [ORB-12078]).

The declaration is a claim about *readers and writers that do not have the
migration*, not about the SQL statements it runs. When in doubt, declare the
stricter class: the cost is a read-only open or the old refusal, while a wrong
`Additive` claim lets an old binary write state the new one misreads.

The class also decides upgrade admission (`orbit_common::fs::generation`):
each binary's compiled compatibility identity carries, per ledger, the newest
non-`Additive` migration (its writer floor) and the newest `Breaking` one (its
reader floor). Older processes keep running beside a newer one only across
`Additive` migrations; see the
[upgrades runbook](../../runbooks/upgrades.md#upgrade-admission-compatibility-generations).

[ORB-13631] audited every declaration against this writer rule. Store schema
v8, v12, v16, v18, v21, v27 and v28 and layout v2 had been declared
`Additive` while older writers could not keep them correct (backfills,
`NOT NULL` columns, triggers and projections); they are now `ReadCompatible`.
Only the `compat` field of those shipped entries changed — their versions,
names and SQL are untouched.

## 2 The compatibility record

An older binary cannot classify migrations it has never seen, so the binary
that applies them records the classification:

| State | Record location | Written when |
| --- | --- | --- |
| `.orbit/` layout | `<orbit_dir>/state/layout.compat` | after each migration advances `state/layout.version` |
| Store database | `schema_meta` row `migration.compat` | inside the same transaction that commits a migration and its ledger row |

The record (`contracts::CompatibilityRecord`) is JSON: a `format`, the
`version` it describes, every `Breaking` migration at or below that version,
and (`read_only`, an optional field format 1 readers ignore) every
`ReadCompatible` one. It is written only when a migration is applied — the up-to-date
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
5. A `ReadCompatible` entry above the supported version →
   `ForwardCompatibleOpen` read-only for the store. The layout cannot be held
   read-only (§4), so there it refuses like step 4.
6. Otherwise — every newer migration `Additive` → `ForwardCompatibleOpen`
   with `writable`: the binary keeps reading and writing.

A record without the `read_only` list predates the writer classification: a
newer store then opens read-only and a newer layout writable, as before.

Every refusal keeps the previous message shape (`… newer than the newest
version this orbit binary supports (N); …; upgrade orbit …`) with the reason
inserted, so existing operator habits and log greps still match.

## 4 Read-only is enforced, not promised

For the store database, a read-only forward-compatible open pins the writer connection
with `PRAGMA query_only=ON` before `Store::open` returns, and `Store` refuses
its write surfaces (`with_transaction*`, store-metadata writes) with a scoped
`OrbitError::Migration` naming the operation. The pragma is the guarantee;
the early refusals only make the error actionable instead of
`attempt to write a readonly database`. Reader-pool connections are already
`query_only`, so reads are unaffected.

For the layout, the pre-flight applies no migration and does not rewrite the
marker or the record. There is no single choke point for arbitrary `.orbit/`
file writes, so a layout change an older *writer* could damage refuses older
binaries whether it is declared `ReadCompatible` or `Breaking`.

## 5 Surfaces

Every entry point — CLI, MCP server, dashboard, and `ORBIT_BIN`-inherited
agent subprocesses — reaches this state through `Store::open` and the
runtime's layout pre-flight, so all of them gain the same behaviour without
their own compatibility logic.

`orbit migrate` reports it: `MigrateStatus` carries
`layout_forward_compatible` / `schema_forward_compatible`, and
`forward_compatible_only()` is true when nothing newer is breaking, and
`forward_compatible_writable()` when every newer migration is additive. The
dry-run path then reports a forward-compatible workspace as a successful inspection
instead of erroring, while a breaking-newer workspace keeps the previous
refusal and exit code. Pending listings, auto-apply on open by a newer
binary, and the apply path are otherwise unchanged.

## 6 Concerns & Honest Limitations

- **Forward compatibility starts from the first release that writes records.**
  A binary that predates [ORB-12434] still refuses any newer state, and state
  advanced only by such binaries carries no record, so it is refused too.
  This contract pays off from the next schema bump onward.
- **`Additive` is a human claim.** Nothing verifies that a migration marked
  additive is one. Records already stamped keep the class they were written
  with, so a corrected declaration protects only state advanced by a binary
  that carries it.
- **The layout half is not write-gated.** See §4: an additive-newer layout
  permits ordinary operation, so the classification carries the whole weight.
- **Audit appends require a writable, supported schema.** Native CLI commands
  and tool dispatch from a foreign-generation reader share an append path
  through a separate connection to the existing database, without migration,
  when its schema exactly matches this
  binary. The runtime's state handles remain read-only. A newer read-only store
  or unwritable database still refuses the audit write; the CLI warns without
  failing the read result. Native CLI audit records retain the command's success
  or failure outcome; tool dispatch owns its audit row and suppresses the
  command guard's duplicate emission.
- **The task registry fences binaries from before its ledger.** The global
  task registry runs this contract on its own ledger (§8). Binaries from
  before that ledger read only its `PRAGMA user_version`, so they cannot be
  held read-only: the first registry migration that is not `Additive` refuses
  them outright, as the layout does. The recovery authority runs on a ledger
  of this kind too, and the store ledger absorbed the feature ledgers (§9);
  see
  [One versioned ledger per SQLite database](./4_decisions.md#one-versioned-ledger-per-sqlite-database-with-frozen-migration-bodies).

## 7 Frozen store migration bodies

The store registry in `driver/sqlite/migration/ledger.rs` is append-only.
Each shipped entry owns its body and private shape-defining helpers; only
schema-neutral primitives in `migration/introspect.rs` are shared. Later
schema changes append an entry rather than altering what an earlier version
produces. The freeze-in-place copied the existing helper bodies without
changing SQL or operation order. The baseline tools and learning helpers now
live privately in v1. v13 and v15 still defensively re-create missing tables;
removing those paths requires the separate release-window host scan described
in [the ledger decision](./4_decisions.md#one-versioned-ledger-per-sqlite-database-with-frozen-migration-bodies).

Data backfills may use current domain code when they stamp the derivation
version, as v16/v18 do with `actor_alias_version`; each migration owns its
backfill helper, while a later entry can re-derive rows with a newer map.

The store schema golden applies entries 1..N to an empty database independently
for every N. It captures the normalized `sqlite_schema` rows (including
indexes and SQL, excluding physical root-page numbers), ordered deterministically.
The checked-in `migration/tests/store_schema.golden` stores lossless row additions
and removals between consecutive full dumps, so replaying the deltas reconstructs
all prefixes, including entries that leave the schema unchanged. `make goldens`
checks it; `make goldens UPDATE=1` regenerates it. A new entry adds a new prefix;
a change to an existing prefix needs an explanation and must not silently
re-bless a changed shipped shape. Legacy recreation and actor backfill fixtures
also run through the public store-open boundary, because an empty schema dump
cannot prove data compatibility.

## 8 The task registry ledger

`tasks/index.sqlite` is a separate SQLite file, so it has a registry of its own
(`driver/sqlite/task_registry/ledger.rs`), run by the store's ledger engine.
The engine takes a `LedgeredDatabase` naming the state component (`task
registry`), and writes the same `schema_meta` ledger rows and
`migration.compat` record into that file. Its entries follow the rules of §1
and §7: append-only, frozen bodies, and a per-prefix schema golden
(`task_registry/tests/registry_schema.golden`). Two engine options exist for
this database:

- Registry migrations run with foreign-key enforcement off. SQLite ignores
  `PRAGMA foreign_keys` inside a transaction, and the baseline's pre-v4
  rewrite rebuilds `workspace_bindings`, the parent of every other registry
  table; with enforcement on, the drop would cascade into the children.
- After each entry, in the same transaction, the engine writes the
  `user_version` fence for binaries from before the ledger: 5 while every
  applied entry is `Additive`, 7 from the first one that is not. Never 6,
  which those binaries may recover.

Registry v1, `registry_baseline`, is a frozen copy of the pre-ledger v5
setup, the `task_action_keys` table and the `task_envelope_stamps` cache,
which binaries before the ledger created on first record. A registry with no
ledger adopts it on its first writable open by running v1, which is
idempotent over every shipped shape: `user_version` up to 5, including the
path-coupled bindings before v4 and the allocator before v5, and the
recognized v6. The pre-v5 rewrites and the v6 recognition now live only in
that body; the recognition ignores the ledger's own `schema_meta` table,
which the engine creates in the same transaction, and still mirrors envelope
stamps a marked v6 may or may not carry. An unrecognized shape refuses
before anything is written. Seeding the allocator is data, not shape: every
writable open that finds no `local` allocator row seeds it after the ledger
has run.

A registry whose ledger is newer than the binary is decided from its record,
as in §3: additive-newer stays writable; read-compatible-newer opens with
`query_only` on the writer connection and no read pool, so the registry
reports itself read-only and its callers skip writes; breaking-newer is
refused. A registry opened for observation never adopts the ledger: without a
ledger it is readable at `user_version` 5 with action keys, and envelope stamp
reads then check that the table exists. Once adopted, readers rely on v1.

Upgrade admission carries the registry ledger as `task_registry` in the
compiled `CompatibilityIdentity`, compared like the store schema. An identity
or envelope recorded by a binary from before this ledger lacks it; it reads
as version 0 with no floors, a binary that knows no registry migration. That
fails closed: any registry migration with a writer or reader floor refuses
it, matching the `user_version` fence.

## 9 The absorbed feature ledgers

Automation (v1–v5), review (v1–v4) and local pull (v1) kept independent
registries in `feature_schema_meta`, each with a `feature.compat.<feature>`
record in `schema_meta`, applied the first time a feature store opened. Store
entry v49, `absorb_feature_ledgers`, runs each registry to completion inside
the store migration's transaction, through the same feature engine
(`migration/feature.rs::apply_feature_registry`) on the migration's
connection. The feature rows and records come out exactly as a feature open
wrote them, and a feature an earlier open advanced resumes where it stopped.

The registries now live, frozen, in `migration/feature_ledgers/`, with private
copies of the automation v4 and v5 member repairs. Nothing appends to them, and
`FeatureMigration` is private to the migration module. A later change to an
automation, review or local-pull table is a store ledger entry. Feature stores
no longer initialize anything when they open, and readers no longer check
whether `local_pull_admissions` exists, because every store this binary opens
writable is at v49 or newer.

v49 is `Additive`. A binary from before it opens the store writable, and its
own feature open finds each feature at the version it supports and applies
nothing. A binary that lacks a feature migration still decides from that
feature's record: data-only and additive migrations above it keep it running,
and a breaking one refuses, naming it. Upgrade admission keeps reporting the
frozen feature versions and breaking floors in `CompatibilityIdentity`,
because those older binaries still compare them.

## Task References

- [ORB-10003] versioned schema ledger; [ORB-10012] layout registry and
  `orbit migrate`.
- [ORB-11994] / [ORB-12078] removed the task checkout projections, the
  worked example of a breaking layout migration.
- [ORB-12434] added the compatibility declaration, records, decision, and
  read-only enforcement.
- [ORB-13631] extended `Additive` to cover older writers, added
  `ReadCompatible`, and made the classification the basis of upgrade
  admission.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
