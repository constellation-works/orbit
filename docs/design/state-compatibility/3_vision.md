---
title: State Compatibility — Vision
owner: claude
last_updated: 2026-09-13
last_validated: 2026-09-13
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
   It solves the same problem with a different mechanism; unifying them would
   remove one concept at the cost of a migration of its own.
5. **How far back should read-only support reach?** The record makes it
   possible to support arbitrarily old binaries; a support window may be
   preferable to an open-ended promise.

## 2 Prior Work

### Database engines

SQLite's `PRAGMA application_id` / `user_version` give a file a version but
no compatibility semantics; the caller supplies the policy. PostgreSQL takes
the opposite stance with `catversion`: any catalog change is breaking, and a
mismatch refuses the whole cluster — Orbit's pre-[ORB-12434] behaviour.

### Application-managed schemas

Firefox's Places database and Chromium's `sql::MetaTable` both distinguish a
current version from a *compatible* version — the lowest reader that may open
the file. This contract is that idea with the floor derived from named
migrations instead of a single number, so the diagnostic can name what is
missing.

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
- Read-only is enforced by the storage engine rather than by convention, so
  the guarantee does not depend on reviewing future write paths.

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
