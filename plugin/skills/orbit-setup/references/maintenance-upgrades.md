# Maintenance: upgrades

## Contents

- [Managed resources and upgrades](#managed-resources-and-upgrades)
- [Persistent MCP clients during upgrades](#persistent-mcp-clients-during-upgrades)
- [Database and layout upgrades](#database-and-layout-upgrades)

## Managed resources and upgrades

```bash
orbit workspace sync --check --json   # no writes; exit 3 means pending changes
orbit workspace sync --json           # converge managed defaults
orbit workspace sync --all --json     # every workspace registered on this host
orbit workspace sync --check --all    # preview all; writes nothing, exit 3 = pending
orbit skill list
orbit skill doctor
orbit skill link                      # repair supported user skill symlinks
```

Run sync from an initialized, registered checkout after upgrading the binary,
or run `orbit workspace sync --all` once per host from any directory to converge
every registered workspace. `--all` reports `synced`, `already current`,
`skipped` or `failed` per workspace, keeps going past a failure (exit 1), and
skips, never rewrites, a workspace a live drain or run holds; rerun it when the
drain ends.

Single-checkout `orbit workspace sync` does not check for live
runs, so run it only when the workspace is idle. `--all` never reaches across
hosts and cannot be combined with `--workspace`.
It reconciles both host-global and workspace-local managed assets, including
skill references, jobs, activities, routines, and auto-task definitions.
Provenance distinguishes untouched shipped content from local edits.

Read
`preserved` and `binding_drift` outcomes; a successful sync does not mean a
customized override was overwritten. Never fabricate or edit the managed-asset
manifest to force replacement. Review custom overrides against the installed
catalog and update them deliberately.

Sync updates managed files, not workspace ownership or task publication. The
plugin skill bundle updates through its plugin distribution; global skill
symlinks and a plugin installation are distinct delivery paths. The `orbit skill`
commands that inspect and repair the global skill links are in
[Skill management](maintenance-skills.md#skill-management).

## Persistent MCP clients during upgrades

`orbit update --contract --json` describes candidate protocol support without
opening state. The updater requires this protocol before replacing an executable;
a missing/incompatible candidate is refused, including pre-fix downgrades.

Use `orbit update --preflight --json` against the configured executable and
same authorities before a wrapper changes the installation. Exit 0 reports
`schema_version: 1`, `admitted: true`, `reservation: false`,
`contract: executable-generation-v1`, `admission_contract: compatibility-generation-v2`,
and the `admission_roots` it locked;
exit 1 refuses admission on stderr and names the refusing authority.

`--root`, then `ORBIT_ROOT`, otherwise isolated `HOME=` / the host-global
root selects the first authority, reported as `global_root` (scratch init in
a read-only `~/.orbit` sandbox stays unblocked). When an override names
something other than the host-global root, the host-global root is locked
*as well*: the replaced executable is the running host binary, which no root
override moves, and clients started without an override pin the host-global
root.

The initialized workspace that convergence would use is locked too
when it is a different directory — the initialized override, or, when that
override is only an uninitialized generation root, the workspace discovered
from the working directory. `--preflight` probes that uninitialized root without
requiring `orbit workspace init`; `orbit update` itself still refuses it
before convergence. Spellings of the same directory are one authority.

A root
override therefore isolates state, not host-binary replacement — a live client
on any of those authorities refuses the upgrade, and so does an authority
whose `.generation.lock` this process cannot write (a read-only `~/.orbit`,
say): the update could never record the candidate there, so it is refused
before anything is staged rather than after the binary is replaced.

`orbit update` admits against that same set for the invocation; a green
preflight is not evidence for an update that would resolve different roots. It opens no runtime or
stores and may create coordination lock files. It is an observation, not a
reservation. `orbit update` reacquires and holds admission through
replacement, then pins the candidate in every locked authority through
convergence.

External installers must quiesce clients; a standalone preflight
is not race-free, so preflight plus a raw copy is never a way to deploy a
local build — use `--local-candidate` ([below](maintenance-local-deploys.md#deploying-a-local-build-pinned-to-a-source-commit)). `orbit update --check` checks releases, not running-client
compatibility.

Participating CLI/MCP processes hold a generation lock for their entire
lifetime. Admission depends on the update: for a staged local candidate, only
a live long-lived participant that registered a resume capability the candidate
resumes is admitted beside the updater and pinning waits for it to hand
over — in this tree that is a non-remote stdio `mcp serve` whose stdin can be
polled and whose capability is `mcp-stdio-v1` —

and short-lived participants,
including clock ticks and one-shot commands, are waited for; a live dashboard
(`orbit web serve`), a drain (`orbit job run-pipeline-worker`), `orbit mcp listen`,
an `mcp serve` that did not register a capability the candidate resumes, and a
holder that never registered (a v1 binary or a sandboxed child that cannot write
the Orbit root) still block the update and make it refuse before the installed
executable is replaced.

Without a candidate, a live long-lived participant still
blocks. Do not kill an idle handover-capable client to make room. Ordinary v2 runtime admission instead compares store,
layout and feature compatibility: compatible builds may run and apply additive
migrations side by side despite different executable digests. A breaking newer
writer records a pending switch and waits up to 120 seconds by default for
participants to yield at safe points; an incompatible older build is refused.

With a v1-owned generation record, a read-only command whose compiled store schema
equals the live store schema may join that pin without rewriting `.generation.lock`
(`task show`/`list`/`flow`, `run history`/`show`, `search`, `workspace list`/`show`,
`tool list`, `friction list`). That v1 joiner still holds the shared flock, so
`orbit update` stays refused until it exits.

Writers, MCP/web serve, `migrate --confirm`,
and a differing schema are still refused; schema equality is exact, not
additive-newer. Additive-newer read-only compatibility still applies
to a matching digest. This covers stdio/operator, TCP listener, federated local,
destination SSH and managed processes without changing their authority. Quiesce
via the owning client/operator when replacement is refused.

After an installed
executable changes, supported long-lived processes can re-exec at idle boundaries;
MCP stdio preserves its session and drain coordinators reattach to their run.
The clock resumes eligible runs interrupted by the current upgrade at most once,
leaving elapsed, stopped or superseded drains alone. It does not reclaim claims
or replay uncertain mutations. For a lost reply, inspect the
durable operation/audit before any retry.

Never delete the root's
`.generation.lock` or `.generation-admission.lock` to force admission.

Existing pre-fix processes do not hold these locks: the first installation needs
explicit quiescence and reconnection to the same configured authority. A desktop
restart alone does not prove an unmanaged backend exited. No shadow stores or
ad-hoc MCP servers are part of this contract. Additive-newer compatibility permits
some unaudited CLI reads; MCP tool calls, including workspace discovery, require
durable audit writes and cannot use that read-only fallback.

## Database and layout upgrades

```bash
orbit migrate               # inspect pending migrations without applying
orbit migrate --confirm     # apply
orbit migrate --backfill-diff-stats [--limit N]  # apply, then record diff stats for done tasks landed earlier
orbit migrate --backfill-task-crews  # apply, then set task.crew from each task's latest delivery (run or claimed-leaf settlement)
orbit migrate --backfill-dispatch-effort  # apply, then record the launch effort of agent invocations that predate dispatch identities
orbit migrate --backfill-claimed-effort  # on a pull follower: apply, then report to the owner the effort and model of claimed leaves it settled without them
orbit migrate --adopt-authority ~/.orbit  # record this home as the authority protecting a workspace that recorded none (stop its clients first)
```

Both ledgers — the SQLite schema and the workspace layout — auto-apply when a
runtime opens, so most upgrades need nothing. The bare command and `--dry-run`
are the only way to *see* what is pending without applying it, which is what you
want before upgrading a machine that matters.

Layout version 9 fences older Orbit clients that cannot preserve skipped-step
checkpoint markers: older writers drop the marker, and older readers restore a
skipped step's null as an output on resume, changing downstream template
behavior. The layout fence protects the workspace; the marker field itself is
not necessarily rejected by older binaries. Layout version 8 also protects
script output files and parallel script-branch checkpoints.

For a workspace
used from multiple hosts, update every host that can open it before any host
opens or migrates it to version 9; do this as one coordinated rollout and keep
old clients stopped. See [Upgrading a workspace together](multi-host.md#upgrading-a-workspace-together)
for the host-by-host order. On each host, `orbit --version` confirms the
installed binary, while `orbit update --check` only reports available releases
and does not verify layout compatibility.

`orbit update` run inside an
initialized workspace immediately runs `migrate --confirm` followed by
`workspace sync` as post-replacement convergence, so during the coordinated
rollout run the per-host updates from a directory that is not inside any
initialized workspace (with no `--root` or `ORBIT_ROOT` override); the JSON
report showing `"workspace_root": null` with both steps skipped confirms no
migration ran yet.

Use `orbit migrate` to inspect pending
changes, then apply them with `orbit migrate --confirm` when needed. After the
upgrade, use `orbit doctor` on each host and `orbit migrate` to confirm no
migration remains pending.

When Orbit reports that safe SQLite permission repair is unavailable, it
refuses to change database permissions in place. Stop every process using that
database first, including workers, MCP servers, timers, dashboards, and other
Orbit commands. Locate the database and its existing `-wal` and `-shm` sidecars
in the configured state root.

While all users are stopped, restrict the main
file and each existing sidecar to owner-only access: use `chmod 600` for
writable state, or retain `chmod 400` for a read-only publication. Do not
create sidecars that are absent. Check the actual files and their permissions,
then restart Orbit and retry the command. Run `orbit doctor` and `orbit migrate`
afterward to verify the state opens cleanly and no migration is pending.
