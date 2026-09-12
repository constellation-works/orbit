---
type: runbook
summary: Recover a corrupted Orbit SQLite database from backup, salvage, or regeneration.
tags: [operations, sqlite, corruption, recovery]
paths: ["crates/orbit-store/**", "crates/orbit-cmd/src/doctor.rs"]
related_features: [orbit-core]
related_artifacts: [ORB-10014, ORB-10473]
last_validated: 2026-09-12
---

# Recover a Corrupted Database

Use this runbook when `orbit doctor` reports a failed SQLite integrity check or Orbit cannot
open one of its databases.

## Recognize the failure

`orbit doctor` runs `PRAGMA quick_check` on the store DB (`~/.orbit/orbit.db`). Both common
failure shapes exit 1:

```text
# Page-level corruption; the store still opens:
│ database         ERROR     integrity check failed: store error: quick_check: database disk image is malformed │

# Severe corruption; the store cannot open and doctor aborts before the table:
error: store error: database disk image is malformed
```

## Safety prerequisites

Stop Orbit workers, MCP servers, timers, and dashboards before replacing a database. Preserve
the damaged main file and any `-wal` and `-shm` sidecars together before attempting salvage.
Do not overwrite the last known-good backup.

## Recover in priority order

### 1. Restore from backup

Prefer a consistent backup for `~/.orbit/orbit.db` because audit and run history are
authoritative and cannot be regenerated. Follow
[Inventory and protect Orbit state](./state-and-backup.md), then run `orbit doctor`.

### 2. Salvage with `sqlite3`

Use salvage only when no suitable backup exists. Rows on corrupted pages are lost; in one
verified test, a corrupted `job_runs` table came back empty while other tables survived.

```sh
sqlite3 ~/.orbit/orbit.db ".recover" | sqlite3 ~/.orbit/orbit.recovered.db
sqlite3 ~/.orbit/orbit.recovered.db "PRAGMA integrity_check;"   # expect: ok

# The next commands replace the active store. Keep the damaged backup made above.
mv ~/.orbit/orbit.recovered.db ~/.orbit/orbit.db
rm -f ~/.orbit/orbit.db-wal ~/.orbit/orbit.db-shm
orbit doctor
```

Do not install the recovered database unless `PRAGMA integrity_check` returns `ok`.

### 3. Regenerate a derivable database

| DB | Derivable? | Rebuild |
|---|---|---|
| `<ws>/.orbit/state/semantic.db` | yes | delete the file, then `orbit semantic index` |
| `~/.orbit/tasks/index.sqlite` | yes, from task bundles | `orbit task reindex` |
| `~/.orbit/orbit.db` | **no**—audit + run history | restore or salvage; deleting it is a last resort that loses history, although task content survives in file bundles and decision reasoning survives in `docs/` |

Graph is not a recoverable database subsystem: [Retire and delete Orbit's code-graph subsystem](../design/_archive/orbit-graph/4_decisions.md#retire-and-delete-orbits-code-graph-subsystem) retired it. If an older checkout left
`.orbit/graph` or shared `.orbit/knowledge/graph` state behind, remove both through
`orbit doctor --remove-graph`. The command is explicit and idempotent; ordinary `orbit doctor`
does not read or modify either location. Task symbol selectors continue to work from their
workspace-contained file anchors alone, with symbol/kind text treated as opaque metadata.

## Verification

Run `orbit doctor` and require the `database` check to report `ok`. Then inspect a known task
and recent run or audit record to determine which authoritative history survived.

Related: [Check Orbit health](./health-checks.md) ·
[Inspect the audit trail](./audit-trail.md).
