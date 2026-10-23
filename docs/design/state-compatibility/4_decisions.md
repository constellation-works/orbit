---
title: State Compatibility — Decisions
owner: claude
last_updated: 2026-10-12
last_validated: 2026-10-12
status: Draft
feature: state-compatibility
doc_role: decisions
type: design
summary: Why forward compatibility is declared by the writing binary rather than inferred from a version number, why read-compatible store opens are enforced read-only by SQLite itself, when a compatibility path may be deleted, and why every SQLite schema change goes through one frozen, versioned ledger per database.
tags: [state-compatibility, migrations, upgrades]
paths: ["crates/orbit-store/src/contracts/compat.rs", "crates/orbit-store/src/driver/sqlite/connection.rs", "crates/orbit-store/src/workflow/layout/marker.rs", "crates/orbit-store/src/driver/sqlite/migration/**", "crates/orbit-store/src/driver/sqlite/task_registry/schema.rs"]
related_features: [state-compatibility]
related_artifacts: [ORB-12434, ORB-15732, ORB-15729]
---

# State Compatibility — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

## Compatibility is declared by the binary that migrates, never inferred from the version number

**Recorded:** 2026-09 · [ORB-12434]
**Code anchors:** `crates/orbit-store/src/contracts/compat.rs::evaluate_newer_state`, `crates/orbit-store/src/workflow/layout/marker.rs::write_compat_record`, `crates/orbit-store/src/driver/sqlite/migration/ledger.rs::write_compat_record`

### Context

Both state ledgers refused any recorded version above the running binary's
supported version. That is the only safe rule available to a binary reasoning
from a number alone: it cannot know whether version N+1 added a column or
deleted a table. The consequence was that every schema or layout bump broke
every stale binary on the host for *every* command, and the workarounds were
always out of band (copying binaries, matching error text in deploy scripts).

The missing information exists — but only in the binary that shipped the
migration, which has already run by the time an older binary shows up.

### Decision

Each migration entry declares `Additive`, `ReadCompatible`, or `Breaking`,
and the binary that applies a migration writes that classification into the
state beside the version it stamps (`state/layout.compat`; the
`migration.compat` row in `schema_meta`). An older binary refuses when a
breaking migration sits above its own supported version — and then names that
migration. A newer store with a `ReadCompatible` migration opens read-only;
the workspace layout cannot enforce read-only writes, so it refuses that
migration. Where the record carries writer-compatibility data, additive-only
newer state remains readable and writable. Older store records without that
data remain read-only. A missing, stale, unknown-format, or unreadable record
refuses exactly as before, so the contract fails closed in every case it
cannot evaluate.

`Additive` is a claim about binaries that *lack* the migration: they must
still read the state correctly, and — for the layout, which has no single
write choke point — write it safely. Anything that removes, renames, or
reinterprets state older binaries touch is `Breaking`. Declare `Breaking`
when in doubt.

### Consequences

- A schema bump is no longer a flag day: an old `orbit task list`,
  `orbit run history`, or `orbit search` keeps working against an additive-newer
  workspace, and can read a newer store with a `ReadCompatible` migration
  without writing it.
- A refusal that remains is actionable — it names the first incompatible
  migration the binary lacks instead of only giving a version number.
- Cost: the classification is a hand-written claim that nothing verifies, and
  the registries are append-only, so a migration mismarked `Additive` cannot
  be withdrawn from state already stamped. A later `ReadCompatible` migration
  makes newer stores read-only; a later `Breaking` migration raises the reader
  floor. A wrong marker is strictly worse than the flag day it avoids, which
  is why "when in doubt, `Breaking`" is part of the rule and not advice.
- Cost: nothing is gained for binaries that predate the contract; forward
  compatibility only begins with the first release that writes records.

## A read-compatible store open is pinned read-only in SQLite, not by convention

**Recorded:** 2026-09 · [ORB-12434]
**Code anchors:** `crates/orbit-store/src/driver/sqlite/connection.rs::Store::open`, `crates/orbit-store/src/driver/sqlite/connection.rs::Store::refuse_forward_compatible_write`

### Context

"An older binary must never rewrite a newer store" cannot be delivered by
auditing call sites: `Store` hands out its writer connection
(`Store::connection`), feature crates run their own SQL inside
`StoreTx::connection`, and any future write path would have to remember the
rule. The projection-symlink incident ([ORB-11994], [ORB-12078]) is the
standing reminder that old writers do reach state they should not.

### Decision

When the schema ledger reports a newer store that is not write-safe for this
binary, `Store::open` issues `PRAGMA query_only=ON` on the writer connection
before the handle exists. SQLite then rejects every write through every path
that connection reaches. Additive-only newer stores remain writable. The
store's own write entry points additionally refuse early with a scoped
`OrbitError::Migration` that names the attempted operation, the supported
version, and the recorded version — a diagnostic layer over the guarantee,
not the guarantee.

### Consequences

- The invariant holds for code that has not been written yet, including
  feature-crate SQL that bypasses `Store`'s typed methods.
- A read-only open of a newer store is provably byte-preserving, which is
  what the regression test asserts.
- Cost: the failure for an unconverted write path is a SQLite readonly error
  rather than Orbit's scoped message, so any new write surface that wants the
  actionable diagnostic must ask for it. The alternative — trusting call
  sites to check a flag — would have produced better messages and a weaker
  guarantee.

## Compatibility paths expire on a 30-day, release-gated window

**Recorded:** 2026-10-11 · [ORB-15732]

### Context

A compatibility path is code kept only so an older shape keeps working: a
serde alias, a legacy reader of a persisted record, a re-export of a retired
name, a hidden or alias CLI command or flag, a config key alias, or a
deprecated MCP tool field or HTTP route. Without a rule for when one may go,
each cleanup sweep re-decides it, and nothing answers whether anyone still
depends on the old shape. Followers upgrade in lockstep with the owner host,
and the owner is the only external consumer, so release history and consumers
can be checked directly.

The alternative was open-ended support: keep each path until someone removes
it by hand. That needs no age, release, or consumer check, but every sweep
then re-argues each path with no rule to apply.

### Decision

A compatibility path may be deleted once all four conditions hold:

1. The commit that made the path compatibility-only is at least 30 days old.
2. The oldest release tag containing that commit is at or below the running
   version of the owner host.
3. For a CLI, config, MCP, or HTTP surface: no consumer exists outside the
   definition and its tests in this repository, the dashboard assets, the
   workspace's auto-task definitions, or the sibling Constellation checkouts.
4. For a persisted-format reader: either a migration rewrites the remaining
   old-shape records on open, or a read-only scan of every host holding the
   state finds none. At the time of this decision those hosts are dk-server-2,
   dk-server-1, and the Mac.

The rule covers every compatibility path in Orbit's CLI, config, MCP, HTTP,
and persisted state, including one added later; its 30-day window starts at
the commit that makes it compatibility-only. The plugin protocol is excluded
and is not governed by this rule. The `code-health-sweep` auto-task's Pass C
applies this rule.

### Consequences

- A path that meets all four conditions needs no further owner decision to
  remove.
- A path inside its 30-day window, or whose oldest release is above the owner
  host's version, stays even when nothing consumes it.
- Cost: conditions 3 and 4 are checks by search and scan, not guarantees. A
  consumer outside the listed places, or a host missing from the scan, goes
  unseen, and a deletion made on a clean result can still break it.
- Cost: an unused path is kept for at least 30 days after it becomes
  compatibility-only, and until the owner host runs a release containing that
  commit.

## One versioned ledger per SQLite database, with frozen migration bodies

**Recorded:** 2026-10-12 · [ORB-15729]
**Code anchors:** `crates/orbit-store/src/driver/sqlite/migration/ledger.rs::MIGRATIONS`, `crates/orbit-store/src/driver/sqlite/migration/baseline.rs::apply_baseline_schema`, `crates/orbit-store/src/driver/sqlite/migration/audit_events.rs::ensure_audit_events_schema_v15`, `crates/orbit-store/src/driver/sqlite/task_registry/ledger.rs::MIGRATIONS`, `crates/orbit-store/src/driver/sqlite/task_registry/baseline.rs::apply_registry_baseline`, `crates/orbit-store/src/driver/sqlite/migration/feature.rs::FeatureMigration`

### Context

Four mechanisms change the schemas of the SQLite databases in `orbit-store`,
and only the first can tell an older binary what it may do with the result:

1. **The store ledger** (`ledger::MIGRATIONS`, v1–v45) carries the
   compatibility record from the decisions above. At decision time its
   bodies were not frozen: the immutable v1 baseline called the shared
   `ensure_audit_events_schema`, as did nine later entries (v15–v18, v24,
   v25, v30–v32). v15 also re-ran the whole v7 body, v13 re-ran
   `ensure_task_reservations_schema`, and v10 shared `ensure_invocation_schema_v1`
   with the baseline. Editing a helper changed every shipped caller, and
   no check guarded the schema each prefix produced. The freeze-in-place
   now gives each version private copies and checks every prefix with a
   schema golden. v13 and v15 still re-create earlier shape when a ledger
   skipped the baseline; deleting those paths remains a separate change.
2. **The task registry** (`tasks/index.sqlite`, a separate file) has no
   named migrations. `PRAGMA user_version = 5` is a reader floor.
   `apply_schema` re-runs idempotently below 5, and additive setup
   (`ensure_action_keys`) runs on any writable open that lacks it. A recovery
   path also accepts the shipped PR1509 v6 by comparing the whole schema
   against an in-memory reference. A registry change must therefore be either
   invisible to v5 readers or a new number that every older binary refuses
   outright. The registry now runs on its own ledger, adopted through its v1
   baseline.
3. **Tables created on first use**: `task_envelope_stamps` in the registry.
   Whether it exists varies from registry to registry, so the v6 recognition
   mirrors it conditionally, and a read-only registry never gets it. Registry
   v1 now creates it; only a pre-ledger registry opened read-only may lack it.
4. **Feature ledgers** (`FeatureMigration` in `feature_schema_meta`):
   automation (v1–v5), review (v1–v4) and local pull (v1). They are applied
   lazily when a feature store first opens, are numbered independently, and
   have their own record keys and upgrade-admission floors. They can only say
   `Additive` or `Breaking`, with no read-only hold. Readers also have to
   check whether a table exists yet (`job_run_store/pull.rs::admissions_table_exists`).

The alternatives considered:

- Freeze bodies but keep the per-feature ledgers and the registry's
  `user_version`. That fixes the mutable-body hazard but keeps four answers
  to one question.
- Use a single database-wide counter. That is PostgreSQL's `catversion`
  stance, which
  [the first decision](#compatibility-is-declared-by-the-binary-that-migrates-never-inferred-from-the-version-number)
  already rejected for the store, because a bare number cannot say what an
  older binary may do.

### Decision

There is one schema-evolution mechanism: the store ledger engine in
`migration/ledger.rs`. It is an append-only registry of
`Migration { version, name, compat, apply }` entries. Each entry commits in one
`BEGIN IMMEDIATE` transaction with its `schema_meta` ledger row and its
`migration.compat` record. The rules, for any Orbit SQLite database whose
contents are authority rather than a rebuildable cache:

1. **Every schema change is a ledger entry.** No `CREATE TABLE`,
   `ALTER TABLE` or `CREATE INDEX` runs outside a migration body of that
   database's ledger: not on first use, not on every open, not in a feature
   module. The ledger cannot describe a table that only some databases have.
2. **One ledger per database file, not per feature.** The store database has
   one registry. The task registry is a separate file, so it gets a registry
   of its own, run by the same engine with the same `schema_meta` row and
   record, because a ledger cannot version a file it does not live in.
   Feature modules still own their SQL, but they append it to their
   database's registry.
3. **Shipped bodies are frozen.** A migration body may call only code private
   to that version and the schema-neutral primitives in
   `migration/introspect.rs` (`table_exists`, `add_column_if_missing`, …). It
   may not call a shape-defining helper that another version also calls. A
   later shape needs a later entry. A data backfill may call current domain
   code only if it stamps the derivation version it applied, so a later entry
   can re-derive the rows. v16/v18 and `actor_alias_version` are the model.
4. **Frozenness is checked, not just declared.** A per-version schema golden
   is the `sqlite_schema` dump after applying entries 1..N to an empty
   database, for every N. It fails when an edit changes what a shipped entry
   produces.
5. **A migration assumes its predecessors ran.** It does not re-create
   earlier shape just in case. A fixture that records a ledger version without
   running the entries below it is a broken fixture.

How each existing mechanism moves onto the ledger:

- **Store ledger: freeze in place, without changing behaviour.**
  - Every call to a shared helper in a shipped body becomes a call to a
    private copy of the helper, byte-identical to today's version, so no
    database migrates differently. The shared helpers are then deleted.
  - Removing the defensive re-creation in v13 and v15 is a separate step,
    because it changes what those entries do to a database whose ledger
    skipped the baseline. It is a compatibility-path deletion under the
    [30-day window](#compatibility-paths-expire-on-a-30-day-release-gated-window)
    and needs condition 4: a scan of every host that finds no such database.
- **Task registry: adopt its own ledger.**
  - The first entry, `registry_baseline`, is a frozen copy of today's v5
    `apply_schema`, `ensure_action_keys` and `ensure_envelope_stamps`.
  - A registry at `user_version` ≤ 5, or a recognized v6, adopts the ledger
    by running that entry, which is idempotent over every shipped shape, and
    recording it. This is how store v1 adopted pre-ledger databases. The
    pre-v5 rewrites and the v6 recognition remain only as that adoption path
    until the 30-day window removes them.
  - After adoption, `user_version` is only a fence for pre-ledger binaries,
    which read nothing else. It stays 5 while every registry entry is
    `Additive`. The first non-`Additive` entry sets it to 7, so pre-ledger
    binaries refuse. It never uses 6, which they may recover.
  - Ledger-aware binaries decide from the record, as the store does:
    - additive-newer: writable;
    - read-compatible-newer: read-only, with `query_only` on the writer as in
      `Store::open`;
    - breaking-newer: refused.
  - A registry with no ledger, at `user_version` 5 with action keys, stays
    readable when opened read-only, as it is today.
- **Tables created on first use.** Registry v1 creates
  `task_envelope_stamps`. Its readers stop checking whether the table exists,
  and the v6 recognition loses its conditional mirror once adoption is its
  only caller. No other first-use table exists in `orbit-store`: an `rg` for
  `CREATE TABLE`, `ALTER TABLE` and `CREATE INDEX` outside `migration/` finds
  only the registry and the three feature registries.
- **Feature ledgers: absorb them, then freeze them.**
  - One store entry, `absorb_feature_ledgers`, runs each feature registry to
    its final version (automation v5, review v4, local pull v1). It does this
    through the existing feature path inside the store migration, so the
    `feature_schema_meta` rows and `feature.compat.<feature>` records come out
    exactly as a feature open would have written them.
  - After that entry, the three feature registries are frozen.
    `FeatureMigration` stops being public API, nothing is appended to the
    registries, and the next change to an automation, review or local-pull
    table is a store ledger entry. The lazy `initialize` calls and the
    table-existence checks go, because the store ledger guarantees the tables.
  - Upgrade admission keeps reporting the frozen feature versions and floors,
    because older binaries still compare them.

**Forward compatibility.** This plan stamps no state that an existing binary
would refuse or misread:

- The store freeze changes no SQL that any database runs.
- `absorb_feature_ledgers` is `Additive`:
  - A current binary opens the newer store writable. Its own feature open
    then finds every feature already at the version it supports and applies
    nothing.
  - An older binary that lacks a feature migration decides from that
    feature's record, exactly as it does today.
- Registry adoption adds one `schema_meta` table and keeps
  `user_version` at 5. A pre-ledger binary checks only `user_version` and the
  action-keys column, so it keeps reading and writing that registry.
- The first non-`Additive` registry entry fences pre-ledger binaries out
  with `user_version` 7 rather than holding them read-only, because they
  cannot read the record. This fails closed, as the layout does for a
  `ReadCompatible` migration.

### Consequences

- "What may an older binary do with this state?" has one answer, the
  compatibility record, for every table `orbit-store` keeps as authority. A
  registry or feature change no longer has to choose between being invisible
  and refusing every older binary.
- A shipped migration can no longer be edited by accident through a helper
  it shares, because the golden for its version changes.
- Readers stop checking whether a table exists, and read-only opens see the
  same shape as writable ones.
- Cost: while pre-ledger binaries may still exist, the registry has two
  version concepts, the `user_version` fence beside the ledger. Its first
  non-additive change locks out every pre-ledger binary instead of holding it
  read-only.
- Cost: features lose independent numbering. Two features adding entries in
  the same window collide on the store registry's next version at merge, as
  store migrations already do.
- Cost: freezing duplicates the audit-events DDL into every body that used to
  share it, and the schema goldens grow by one dump per entry.
- Cost: every store creates the feature tables on open, including stores that
  never use the feature.
- Reach: a database that is entirely a derived index, which Orbit can
  rebuild from authority, needs no ledger. The `orbit-search` lexical index,
  which a reindex refills, is one, so this plan leaves it alone. A derived table
  inside a ledgered file still does, because its shape is part of that file's
  schema; the envelope stamps are one. The recovery authority's
  `authority.db` (`orbit-core`) holds authority and now uses the shared engine
  through `orbit-store::maintenance::migration`, with its own additive v1
  ledger in `runtime/recovery_authority/schema.rs`. The frozen baseline
  creates all five shipped tables, including the former first-use worker
  bindings, without reshaping any pre-ledger table. Its `schema_meta` ledger
  and `migration.compat` record stay in the protected file; root validation,
  permissions and sandbox denies are unchanged. Newer additive schemas remain
  writable, read-compatible schemas open query-only, and incompatible or
  unclassified newer schemas are refused. Opens retain WAL sidecars even when
  query-only or refused, so sandboxed workers can still read without a host
  connection. Read-only worker lookups check newer
  compatibility without migrating, and still read optional pre-ledger tables.
  Pre-ledger binaries do not read the record and have no version fence: future
  migrations must continue preserving their table contracts while supported.

### Plan

Filed as proposed tasks. Order: freeze the store first, because the
registry ledger and the feature absorption copy its engine and golden.

1. [ORB-15821] Freeze the shipped store migration bodies and add the
   per-version schema golden.
2. [ORB-15822] Run the task registry on its own ledger, adopting it from
   `user_version` 5/v6 and creating `task_envelope_stamps` in its v1.
3. [ORB-15823] Absorb the automation, review and local-pull feature ledgers
   into the store ledger and freeze them.
4. [ORB-15824] Move the recovery authority's `authority.db` onto the ledger
   engine.

## Task References

- [ORB-12434] introduced the forward-compatibility contract.
- [ORB-11994] and [ORB-12078] removed the task checkout projections whose
  older writers motivated the enforced read-only open.
- [ORB-15732] recorded the 30-day compatibility-path window and its four
  conditions.
- [ORB-15729] decided on one versioned ledger per SQLite database with frozen
  migration bodies; [ORB-15821], [ORB-15822], [ORB-15823] and [ORB-15824]
  implement it.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
