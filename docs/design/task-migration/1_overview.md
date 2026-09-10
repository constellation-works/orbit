---
title: Task Migration — Overview
owner: claude
last_updated: 2026-07-04
last_validated: 2026-08-16
status: Draft
feature: task-migration
doc_role: overview
type: design
summary: Move orbit tasks between machines with export/import (tar.zst) and disjoint id ranges, without hand-written SQL.
tags: [task-migration]
paths: ["crates/orbit-store/src/task_migration/**", "crates/orbit-cli/src/command/task/**"]
related_features: [task-migration, task-artifacts]
related_artifacts: [ORB-00034]
---

# Task Migration — Overview

Orbit tasks live as portable canonical bundles under
`~/.orbit/tasks/workspaces/<ws-id>/<ORB-xxxxx>/`, but the global index
(`~/.orbit/tasks/index.sqlite` — workspace bindings, task/index/tag/relation
rows, and a single monotonic id allocator) had no import/export/rebuild path.
Because every machine allocates task ids from the same local `ORB-00000`
counter, merging two machines' tasks guaranteed id collisions. Task migration
adds `orbit task export`/`import`/`reindex` plus a `tasks.id_start` allocator
floor so tasks move between machines as a three-command operation with a printed
id mapping and no hand-written SQL. ([ORB-00034])

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
- **`id_start`** — a forward-only floor for the allocator, so machine A takes
  `0–9999` and machine B `10000+` and neither ever re-issues the other's ids.
- **Reindex** — rebuild `index.sqlite` rows from the on-disk bundles (source of
  truth), recovering from rsync/manual moves and index drift.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Archive pack/unpack (tar.zst) | [crates/orbit-store/src/task_migration/archive.rs](../../../crates/orbit-store/src/task_migration/archive.rs) | [ORB-00034] |
| Export / transactional import + renumber | [crates/orbit-store/src/task_migration/mod.rs](../../../crates/orbit-store/src/task_migration/mod.rs) | [ORB-00034] |
| Reindex from disk | [crates/orbit-store/src/task_migration/reindex.rs](../../../crates/orbit-store/src/task_migration/reindex.rs) | [ORB-00034] |
| Allocator seed/bump primitives | [crates/orbit-store/src/sqlite/task_registry/store.rs](../../../crates/orbit-store/src/sqlite/task_registry/store.rs) | [ORB-00034] |
| Runtime facades | [crates/orbit-core/src/command/task_migration.rs](../../../crates/orbit-core/src/command/task_migration.rs) | [ORB-00034] |
| CLI surfaces | [crates/orbit-cli/src/command/task/export.rs](../../../crates/orbit-cli/src/command/task/export.rs) | [ORB-00034] |
| `[tasks] id_start` config | [crates/orbit-config/src/raw.rs](../../../crates/orbit-config/src/raw.rs) | [ORB-00034] |

## 4. The migration recipe

Move a workspace's tasks from machine A to machine B:

```sh
# On A — pack the workspace's tasks (omit --workspace to use the current one)
orbit task export --all -o tasks.tar.zst            # or --ids ORB-00001,ORB-00007

# Move the archive
scp tasks.tar.zst B:/tmp/

# On B — import, renumbering any id that already exists locally
orbit task import /tmp/tasks.tar.zst --on-conflict=renumber
```

Import is transactional: it validates the manifest version and every bundle's
integrity *before* touching state, so a corrupt or version-incompatible archive
fails with no partial writes. It resolves the target workspace (the archive's
source workspace if registered locally, else `--workspace <id>`, else it
registers the source workspace id), keeps ids that are free, renumbers the rest,
rebuilds the index rows from bundle YAML, and bumps the allocator past the highest
landed id. When anything is
renumbered, an `<archive>.idmap.json` old→new map is written and printed.

Idempotency is scoped to *kept* ids: re-importing an archive whose ids are free
(or already landed unchanged) is a no-op. A `--on-conflict=renumber` run is not
idempotent — a collision means "these are new local tasks," so each re-run mints
fresh ids. Import a renumber archive once; the printed `.idmap.json` is the
record of what landed.

`--on-conflict=skip` imports the non-colliding tasks and drops the rest;
`--on-conflict=fail` aborts the whole import on the first collision.

### Preventing future collisions

Hand each machine a disjoint id range so cross-machine tasks never collide in
the first place:

```sh
# On the box: start its allocator at 10000 (the Mac keeps 0–9999)
orbit workspace init --task-id-start 10000
```

The counter only moves forward — a lower `--task-id-start` is refused. For a
sticky floor across a shared config, set `[tasks] id_start = 10000` in
`config.toml` (see [../../CONFIG.md](../../CONFIG.md)); it is applied as a
forward-only floor on every runtime build and never errors on an already-
advanced counter. Both paths cap at the allocator's `ORB_TASK_ID_MAX`
(`u32::MAX`): five-digit padding is a minimum display width, not an exhaustion
boundary.

### Recovering a drifted index

If bundles were `rsync`'d or moved by hand, rebuild the index from disk:

```sh
orbit task reindex --workspace <ws-id>   # default: the current workspace
```

Reindex treats the on-disk bundles as the source of truth: it registers any
bundle missing from the index, drops stale bindings whose directory is gone,
rebuilds the index/tag/relation rows and bumps the allocator past the highest
on-disk id. `allocator_state` is otherwise
preserved. Unreadable or partial bundles retain their bytes and any registered
binding/index; healthy neighbors are indexed, and reindex returns an error listing
the unresolved task IDs rather than reporting full success.

Full-bundle readers, writers, creation, deletion, and reindex coordinate through
persistent `.<task-id>.bundle.lock` files beside the canonical bundles.
These lock files must not be removed while Orbit processes run. Writers recheck
the envelope after acquiring the lock, so a queued update cannot recreate a
deleted bundle. Upgrades changing this lock location require restarting all
writers together; older processes use the former in-bundle lock.

Deletion atomically renames `<task-id>/` to `<task-id>.deleted/` and syncs its
parent before removing registry entries, then removes the
tombstone and syncs again. A crash before rename leaves the live task intact.
After rename, deletion retry or reindex rolls forward, including when registry
removal has already succeeded or cleanup left partial contents. A registry
failure retains the entire renamed bundle; a cleanup failure retains its
remaining contents. If both canonical and tombstone paths exist, recovery
reports a conflict and retains both for explicit repair. Tombstones are never
registered as live tasks.

## Task References

- [ORB-00034] — task migration tooling: `orbit task export/import/reindex`, `tasks.id_start` allocator config.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
