# Keeping a workspace healthy

Day-2 operations. The first section is the one that actually bites busy
workspaces; the rest is periodic hygiene.

## Worktree garbage collection

Implementation pipelines create worktrees; deterministic collection or filing
jobs need not. Nothing reclaims them automatically unless
you arrange it, so a workspace that ships on a schedule accumulates worktrees
until the disk fills. **Enable this before, not after, scheduling ship traffic.**

```bash
orbit gc worktrees                          # report only — the default is non-destructive
orbit gc worktrees --estimate-bytes         # dry-run plus a recursive byte estimate
orbit gc worktrees --confirm                # actually remove
orbit gc worktrees --older-than-hours 24    # leave recent runs alone
orbit gc worktrees --run <run_id>           # restrict to one run
orbit gc worktrees --target-only            # report only each worktree's target/ build output
orbit gc worktrees --target-only --confirm  # delete target/ and keep the checkout
```

Collection is conservative: it reaps only worktrees whose associated task has
settled to `done`, `rejected`, or `archived`. A worktree belonging to live work
is never a candidate. Run the report a few times before automating it, then
enable the `worktree-gc` routine for hourly reclamation —
[automation.md](automation.md).

On a replica checkout (one that pulls work from an owner on another machine),
task records live on the owner. A claimed leaf whose claim this follower has
settled with the owner needs no task answer: the owner holds its delivery, so
its worktree is collected and `detail` names the settled claim. For any other
worktree GC asks the owner for the task's status over the owner's tool
surface, through the claim's own route (the owner must be in
`~/.orbit/mcp-destinations.toml`). A transport failure keeps the worktree as
`skipped:owner_unreachable` with the error in `detail`; run GC again once the
owner answers. `skipped:no_owner_route` means the replica has no route to ask
at all — the owner is missing from `mcp-destinations.toml` or the checkout is
not a registered workspace — and `detail` says which. A status lookup that
fails without a transport error is reported as `skipped:owner_lookup_failed`
with the reason in `detail`; it does not establish that the owner is down.
The pull drain also
reclaims each settled leaf's `target/` on its next pass, so follower disk
does not depend on this schedule.

A directory under the worktree root that Git does not list as a worktree is
never removed (`skipped:not_registered_worktree`). `detail` gives the remedy:
`git worktree repair <path>` if it was moved, otherwise inspect it and delete
it by hand once nothing in it is needed.

`--target-only` reclaims per-worktree Cargo `target/` directories, which hold
most of a worktree's size. It deletes only `<worktree>/target` and keeps the
checkout, including uncommitted changes, so a failed or blocked run can still
be rescued. It needs a terminal run with no live recorded worker, not a settled
task. A running run is never touched, and a `target/` holding anything Git
tracks or does not ignore is kept (`skipped:target_not_ignored`).
`bytes_reclaimed` reports the size of the `target/` directory.

## Diagnose and repair

```bash
orbit doctor            # config, database, disk, indexes, locks, runs
orbit doctor --json
```

Run it after an upgrade, after an interrupted run, and whenever something behaves
inexplicably. It has targeted repairs, each narrow on purpose:

| Flag | Repairs |
|---|---|
| `--fix-stale-locks` | Lock files whose recorded holder process is dead. |
| `--fix-stale-task-locks` | Task reservations whose owner and task state are conclusively inactive. |
| `--fix-stale-artifacts` | Retires deprecated skills, jobs, activities, auto-tasks, and routines that Orbit itself wrote. Locally modified ones are preserved, not deleted. |
| `--fix-retired-activity-backends` | Removes known retired `spec.backend` values from agent-loop activities. |
| `--fix-automation-pins` | Deletes this Orbit root and workspace's own state-routine attempt pins that no consumer or live run still names. Legacy shared `refs/orbit/automation/<attempt>` pins and other roots' or workspaces' pins are retained and reported. Refuses while a routine sweep runs. |
| `--remove-graph` | Removes retired graph state from this worktree and the shared workspace. |

`--fix-stale-artifacts` is how a workspace catches up after an Orbit upgrade
drops a shipped definition. It works by content provenance — if you edited a
seeded file, Orbit assumes you meant it and leaves it in place.

For routines, the two settings Orbit's own surfaces change are not edits:
flipping `enabled` (the documented opt-in, and what the dashboard toggle
writes) and deleting the retired `hosts:` key. A routine that differs from a
shipped template only in those still counts as Orbit-written, so an upgraded
workspace converges without you moving files by hand. Orbit deletes outright
only bytes it can prove it wrote; anything else it copies to
`.retired-managed/routines/` before removing it from the active catalog.
Changing a routine's cadence, target, policy, or description — or adding a
comment — is a real edit and is preserved and reported.

## Task locks

```bash
orbit task locks list                      # files held by active tasks and reservations
orbit task locks release <reservation_id>  # operator escape hatch
```

Release only after confirming the holding task is genuinely inactive, and only
through this surface — never by editing the store. The full diagnostic sequence
for a reservation blocking a run is in
[common-failures.md](../../orbit-orchestrate/references/common-failures.md).

## Managed resources and upgrades

```bash
orbit workspace sync --check --json   # no writes; exit 3 means pending changes
orbit workspace sync --json           # converge managed defaults
orbit skill list
orbit skill doctor
orbit skill link                      # repair supported user skill symlinks
```

Run sync from an initialized, registered checkout after upgrading the binary.
It reconciles both host-global and workspace-local managed assets, including
skill references, jobs, activities, routines, and auto-task definitions.
Provenance distinguishes untouched shipped content from local edits. Read
`preserved` and `binding_drift` outcomes; a successful sync does not mean a
customized override was overwritten. Never fabricate or edit the managed-asset
manifest to force replacement. Review custom overrides against the installed
catalog and update them deliberately.

Sync updates managed files, not workspace ownership or task publication. The
plugin skill bundle updates through its plugin distribution; global skill
symlinks and a plugin installation are distinct delivery paths.

## Persistent MCP clients during upgrades

`orbit update --contract --json` describes candidate protocol support without
opening state. The updater requires this protocol before replacing an executable;
a missing/incompatible candidate is refused, including pre-fix downgrades.

Use `orbit update --preflight --json` against the configured executable and
same authorities before a wrapper changes the installation. Exit 0 reports
`schema_version: 1`, `admitted: true`, `reservation: false`,
`contract: executable-generation-v1`, and the `admission_roots` it locked;
exit 1 refuses admission on stderr and names the refusing authority.
`--root`, then `ORBIT_ROOT`, otherwise isolated `HOME=` / the host-global
root selects the first authority, reported as `global_root` (scratch init in
a read-only `~/.orbit` sandbox stays unblocked). When an override names
something other than the host-global root, the host-global root is locked
*as well*: the replaced executable is the running host binary, which no root
override moves, and clients started without an override pin the host-global
root. A root override therefore isolates state, not host-binary replacement —
a live client on either authority refuses the upgrade, and so does an
authority whose `.generation.lock` this process cannot write (a read-only
`~/.orbit`, say): the update could never record the candidate there, so it is
refused before anything is staged rather than after the binary is replaced.
`orbit update` admits against that same set for the invocation; a green
preflight is not evidence for an update that would resolve different roots. It opens no runtime or
stores and may create coordination lock files. It is an observation, not a
reservation. `orbit update` reacquires and holds admission through
replacement, then pins the candidate in every locked authority through
convergence. External installers must quiesce clients; a standalone preflight
is not race-free, so preflight plus a raw copy is never a way to deploy a
local build — use `--local-candidate` (below). `orbit update --check` checks releases, not running-client
compatibility.

Participating CLI/MCP processes pin their executable generation for their entire
lifetime. An update refuses while any is live, and a different executable cannot
auto-migrate underneath them. A read-only command whose compiled store schema
equals the live store schema may join that pin without rewriting `.generation.lock`
(`task show`/`list`/`flow`, `run history`/`show`, `search`, `workspace list`/`show`,
`tool list`, `friction list`). The joiner still holds the shared flock, so
`orbit update` stays refused until it exits. Writers, MCP/web serve, `migrate --confirm`,
and a differing schema are still refused; schema equality is exact, not
additive-newer. Additive-newer read-only compatibility still applies
to a matching digest. This covers stdio/operator, TCP listener, federated local,
destination SSH and managed processes without changing their authority. Quiesce
via the owning client/operator and retry; Orbit does not kill sessions, hand off
connections, reclaim claims or replay mutations. For a lost reply, inspect the
durable operation/audit before any retry. Never delete the root's
`.generation.lock` or `.generation-admission.lock` to force admission.

Existing pre-fix processes do not hold these locks: the first installation needs
explicit quiescence and reconnection to the same configured authority. A desktop
restart alone does not prove an unmanaged backend exited. No shadow stores or
ad-hoc MCP servers are part of this contract. Additive-newer compatibility permits
some unaudited CLI reads; MCP tool calls, including workspace discovery, require
durable audit writes and cannot use that read-only fallback.

## Deploying a local build pinned to a source commit

To ship an unreleased fix, build a clean checkout of the full commit SHA and run
the **candidate's** updater so an older installed build is bootstrapped:

```sh
set -eu
SHA='replace-with-the-full-40-or-64-hex-commit'
WORKSPACE='/absolute/path/to/the-intended-workspace'
test -z "$(git status --porcelain)" # start in a clean Orbit source checkout
git fetch --all
git cat-file -e "$SHA^{commit}"
git checkout --detach "$SHA"
test "$(git rev-parse HEAD)" = "$SHA"
test -z "$(git status --porcelain)"
cargo build --release --locked -p orbit-cli
C="$(pwd -P)/target/release/orbit"
"$C" update --local-candidate "$C" --source-commit "$SHA" \
  --write-candidate-manifest ~/orbit-candidate-"$SHA".json
# Quiesce clients, then run from the intended workspace for discovery/convergence.
cd "$WORKSPACE"
"$C" update --local-candidate "$C" --candidate-manifest ~/orbit-candidate-"$SHA".json \
  --source-commit "$SHA" \
  --install-target ~/.orbit/bin/orbit --json
```

Trust is reported literally as `operator_attested` with `signed_release: false`:
Orbit computes the digest from the accepted bytes and the target from the
executable header, but the source commit is the operator's attestation. Release
signature verification is unchanged and never satisfied by a local candidate;
each platform builds its own candidate from the same commit.

`--install-target` must be the managed `orbit` executable (not a symlink, owned
by the invoking user); package-manager or unknown installs are refused. The
update admits the invocation, host-global, and selected workspace roots for the
whole staging, backup, swap and convergence sequence. Workspace discovery
follows the current directory independently of `HOME`; run from the intended
workspace, and use an isolated checkout as well as isolated `HOME` and install
target for smoke checks. The JSON report's `workspace_root` and
`admission_roots` show what was selected and held. The staged copy is what
installs even if the candidate path changes, an equal version with a different
digest replaces, and live MCP/dashboard/clock/drain clients make it refuse
before anything changes — quiesce them, retry, then reconnect. Rerunning the
same command is idempotent and finishes partial convergence; `needs_recovery`
(exit 4) carries the exact retry command in `local_candidate.retry_command`.
See the upgrades runbook for the full procedure.

## Database and layout upgrades

```bash
orbit migrate               # inspect pending migrations without applying
orbit migrate --confirm     # apply
```

Both ledgers — the SQLite schema and the workspace layout — auto-apply when a
runtime opens, so most upgrades need nothing. The bare command and `--dry-run`
are the only way to *see* what is pending without applying it, which is what you
want before upgrading a machine that matters.

## Audit events

```bash
orbit audit list --since 1h --status failure
orbit audit stats --since 7d
orbit audit export --json > audit.json
orbit audit prune --older-than 90d --confirm
```

The audit store is persistent invocation metadata: who called what, when, and
whether it was denied. It grows without bound until pruned. Prune requires
`--confirm`; export first if the history matters.

## Logs

The global JSONL trace at `~/.orbit/state/logs/orbit.jsonl` rotates from
long-lived processes (`orbit mcp serve`, `orbit sweep`, `orbit web serve`)
and when the active file exceeds its budget (one `metadata()` check on first
write). Short-lived commands, including `orbit --help`, do not open the file
or walk the log directory. Defaults: seven days of archives, a 500 MiB
total budget, a 100 MiB active-file threshold. Tune in `config.toml`:

```toml
[runtime]
log_retention_days = 7
log_max_total_mb   = 500
log_max_file_mb    = 100
```

Values are validated at load — zero is rejected, and `log_max_file_mb` may not
exceed `log_max_total_mb`.

```bash
orbit log tail -n 120
orbit log tail --level warn --since 1h
```

For diagnosing a host-level incident rather than tuning retention, see
[operational-logs.md](operational-logs.md).

## Search indexes

```bash
orbit doctor                              # search-index coverage
orbit search reindex                       # rebuild task chunks
```

Indexing is idempotent and safe to re-run. Reindex after bulk task imports or
a restore. → [search.md](../../orbit/references/search.md)

## What is evidence and must not be edited

Files under `.orbit/state/job-runs/`, `.orbit/state/audit/`, and
`~/.orbit/state/` are the record of what happened. Never edit them to make a
warning disappear or a run look successful. If a run is wrong, fix the cause and
re-run; the failed run stays as history.

For a validated task snapshot and same-authority recovery, use
[publication.md](publication.md). It does not back up logs, credentials, or
scheduler state.

Task bundles can also be moved deliberately — `orbit task export` and
`orbit task import` handle portable archives, and `orbit task reindex` rebuilds
the registry index from on-disk bundles after a restore.

For archive migration, `orbit task export --output <archive.tar.zst> --ids <id>,<id>`
selects tasks (omitting IDs exports all). `orbit task import <archive.tar.zst>`
defaults to renumbering collisions and rewriting references; choose
`--on-conflict fail` or `skip` deliberately when appropriate. Inspect the returned
ID mapping. To keep a read-only mirror of another host's tasks in step, use
`--on-conflict owner-wins`: it replaces only bundles whose task-ID prefix belongs
to that host, never touches locally minted IDs, and is safe to re-run. Import is a different operation from publication's strict
same-authority, identical-retry recovery; do not substitute it to bypass a
publication ownership or divergence error.
