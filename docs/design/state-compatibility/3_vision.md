---
title: State Compatibility — Vision
owner: claude
last_updated: 2026-10-12
last_validated: 2026-10-18
status: Draft
feature: state-compatibility
doc_role: vision
type: design
summary: Open questions about verifying additive claims, widening the read-only guarantee beyond SQLite, and how other systems version state readers can outlive.
tags: [state-compatibility, migrations, upgrades]
paths: ["crates/orbit-store/src/contracts/compat.rs"]
related_features: [state-compatibility]
related_artifacts: [ORB-12434]
---

# State Compatibility — Vision

Scope: what this contract does not yet answer, and the prior art it borrows
from. The shipped mechanism is in [2_design.md](./2_design.md).

## 1 Open Questions

1. **Can an `Additive` claim be tested rather than asserted?** A fixture
   could apply migration N, then run the previous release's read paths
   against the result. That needs an old binary (or a frozen read surface) in
   CI, and a decision about which releases are in scope.
2. **Should the layout half also be write-gated?** Today an additive-newer
   layout permits ordinary writes because `.orbit/` file writes have no single
   choke point. A workspace-scoped "read-only mode" carried by the runtime
   would close that, at the cost of threading the mode through every write.
3. **Should audit events survive a read-only open?** They are currently
   dropped with a warning. A local spool replayed after the upgrade would
   keep the trail intact, but introduces a second write target.
4. **Does the task registry's `user_version` floor belong in this contract?**
   *Resolved 2026-10-12.* Yes. The registry moves onto its own ledger, run by
   the store's ledger engine, and keeps `user_version` only as a fence for
   binaries that predate that ledger. See
   [4_decisions.md](./4_decisions.md#one-versioned-ledger-per-sqlite-database-with-frozen-migration-bodies).
5. **How far back should read-only support reach?** *Resolved 2026-10-11.* The
   record makes it possible to support arbitrarily old binaries, so support is
   bounded by a 30-day, release-gated window instead of an open-ended promise.
   A compatibility path, including a legacy reader of a persisted shape, is
   deleted once the four conditions in
   [4_decisions.md](./4_decisions.md#compatibility-paths-expire-on-a-30-day-release-gated-window)
   hold.

## 2 Prior Work

### Database engines

SQLite's `PRAGMA application_id` / `user_version` give a file a version but
no compatibility semantics; the caller supplies the policy. PostgreSQL takes
the opposite stance with `catversion`: any catalog change is breaking, and a
mismatch refuses the whole cluster — Orbit's pre-[ORB-12434] behaviour.

### Application-managed schemas

Chromium's `sql::MetaTable` distinguishes a current version from a
*compatible* version — the lowest reader that may open the file. Firefox
Places exposes one current database schema version and migrates older schemas.
This contract generalizes the compatibility floor by deriving it from named
migrations instead of requiring a single number, so the diagnostic can name
what is missing.

### Wire formats

Protobuf's unknown-field preservation and Avro's reader/writer schema
resolution make forward compatibility the default for messages. Orbit's state
is not self-describing in that way, so the equivalent has to be recorded
explicitly.

## 3 What May Be Distinctive

- The compatibility floor is expressed as *named migrations*, so a refusal
  points at a change ("v14 `remove_native_learning_subsystem`"), not a number.
- The claim is written by the binary that performs the migration, which is
  the only party that can make it, and read by binaries that ship later than
  the reader but earlier than the writer.
- For the SQLite store, read-only is enforced by SQLite's `query_only` setting
  rather than by convention. The workspace-layout ledger has no equivalent
  write gate: it refuses newer `ReadCompatible` layouts instead of opening
  them read-only.

## 4 References

Orbit-internal:

- [2_design.md](./2_design.md) — the implemented mechanism.
- [Upgrade Orbit Safely](../../runbooks/upgrades.md) — the operator path.

External:

- SQLite, *PRAGMA application_id / user_version*.
- PostgreSQL, *catalog version number (`catversion.h`)*.
- Mozilla, *Places database schema migrations*.
- Google, *Protocol Buffers: updating a message type*.

## Task References

- [ORB-12434] introduced the forward-compatibility contract these questions
  build on.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
