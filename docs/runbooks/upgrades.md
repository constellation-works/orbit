---
type: runbook
summary: Install a new Orbit release with `orbit update`, then review, apply, and verify workspace-layout and store-schema migrations safely.
tags: [operations, upgrades, migrations, recovery]
paths: ["crates/orbit-cmd/src/update/**", "crates/orbit-store/src/workflow/layout/**", "crates/orbit-store/src/driver/sqlite/migration/**"]
related_features: [orbit-core]
related_artifacts: [ORB-10014, ORB-11280, ORB-11344, ORB-11695, ORB-11753, ORB-12013]
last_validated: 2026-09-10
---

# Upgrade Orbit Safely

Use this runbook to replace an Orbit binary that may introduce workspace-layout or
store-schema migrations.

## Upgrade with `orbit update`

```sh
orbit update --check              # is a newer release published? exits 3 when yes
orbit update                      # install the newest published release
orbit update --version 0.19.0     # install one exact release
orbit update --allow-downgrade --version 0.18.0
orbit update --json               # machine-readable report
```

`orbit update` does the whole upgrade in one defined order:

1. Resolve the target version — the newest published release, or the one `--version` names.
2. Refuse an installation Orbit's own installer does not own. An npm, Homebrew, `cargo
   install`, or checkout build is reported with the command that *does* upgrade it, before
   anything is downloaded. A Homebrew install names the fully qualified canonical formula,
   `constellation-works/tap/orbit`, rather than an ambiguous `brew upgrade orbit`. A machine
   that still has the retired `danieljhkim/tap/orbit` formula installed gets a tested migration
   sequence instead — uninstall the legacy formula, then install the canonical one — because the
   two conflict rather than coexisting; a canonical-only install gets the ordinary qualified
   upgrade.
3. Take an exclusive lock in the install directory, so two updates cannot interleave.
4. Re-read the installed binary's version under that lock, and on Linux resolve a replaced
   running inode (`/path/to/orbit (deleted)`) back to the live install path. Equal, newer,
   and older installed versions are decided from that evidence — a writer that started on
   an older snapshot cannot overwrite a newer install that finished while it was discovering
   a release. `--check` stays read-only and does not take the lock.
5. Download the release archive, authenticate the checksum manifest against the trusted
   release signing keys, compare the archive's SHA-256, and extract its single `orbit` member
   into a staging file beside the installed one.
6. Copy the current executable to `<orbit>.previous`, then swap the staged file in with one
   atomic same-directory rename, and confirm the installed binary reports the requested
   version. If it does not, the previous executable is copied into a complete sibling staging
   file and atomically renamed over the replacement, so concurrent launches see either the
   complete replacement or the complete previous executable; the retained backup is not consumed
   and no workspace state is touched.
7. Run `orbit migrate --confirm`, then `orbit workspace sync` — **using the newly installed
   binary**, in the selected workspace. Orbit passes the resolved root to both subprocesses;
   an explicit `--root` remains authoritative even when `ORBIT_ROOT` names another workspace.
   Only the new binary carries the migrations and managed asset definitions for the version
   being installed.

Migration runs before managed-asset sync because a layout migration can move the directories
those assets live in.

Everything before the swap fails with nothing changed. After the swap the command never
reports success on an incomplete upgrade: it exits `4` with `outcome: needs_recovery` and
names the step that failed.

### Recovery and resumption

Re-running `orbit update` is the resume. At the installed version it skips the replacement and
re-runs the same idempotent convergence steps, so a run that failed at `migrate --confirm` or
`workspace sync` is finished by running it again — or by running that one command directly and
reading its diagnostics. When `--root` or `ORBIT_ROOT` selected the workspace, recovery output
includes that root explicitly, so retrying from a different checkout does not silently switch the
workspace being repaired.

The outgoing executable stays at `<orbit>.previous`. Restore it only if `.orbit/` state was not
migrated to a format it cannot read: an older binary refuses unsupported compatibility
versions. Additive storage can remain compatible, as described below. See [Respect the downgrade guard](#respect-the-downgrade-guard).

Without a root override, `orbit update` converges **the workspace you run it from**. `ORBIT_ROOT`
selects an environment-only override, while an explicit `--root` takes precedence over it. Run
the update (or `orbit migrate --confirm` and `orbit workspace sync`) for each other registered
workspace after upgrading, and restart long-lived Orbit services and pipeline workers so newly
dispatched agents inherit the replacement build.

### Downgrades

A release older than the **currently installed** binary — re-read under the update lock, not
the version the running process started with — is refused unless `--allow-downgrade` is passed.
Even then, the staged older binary must be able to open this workspace's state — `orbit update`
runs its `migrate --dry-run` *before* replacing anything and aborts, with that binary's own
diagnostic, when it cannot.

### Release mirrors

`ORBIT_UPDATE_RELEASE_DIR` points `orbit update` at a local mirror instead of GitHub Releases,
for air-gapped or staged rollouts. The layout is `latest-version.txt` plus
`v<version>/{orbit-<target>.tar.gz,orbit-checksums.txt,orbit-checksums.txt.sig}`. Signature and
checksum verification are unchanged — a mirror does not lower the bar. `ORBIT_INSTALL_REPO`
selects a different GitHub repository, as it does for `install.sh`.

## Understand the version ledgers

Two ledgers guard `.orbit/` state and auto-apply on workspace open:

- **Workspace layout:** `.orbit/state/layout.version` plus an ordered migration registry in
  `crates/orbit-store/src/workflow/layout/`. A missing marker means a pre-versioning workspace and is adopted
  as v1. Upgraders serialize on `state/layout.lock`.
- **Store schema:** the `schema_meta` ledger table inside `orbit.db`, backed by
  `crates/orbit-store/src/driver/sqlite/migration/`. Each migration and its ledger row commit in one
  transaction.

The host task registry has a separate reader-compatibility marker:
`PRAGMA user_version` in `~/.orbit/tasks/index.sqlite` (or the configured global
root). Its v5 task/allocator format also supports the additive `task_action_keys`
table. The repaired executable ensures that table when opening a writable v5
registry, without raising the reader-compatibility floor. A complete v5 registry
can still open read-only; missing additive storage requires a writable open.

### Recover a task registry marked version 6

A previous build marked this additive table as registry v6, causing v5 readers
to fail even on ordinary workspace/task access. A build containing the repair
recognizes the shipped compatible v6 columns, keys, indexes, and allocation
constraints, then restores the v5 marker in a transaction. Existing task data,
allocator state, and permanent action reservations remain intact. Repeated or
concurrent recovery is safe. Unknown schema versions or unrecognized v6 shapes
remain refused; recovery never means discarding tables or task data.

Executable upgrade and database recovery are separate steps. An already-installed
old executable does not learn a future migration. Install a build containing this
repair through its owning install channel, then invoke **that exact executable**
to open the registry. No manual SQL or registry reset is needed. `orbit migrate`
reports workspace layout and store-schema migrations; its dry-run report is not
an inventory of task-registry additive setup.

For mixed macOS installations, inspect the paths before rollout:

```sh
type -a orbit
command -v orbit
ls -l /opt/homebrew/bin/orbit
/opt/homebrew/bin/orbit --version
~/.cargo/bin/orbit --version
```

For example, `/opt/homebrew/bin/orbit` may still point to
`Cellar/orbit/0.19.0` while `~/.cargo/bin/orbit` is a different checkout build.
Building the latter does not upgrade Homebrew or change shell command selection.
A version string alone may not distinguish two checkout builds; confirm the
selected executable was built from a revision containing the repair. Upgrade
Homebrew through Homebrew when a release containing the fix is available, or use
an explicitly selected, validated repaired build under the rollout owner's direction.

Quiesce the build that writes v6 and take a consistent backup of the registry
and canonical bundles using [the state inventory](./state-and-backup.md). Then
open through the repaired build on that host, for example:

```sh
/path/to/repaired/orbit workspace list
/path/to/repaired/orbit tool run orbit.task.show --input '{"id":"<real-task-id>","model":"codex"}'
```

After this supported open has recovered a compatible v6 registry, a v5 reader
can again read its existing workspaces and tasks. Before that open, an old v5
executable still refuses v6. Do not keep the defective v6 writer running: it can
raise the marker again. Align `PATH`, any explicit `ORBIT_BIN`, MCP/service
launch paths, and restarted workers with the intended executable. Other host
store/layout/host-config compatibility guards still apply; registry recovery is
not a guarantee that every older release can read every other store.

If recovery is refused, use the reported database and executable paths to check
which installation is running. Upgrade the selected executable or escalate the
unrecognized format to the rollout owner; never lower `user_version` manually.
This repair's automated fixtures run on Linux. macOS Homebrew/PATH behavior and
live host rollout require verification on the affected Mac by the rollout owner.

## Back up before a major upgrade

Stop or quiesce independently managed Orbit processes, then create consistent backups:

```sh
cp -a <workspace>/.orbit <workspace>/.orbit.bak
sqlite3 ~/.orbit/orbit.db "VACUUM INTO '/backups/orbit.db'"
```

See [Inventory and protect Orbit state](./state-and-backup.md) for WAL-safe alternatives and
the complete authoritative-state inventory.

## Review and apply migrations

```sh
orbit migrate --dry-run    # list pending without applying; exit 1 when any are pending
orbit migrate              # same safe inspection default
orbit migrate --confirm    # open the workspace, auto-apply, and report
orbit migrate --json       # machine-readable inspection report
```

Example dry run on a pre-upgrade workspace:

```text
$ orbit migrate --dry-run
│ COMPONENT          CURRENT   SUPPORTED │
│ workspace layout   0         3         │
│ store schema       0         17        │
Pending migrations:
  layout v1 (baseline) — adopt the versioned .orbit/ layout (records the current shape; changes nothing)
  layout v2 (archive-friction-tasks) — rewrite removed friction statuses as archived
  layout v3 (remove-task-checkout-projections) — remove verified legacy .orbit/tasks symlinks without following them or touching canonical task bundles
  schema v1 (baseline) through schema v20 (invocations_ts_index)
error: execution failed: 3 migration(s) pending; run `orbit migrate --confirm` to apply
```

Bare `orbit migrate` and the compatibility-explicit `--dry-run` form inspect without opening
the runtime. Applying pending migrations always requires `--confirm`; the command never prompts
or reads stdin.

Before applying layout v3, pause admissions and stop every older Orbit process
that can write tasks. Install the new binary, apply or trigger the workspace
upgrade, and only then restart workers. Older binaries still contain the
retired projection writer and can recreate links while they remain running.

## Respect the downgrade guard

A workspace or DB written by a newer Orbit refuses to open rather than corrupting state:

```text
error: schema migration failed: workspace '….orbit' has .orbit layout version 99, newer
than the newest version this orbit binary supports (1); upgrade orbit to open this workspace
```

The schema ledger has the same guard:
`store database schema version N is newer than the newest version this orbit binary supports`.
Upgrade the binary. Never hand-edit `layout.version` to force the workspace open.

## Mixed binaries and the workspace semantic index

`.orbit/state/semantic.db` is a **forward-only** layout, independent of the workspace-layout and store-schema ledgers above. A current Orbit binary migrates `corpus_fts` from inline metadata columns (`source_kind`, `source_id`, `field`) to an external-content FTS5 table over `chunks`. The migration is in place: it does not rewrite task or doc source records, and it does not rebuild embeddings.

Older binaries still run the pre-migration BM25 projection:

```sql
SELECT source_kind, source_id, field, rowid, bm25(corpus_fts)
FROM corpus_fts
WHERE corpus_fts MATCH ?1 AND source_kind=?2
```

Against a migrated index that query fails with `no such column: source_kind`. Hybrid search on that older process then falls back to lexical ranking. Plain lexical task and doc lookup does not use `semantic.db` and is not broken.

This mismatch is not a dual-read contract. Restoring the old FTS columns would let an older writer insert into `corpus_fts` without writing `chunks`, desynchronizing the index. Do not delete `semantic.db` to make the older binary work, and do not downgrade the schema.

### Which process to upgrade or restart

The binary that **already migrated** the file is current. Restart or upgrade every **other** Orbit process that still has the file open — typically a Homebrew or MCP install that is older than the cargo/`~/.orbit/bin` build:

```sh
type -a orbit
/opt/homebrew/bin/orbit --version
~/.cargo/bin/orbit --version
~/.orbit/bin/orbit --version
```

On macOS, `lsof` on `.orbit/state/semantic.db` shows which process holds the migrated index. Align `PATH`, any explicit `ORBIT_BIN`, MCP client command paths, and long-lived dashboard/pipeline workers with the current binary, then restart those processes so they reopen the file. Building or running a newer cargo `orbit` does not upgrade Homebrew or change which executable an already-started MCP server is using.

`orbit update` is the install-channel upgrade for Orbit-owned binaries; Homebrew packages upgrade through Homebrew. This runbook does not authorize replacing a global executable, restarting a host service, or rebuilding the live index as part of diagnosing the mismatch.

### Distinguish lexical fallback from full hybrid success

Ask the **same executable** the MCP client or agent is using, not a different `orbit` on `PATH`:

```sh
/path/to/suspect/orbit tool run orbit.search --input '{"query":"<term>","kind":"task","hybrid":true,"limit":2,"model":"codex"}'
```

Read `mode` and `notes` together:

| Observation | Meaning |
| --- | --- |
| `mode` is `hybrid` and `notes` is empty | Full hybrid success on a current runtime. |
| `mode` is `lexical` and a note contains `falling back to lexical` plus `semantic index layout is incompatible` | The answering process cannot read this `semantic.db` layout. Upgrade/restart **that** process. Lexical hits are not hybrid ranking. |
| `mode` is `lexical` and a note contains `no such column` / `source_kind` | Same mismatch, reported by an older binary that does not yet translate the SQL error. Same remedy: upgrade/restart that process. |
| `mode` is `lexical` with a companion/embeddings fallback note, no layout diagnostic | Hybrid was skipped for an unrelated reason (missing companion, empty embeddings). Layout is fine. |
| Hybrid unset / `hybrid: false` | Lexical-only by request. Success here does not prove hybrid works. |

A current binary on the same workspace answering `mode: hybrid` with empty notes, while an older MCP process on the same `semantic.db` falls back, is the mixed-runtime case — not a corrupt index.

## Verify the upgrade

`orbit update` performs steps 1–3 below for the workspace it runs in. Do the same by hand when
a package manager owns the binary, and run steps 2–5 in every other registered workspace:

1. Replace or upgrade the binary.
2. Run `orbit migrate` (or `orbit migrate --dry-run`) to review pending layout/store changes,
   then `orbit migrate --confirm` to apply them.
3. Run `orbit workspace sync` to apply the provenance-safe managed-artifact actions. Operator
   edits, user-authored name collisions, and existing routine `name`/`hosts` bindings are
   preserved and reported with their paths. `orbit workspace sync --check` reviews the same
   actions read-only and exits nonzero when managed artifacts need convergence.
4. Run `orbit doctor` and require all relevant checks to pass.
5. Restart any independently managed dashboard process after swapping the binary.

These commands are intentionally independent. `workspace sync` converges local definitions
embedded in the installed binary; it does not install a newer binary, pull a repository, sync
another host, or run general layout/store migrations. `workspace init` remains the one-time
registration/bootstrap operation, while `doctor` diagnoses health and performs only its named,
narrow repairs.

Agent subprocesses inherit an `ORBIT_BIN` pinned to the Orbit executable that dispatched
them, and that executable's directory is placed first on their `PATH`. An operator-set
`ORBIT_BIN` takes precedence. After replacing `~/.orbit/bin/orbit`, restart long-lived Orbit
services and pipeline workers so newly dispatched agents inherit the replacement build. Verify
both the explicit path and the ordinary command resolve the same tool-capable binary:

```sh
~/.orbit/bin/orbit tool run orbit.task.show --input '{"id":"<real-task-id>","model":"codex"}'
command -v orbit
orbit tool run orbit.task.show --input '{"id":"<real-task-id>","model":"codex"}'
```

If `~/.orbit/host.toml` uses a schema newer than an installed binary supports, upgrade and
restart that binary. Never edit `schema_version` downward to bypass the guard.

See [Check Orbit health](./health-checks.md) for `orbit doctor` and dashboard-readiness
semantics. If verification fails, stop writers and restore the backups before further recovery.
