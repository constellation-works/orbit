---
type: runbook
summary: Locate Orbit state and perform WAL-safe backups, explicit task publication, restores, and task migrations.
tags: [operations, backup, restore, state, sqlite, task-publication]
paths: ["crates/orbit-cli/src/command/workspace/source_remote.rs", "crates/orbit-common/src/types/workspace.rs", "crates/orbit-config/src/**", "crates/orbit-registry/**", "crates/orbit-store/**", "crates/orbit-web/src/state.rs"]
related_features: [orbit-core, remote-access, task-publication]
related_artifacts: [ORB-10014, ORB-10294, ORB-10473, ORB-11077, ORB-11376, ORB-11426]
last_validated: 2026-09-06
---

# Inventory and Protect Orbit State

Use this runbook to determine what Orbit state is authoritative, choose what to back up,
restore a store database, or move task bundles between machines.

## State inventory

Two roots hold Orbit state. **Workspace state** lives in `<repo>/.orbit/`;
**user/machine state** lives in `~/.orbit/` (override with `--root <dir>`, highest
precedence). Path layout is defined in
`crates/orbit-common/src/types/workspace.rs` (`WorkspacePaths`) and
`crates/orbit-config/src/persistence.rs` (`PersistenceConfig`).

### Workspace `.orbit/`

| Path | What it is | Authoritative or regenerable |
|---|---|---|
| `config.yaml` | workspace identity (`workspace_id`) | authoritative |
| `config.toml` | optional workspace runtime config (layers over global per key, with security-sensitive exceptions—see [CONFIG.md](../CONFIG.md)) | authoritative |
| `frictions/` | legacy friction import/rollback tree; live records and taxonomy are published under the global root | preserve until migration evidence is no longer needed |
| `resources/` | workspace overrides for activities/jobs/executors/policies | authoritative |
| `graph/`, `knowledge/graph/` | retired graph state left by older Orbit versions | non-authoritative; remove explicitly with `orbit doctor --remove-graph` |
| `state/layout.version` | plain-text workspace layout version marker | regenerable marker (see [upgrades](./upgrades.md)) |
| `state/layout.lock` | advisory lock taken during layout upgrades | transient |
| `state/semantic.db` | semantic/vector index (docs and tasks) | regenerable (`orbit semantic index`) |
| `state/scoreboard/` | rolling counters (`pr.json`, `task_review.json`, `tokens.json`, …) | mostly regenerable |
| `state/job-runs/` | legacy file-based run bundles; new runs live in SQLite | not used for new runs; retain if old run evidence matters |
| `state/audit/blobs/` | redacted content-addressed blobs referenced by global `v2_audit_events` | preserve with audit history when detailed output matters |
| `state/logs/`, `state/diagnostics/`, `state/worktrees/` | workspace-local scratch and diagnostics | regenerable |

### Global `~/.orbit/`

| Path | What it is | Authoritative or regenerable |
|---|---|---|
| `config.toml` | global runtime config (created by `orbit init`) | authoritative |
| `workspaces.json` | registry of workspaces on this machine (logical workspaces + local checkouts, including declared owner and `owner`/`replica` role) | authoritative |
| `host.toml` | this machine's stable identity (`machine_id`, `host_id`, `task_prefix`) | authoritative |
| `registry-cache.json` | legacy file from the removed fleet-registry path | inert; no live reader or refresher, so remove only after backup if cleanup is desired |
| `orbit.db` (+ `-wal`, `-shm`) | **the** store DB for audit events (`audit_events`, `v2_audit_events`), job runs + checkpoints (`job_runs`, `job_run_steps`), task reservations, indexes, routine state, and the `schema_meta` migration ledger | **authoritative** for live history; old host/profile tables may remain from shipped migrations but are not registry, routing, health, or authorization authority |
| `tasks/index.sqlite` | global task-ID allocator + registry index | regenerable (`orbit task reindex`) |
| `tasks/workspaces/<ws-id>/<task-id>/` | canonical task bundles (survive repo moves) | **authoritative** |
| `frictions/workspaces/<ws-id>/` | live tag taxonomy plus the published legacy record tree used for one-time import/rollback | mixed: taxonomy is authoritative configuration; record files are legacy evidence after SQLite import |
| `resources/activities/`, `resources/jobs/` | managed defaults plus operator-authored activity/job YAML; hidden manifests retain managed content provenance | mixed: current defaults are regenerable, but untracked YAML and `resources/.retired-managed/` backups are **authoritative until reviewed** |
| other `resources/`, `skills/` | default executor/policy defs and skills; `resources/.orbit-global-defaults.json` records which embedded default set was last reconciled here | regenerable (`orbit init` reseeds) |
| `state/logs/orbit.jsonl` (+ rotated archives) | unified JSONL log sink for all Orbit processes | disposable |
| `state/task-publication/` | private Git object/work-tree caches plus pending-push reconciliation records | regenerable after a cleanly recorded success; retain during push-success/local-record recovery |
| `embed/` | semantic-search companion binary + models | regenerable (`orbit semantic install`) |
| `bin/` | installed Orbit binary (when installed via `install.sh`) | reinstallable |

Task bundles are not projected into workspace `.orbit/` directories. During
the layout-v3 upgrade, Orbit removes only checkout task links that match the
legacy canonical target shape and leaves ambiguous entries untouched. Stop all
older Orbit processes before installing the upgraded binary, then open each
workspace once with the new binary. An older process left running can recreate
the retired links until it is restarted.

> **Registry compatibility residue.** The immutable Store migration ledger can leave
> `hosts`, `host_aliases`, `workspace_ownership`, `host_workspace_presence`,
> `workspace_execution_profiles`, and `hub_registry_metadata` tables in `orbit.db`.
> Current `orbit-registry` has no SQLite dependency and no production path reads or writes
> those tables. They are not runtime authority for identity, workspace routing, health,
> crew discovery, or authorization.

> **Live registry refresh (ORB-10294).** A running `orbit web serve` no longer needs a
> restart to pick up `workspaces.json` changes. It reloads the registry at each request
> boundary, so a native `orbit workspace init` / `remove` — or a re-pointed checkout
> binding — becomes visible through `/api/workspaces` and routable through the
> workspace-scoped API on the next request; a removed workspace's cached runtime is evicted
> without disturbing the others. **Operator recovery semantics:** a checkout path that
> disappears after startup is reported `invalid` (inactive) rather than deleted — restore or
> re-point the path and the next request re-activates it, no restart needed. A malformed or
> half-written `workspaces.json` (e.g. an editor mid-save) never replaces the last good
> in-memory set: the server keeps serving the previous workspaces and logs a credential-safe
> diagnostic (the registry path plus the parse error, never the file contents) until the file
> parses again. A malformed registry present *at server startup* is still fatal — fix the file
> before launching. See [remote-access design §2.1](../design/remote-access/2_design.md) and
> [Registry snapshots are authoritative; runtimes are cached](../design/remote-access/4_decisions.md#registry-snapshots-are-authoritative-runtimes-are-cached).

> **Managed activity/job refresh (ORB-10684 / [Track bundled activity and job ownership by content digest before retirement](../design/activity-job/4_decisions.md#track-bundled-activity-and-job-ownership-by-content-digest-before-retirement)).** `orbit init` records
> the digest it wrote for each bundled activity and job in the resource
> directory's `.orbit-managed-assets.json`. A later refresh deletes a retired
> file only when it still matches that digest. Locally modified retired files
> move to `resources/.retired-managed/{activities,jobs}/`, outside active
> catalogs; back up and review those files as operator data. On a legacy root
> with no manifest, non-matching YAML stays in place and init names it in a
> warning. Move or delete only the named stale file after confirming it came
> from an older release, then rerun `orbit init` and the affected list command.
> A runtime open reconciles the managed catalogs only when the root does not
> already carry the running binary's `resources/.orbit-global-defaults.json` stamp — a
> fresh root, an upgrade, or a different Orbit build. Restoring a managed file
> removed or corrupted by hand is therefore an explicit `orbit init` /
> `orbit workspace sync` step rather than a side effect of the next command.

### Retired graph state and task selectors

[Retire and delete Orbit's code-graph subsystem](../design/_archive/orbit-graph/4_decisions.md#retire-and-delete-orbits-code-graph-subsystem) retired graph as an Orbit capability. Task `symbol:<path>#<symbol>:<kind>` context
selectors now use only `<path>` as a canonical workspace-contained file anchor; the symbol and
kind are opaque descriptive metadata. No health, task, or dashboard path probes graph state or
resolves symbols through it.

Older worktrees may still contain worktree-local `.orbit/graph`, while the shared workspace may
contain `.orbit/knowledge/graph`. `orbit doctor --remove-graph` removes exactly those two
locations and is safe to repeat. Ordinary `orbit doctor` is read-only with respect to both.

### Git-committed versus local state

`orbit workspace init` manages a selective `.gitignore` block. It keeps generated state
local while allowing the versioned configuration and definition directories that Orbit
owns today:

```gitignore
.orbit/*
!.orbit/auto_tasks/
!.orbit/resources/
!.orbit/routines/
!.orbit/config.toml
.orbit/**/*.lock
```

Any additional operator-authored paths require an explicit repository policy; do not
assume workspace initialization commits them.

#### Finalizing generated onboarding files

`orbit workspace init` updates `.gitignore` and creates untracked definition files under
`.orbit/auto_tasks/` and `.orbit/routines/`. Orbit intentionally does not auto-commit,
stash, or discard working-tree modifications.

For local delivery (`--ship-mode local`), the base branch landing checkout must be clean
to ensure safe fast-forward merges. Operators should review and finalize generated
onboarding definitions before dispatching local implementation workflows:

```bash
git add .gitignore .orbit/auto_tasks .orbit/routines
git commit -m "chore: initialize Orbit workspace definitions"
```

If local shipping is attempted while the landing checkout remains dirty, the workflow
fails early before agent implementation runs, reporting the unmerged or dirty landing
state so operators can safely commit or remediate the files without lost work.

### Recover a missing or corrupt checkout identity

Do not hand-create `.orbit/config.yaml` or edit `workspaces.json`. First preserve any
incident evidence outside the checkout when an investigation requires an operator-owned
copy. Then, from the registered checkout root, rerun the original initializer arguments
with `--force`, including the registered `--name`:

```bash
orbit workspace init --name <registered-name> --force
```

Recovery is accepted only when the global registry unambiguously matches both that logical
workspace and the current checkout's repository/data-root paths. A missing or malformed
identity is restored atomically from that binding. Malformed bytes (including a zero-byte
file) are first archived beneath
`.orbit/state/recovery/workspace-identity/config.yaml.<timestamp>.corrupt`; a missing file
has no bytes to archive. A parseable identity naming another workspace is still refused,
even with `--force`. Valid identity, parent-workspace identity, and unrelated registry
records are not recovery inputs and are not rewritten. [ORB-11376]

### Rebind the source remote after a repository move

Use the source-remote command when a Git repository moves to a different owner, name, or
host but remains the same logical Orbit workspace. Do not rerun `workspace init`, edit
`workspaces.json`, or perform a broad URL replacement: the supported operation changes only
the logical workspace's registered `git_remote`. Workspace ID, owner identity, task bundles,
checkout roles, and checkout/path registrations remain unchanged.

Inspect the current registration and save the old URL for rollback:

```sh
ORBIT_WORKSPACE=ws_example
NEW_SOURCE_REMOTE=git@github.com:new-owner/new-repository.git

orbit --workspace "$ORBIT_WORKSPACE" workspace show --format json
orbit --workspace "$ORBIT_WORKSPACE" workspace source-remote show --json
orbit --workspace "$ORBIT_WORKSPACE" workspace publication show --json
```

Only the declared owner machine can rebind the source remote. Replica checkouts fail closed.
The new value must be a portable Git URL without embedded credentials; local paths and
checkout-local aliases such as `origin` are refused. First preview the exact old and new
repository identities without writing:

```sh
orbit --workspace "$ORBIT_WORKSPACE" workspace source-remote rebind \
  --remote "$NEW_SOURCE_REMOTE" --dry-run --json
```

An existing task-publication binding prevents the write because its stored source fingerprint
is part of that local binding. Orbit does not rewrite publication lineage or old snapshots as
part of a source move. Record the complete `workspace publication show --json` output, then
remove the binding explicitly before retrying:

```sh
orbit --workspace "$ORBIT_WORKSPACE" workspace publication remove --confirm --json
orbit --workspace "$ORBIT_WORKSPACE" workspace source-remote rebind \
  --remote "$NEW_SOURCE_REMOTE" --json
```

After the repository provider transfer succeeds, update the checkout's Git `origin` separately;
Orbit does not mutate `.git/config`. Verify both identities and normal workspace resolution:

```sh
git remote set-url origin "$NEW_SOURCE_REMOTE"
git remote get-url origin
orbit --workspace "$ORBIT_WORKSPACE" workspace source-remote show --json
orbit --workspace "$ORBIT_WORKSPACE" workspace show --format json
orbit --workspace "$ORBIT_WORKSPACE" task list --limit 1 --format json
```

If publication was previously configured, use `workspace publication bind` with the captured
remote, branch, and publication ID after reviewing that they still describe the intended
dedicated publication repository. This creates a fresh local binding and clears local
last-success metadata; it does not move, rewrite, or delete snapshots in the publication
repository. Follow the publication runbook's verification procedure before the next publish.

To roll back, run the same source-remote command with the saved old URL, then restore the
checkout's `origin`. If a publication binding was already recreated, remove it explicitly
first; source rebinding never changes that binding automatically. Recreate the old binding
from the captured settings only after the old source identity is restored.

## Back up Orbit

### What to back up

- **Workspace:** the `.orbit/` directory. You may skip the retired `graph/` and
  `knowledge/graph/` locations plus regenerable state. Preserve `state/audit/blobs/` with
  the matching database when detailed audit output matters, and retain legacy
  `state/job-runs/` if its old run evidence matters. Git already backs up any selected
  artifacts the repository deliberately commits.
- **Global root:** `~/.orbit/config.toml`, `host.toml`, `workspaces.json`, `tasks/`
  (canonical bundles), `orbit.db`, `frictions/`, and `resources/` whenever it contains operator-authored YAML or
  `.retired-managed/` recovery copies. The database holds non-derivable audit and run history.
- **Safe to lose or regenerate:** retired `graph/` and `knowledge/graph/`, `state/semantic.db`,
  `tasks/index.sqlite`, `~/.orbit/embed/`, `~/.orbit/state/logs/`, scoreboard counters.

### Preserve SQLite consistency

All Orbit DBs run in WAL mode. A plain `cp` of a live `*.db` without its `-wal` and
`-shm` sidecars can produce a torn copy. Use one of these options, in order of preference:

```sh
# 1. Cold copy—no Orbit processes running (stop orbit-web and timers first):
cp -a ~/.orbit ~/orbit-backup-$(date +%F)

# 2. Live, consistent single-DB snapshot (works while Orbit runs):
sqlite3 ~/.orbit/orbit.db "VACUUM INTO '/backups/orbit.db'"
# or: sqlite3 ~/.orbit/orbit.db ".backup /backups/orbit.db"

# 3. Portable task backup or machine migration (tasks only):
orbit task export --all -o tasks-backup.tar.zst
```

Both `VACUUM INTO` and `.backup` produce a checkpointed, sidecar-free file. If you must
file-copy a live DB, copy `*.db`, `*.db-wal`, and `*.db-shm` together.

### Publish task snapshots to a dedicated repository

Task publication is an explicit, task-only durability channel. It does not
replace the global-root/database backup above: audit events, run history,
claims, reservations, configuration, host identity, and runtime caches are not
published. No task mutation publishes automatically, and v1 seeds no publication
routine. A future routine trigger must be configured separately.

Follow [Publish Orbit Tasks to a Dedicated Repository](./task-publication.md) for the complete
binding, SSH and HTTPS authentication, attachment policy, status verification, inspection,
same-authority recovery, and workspace-identity troubleshooting procedure.

## Restore Orbit

Stop local Orbit workers, MCP servers, and dashboards before restoring. The commands below
overwrite the current store database; retain a copy until verification succeeds.

```sh
# Put the snapshot back and drop stale sidecars from the old incarnation.
cp /backups/orbit.db ~/.orbit/orbit.db
rm -f ~/.orbit/orbit.db-wal ~/.orbit/orbit.db-shm

# Rebuild derived indexes as needed.
orbit task reindex
orbit semantic index      # if semantic search is installed
orbit doctor --remove-graph # remove retired local/shared graph state, if present

orbit doctor              # verify; see health-checks.md
```

Task bundles restored by file copy (for example, an rsync of `~/.orbit/tasks/`) need
`orbit task reindex` afterward. For cross-machine moves, prefer
`orbit task export` / `orbit task import --on-conflict=renumber`.

## Verification

Run `orbit doctor` and confirm that the `database` row reports `ok`; see
[Check Orbit health](./health-checks.md) for exit-code semantics. Then show or search a
known task to confirm that task bundles were reindexed.

Related: [Recover a corrupted database](./database-recovery.md) ·
[Upgrade Orbit safely](./upgrades.md).
