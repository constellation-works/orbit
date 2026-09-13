---
title: State Compatibility — Decisions
owner: claude
last_updated: 2026-09-13
last_validated: 2026-09-13
status: Draft
feature: state-compatibility
doc_role: decisions
type: design
summary: Why forward compatibility is declared by the writing binary rather than inferred from a version number, and why a forward-compatible open is enforced read-only by SQLite itself.
tags: [state-compatibility, migrations, upgrades]
paths: ["crates/orbit-store/src/contracts/compat.rs", "crates/orbit-store/src/driver/sqlite/connection.rs", "crates/orbit-store/src/workflow/layout/mod.rs"]
related_features: [state-compatibility]
related_artifacts: [ORB-12434]
---

# State Compatibility — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

## Compatibility is declared by the binary that migrates, never inferred from the version number

**Recorded:** 2026-09 · [ORB-12434]
**Code anchors:** `crates/orbit-store/src/contracts/compat.rs::evaluate_newer_state`, `crates/orbit-store/src/workflow/layout/mod.rs::write_compat_record`, `crates/orbit-store/src/driver/sqlite/migration/ledger.rs::write_compat_record`

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

Each migration entry declares `Additive` or `Breaking`, and the binary that
applies a migration writes that classification into the state beside the
version it stamps (`state/layout.compat`; the `migration.compat` row in
`schema_meta`). An older binary reads the record and refuses only when a
breaking migration sits above its own supported version — and then names that
migration. A missing, stale, unknown-format, or unreadable record refuses
exactly as before, so the contract fails closed in every case it cannot
evaluate.

`Additive` is a claim about binaries that *lack* the migration: they must
still read the state correctly, and — for the layout, which has no single
write choke point — write it safely. Anything that removes, renames, or
reinterprets state older binaries touch is `Breaking`. Declare `Breaking`
when in doubt.

### Consequences

- A schema bump is no longer a flag day: an old `orbit task list`,
  `orbit run history`, or `orbit search` keeps working against an
  additive-newer workspace instead of failing at open.
- The refusal that remains is actionable — it names the first breaking
  migration the binary lacks instead of a version number.
- Cost: the classification is a hand-written claim that nothing verifies, and
  the registries are append-only, so a migration mismarked `Additive` cannot
  be corrected for state already stamped — only a later breaking migration
  raises the floor again. A wrong marker is strictly worse than the flag day
  it avoids, which is why "when in doubt, `Breaking`" is part of the rule and
  not advice.
- Cost: nothing is gained for binaries that predate the contract; forward
  compatibility only begins with the first release that writes records.

## A forward-compatible open is pinned read-only in SQLite, not by convention

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

When the schema ledger reports a forward-compatible open, `Store::open`
issues `PRAGMA query_only=ON` on the writer connection before the handle
exists. SQLite then rejects every write through every path that connection
reaches. The store's own write entry points additionally refuse early with a
scoped `OrbitError::Migration` that names the attempted operation, the
supported version, and the recorded version — a diagnostic layer over the
guarantee, not the guarantee.

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

## Task References

- [ORB-12434] introduced the forward-compatibility contract.
- [ORB-11994] and [ORB-12078] removed the task checkout projections whose
  older writers motivated the enforced read-only open.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
