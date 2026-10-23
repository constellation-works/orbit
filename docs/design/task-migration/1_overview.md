---
title: Task Migration — Overview
owner: claude
last_updated: 2026-10-10
last_validated: 2026-10-19
status: Draft
feature: task-migration
doc_role: overview
type: design
summary: Move orbit tasks between hosts with export/import (tar.zst); task_prefix identifies each task's canonical owner, while an admitted follower may execute a fenced claim without taking authority.
tags: [task-migration, multi-host, task-prefix]
paths: ["crates/orbit-store/src/workflow/task/**", "crates/orbit-cli/src/command/task/**", "crates/orbit-core/src/bootstrap/task_migration.rs", "crates/orbit-cmd/src/registry/runtime/mod.rs"]
related_features: [task-migration, task-artifacts, host-registry]
related_artifacts: [ORB-00034, ORB-10721, ORB-12126]
---

# Task Migration — Overview

Orbit tasks live as portable canonical bundles under
`~/.orbit/tasks/workspaces/<ws-id>/<ORB-xxxxx>/`, but the global index
(`~/.orbit/tasks/index.sqlite` — workspace bindings, task/index/tag/relation
rows, and a single monotonic id allocator) had no import/export/rebuild path.
Task migration adds `orbit task export`/`import`/`reindex` so tasks move between
hosts as a three-command operation with a printed id mapping and no hand-written
SQL ([ORB-00034]). Hosts no longer share an id space: each mints under its own
`[machine].task_prefix` in global `~/.orbit/config.toml` ([ORB-10721],
[host-registry](../host-registry/2_design.md)), and the host that minted a task
is its sole writer — see [§5](#5-multi-host-authority) and
[4_decisions](./4_decisions.md).

## 1. Motivation

The concrete driver: migrate pre-existing tasks from a Mac (workspace
`orbit-8fb91e`) onto the box. Canonical bundles are `scp`-able, but dropping
them on the target does nothing until the registry knows about them, and their
ids may already be taken locally. Two problems had to be solved together:
**portability** (pack bundles + enough metadata to rehome them) and **id
collision** (renumber on import, and prevent future clashes by handing each
machine a disjoint id range).

## 2. Core Concepts

- **Canonical bundle** — the on-disk source of truth for one task (`task.yaml`
  envelope + markdown bodies + `events`/`comments` JSONL + legacy review-thread
  and artifact sidecars). Export copies these verbatim.
- **Manifest** — the single non-bundle entry in an archive: format/schema
  version, source workspace id + slug, task-id list, and `exported_at`.
- **Global allocator** — one `local` authority in `allocator_state`. Task ids
  are a *global* primary key across all workspaces on a machine, so a collision
  is against the whole registry, not one workspace.
- **Renumber** — on an id collision, `--on-conflict=renumber` allocates a fresh
  local id and rewrites every relation target (including the `ChildOf` parent
  link) *within the imported set*, then writes an old→new mapping file.
- **`task_prefix`** — the per-host id namespace (`ORB-`, `DANI-`, …), chosen once
  at `orbit init`, stored in `[machine].task_prefix` in global `~/.orbit/config.toml`,
  and projected into the allocator before any runtime opens. Two
  hosts with different prefixes cannot collide, whatever their counters say.
- **Owner** — the host whose prefix a task carries. Only the owner mutates the
  task; any copy elsewhere is a read-only mirror.
- **Owner-wins** — the import policy that syncs those mirrors:
  `--on-conflict=owner-wins` replaces a colliding foreign-prefix bundle with the
  owner's copy, leaves a colliding local-prefix bundle alone, and never
  renumbers.
- **`id_start`** — a forward-only floor for the allocator. Predates prefixes
  (machine A took `0–9999`, machine B `10000+`); now redundant for collision
  avoidance and kept only as a harmless floor.
- **Reindex** — rebuild `index.sqlite` rows from the on-disk bundles (source of
  truth), recovering from rsync/manual moves and index drift. It advances the
  local allocator past on-disk IDs with this host's task prefix, including
  unreadable bundles retained for repair. Foreign-prefix mirrors are indexed
  without moving that counter. A local ID at `u32::MAX` exhausts allocation.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Archive pack/unpack (tar.zst) | [crates/orbit-store/src/workflow/task/archive.rs](../../../crates/orbit-store/src/workflow/task/archive.rs) | [ORB-00034] |
| Export / validated import + renumber | [crates/orbit-store/src/workflow/task/export.rs](../../../crates/orbit-store/src/workflow/task/export.rs), [crates/orbit-store/src/workflow/task/import.rs](../../../crates/orbit-store/src/workflow/task/import.rs) | [ORB-00034] |
| Reindex from disk | [crates/orbit-store/src/workflow/task/reindex.rs](../../../crates/orbit-store/src/workflow/task/reindex.rs) | [ORB-00034] |
| Allocator seed/bump + prefix primitives | [crates/orbit-store/src/driver/sqlite/task_registry/allocator.rs](../../../crates/orbit-store/src/driver/sqlite/task_registry/allocator.rs) | [ORB-00034], [ORB-10721] |
| Prefix projection into the allocator | [crates/orbit-cmd/src/registry/runtime/selection.rs](../../../crates/orbit-cmd/src/registry/runtime/selection.rs) | [ORB-10721] |
| Runtime facades | [crates/orbit-core/src/bootstrap/task_migration.rs](../../../crates/orbit-core/src/bootstrap/task_migration.rs) | [ORB-00034] |
| CLI surfaces | [crates/orbit-cli/src/command/task/export.rs](../../../crates/orbit-cli/src/command/task/export.rs), [import.rs](../../../crates/orbit-cli/src/command/task/import.rs) | [ORB-00034] |
| `[tasks] id_start` config | [crates/orbit-config/src/raw.rs](../../../crates/orbit-config/src/raw.rs) | [ORB-00034] |
| Owner-wins mirror sync | [crates/orbit-store/src/workflow/task/import.rs](../../../crates/orbit-store/src/workflow/task/import.rs) | [ORB-12126] |

## 4. The migration recipe

Move a workspace's tasks from machine A to machine B:

```sh
# On A — pack the workspace's tasks (omit --task-workspace to use the current one)
orbit task export --all -o tasks.tar.zst            # or --ids ORB-00001,ORB-00007

# Move the archive
scp tasks.tar.zst B:/tmp/

# On B — import into B's task-registry workspace, renumbering any id that already exists locally
orbit task import /tmp/tasks.tar.zst \
  --task-workspace <target-task-workspace-id> --on-conflict=renumber
```

Archives hold only directories and regular files. Export refuses a bundle that
contains a link, so an archive never carries the bytes of a file the link points
at, and import refuses an archive entry that is a link or special file.

Import validates the manifest version and every bundle's integrity *before*
touching state, so a corrupt or version-incompatible archive fails with no
partial writes. During the mutation phase, fresh bundles and a source workspace
registration created by the import are rolled back if a later write fails;
owner-wins replacements remain because the owner's copy is authoritative.
`--task-workspace` selects the task-registry
partition, whose id is the `workspace_id` in B's checkout `.orbit/config.yaml`;
the checkout must already be registered locally. If omitted, import resolves the
archive's source workspace if registered locally, otherwise it registers the
source workspace id without a checkout. It keeps ids that are free, renumbers the rest,
rebuilds the index rows from bundle YAML, and bumps the allocator past the highest
landed id. When anything is
renumbered, an `<archive>.idmap.json` old→new map is written and printed.
Importing the highest valid local task ID advances the allocator to its
exhausted state, so later local ID allocation fails explicitly. A renumber
whose kept or registered IDs already reach that ceiling fails before publishing
any imported bundles or registering the source workspace.

Idempotency is scoped to *kept* ids: re-importing an archive whose ids are free
(or already landed unchanged) is a no-op. A `--on-conflict=renumber` run is not
idempotent — a collision means "these are new local tasks," so each re-run mints
fresh ids. Import a renumber archive once; the printed `.idmap.json` is the
record of what landed.

`--on-conflict=skip` imports the non-colliding tasks and drops the rest;
`--on-conflict=fail` aborts the whole import on the first collision;
`--on-conflict=owner-wins` syncs mirrors from their owning host — see
[§5](#5-multi-host-authority).

### Preventing future collisions

Each host mints under its own `task_prefix` ([ORB-10721]): `orbit init` asks
for it once, global `~/.orbit/config.toml` holds it, and `RegisteredRuntimeFactory`
projects it into the allocator before any runtime opens. Two hosts with different prefixes
share no id, so cross-host imports keep their ids and `--on-conflict` never
fires on a well-formed fleet. A conflicting prefix after allocation has begun
fails closed rather than renaming issued ids.

The older range-splitting knob still exists and is harmless:

```sh
orbit workspace init --task-id-start 10000     # or [tasks] id_start in config.toml
```

The counter only moves forward — a lower `--task-id-start` is refused; the
config form (see [../../CONFIG.md](../../CONFIG.md)) is applied as a forward-only
floor on every runtime build. An exhausted allocator reports exhaustion during
this maintenance step as well as during allocation.
Both paths cap at the allocator's `ORB_TASK_ID_MAX` (`u32::MAX`): five-digit
padding is a minimum display width, not an exhaustion boundary.

### Recovering a drifted index

If bundles were `rsync`'d or moved by hand, rebuild the index from disk. Keep
`tasks/index.sqlite` in place: reindex repairs it without discarding anything
it alone records.

```sh
orbit task reindex --task-workspace <task-workspace-id>   # default: the current workspace
```

Reindex treats the on-disk bundles as the source of truth: it registers any
bundle missing from the index, drops stale bindings whose directory is gone,
rebuilds the index/tag/relation rows and bumps the allocator past the highest
on-disk id. `allocator_state` is otherwise
preserved. Unreadable or partial bundles retain their bytes and any registered
binding/index; healthy neighbors are indexed, and reindex returns an error listing
the unresolved task IDs rather than reporting full success.

The command works from either supported layout. With a repo-local root, run it
from the checkout whose `.orbit` directory identifies the workspace; the task
index and bundles remain under the home Orbit root. With an external root, keep
the checkout as the current directory and pass the same root that was used for
workspace initialization (`orbit --root /path/to/orbit-root task reindex`).

If `orbit task list` reports that on-disk bundles are missing from the task
index, run the matching reindex command before treating an empty list as task
loss. The bundle partitions remain the recovery source until reindex completes.

### Recovering a lost task registry

`tasks/index.sqlite` is not a disposable cache. One registry serves every
workspace on the Orbit root, and it alone records the allocator's issued-id
high-water mark, the minting prefix, and each workspace and checkout binding.
Bundles can restore bindings and index rows, but not the high-water mark: ids
of deleted tasks and of reservations that never landed leave no bundle behind.
Never delete the registry to fix drift; reindexing one workspace after deleting
it is not a recovery.

When the registry is missing or unusable:

1. Stop every Orbit process that uses the root (MCP servers, the dashboard,
   scheduled routines, running jobs) so nothing writes while you recover.
2. Move the registry aside rather than deleting it, keeping
   `index.sqlite`, `index.sqlite-wal` and `index.sqlite-shm` together. If a
   backup of all three exists, restore it and run the drift procedure above
   for every workspace. A restored registry does not refuse allocation, so if
   ids may have been issued after the backup was taken, also do step 5 before
   restarting writers.
3. Run any Orbit command. A registry created while `tasks/workspaces/` already
   holds partitions starts with an unknown high-water mark, and every task-id
   allocation is refused until step 5. Reindex still works.
4. Reindex every partition: from each workspace's checkout, run
   `orbit task reindex` (with `--root` for an external root). Every
   workspace's tasks become visible again, including workspaces that share one
   external root.
5. Establish the high-water mark: from one checkout, run
   `orbit workspace init --force --task-id-start <N>`, with `N` above every id
   this host has ever issued, not just the highest surviving bundle. Use the
   moved-aside registry, publication mirrors, peers' records or delivery
   history as evidence, and leave a margin when in doubt. The counter only
   moves forward, and this explicit seed is what lifts the refusal; a
   configured `[tasks] id_start` floor does not.

Full-bundle readers, writers, creation, deletion, and reindex coordinate through
persistent `.<task-id>.bundle.lock` files beside the canonical bundles.
These lock files must not be removed while Orbit processes run. Writers recheck
the envelope after acquiring the lock, so a queued update cannot recreate a
deleted bundle. Upgrades changing this lock location require restarting all
writers together; older processes use the former in-bundle lock.
Reindex works in bounded, task-ID-ordered batches of bundles, so the locks
it holds at once — and their open descriptors — stay bounded however large the
workspace grows. It first registers every readable bundle's binding batch by
batch, then, for each batch, holds its locks from the final envelope reads
through index publication. An ordinary update or deletion of a task in that
batch waits for publication, then commits its own state.

Deletion atomically renames `<task-id>/` to `<task-id>.deleted/` and syncs its
parent before removing registry entries, then removes the
tombstone and syncs again. A crash before rename leaves the live task intact.
After rename, deletion retry or reindex rolls forward, including when registry
removal has already succeeded or cleanup left partial contents. A registry
failure retains the entire renamed bundle; a cleanup failure retains its
remaining contents. If both canonical and tombstone paths exist, recovery
reports a conflict and retains both for explicit repair. Tombstones are never
registered as live tasks.

## 5. Multi-host authority

Prefixes make it safe for the same repository to be a workspace on more than
one host. A host can mint and run its own tasks, and an admitted follower can
also execute a claim for a task owned by another host. Task authority stays
with one canonical owner; execution on a follower does not make its local
mirror writable or create a second writer. The rules that keep the stores
coherent are:

- **The task prefix identifies its canonical owner.** Status, verdicts,
  relations, artifacts, and other task mutations remain in the owner store.
  Copies on other hosts are read-only mirrors; a mirror does not grant mutation
  authority.
- **An admitted follower may execute an owner task under a claim.** The
  follower's pull drain requests a fenced claim from the owner, runs the
  claimed task under its original ID, and returns its candidate and evidence
  through the claim protocol. Owner-routed task access keeps the owner as the
  only writer. See the [distributed-drain setup procedure](../../runbooks/distributed-drain.md#procedure-owner-and-follower-setup)
  for the shipped operator path.
- **Off-host clients that own no checkout reach the owner over MCP** rather than
  minting locally (`orbit mcp serve --mode remote <host>`,
  [remote-access](../remote-access/1_overview.md)). A route that targets a
  specific host fails loudly when that host is down; it never falls back to the
  local prefix, because the operator chose which host runs the work.
- **Owner-wins import syncs the mirrors.** The other policies are one-shot: a
  foreign task that already landed and then changed on its owner is a *conflict*
  on re-import (`renumber` duplicates it, `skip` drops the update, `fail`
  aborts). `--on-conflict=owner-wins` resolves it from the id alone — a
  colliding foreign-prefix bundle is replaced by the owner's copy (reported
  `updated`), a colliding local-prefix bundle is left untouched (reported
  `skipped-local-owned`), and nothing is ever renumbered ([ORB-12126]). If the
  task id has no registry binding but its canonical bundle directory remains,
  owner-wins refreshes that orphaned mirror and restores its binding — but only
  for a foreign-prefix id, because a local-prefix bundle is locally owned
  whether or not the index still binds it ([ORB-12164]); `reindex` remains the
  recovery command for other index drift. Because it never mints,
  foreign relation targets survive verbatim, no `.idmap.json` is written, and
  re-running the same archive reports every task as `already-present` — so it
  is safe on a timer:

  ```sh
  orbit task import /tmp/peer-tasks.tar.zst --on-conflict=owner-wins
  ```

  Pull before push: a cross-host `depends_on` only resolves once its target's
  mirror has landed. A failed run is re-runnable: fresh bundles and any
  workspace registration created by that run are rolled back, while owner-wins
  replacements already applied stay because the owner's copy is the newer one.

Prefix ownership keeps task authority singular while permitting fenced
execution on an admitted follower. The historical execute-where-you-mint rule
is superseded for claimed execution; see [4_decisions](./4_decisions.md) for
that record and the current [distributed-drain setup procedure](../../runbooks/distributed-drain.md#procedure-owner-and-follower-setup).

## Task References

- [ORB-00034] — task migration tooling: `orbit task export/import/reindex`, `tasks.id_start` allocator config.
- [ORB-10721] — per-host `task_prefix` in global `~/.orbit/config.toml`, projected into the allocator.
- [ORB-12126] — owner-wins cross-host sync: import policy that overwrites only foreign-prefix bundles.
- [ORB-12164] — the unbound-bundle repair path honors the local-prefix guard.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
