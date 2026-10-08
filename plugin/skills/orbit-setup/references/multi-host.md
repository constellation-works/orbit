# Workspaces on multiple hosts

A repository checkout, a logical workspace, and a machine's live task store are
different things. Sharing Git history does not synchronize Orbit's control
plane. Select one authoritative owner for a logical workspace and use the CLI
or MCP route to its live store when operating its tasks.

## Owners and replicas

`orbit init` establishes a stable machine ID, a display machine name, and an
immutable 2–5 uppercase ASCII-letter task prefix, written as `[machine]` in the
global `~/.orbit/config.toml`. `orbit config show` reports the identity, and
`orbit config get machine.id` prints one field. A workspace registration records its logical ID, source repository,
owner machine, local checkout, and checkout role.

From the repository being registered:

```bash
orbit workspace init --role owner --base-branch <integration-branch>
orbit workspace show
```

Omitting role retains the compatible local-owner default. On a second machine,
use an explicit replica registration, with the actual owner's machine ID:

```bash
orbit workspace init --role replica --owner <owner-machine-id>
orbit workspace show
orbit workspace role <workspace-id> replica --owner <owner-machine-id>
```

`workspace role` validates or reasserts an existing role; it is not a takeover
or arbitrary role-conversion command. A replica must not originate owner-only
task mutations or publish/restore the owner's live task set. Route those
operations to the owner. Copying a source checkout or publication does not
transfer ownership. Conflicting identity/ownership declarations fail closed;
do not delete registry or identity files to get past them.

## What travels

| Travels through Git | Stays on this checkout or host |
|---|---|
| Source and documentation | Workspace `.orbit/` (gitignored per-user state): config, routines, auto-task templates, resource overrides, plugin pins |
| Dedicated publication repository: explicitly published task snapshots | Machine identity (`[machine]` in the global config), workspace registry and owner/replica declarations |
| | Task coordination store, locks, reservations, run evidence, scheduler cursors and pauses |
| | Publication binding and last-success metadata, audit store, logs, search indexes |

A second host gets the shipped definitions from `orbit workspace init`, not from
Git. Copy any local edits across deliberately.

Task publications are the supported explicit snapshot path; ordinary source
Git sync is not live task replication. Publication inspection preserves the
snapshot's authority/freshness labels and does not import records.
See [publication.md](publication.md).

## Task-ID allocation

Give independently allocating hosts distinct prefixes at first initialization:

```bash
orbit init --non-interactive --machine-name <name> --task-prefix <PREFIX>
```

Reserved prefixes are refused. The prefix cannot be renamed later. For legacy
hosts that must share a prefix, `workspace init --task-id-start <N>` and
`tasks.id_start` provide a forward-only numeric floor, not a reserved range or
cross-host lock. A shared config file also shares that floor. The ID space is
bounded, so distinct prefixes are preferable to guessed disjoint ranges.

## Register and select a host

On the calling machine:

```bash
orbit host add <ssh-target>
orbit host list
```

The add command reads the remote identity, including its task prefix. On the
CLI or federated MCP, an ID-only task show/update routes to the host whose
prefix the ID carries, without an SSH command or `--workspace`. An explicit
workspace selects that store instead; writes still obey the sole-writer rule.
Workspace-scoped operations such as task creation, listing and eligibility do
not route by prefix. The full routed-tool list and typed-error remedies are in
[tool-surface.md](../../orbit/references/tool-surface.md#task-ids-and-host-selection).

For a remote workspace-scoped tool call, use `--host <name-or-machine_id>`
with `--workspace <workspace-name-or-ws_id>` on `orbit tool run`. Orbit reads
the host's live list and copies its matching selector. For a follower drain,
run from the replica checkout:

```bash
orbit run auto --host <owner-name> --pull <workspace-name-or-ws_id> --for 8h
```

Without `--host`, `--pull` requires the full host-qualified selector copied
unchanged from federated discovery. Host-local commands such as `workspace`,
`doctor` and `run show` do not accept this routing flag; use them on the
execution host. Registration, the doctor's `hosts` row and the one-release
legacy migration are in [remote-access.md](remote-access.md).

## Scheduling and claims

Routine definitions carry no host field: every host with a registered owner
checkout and an enabled clock evaluates them against its own store. Inspect
their seeded names with `orbit routine list`. Definitions and enablement are
per-user, gitignored checkout state; copy changes deliberately. Last-fire timestamps and pauses stay host-local. Two hosts running the same
routine each evaluate it independently, and `overlap: forbid` is local — pause
it on the hosts that should not run it.

```bash
orbit config set --global machine.name <new-name>
```

Renaming changes only the display name; `machine.id` and `machine.task_prefix`
are read-only and `orbit config set` refuses both. Routine definitions name no
machine, so nothing versioned has to be rewritten.

Workspace claims coordinate operators acting on **the same authoritative
store**. `--claim-token` or `ORBIT_WORKSPACE_CLAIM_TOKEN` presents an existing
claim token; it does not acquire a claim or synchronize independent stores.
Never use a token as justification for shipping the same logical backlog from
two independent owners. Route both operators to the owner, and partition work
through its task selectors and reservations.

## Verify another host

Check machine identity, source remote, workspace role/owner, tool capabilities,
routine pins, and the actual executable/version before enabling work there.
`orbit sweep --dry-run` and `orbit doctor` give local operational evidence.
From a client, discover through the authoritative MCP connection and use the
returned workspace selector. For federation, preserve its host qualification.
See [remote-access.md](remote-access.md). Matching binaries, a replica role and
a working owner probe are installation; the rollout is an explicit
`orbit run auto --pull <selector>` on the replica. There is no destination
callers file and no follower merge. Command-first setup, the pull drain,
migration and claimed-attempt recovery:
[distributed-drain.md](../../orbit/references/setup/distributed-drain.md).
