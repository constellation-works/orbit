---
type: runbook
summary: Check Orbit workspace, database, dashboard, log-sink, job-run, and routine-clock health.
tags: [operations, health, doctor, dashboard, routines]
paths: ["crates/orbit-cmd/src/doctor/mod.rs", "crates/orbit-core/src/application/job/run/reconcile.rs"]
related_features: [orbit-core, activity-job, routines]
related_artifacts: [ORB-10005, ORB-10070, ORB-10473, ORB-10501, ORB-10558, ORB-10986, ORB-11791, ORB-12109, ORB-12223, ORB-12244, ORB-12968]
last_validated: 2026-10-09
---

# Check Orbit Health

Use this runbook for local diagnosis, readiness monitoring, or verification after a restore,
database recovery, or upgrade.

## Run `orbit doctor`

Without repair flags, `orbit doctor` diagnoses workspace and host infrastructure, definition
artifacts, routed provider CLIs and usage limits, MCP configuration, and the clock unit.
An `info` row is a fact about the setup, neither a pass nor a problem; like `ok`, it is
left out of the findings section. Individual probes
normally report a row on failure; runtime/store startup failures can prevent a report.
Default database checks read the header and schema ledger without scanning data pages.
Use `orbit doctor --deep` after a restore or when corruption is suspected to also run
SQLite `PRAGMA quick_check`; it reads the entire store and can take much longer on a cold cache.

| Check | What it reports | Status and next step |
|---|---|---|
| `config` | Effective global + workspace configuration | `ok` when valid; `warning` for ignored crew properties or lanes pointing at disabled crews (possibly several rows); `error` for selection/parse failures. Correct the named config field. |
| `database` | Store DB readability and schema-ledger version versus this binary; `--deep` adds `PRAGMA quick_check` | `ok` when readability and version match (and integrity passes with `--deep`); `warning` for an older schema or unreadable ledger; `error` for open/integrity failure or a newer schema. Follow the diagnostic; newer schemas need a newer binary. |
| `disk-space` | Free space on the volume holding the local `.orbit` | `warning` below 1 GiB or 5%, or when the probe fails; `error` below 256 MiB or 1%. Free space on that volume. |
| `search-index` | Lexical index chunk count and indexed-task count versus stored-task count | `ok` when task counts agree, including an empty workspace; `warning` on a count mismatch or read failure. A mismatch names `orbit search reindex`; this check does not inspect embeddings or prove freshness when counts agree. |
| `stale-locks` | Immediate `*.lock` files in the workspace-local `state_dir` whose recorded holder PID is dead | `ok` with the number scanned when none are stale; otherwise `warning` with holder details and `orbit doctor --fix-stale-locks`. No recursive scan of task, learning, or ADR trees. |
| `job-runs` | Orphaned `pending` or `running` runs with no live worker process | `warning` for orphans or inspection failures; otherwise `ok`. Follow the named run's resume/cancel guidance; see [stuck runs](./stuck-job-runs.md). |
| `pull-protocol` | The latest follower pull drain's typed protocol skew, including a failed terminal drain | `warning` when its request fingerprint differs from the owner's or inspection fails; otherwise `ok`. Deploy matching owner/follower builds, restart long-lived processes and start a new drain. A newer healthy window supersedes the warning. |
| `pull-settlements` | Replica leaf outcomes recorded locally but not delivered to the owner, including count and oldest age | `warning` while delivery is owed or inspection fails; otherwise `ok`, including workspaces that never pulled. Once the owner is reachable, `orbit run auto --stop` flushes pending delivery ([distributed drain](./distributed-drain.md#claim-inspection-and-manual-recovery)). |
| `task-reservations` | Active reservations whose owner run or terminal task association proves inactivity | `warning` for conclusively stale reservations or inspection failure; otherwise `ok`. `orbit doctor --fix-stale-task-locks` rechecks before releasing; see below. |
| `task-relations` | Relation/dependency targets in canonical task bundles that resolve to no registered task and so block a task-index rebuild (task reads then serve from a bundle scan) | `warning` for dangling targets or audit failure, naming each edge's source, relation type and target and marking edges the generated index no longer holds; otherwise `ok`. Drop the edge through `orbit.task.update` `relations` or restore the target; reads repair the index without a restart. |
| `infra-blocked-tasks` | Tasks blocked by a missing provider launcher, distinguishing launchers that now resolve from ones still missing | `warning` for either group or classification failure; otherwise `ok`. Install a still-missing launcher, then use `orbit task recheck-blocked --confirm` to requeue cleared infrastructure blocks. |
| `blocked-task-recovery` | Final-recovery backstop: pending, human-intervened, too-old, unattributed, and decided block episodes | `skipped` for claimed workers, replicas, or empty `workflow.final_recovery_crews`; `warning` on inspection failure or tasks still blocked after final recovery; otherwise `ok` (pending recovery alone does not warn). Read each named task's `final_recovery` comment before a manual decision. |
| `automation-consumers` | Stalled/wedged delivery consumers and enabled definitions with an unresolvable branch or an owner this host cannot serve | `warning` for findings or inspection failure; otherwise `ok`. Follow the row's recover/reset, branch, ownership, or disable guidance; disabled definitions do not trigger branch/ownership findings. |
| `review` | Before-PR and after-landing review switches, sources, crew/minutes, and next batch due | `error` if inspection fails or enabled review cannot run here: incompatible local ship route, missing crew, or an unhealthy after-landing consumer (missing, wrong owner, stalled, wedged, held, unresolved branch/crew, or remote-tracking lag beyond its wait budget). Otherwise `ok`, including disabled review. Fix the named review configuration/consumer; this probe does not fetch. |
| `forge-remote` | Whether a workspace registered for PR delivery has a Git remote naming a network host (`git remote -v`, after `insteadOf` rewriting) | `skipped` when the workspace is not registered for PR delivery; `warning` when no remote names a network host, naming the remotes. Admission refuses such PR deliveries before any run starts. Run `orbit workspace ship-mode local`, add a remote on the forge host, or tag one task `delivery:task_local_pipeline`; otherwise `ok`. Any network host counts, since GitHub Enterprise hosts and SSH aliases are known only to `gh` and `ssh`. |
| `host-shutdown` | Scheduled host shutdown/reboot, with mode and time (Linux logind's `/run/systemd/shutdown/scheduled`) | `warning` while a schedule holds unattended admissions; otherwise `ok`. Admissions resume after it clears; see [scheduled shutdown](./distributed-drain.md#scheduled-host-shutdown-or-reboot). |
| `env-pass` | Operator-added `execution.env.pass` names this process's environment does not hold (unset or empty); names only, never values. The built-in defaults are not reported | `warning` naming each unset variable; otherwise `ok`. Agents inherit the launching shell's environment, so start drains from a login shell or export the variable in the service's environment, or drop the name from `execution.env.pass`. `orbit run auto`, `run ship` and `run job` print the same warning at start and record it on the run. |
| `claude-worker-token` | macOS only, when a routed crew uses `claude`: the workspace's effective `execution.env.pass` names `CLAUDE_CODE_OAUTH_TOKEN` (or `ANTHROPIC_API_KEY`), and, with the sweep clock installed, `~/.orbit/clock.env` holds one that the same `pass` names. A `clock.env` holding only a name the `pass` omits warns as a mismatch. Names only, never values | `warning` naming what is missing; `skipped` off macOS or with no routed Claude crew; otherwise `ok`. Without the token `claude` falls back to the Claude Desktop login, which the Desktop revokes mid-run, and Orbit refuses such activities. Add the name to `pass` and to `clock.env` (mode 600); see the orbit-setup macOS reference. |
| `validation-env` | Required-validation PATH source, login-shell probe mode/fallback, and `python3`/`git`/`make` locations and shadowing | `skipped` when `workflow.required_validation_commands` is empty; `warning` for probe/preflight/fallback or shadowing concerns; otherwise `ok`. Correct shell startup or `workflow.validation_env` as advised. A listed missing tool alone does not change the status, and this row does not execute validation commands. |
| `orphan-task-stores` | Host-global task-store partitions and their live, missing, unreachable, or absent checkout claims | `skipped` when no partitions exist; `warning` for findings or inspection failure; otherwise `ok`. Reindex unowned populated stores, restore unreachable checkouts, or use the confirmed repair only for removable partitions; see below. |
| `tracked-orbit-files` | Git-tracked paths under `.orbit/` | `warning` when tracked (`git rm -r --cached .orbit`); `ok` when none; `skipped` outside Git or when Git cannot answer. |
| `plugin-builds` | Host source-built plugin provenance and build drift | `skipped` when no plugin was built from source; `ok` for healthy or intentional findings; `warning` for actionable drift or inspection failure. Follow the plugin row's step and rerun `orbit plugin doctor`. |
| `empty-task-stubs` | Empty task-bundle directories, or ones holding only `.task.yaml.lock`, that never received `task.yaml` | `warning` for stubs or scan failure; otherwise `ok`. `orbit task reindex` clears stubs. Data-bearing directories are handled by the next row. |
| `unresolved-task-bundles` | Directories missing `task.yaml` that still hold bundle content (`events.jsonl`, `artifacts/`, …) | `warning` for retained unresolved data or scan failure; otherwise `ok`. Restore `task.yaml` or move the directory aside; reindex will not delete it. |
| `artifacts-skills`, `artifacts-jobs`, `artifacts-activities`, `artifacts-auto-tasks`, `artifacts-routines` | One row per definition kind: missing shipped defaults, stale, forked, deprecated, residual, or faulty catalog content; the skills row also inspects provider discovery links. An auto-task file that differs from its bundled default only in operator settings is `stale` (`orbit workspace sync` moves the settings into the settings table); one with body edits is `forked`, naming the differing fields, and is preserved | `skipped` for no files/findings, `ok` for healthy content, `error` only for an unloadable/missing shipped default, `warning` for other findings. Each finding names its remedy; use `orbit workspace sync` for shipped-default convergence and the scoped repair/manual edit when advised. |
| `artifacts` | Fallback when the definition-artifact inspection itself fails | One `warning` replaces the per-kind rows. Resolve the runtime/store error and rerun doctor. |
| `state-directory-permissions` | Unix group/world write bits on Orbit-owned state directories | `warning` for writable directories (`orbit doctor --fix-state-directory-permissions`); `error` for resolution/inspection failure; otherwise `ok`. `skipped` on non-Unix platforms. See scan and repair scope below. |
| `git-protection` | The Linux/macOS sandbox's Git metadata scan on the registered checkout: symlinks, special files and hard links in `.git`, `objects/` excepted for Git's own `tmp_obj_*`, `tmp_pack_*` and `tmp_idx_*` names linked only inside the store | `skipped` on other platforms; `warning` naming the refused entry and the fix (delete a leftover temporary file, run `git fsck`) or the inspection failure; otherwise `ok`. A drain on a host with an OS-sandboxed executor runs the same scan before it is submitted and is refused with that message. |
| `provider:<crew>` | One row per enabled crew selected by default/system/positive-weight complexity routing: executor definition and CLI lookup | `ok` when the CLI resolves (authentication is **not** checked); `skipped` for an executor without a CLI command; `error` for missing crew/executor/CLI or inspection failure. Restore the named dependency or change routing. Disabled crews are omitted. |
| `provider-routing` | Fallback when effective routing config or a complexity crew pool cannot be inspected | One `error` replaces the per-crew rows. Repair the named configuration. |
| `provider-limits:<provider>` | One row per provider an enabled crew uses: this host's live usage readings against the provider's `workflow.provider_limit_*` threshold | `ok` below the threshold, listing the readings; `warning` while a reading gates the provider, naming the window, use, reset and skipped crews (admission resumes them after the reset); `info` (`no usage signal; Orbit learns limits from failures`) for a provider that reports no usage windows and has no `workflow.provider_limit_budgets` entry (a budgeted provider lists its ledger reading). See [provider usage limits](../CONFIG.md#provider-usage-limits). |
| `provider-limits:workflow.system_crew`, `provider-limits:operation.review_crew` | That lane's crew uses a provider a reading gates | `warning` with the reset. These lanes are not gated, so their runs may fail until then; wait, or point the setting at a crew on another provider. A single `provider-limits` `warning` replaces every row when the store cannot be read. |
| `mcp-registration` | Recognized client configuration for this workspace | `ok` when a registration is found (connection is **not** checked); otherwise `warning` naming `orbit mcp init --auto` or `orbit mcp init --client <client>`. |
| `mcp-callers` | Retired destination-side caller-authorization files under the user's `~/.orbit` | `warning` when inert files remain; otherwise `ok`. Remove the obsolete files; SSH keys govern remote access. The row is omitted when the home directory cannot be resolved. |
| `clock-unit` | Installed launchd/systemd sweep unit's invocation, program path, and `--version` | `skipped` when no unit is installed; `ok` when matching; `warning` for a different path, stale invocation, unrunnable program, or inspection failure; `error` for version mismatch. Follow the unit-file/access remedy or `orbit clock repair`; this row does not establish native timer readiness. |
| `store-retention` | Reclaimable audit rows, terminal-run pipeline state, candidate audit blobs, and SQLite file/free-list space under configured retention windows | `ok` when measured; `warning` if inspection fails. This is a read-only estimate. Plan audit and run cleanup with `orbit gc audit` and `orbit gc runs`, then pass `--apply` to delete. The blob amount is an upper bound until the audit reference scan. |

The skills check also scans symlinks directly under `.agents/skills` and `.claude/skills`
beside the selected global root (`~/.orbit` by default, or the root selected with
`--root` / `ORBIT_ROOT`). A missing link target produces a warning naming the link
path and advising you to restore the target or manually remove that dangling symlink.
`orbit skill doctor` reports the same warning; `orbit doctor --json` and the dashboard
Health tab carry it in the skills row. Diagnosis leaves discovery entries and targets
untouched, and `--fix-stale-artifacts` does not remove these links.

The state-permission probe checks the global and workspace Orbit roots themselves, then
recurses through global `state/`, `tasks/`, `cache/`, `frictions/` and workspace `state/`,
`tasks/`, `frictions/`. Missing roots are ignored. The global and workspace owner roots
are resolved first; child symlinks, including linked subtree roots, are not followed.
The scan excludes `state/worktrees/` and any
`target/` directory, including those directories' own modes: Orbit does not own run
checkout contents or Cargo output. Entries disappearing during a walk are skipped; other
access errors still report an error. Warnings count writable Orbit-owned directories and
group them by distinct scanned roots so an operator can find the actionable subtrees.
To repair legacy writable directories in one pass, run:

```sh
orbit doctor --fix-state-directory-permissions
```

On Unix, this sets each reported writable directory to `0700` and then runs
the ordinary diagnostics. It uses the same roots and exclusions as the probe:
run worktrees, Cargo target trees, child symlinks and files are untouched.
Every path component is opened without following symlinks, and permissions
are changed through the opened directory handle, so a symlink swapped in
after scanning cannot redirect the repair. Missing directories are skipped;
other access errors abort the repair and may leave earlier directories repaired.
Running it again is safe and changes nothing when no writable directories remain.
The flag is CLI-only and refuses on non-Unix platforms.

The separate filesystem-lock probe scans only immediate
files in `state_dir`; on non-Unix platforms holder PIDs are conservatively treated as alive.

Explicit repair flags prepend these additional rows before the diagnostics. They are not
emitted during ordinary diagnosis; a repair error can abort the command before a report.
Do not run a repair merely to collect health evidence.

| Conditional check | Emitted after a successful repair |
|---|---|
| `fix-stale-locks` | `ok`, number of dead-holder records cleared (lock files preserved) |
| `fix-stale-task-locks` | `ok`, number of conclusively stale reservations released |
| `fix-state-directory-permissions` | `ok`, number of writable Orbit state directories restricted to owner-only access |
| `remove-graph` | `ok`, number of retired graph locations removed |
| `fix-stale-artifacts` | `ok`, number of deprecated Orbit-written artifacts retired |
| `fix-retired-activity-backends` | `ok`, number of activity files repaired; skipped files are reported on stderr |
| `fix-orphan-task-stores` | `ok`, empty/populated partition and deleted bundle counts; requires `--confirm` |
| `fix-automation-pins` | `ok`, reclaimed owned attempt pins and the in-flight, live-run, assessed, legacy and foreign-owner pins retained; `warning` when ownership was refused while owned pins exist or a pin could not be deleted (see [below](#release-leaked-automation-attempt-pins)) |

Example excerpt (other checks omitted; counts vary by workspace):

```text
$ orbit doctor --format table
│ CHECK         STATUS    DETAILS                                           │
│ database      ok        database readable (integrity scan: orbit doctor --deep); schema version 37 matches this binary │
│ search-index  ok        26430 chunks, 3707 indexed tasks / 3707 stored tasks │
│ stale-locks   ok        3 lock file(s) scanned, none stale                  │
…
```

The command exits nonzero only when at least one check is `ERROR`; warnings and skips exit
zero. `--json` emits an array of objects with `check`, `status`, `message`, `remediation`, and `duration_ms`
fields; statuses are lowercase and `remediation` is `null` for healthy/skipped rows. Human
output prints the same guidance as an `Action:` line and includes elapsed seconds on rows
that take more than one second. Durations include unsuccessful and skipped probes; multiple
rows emitted by a single probe share its duration, so row durations are not additive.
Lock files flagged by `stale-locks`
include holder diagnostics. Run `orbit doctor --fix-stale-locks` to clear a dead holder's
record after acquiring the advisory lock and rechecking the holder. The repair preserves
the lock file so queued openers and new openers share the same inode; do not delete it.

### Repair stale task reservations

The `task-reservations` check is read-only and intentionally narrower than task-lock listing.
It warns only when Orbit can prove one of these conditions:

- the recorded owner run no longer exists;
- the recorded owner run is terminal;
- the existing run-owner classifier proves a pending/running owner orphaned; or
- an unowned reservation has one or more associated tasks and every one is `done`, Orbit's
  terminal task status.

Fresh reservations, reservations owned by live or inconclusively probed runs, and unowned
reservations with empty, missing, mixed, or non-terminal task associations remain untouched.
Each warning names the `reservation-…` id, its task/run context, the stale reason, and the exact
repair command:

```sh
orbit doctor --fix-stale-task-locks
```

The repair re-reads and reclassifies each candidate immediately before releasing it, uses the
normal task-lock release audit path with the `doctor_stale_task_lock` reason, and is idempotent.
This is distinct from `--fix-stale-locks`, which handles dead-holder filesystem `.lock` files.

There is deliberately no blanket `--fix` or resolve-all option. Configuration repair, database
recovery, job cancellation, graph cleanup, id-allocation retirement, filesystem holder-record cleanup,
task-reservation release, retired activity-backend cleanup, orphan task-store deletion, and
automation attempt-pin release have different evidence and safety gates, so each repair remains
explicit and safety-scoped.

For definition convergence after installing a new Orbit binary, or to restore a shipped
default that was deleted by hand, use `orbit workspace sync` rather than a doctor repair.
Sync uses managed manifests to create newly shipped definitions, refresh or retire only
provably unedited Orbit-written instances, migrate legacy routine provenance, and preserve
operator content. `orbit workspace sync --check` is the read-only fleet inspection form.
Doctor reports a missing shipped default as an `artifacts-*` error (it does not consult the
warm-open defaults stamp, and it does not rewrite the file); run `orbit init` or
`orbit workspace sync` to restore it. Doctor performs only the specific repair named by an
individual flag; neither command upgrades the binary or pulls a remote repository.

### Reclaim orphaned task stores

`orbit workspace teardown <workspace> --confirm` requires an explicit registered name,
`ws_*` id, or absolute checkout path — it never infers the target from cwd. It prints the
resolved catalog name and id, checkout root, and task-store partition path, then deregisters
the workspace and deletes both the checkout's `.orbit/` and the global task-store partition
its task state is bound to, retiring that partition's rows in `~/.orbit/tasks/index.sqlite`
in the same step. The summary names the catalog workspace the deleted partition belonged to.

A partition directory is named for a **task-store partition id**
(`workspace_bindings.workspace_id` in `~/.orbit/tasks/index.sqlite`), minted as `<slug>-<hash>`
for a checkout that binds without an explicit id. That is a different namespace from the
workspace-registry id in `~/.orbit/workspaces.json`: `orbit workspace init` passes its catalog
`ws_*` id in as the partition id, so those workspaces spell both the same, while a legacy
`<slug>-<hash>` partition and the synthetic `ws_unbound-data-dir` partition have no catalog row at
all. Partition ownership is therefore always resolved through the task registry. A task-registry checkout claim is live while its recorded `repo_root`
is present on disk; a catalog checkout supplies the same per-checkout evidence when `workspace init`
has only a path-free task-registry registration. The shared external Orbit root (`--root
<data-dir>`) is not used as checkout evidence because several checkouts may share it. A missing
checkout beneath a populated, readable immediate parent is reported as stale. An unreadable path,
an empty parent, or a missing parent is reported as unreachable. Checkoutless
catalog entries and the synthetic `ws_unbound-data-dir` partition every `--root <data-dir>` write
lands in remain claims. `orphan-task-stores` names each
reported workspace id, its partition path, and its task-bundle count.

**Evidence rule for deleting task bundles.** A partition that holds task bundles is deleted by the
repair only when its bound checkout is *confirmed gone*: `repo_root` stats as absent, and the
immediate parent directory can be fully listed, contains other entries, and has no entry for the
checkout. An empty parent may be the directory exposed by an unmounted volume; a missing parent
may be a removed mount point. Neither confirms deletion, and doctor never walks farther up to
infer absence from a different ancestor. Any other filesystem answer — `EACCES` from an unsearchable parent, `EIO` or
`ENOTCONN` from a dropped mount, a path that resolves through a non-directory — classifies the
binding as **unreachable**: the checkout may be intact behind the failure, so the partition is
retained, reported with the failing path and error, and never deleted by
`--fix-orphan-task-stores`. A partition with **no task bundles** carries nothing to recover, so the
older rule still applies to it: any binding that is not a live claim reclaims the directory.

An unclaimed partition that **still holds task bundles** and has no checkout binding at all is
likewise never deleted by the repair. On disk it is indistinguishable from a live checkout's
partition whose registry row was lost, and every `orbit` subcommand recreates an empty
`~/.orbit/tasks/index.sqlite` before any check runs — so a lost or restored-without `index.sqlite`
registry makes every checkout other than the one you are standing in look abandoned. That warning
points at recovery instead:

```sh
cd <the checkout that owns the bundles>
orbit task reindex
```

`orbit task reindex` rebuilds the registry rows for one partition from its bundle directories, so
it must be run once per affected checkout.

For an **unreachable** partition, restore access first — remount the volume, repair the directory
permissions that hide the checkout — and re-run `orbit doctor`. A reachable checkout becomes a live
claim again and the row clears itself. A checkout that is genuinely gone beneath a populated,
readable immediate parent becomes a confirmed-stale binding, which the repair can then remove.
A deleted checkout whose parent is empty or also deleted stays unreachable; reclaim its task
partition only as a deliberate manual step, or use `orbit workspace teardown` before deleting
the checkout.

For a partition whose binding is confirmed stale, the missing `repo_root` is the evidence that the
checkout is gone. The confirmed repair removes that partition, including its task bundles, and
retires the stale registry rows — as it does for every empty unclaimed partition.

**Recovery ordering for a deleted checkout.** Doctor can already classify the partition as stale
while the workspace is still in the catalog (the catalog checkout's `repo_root` is the evidence).
You may then either run the repair immediately, or first deregister the dead workspace:

```sh
ORBIT_OPERATOR=1 orbit workspace remove <workspace-name-or-id-or-absolute-checkout-path>
```

`workspace remove` changes only the catalog; it does not delete `.orbit` or task bundles. It
**retains** the task-registry workspace binding and copies the catalog checkout into that
registry so the leftover partition stays stale rather than flipping back to a claimed imported
archive. A populated leftover is reported with its path, bundle count, and the reclaim command
below. After removal, `orbit doctor` still warns on that partition; it must not report `ok`.

```sh
orbit doctor --fix-orphan-task-stores --confirm
```

Either order works: repair while the catalog still lists the checkout, or `workspace remove`
then the same repair. Skipping the repair after `workspace remove` leaves the bundles on disk
and unreachable through workspace selectors.

Do not use that repair for an unknown or unreachable populated partition; reindex or restore it
first. Empty and removed mount points are preserved, but bindings contain paths rather than a
persisted mount identity. A mount point that exposes other entries after unmounting can still be
mistaken for a populated checkout parent. Remount before running the repair on a host with
removable or network-mounted checkouts; directory contents alone cannot prove a volume is mounted.

The repair deletes partition directories, so it refuses to run without `--confirm` before any
other requested repair runs, leaving their repair targets unchanged. It resolves the
claims once, deletes empty unclaimed partitions and populated partitions whose checkout is
confirmed gone, retires any registry rows naming them, and is idempotent: running it again after a partition is gone is a
no-op.

### Repair retired activity backends

`artifacts-activities` uses the same load and tool-allowlist path as production activity
catalog construction, including workspace-local `.orbit/resources/activities/` files. A
schemaVersion 2 `agent_loop` activity that still declares `spec.backend: http` or
`spec.backend: auto` is a warning that names the file, the rejected field/value, the catalog
parse error, and one opt-in repair:

```sh
orbit doctor --fix-retired-activity-backends
```

The repair deletes only that obsolete `spec.backend` key, leaves unknown backend values and
unrelated malformed activities untouched (and reports them for a manual edit), and is
idempotent across every activity catalog directory in the workspace. Each configured catalog
directory is a root even when it is a link, but nothing below it is followed: a linked `.yaml`
file, a linked directory, or a special file such as a FIFO is reported by doctor and listed as
skipped by the repair, and its target is never read or rewritten. To have such an activity
checked and repaired, replace the link with a regular file inside the catalog.

### Release leaked automation attempt pins

Admitting a state-routine attempt (the task-pilot routine) pins the commit it froze in a
namespace owned by this Orbit root and workspace, `refs/orbit/pins/v1/<owner digest>/<attempt id>`,
whose owner record is `refs/orbit/pin-owners/v1/<owner digest>`. Settlement, terminal failure
or retirement releases the pin unless an accepted pre-upgrade `material_v1` assessment still
needs that revision for its compatibility check. A crash between the checkpoint and the
release, or a release refused for want of ownership proof, can leave an owned pin behind.

Earlier releases pinned every attempt in one shared namespace,
`refs/orbit/automation/<attempt id>`, and never released them. Those legacy pins record no
owner: another Orbit root or an earlier client sharing the repository may still need one, so
Orbit never deletes them. Count each kind with:

```sh
git for-each-ref 'refs/orbit/pins/v1/' | wc -l          # owned, all roots and workspaces
git for-each-ref 'refs/orbit/automation/*' | wc -l      # legacy shared attempt pins
```

and reclaim this root and workspace's unreferenced pins with:

```sh
orbit doctor --fix-automation-pins
```

The repair holds the routine sweep lock, so it refuses while a sweep is evaluating consumers;
rerun it after the sweep finishes. It first requires this namespace's owner record to match
this root, workspace, machine and Git directory; a missing or conflicting record refuses and
retains every owned pin. It lists the owned pins, then inventories this owner's consumers and
this workspace's pending or running task-pilot runs. A pin is kept when an attempt is still in
flight or retrying, a live run carries it, or an accepted assessment names it (an assessment
certified before the material fingerprint became configurable is carried forward through that
pin). Every other owned pin is deleted only while it still names the commit it was listed
with, so a ref repointed meanwhile survives and is reported. Any inventory failure deletes
nothing, and the repair is idempotent.

Everything else is retained and reported, never deleted: legacy shared pins, the namespaces of
other Orbit roots and workspaces sharing the Git directory, delivery batch pins
(`refs/orbit/automation/<consumer digest>/<batch>/…`), and refs that are not 64-hex attempt
ids. Legacy pins can only be deleted by hand once every root and client that shares the
repository is known not to need them. The row is a warning when ownership was refused while
owned pins exist or a pin could not be deleted.

Graph is retired under the "Retire and delete Orbit's code-graph subsystem" decision ([ORB-10491]) and is not inspected by ordinary health checks. To remove
leftover state explicitly, run `orbit doctor --remove-graph`. This deletes only the current
worktree's `.orbit/graph` and the shared workspace's `.orbit/knowledge/graph`, and only when
each path stays inside its resolved root. An intermediate symlink is not followed: a missing
target is left alone, and a target that exists is reported and preserved, including files
outside the root. A symlink at the final `graph` component is unlinked without being followed.
The command is idempotent when either location is absent. Combine it with `--json` for a
single JSON result with no cleanup prose on stdout. Without `--remove-graph`, `orbit doctor`
leaves both locations untouched.

## Probe dashboard health

The loopback-only dashboard (`orbit web serve`, default `127.0.0.1:7878`) exposes liveness
and readiness:

```sh
curl -s localhost:7878/healthz                       # -> "ok"; cheap liveness, always 200
curl -s 'localhost:7878/healthz?detailed=true' | jq  # readiness; HTTP 503 if any check fails
```

Example detailed response:

```json
{
  "status": "ok",
  "workspaces_open": 1,
  "checks": [
    {"name": "sqlite_writable", "status": "ok",   "detail": "store database accepts writes", "workspace": "default"},
    {"name": "log_sink",        "status": "ok",   "detail": "~/.orbit/state/logs/orbit.jsonl accepts appends"}
  ]
}
```

Each detailed check is time-bounded to two seconds and runs per workspace.
`sqlite_writable` executes `BEGIN IMMEDIATE; ROLLBACK` without mutation. Point uptime
monitoring at the detailed form.

## Check the host clock

Routines are Orbit's scheduling surface. Install or refresh the host clock with
`orbit routine init --install-clock`. Verify the native clock separately from routine due
state:

```sh
orbit clock status
orbit routine list
orbit clock tick --json
orbit doctor
```

`orbit clock status` prints the unit's program path and the version that program
reports next to `platform:`. A version that does not match this binary is flagged on the
same line as `mismatch: running <version> at <path>`. A unit that still invokes the
compatibility alias `orbit sweep` is reported as stale. `orbit clock status --format json`
emits the same report as one JSON object: the clock's `state` (`enabled`, `paused`, or
`unhealthy`), cadence, platform, and health fields, plus a `program` object with the unit's
program path, version, and comparison `verdict`. `orbit doctor`'s `clock-unit` row is the
same comparison in check form:

- **ok** — the unit invokes this binary (canonical path and version match)
- **warning** — the unit's program path differs but `--version` matches (two installs; the
  clock can drift), or the named program is missing/unrunnable
- **ERROR** — the unit's program reports a different version than this binary. The row names
  the unit file, both program paths, and both versions. Rewrite the unit with
  `orbit clock repair`, or repoint the package-manager install the unit
  names so it is this version
- **skipped** — no launchd plist or systemd user service is installed

`orbit clock repair` is the repair for every one of those rows. It rewrites the installed unit
to this binary and re-registers it with launchd/systemd, prints what it changed, and exits
non-zero when the manager would not reload the rewritten unit. A unit that already names this
binary is left alone, and a paused clock is corrected on disk without being resumed — use
`orbit clock enable` to resume it.

A failed reload is remembered in `~/.orbit/clock.reload-pending`: re-running `orbit clock
repair` (or `orbit update`) retries the `launchctl load` / `systemctl --user restart` even
though the unit file already names this binary, reports `reloaded` once the manager accepts
it, and keeps exiting non-zero until then. `orbit clock enable` and `orbit clock pause` clear
the pending retry — the operator's explicit choice wins over a repair that is still catching up.

Operators do not have to reach for it after an ordinary upgrade: `orbit update` runs
`orbit clock repair` as its last convergence step, so a unit orphaned by an install at a new
path is repaired in the same command that moved the binary. The path that still needs a
manual run is a binary installed by something other than `orbit update` (`brew`, `cargo
install`, a checkout build) while the unit names the old one.

A hand-run `orbit sweep` / `orbit clock tick` from a binary the unit does not name prints one
warning line to stderr naming the unit, the program it runs, and this binary. The scheduled
pass never prints it — it *is* the program the unit names — so seeing it means unattended
sweeps are running a different build than the one you just invoked.

`orbit update --check` reports the binary on `PATH` / the cargo install; it does not inspect
what the clock unit invokes. After an upgrade, run `orbit doctor` or `orbit clock
status` before assuming unattended sweeps are running the new binary.

A sweep that discovers workspaces but fails to open every one of them exits non-zero and
prints a single `sweep.no_workspace_loaded` row naming this binary's version and the first
load error. Partial load errors still print one `load error […]` line per workspace and
exit 0. An unconfigured host (no registered workspaces) remains a clean no-op.

On Linux, a healthy enabled status includes an active timer, a finite next systemd trigger,
and an effective cadence. `clock: unhealthy` with an inactive effective cadence means the
timer is enabled but elapsed, unscheduled, inactive, or could not be probed. Inspect the
printed diagnostic, then run `orbit clock enable`: it rewrites stale installed service and
timer files if needed, restarts the timer even when it is already enabled, and returns success
only after verifying a finite next trigger. (`orbit clock enable` resumes a paused clock;
`orbit clock repair` only repoints a unit whose program moved, and never resumes one.) If that verification fails, inspect the
`systemctl --user status` and `journalctl --user` commands in the error rather than repeating
enable. The generated timer schedules its first sweep from every timer activation and then
recurs from service activation; installation and cadence changes perform the same
post-activation verification.

Systemd enablement and runtime activity can differ. A disabled timer that is still active is
reported as `unhealthy` with `running: true` and its next trigger when known. Run `orbit clock
pause` to stop it. Pause checks the resulting inactive state and absence of a future trigger
before reporting success; an unavailable manager or ambiguous state makes pause fail. The
dashboard also marks this state unhealthy and offers **Pause clock**.

On macOS, launchd keeps reporting a loaded agent as loaded long after its program stops
working, so `clock: enabled` is not on its own evidence that sweeps are firing. An enabled
launchd clock is `unhealthy` with an inactive effective cadence when the plist names a
program that cannot report a version (the same detection behind `orbit doctor`'s
`clock-unit` row), when `launchctl print gui/<uid>/com.orbit.sweep` reports a non-zero
`last exit code` for the most recent run, when that dump lists `penalty box` under
`properties`, or when the dump cannot be read at all. Each case prints the reason and the
same recovery: `orbit clock enable` rewrites the unit to this binary and reloads it. The
dashboard's Host Sweep Clock card shows the same states — a degraded clock is badged
`missed`, not `healthy`, even though the launchd service itself is still enabled.
It does not replay every tick missed during host or manager downtime: on the next sweep,
routine `missed_run: catch_up_once` fires once for a gap while `skip` waits for the next natural
cron slot.

If an `overlap: forbid` routine remains `overlap_in_flight` after a restart, run one explicit
`orbit clock tick --json` and inspect the referenced run. The tick releases a dispatched in-flight
fire immediately only when the recorded owner process is conclusively gone; a live or
unprobeable owner remains protected until terminal or until the routine timeout. Use
`orbit doctor` and the stuck-job-run runbook below when the run itself remains orphaned.

The dashboard also exposes `GET /api/routines`. `qa-sweep` and other auto-task
definitions are ordinary workspace data under `.orbit/auto_tasks/`, evaluated in-process
by the host tick. It mints tasks from every due, enabled definition and creates no
scheduler run; it does not itself edit docs, design records, learnings, friction records,
or comments.

## Verification and escalation

Record the exact failing check, full details, binary version, config path, workspace, and
relevant environment overrides before diagnosing. Use
[Recover a corrupted database](./database-recovery.md) for integrity failures and
[Recover stuck job runs](./stuck-job-runs.md) for orphaned-run findings.
