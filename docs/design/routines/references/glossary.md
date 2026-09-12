---
type: glossary
summary: Vocabulary for the routines scheduler feature.
last_validated: 2026-09-12
tags: [routines, scheduler]
---

# Routines — Glossary

Routines-specific vocabulary. Standard scheduler terms (cron expression, timer, daemon)
are excluded unless this feature gives them a specific meaning. Terms shared with the
activity-job feature (activity, job, run, catalog) are defined in
[../../activity-job/references/glossary.md](../../activity-job/references/glossary.md).

| Term | Meaning |
|------|---------|
| Clock | The per-user OS unit (launchd/systemd) that invokes the tick on a whole-minute cadence; host-local, configured and controlled through `orbit clock`. See [2_design.md](../2_design.md). |
| Fire | One scheduled dispatch of a routine's target; an ordinary run tagged `origin: routine/<name>`. See [2_design.md §3](../2_design.md). |
| Fire intent | The idempotency record (routine name + scheduled slot) written before dispatch so a slot never double-fires. See [2_design.md §3](../2_design.md). |
| Host identity | The `host_id` in `~/.orbit/host.toml`. It names run ownership and display only; it takes no part in deciding what a host evaluates. See [2_design.md §2](../2_design.md). |
| Local pause | A host-local, SQLite-persisted suppression of one routine (`orbit routine pause`); never versioned. See [2_design.md §4](../2_design.md). |
| Missed-run policy | Per-routine handling of slots that elapsed while the host was down: `catch_up_once` or `skip`. See [2_design.md §1](../2_design.md). |
| Owner checkout | A registered checkout whose logical workspace this machine owns (host-registry). The unit of scheduling: each one is an independent schedule against its own store. See [2_design.md §2](../2_design.md). |
| Routine | A versioned YAML definition of recurring work: trigger, target, enabled flag, policy. See [2_design.md §1](../2_design.md). |
| Routine source | Any registered, active owner checkout on the host; where routine YAML lives. Registration is the whole opt-in. See [2_design.md §2](../2_design.md). |
| Sweep | Compatibility alias for `orbit clock tick`. See [2_design.md §3](../2_design.md). |
| Tick | One invocation of the pass by the clock; evaluates routines then auto-task definitions for every owner checkout on the host. See [2_design.md §3](../2_design.md). |
