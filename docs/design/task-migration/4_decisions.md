---
title: Task Migration — Decisions
owner: claude
last_updated: 2026-09-11
last_validated: 2026-09-11
status: Draft
feature: task-migration
doc_role: decisions
type: design
summary: Why task authority is per task_prefix rather than per host, why a task runs where it was minted, and why disjoint id ranges were retired in favour of prefixes.
tags: [task-migration, multi-host, task-prefix]
paths: ["crates/orbit-store/src/workflow/task/**", "crates/orbit-cmd/src/registry_runtime.rs", "crates/orbit-config/src/raw.rs"]
related_features: [task-migration, host-registry]
related_artifacts: [ORB-00034, ORB-10721, ORB-12126]
---

# Task Migration — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

## Task authority is per prefix, not per host

**Recorded:** 2026-09 · [ORB-12126]
**Code anchors:** `crates/orbit-store/src/workflow/task/mod.rs::import_tasks`, `crates/orbit-cmd/src/registry_runtime.rs::sync_task_prefix`

### Context

The July 2026 drain made one box the single task authority and froze task
creation everywhere else (the constellation's `RB-10` runbook). That held until
the box saturated at roughly ten concurrent runs and every Orbit operation from
a laptop had to round-trip to it. The alternatives on the table were (a) keep
the single authority and scale the box, (b) allow a second live store and merge
task state between hosts, or (c) allow a second live store and partition the
write set so nothing ever needs merging.

### Decision

Authority follows the id's `task_prefix` ([ORB-10721]), not the machine. Any
host may register any workspace and mint tasks in it under its own prefix. The
host that minted a task is its sole writer; a copy on any other host is a
read-only mirror. `import_tasks` carries the matching owner-wins conflict
policy: it replaces foreign-prefix bundles with the owner's copy and never
overwrites a local-prefix one ([ORB-12126]), which makes a repeated import a
mirror sync rather than a collision.

### Consequences

- Duplicate workspaces across hosts are supported, with zero merge logic — the
  importer decides overwrite-or-skip from the id alone.
- The single-authority policy is superseded; "where is the task?" is now
  answered by its prefix.
- Cost: two hosts can never collaborate on one task. Reassigning a task to the
  other host means re-minting it there (a new id) and closing the original —
  identity does not survive a change of owner.

## Execute where you mint

**Recorded:** 2026-09 · [ORB-12126]

### Context

Once a second host can mint, the tempting shape is "mint anywhere, let the
strongest host run it." That reintroduces a second writer — the executing host
mutates status, verdicts, and artifacts on a task it does not own — and with it
every conflict the prefix partition was chosen to avoid.

### Decision

A task is dispatched and run only by the host whose prefix it carries. Choose
the prefix by choosing where to run: mint on the laptop for laptop work, on the
box for box work. An off-host client that owns no checkout mints on the target
host over MCP ([remote-access](../remote-access/1_overview.md)) rather than
locally. A route that targets a specific host fails loudly when that host is
unreachable; it never falls back to the local prefix.

### Consequences

- The sweep, the dispatcher, and every `orbit task start`-shaped surface may
  refuse a foreign-prefix task outright; that refusal is correct behaviour.
- Capacity is added by minting on the extra host, not by load-balancing an
  existing backlog across hosts.
- Cost: no cross-host scheduling. A backlog minted on a saturated host stays
  there; relieving it means re-minting, not migrating.

## Disjoint id ranges (superseded by prefixes)

**Recorded:** 2026-07 · [ORB-00034] · superseded 2026-08 by [ORB-10721]
**Code anchors:** `crates/orbit-config/src/raw.rs` (`[tasks] id_start`), `crates/orbit-store/src/driver/sqlite/task_registry/store.rs`

### Context

Before host identity existed every machine allocated from the same `ORB-00000`
counter, so merging two machines' tasks guaranteed collisions. The migration
tooling needed a way to keep future ids disjoint without a coordinator.

### Decision

Give each machine a forward-only allocator floor (`--task-id-start` /
`[tasks] id_start`) so machine A takes `0–9999` and machine B `10000+`.

### Consequences

- Cross-machine imports kept their ids for as long as the ranges held.
- Superseded: `task_prefix` namespaces ids by host, so the ranges no longer
  carry the collision guarantee. The floor remains as a harmless historical
  artifact; removing it is not worth a migration.
- Cost: the range was a convention, not a contract — nothing stopped a machine
  from running past its range or a third machine from being added without one.
  That fragility is what prefixes replaced.

## Task References

- [ORB-00034] — task migration tooling and the `id_start` allocator floor.
- [ORB-10721] — per-host `task_prefix` in `host.toml`, projected into the allocator.
- [ORB-12126] — owner-wins cross-host sync policy.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
