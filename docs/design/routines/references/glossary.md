---
type: glossary
summary: Vocabulary for the routines scheduler feature.
last_validated: 2026-09-10
tags: [routines, scheduler]
---

# Routines — Glossary

Routines-specific vocabulary. Standard scheduler terms (cron expression, timer, daemon)
are excluded unless this feature gives them a specific meaning. Terms shared with the
activity-job feature (activity, job, run, catalog) are defined in
[../../activity-job/references/glossary.md](../../activity-job/references/glossary.md).

| Term | Meaning |
|------|---------|
| Fire | One scheduled dispatch of a routine's target; an ordinary run tagged `origin: routine/<name>`. See [2_design.md §3](../2_design.md). |
| Fire intent | The idempotency record (routine name + scheduled slot) written before dispatch so a slot never double-fires. See [2_design.md §3](../2_design.md). |
| Host identity | The `host_id` in `~/.orbit/host.toml`, matched against a routine's `hosts` list. See [2_design.md §2](../2_design.md). |
| Local pause | A host-local, SQLite-persisted suppression of one routine (`orbit routine pause`); never versioned. See [2_design.md §4](../2_design.md). |
| Missed-run policy | Per-routine handling of slots that elapsed while the host was down: `catch_up_once` or `skip`. See [2_design.md §1](../2_design.md). |
| Routine | A versioned YAML definition of recurring work: trigger, target, hosts, enabled flag, policy. See [2_design.md §1](../2_design.md). |
| Routine source | A registered workspace opted in via `[routines] role = "source"`; where routine YAML lives. See [2_design.md §2](../2_design.md). |
| Sweep | The stateless `orbit sweep` pass the OS clock invokes each minute to fire due routines on this host. See [2_design.md §3](../2_design.md). |
