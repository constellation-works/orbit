---
type: runbook
summary: Install a new Orbit release with `orbit update`, then review, apply, and verify workspace-layout and store-schema migrations safely, including what an older binary may still do with a newer workspace.
tags: [operations, upgrades, migrations, recovery]
paths: ["crates/orbit-cmd/src/update/**", "crates/orbit-store/src/workflow/layout/**", "crates/orbit-store/src/driver/sqlite/migration/**", "crates/orbit-store/src/contracts/compat.rs"]
related_features: [orbit-core]
related_artifacts: [ORB-10014, ORB-11280, ORB-11344, ORB-11695, ORB-11753, ORB-12013, ORB-12434]
last_validated: 2026-09-20
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
3. Acquire generation admission against the same resolved authority `--preflight` uses,
   refusing while any participating Orbit process is live, then take the exclusive
   install-directory lock so two updates cannot interleave.
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
8. Run `orbit clock repair`, again as the newly installed binary. The launchd/systemd sweep
   unit embeds an absolute program path, so an install that lands somewhere else — Homebrew
   to `~/.orbit/bin`, say — leaves the unit invoking a binary that may no longer exist. The
   step rewrites the unit to the installed binary and re-registers it, and the update report
   carries the line it printed. A unit that already names this binary is left untouched; a
   paused clock is corrected on disk but not resumed.

Migration runs before managed-asset sync because a layout migration can move the directories
those assets live in. The clock unit is converged last, and only after the workspace steps
succeed: a clock re-armed against a half-migrated workspace would just fail every minute.
Unlike the first two, it is host state, so it runs even outside an initialized workspace.

`install.sh` runs the same `orbit clock repair` after it installs the binary, so a host that
moves between install locations (Homebrew or `cargo install` to `~/.orbit/bin`) does not need
`orbit update` to notice. A binary installed by a package manager that runs neither — `brew
upgrade`, `cargo install` — still needs one `orbit clock repair` by hand; `orbit doctor`'s
`clock-unit` row and a hand-run `orbit sweep` both name it.

Everything before the swap fails with nothing changed. After the swap the command never
reports success on an incomplete upgrade: it exits `4` with `outcome: needs_recovery` and
names the step that failed.

### Persistent MCP clients and upgrade admission

`orbit update --contract --json` reports protocol support without opening state:
`{"schema_version":1,"contract":"executable-generation-v1"}`. The supported
updater requires this response from a candidate before installation; a missing
or incompatible protocol refuses, including a downgrade to an unprotected build.

`orbit update --preflight --json` is the wrapper-facing admission probe. It
opens no runtime, migrates no store, downloads nothing, and changes no binary
or managed resource. It uses OS locks under every **generation authority** the
invocation can be refused by, in the order the update takes them:

1. The invocation's own resolution — `--root`, then `ORBIT_ROOT`, otherwise the
   host-global root (normally `~/.orbit/`; managed children retain their
   supplied registry root). This is the authority this process would pin as a
   client, and it is reported as `global_root`.
2. The host-global root as well, whenever a `--root` / `ORBIT_ROOT` override
   named something else. A root override does not move what `orbit update`
   replaces: the executable is the running one (`~/.orbit/bin/orbit` for a
   managed install), and every client started *without* an override — including
   the persistent `orbit mcp serve` processes this protocol exists for — pins
   the host-global root. Admitting against the override alone would replace the
   binary those clients are running and leave their record naming a generation
   no later host-global process could ever take over from.

Both are listed in `admission_roots`, and a refusal names the authority it came
from. So a live client refuses the upgrade whether it is pinned under
`--root`/`ORBIT_ROOT` or on the host-global root, and a green preflight is only
evidence for an update that used the same invocation's root resolution. There is
no path where preflight consults a different set of authorities than the
following `orbit update` locks.

Admission also requires each authority's `.generation.lock` to be *writable*,
and checks that before anything is downloaded, staged or replaced. Writability
belongs to the record rather than to the lock — a participant can join an
already-recorded generation from a read-only mount — so an authority that can
never record a takeover would otherwise only refuse at pin time, once the
executable had already been swapped. A `~/.orbit` on a read-only mount, or one
whose record another user owns, therefore refuses `orbit update --root
<scratch>` up front with `the record cannot be written from here` naming that
root, rather than replacing the binary and returning `needs_recovery` against a
host-global record it cannot correct. `--preflight` takes the same admissions,
so a green preflight is evidence the update can pin every authority it
reported.

Isolated `HOME=` is the other working isolation — it relocates `~/.orbit`
itself, which is what in-process MCP roundtrip fixtures use, and it moves the
host-global authority with it. What a root override protects is *state*
isolation, not host-binary replacement: a read-only unpinned `~/.orbit` (the
agent-executor / Cowork sandbox) still cannot block `orbit --root <scratch>
init`, because that invocation pins only its own resolved root and an unpinned
root refuses nothing. `orbit update` and its `--preflight` are the exception:
they admit against the host-global root too, so a `--root` override never
exempts a host-binary replacement from that root's live clients — nor from a
record it cannot write — and an environment with no resolvable home has no
host-global authority to observe them through, so `orbit update` refuses there
rather than replacing blind. Replace the host binary from outside such a
sandbox, or give the sandbox a writable host-global root. Coordination lock
files may be created. Exit 0 returns:

```json
{"schema_version":1,"admitted":true,"reservation":false,"contract":"executable-generation-v1","global_root":"/srv/project","admission_roots":["/srv/project","/home/operator/.orbit"]}
```

Exit 1 with `upgrade admission refused` on stderr means stop before installation.
`--json` emits the CLI's normal JSON error envelope on stderr. This is an
observation, **not a reservation**. Constellation's wrapper should call it using
the configured executable, user, environment and authority, without an ad-hoc
MCP server or alternate store. Use `orbit update` for replacement through the
supported installer: it acquires admission again against that same set of
authorities, retains it across staging and replacement, and pins the candidate
generation in each of them. `--check` only checks release availability and is not this probe.
External installers do not hold Orbit's admission across their file operations;
they must quiesce clients before replacement. A preflight alone does not make an
external installer race-free.

`orbit update` does not signal live drain workers. It requires exclusive
generation admission before replacing the executable and refuses while any
worker holds a shared pin. An external binary copy bypasses that admission and
can run while drains are live; an external installer or service restart may
signal them independently. A new writing `orbit clock tick` then refuses while
the old generation stays pinned, and logs one dated hold summary when it can
run again. A claimed worker terminated without a recorded cancellation is
`interrupted` with `worker_terminated` whether its supervisor observes SIGTERM
or stale-owner reconciliation sees the dead process first. The reconciler
cannot determine which signal or installer killed an already-gone process.

Every participating CLI process pins its executable generation before runtime
bootstrap and retains that pin until exit. This includes ordinary/ operator MCP
stdio, the TCP listener, the local part of a federated mux, destination-side SSH
servers, and managed workers. The proxy does not grant authority at its
remote destination; that destination admits its own process. All existing
workspace selection, operator/agent capability, remote caller and managed-run
checks still run. Admission grants none of those permissions.

On macOS, a managed child with `ORBIT_REGISTRY_ROOT` joins its parent's host
generation pin and keeps global stores on that registry even if it sets
`ORBIT_ROOT` to select shared workspace data. The workspace `.orbit` generation
record may be absent and cannot be created by the child sandbox. `orbit update`
and `orbit update --preflight` still check both the explicit workspace authority
and the host-global authority.

The policy is deliberately conservative: any live process prevents ordinary
`orbit update`, even an update with the same schema or version. A *writing*
command from a different executable generation cannot open a runtime while that
authority is pinned, so launching a newly installed executable cannot silently
auto-migrate underneath an older participating MCP process. A *read-only*
command (`RuntimeNeed::ReadOnly` — `task show`/`list`/`flow`, `run history`/
`show`, `search`, `workspace list`/`show`, `tool list`, `friction list`, and
other observation verbs) may join the live generation without rewriting
`.generation.lock` when its compiled store schema equals the store's current
schema. The joiner takes the same shared flock, so `orbit update` still refuses
while it runs. Schema equality is exact, not ORB-12434 additive-newer; a
matching digest still uses that additive-newer read-only compatibility after
admission. A differing digest whose schema does not match is refused for
read-only commands too, naming both schema versions. Writer refusals say the
command writes. The updater changes its exclusive pin to the candidate
generation before convergence children start. An old pinned executable cannot
enter that gap. Identical executable copies share admission; version strings
alone are not compatibility evidence. On Linux, the digest comes from
`/proc/self/exe`, including a deleted running inode. On macOS the native Mach-O
image UUID must match the loaded image before the opened descriptor is hashed;
a replaced path or unsupported image format refuses admission.

A participant that can only read the admission files — a read-only mount, or a
sandboxed child denied writes under its authority root — still joins the
generation already recorded there, because a lock needs a descriptor rather than
permission to rewrite bytes. It can never record a takeover: a differing
generation is refused with `the record cannot be written from here`, leaving the
record intact rather than partially written. A managed nested child therefore
runs against the generation its host recorded; widen nothing to change that,
and give a child a writable authority root only when it must own one.

Refusal leaves the connected client and its in-flight calls running. There is
no server handoff, connection replacement, mutation retry or blind replay. If a
mutation committed but its reply was lost, inspect the durable task/audit through
the same authority before deciding what to do next. Quiesce through the process's
owning client/operator, then retry the update. Orbit does not kill sessions,
change identities or reclaim claims. OS locks release on exit/crash; never unlink
`.generation.lock` or `.generation-admission.lock` to force admission. Keep these
files in the authoritative root and out of lock-file garbage collection.

**Bootstrap limitation:** processes from before this fix do not participate.
Before the first protected upgrade, explicitly quiesce every pre-fix backend
(including unmanaged or pinned executables), install the fix, and reconnect using
the same configured authority. Restarting a desktop window is not evidence that
its backend exited. The protocol cannot retroactively protect a pre-fix process,
an external writer, or a process using another authority root. Normal schema and
layout compatibility checks remain in force; admission is not a downgrade waiver.

### Recovery and resumption

Re-running `orbit update` is the resume. At the installed version it skips the replacement and
re-runs the same idempotent convergence steps, so a run that failed at `migrate --confirm`,
`workspace sync`, or `clock repair` is finished by running it again — or by running that one
command directly and reading its diagnostics. `clock repair` fails when the unit manager
refuses to reload the rewritten unit; it names the `launchctl`/`systemctl` command to run. When `--root` or `ORBIT_ROOT` selected the workspace, recovery output
includes that root explicitly, so retrying from a different checkout does not silently switch the
workspace being repaired.

The outgoing executable stays at `<orbit>.previous`. Restoring it is safe when the state it
must open is newer only by additive migrations — it then serves reads and refuses writes —
and refused when a breaking migration separates the two. See
[Run an older binary against a newer workspace](#run-an-older-binary-against-a-newer-workspace).

Without a root override, `orbit update` converges **the workspace you run it from**. `ORBIT_ROOT`
selects an environment-only override, while an explicit `--root` takes precedence over it. Run
the update (or `orbit migrate --confirm` and `orbit workspace sync`) for each other registered
workspace after upgrading, and restart long-lived Orbit services and pipeline workers so newly
dispatched agents inherit the replacement build.

### Downgrades

A release older than the **currently installed** binary — re-read under the update lock, not
the version the running process started with — is refused unless `--allow-downgrade` is passed.
Even then, the staged older binary must be able to open this workspace's state — `orbit update`
runs its `migrate --dry-run --json` *before* replacing anything and requires an
explicit up-to-date report with matching current/supported layout and schema versions.
Missing reports and additive-newer read-only success both refuse replacement.

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

Each ledger also carries a compatibility record written by whichever binary applied the
last migration — `.orbit/state/layout.compat` and the `migration.compat` row in
`schema_meta`. That record is what lets a binary older than the workspace decide whether
it may still read it; see
[Run an older binary against a newer workspace](#run-an-older-binary-against-a-newer-workspace).
Both files are Orbit-owned state: read them for diagnosis, never edit them.

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

## Run an older binary against a newer workspace

A binary older than the workspace no longer fails every command on the version number
alone. Each migration declares itself **additive** (an older binary reads the result
correctly) or **breaking** (it removes, renames, or reinterprets state older binaries
use), and the binary that applies a migration records that classification beside the
version it stamps — `state/layout.compat` for the layout, the `migration.compat` row in
`schema_meta` for the database. An older binary reads the record and takes one of two
paths. The contract is described in
[docs/design/state-compatibility](../design/state-compatibility/2_design.md).

### Additive-newer: unaudited CLI reads

Generation admission may first refuse a different executable while a participating
process is live. Once admitted, reader compatibility applies independently.
MCP `tools/call` is **not** an unaudited read: even `orbit.workspace.list` must
write its durable audit event. An old process with a read-only newer store is
therefore not a usable MCP authority, even when `orbit task show` works at the CLI.
Do not use successful CLI reads or candidate-vs-store checks as proof of MCP
continuity.

When nothing breaking sits above the binary's supported version, the workspace opens
**read-only**. `orbit task list`, `orbit task show`, `orbit run history`, and
`orbit search` work; every write is refused with its own diagnostic, and the older
binary never migrates, restamps, or otherwise rewrites the newer state:

```text
error: schema migration failed: cannot open a write transaction: this orbit binary
supports store schema version 20 and the store records version 21, so it was opened
read-only; reads are served normally — upgrade orbit to write to this store
```

`orbit migrate` (and `--dry-run`) report this as a successful inspection and name it:

```text
read-only: store schema version 21 is newer than this binary's supported version 20,
but only by additive migrations; opened read-only

This workspace is newer than this binary, by additive migrations only: read-only
commands work and writes are refused. Upgrade orbit to write to it.
```

Two limits are worth knowing before relying on this. Audit events are writes, so a
read-only command records no audit row and prints a `failed to write audit event`
warning. And an additive-newer *layout* (as opposed to store schema) is not
write-gated: additive is a declaration that older binaries stay safe, which is why
anything an older writer could damage — layout v3's task projections, for instance — is
declared breaking instead.

### Breaking-newer, or unclassified: still refused

A breaking migration the binary lacks refuses the open, now naming that migration:

```text
error: schema migration failed: workspace '….orbit' has .orbit layout version 4, newer
than the newest version this orbit binary supports (3); migration v4 (relocate-run-state)
is a breaking change this binary does not have; upgrade orbit to open this workspace
```

State written **before** this contract shipped carries no compatibility record, and so
does state whose record is stale (a crash between stamping the version and writing the
record) or unreadable. All three refuse with the reason named, exactly as every newer
version did previously:

```text
error: schema migration failed: workspace '….orbit' has .orbit layout version 99, newer
than the newest version this orbit binary supports (3); it records no forward-compatibility
metadata, so this binary cannot tell whether the newer migrations are additive; upgrade
orbit to open this workspace
```

In every refusing case the remedy is the same as before: upgrade the binary that is
reporting it — through its own install channel — and re-run. Never hand-edit
`layout.version` or `layout.compat`, and never delete the record to force an open: that
converts a refusal into a binary operating on state it cannot interpret.

## Lexical search migration

Search now uses SQLite FTS5 BM25 only. `orbit semantic` (install, uninstall,
stats, index), `orbit search --hybrid`, `orbit search similar`, and the
`orbit.semantic.*` tools are removed. `orbit.search` rejects `semantic` and
`hybrid` inputs. Use distinctive query terms and `orbit search reindex` to
rebuild task chunks after imports or restores.

The search database retains its `state/semantic.db` filename so existing
backup and sandbox paths still work. On the first writable open, Orbit preserves
`chunks` and `corpus_fts`, migrates older inline FTS tables, drops the obsolete
`embeddings` table and its indexes plus `id_allocations`, and runs `VACUUM` to
reclaim disk space. Read-only opens do not migrate. Task create/update/delete
maintain chunks synchronously; `orbit doctor` reports `search-index` counts.

Stop older Orbit writers and upgrade/restart MCP, dashboard, and pipeline
processes together. The index migration is forward-only; older binaries must
not write the migrated database. Align PATH and explicit `ORBIT_BIN` settings
with the installed release before restarting those processes.

The `orbit-search-companion` binary and downloaded models are no longer used
or shipped. After stopping old processes, inspect `~/.orbit/embed/`, then
manually remove that dedicated directory (including `bin/`, `models/`, and the
active-model file) to reclaim the former model downloads. This does not affect
task records or the lexical index. No automatic deletion of that global
directory is performed.

Legacy `[semantic]` configuration and `search.model` are ignored with a warning
for the removal release; delete them from config.toml. Config get/set reject
these retired keys with migration guidance. The runtime no longer honors
`ORBIT_SEARCH_COMPANION*` environment overrides.

## Verify the upgrade

`orbit update` performs steps 1–3 below for the workspace it runs in. Do the same by hand when
a package manager owns the binary, and run steps 2–5 in every other registered workspace:

1. Replace or upgrade the binary.
2. Run `orbit migrate` (or `orbit migrate --dry-run`) to review pending layout/store changes,
   then `orbit migrate --confirm` to apply them.
3. Run `orbit workspace sync` to apply the provenance-safe managed-artifact actions. Operator
   edits, user-authored name collisions, and existing routine `name`/`hosts` bindings are
   preserved and reported with their paths. A routine that differs from a template a prior
   release shipped only in the settings Orbit's own surfaces change — `enabled`, the retired
   `hosts:` key — is not an operator edit: it retires or refreshes (keeping its `enabled`
   setting) so the upgrade converges without moving files by hand. `orbit workspace sync
   --check` reviews the same actions read-only and exits nonzero when managed artifacts need
   convergence.
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

If `~/.orbit/config.toml` uses settings newer than an installed binary supports, upgrade and
restart that binary. Never edit `schema_version` downward to bypass the guard.

See [Check Orbit health](./health-checks.md) for `orbit doctor` and dashboard-readiness
semantics. If verification fails, stop writers and restore the backups before further recovery.
