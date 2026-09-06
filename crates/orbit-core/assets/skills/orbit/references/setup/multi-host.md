# Workspaces on multiple hosts

A repository checkout, a logical workspace, and a machine's live task store are
different things. Sharing Git history does not synchronize Orbit's control
plane. Select one authoritative owner for a logical workspace and reach it
through MCP when operating its tasks.

## Owners and replicas

`orbit init` establishes a stable machine ID, a display host name, and an
immutable 2–5 uppercase ASCII-letter task prefix. `orbit host show` reports the
identity. A workspace registration records its logical ID, source repository,
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
orbit workspace role <workspace-id> replica
```

`workspace role` validates or reasserts an existing role; it is not a takeover
or arbitrary role-conversion command. A replica must not originate owner-only
task mutations or publish/restore the owner's live task set. Route those
operations to the owner. Copying a source checkout or publication does not
transfer ownership. Conflicting identity/ownership declarations fail closed;
do not delete registry or identity files to get past them.

## What travels

| Git-versioned definitions | Host-local state |
|---|---|
| Workspace config, routines, auto-task templates, resource overrides | Host identity, workspace registry and owner/replica declarations |
| Source and documentation | Task coordination store, locks, reservations, run evidence, scheduler cursors and pauses |
| Dedicated publication repository: explicitly published task snapshots | Publication binding and last-success metadata, audit store, logs, search indexes |

Task publications are the supported explicit snapshot path; ordinary source
Git sync is not live task replication. Publication inspection preserves the
snapshot's authority/freshness labels and does not import records.
See [publication.md](publication.md).

## Task-ID allocation

Give independently allocating hosts distinct prefixes at first initialization:

```bash
orbit init --non-interactive --host-name <name> --task-prefix <PREFIX>
```

Reserved prefixes are refused. The prefix cannot be renamed later. For legacy
hosts that must share a prefix, `workspace init --task-id-start <N>` and
`tasks.id_start` provide a forward-only numeric floor, not a reserved range or
cross-host lock. A shared config file also shares that floor. The ID space is
bounded, so distinct prefixes are preferable to guessed disjoint ranges.

## Scheduling and claims

Routine definitions use explicit `hosts: [<host-id>]`; inspect their actual
seeded names and pins with `orbit routine list`. Definition enablement travels
through Git; last-fire timestamps and pauses do not. Two hosts pinned to the
same routine each evaluate it independently, and `overlap: forbid` is local.

```bash
orbit host rename <current-name> <new-name>
```

Renaming updates the host identity and local owner records, but does not rewrite
versioned routine host pins. Update those definitions deliberately as well.

Workspace claims coordinate operators acting on **the same authoritative
store**. `--claim-token` or `ORBIT_WORKSPACE_CLAIM_TOKEN` presents an existing
claim token; it does not acquire a claim or synchronize independent stores.
Never use a token as justification for shipping the same logical backlog from
two independent owners. Route both operators to the owner, and partition work
through its task selectors and reservations.

## Verify another host

Check host identity, source remote, workspace role/owner, tool capabilities,
routine pins, and the actual executable/version before enabling work there.
`orbit sweep --dry-run` and `orbit doctor` give local operational evidence.
From a client, discover through the authoritative MCP connection and use the
returned workspace selector. For federation, preserve its host qualification.
See [remote-access.md](remote-access.md).
