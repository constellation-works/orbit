---
type: design
summary: "Spec: operator-registered remote hosts — the host file, orbit host add/list/show/rename/remove, dashboard Settings › Hosts, live identity probe, migration from mcp-destinations.toml"
last_validated: 2026-10-07
title: Spec — Host commands
owner: opus
status: Draft
feature: host-registry
tags: [host-registry, federated-mcp, distributed-drain, spec]
related_features: [host-registry, federated-mcp, distributed-drain]
related_artifacts: [ORB-14448, ORB-14449, ORB-14451, ORB-14447, ORB-14261, ORB-12725]
---

# Spec: Host commands

The operator registers each remote Orbit host once, with `orbit host add`. From then on the
CLI-owned host file is the only record of remote membership. Federated MCP, pull drains,
replica worktree GC and task-prefix routing ([host-routing](./host-routing.md)) all read it.
An entry stores the operator's name for the host, its SSH target, and two facts that never
change on that host: `machine_id` and `task_prefix`. Anything that can change, such as
reachability, Orbit version, pull protocol or workspaces, is read live from the host when
asked and never persisted. Operators manage entries from the CLI or from the dashboard's
Settings › Hosts view. Status: specified; implementation is [ORB-14448] (CLI) and
[ORB-14451] (dashboard).

## Why This Exists

Before this spec, remote membership was a hand-edited `~/.orbit/mcp-destinations.toml` row
with two keys, `ssh` and `machine_id`. Nothing wrote the file. Five error messages told the
operator to edit it, and no host knew another host's task prefix, version or protocol. The
results:

- a replica's worktree GC asked its workspace owner about a task the replica had minted
  itself, every hour for weeks (DANI-10433, [ORB-14447]);
- owner/follower pull-protocol skew ([ORB-14261]) surfaced only when admission refused;
- operators copied `hm_…/ws_…` selectors into `--pull` by hand.

The reasoning, and the boundary that keeps this from becoming a fleet control plane, is the
decision
[The host registry is operator configuration, not a fleet control plane](../4_decisions.md#the-host-registry-is-operator-configuration-not-a-fleet-control-plane).

## Vocabulary

- **Host**: one Orbit installation, identified by its `[machine] id`. The **local host** is
  the installation running the command. A **remote host** is one reached over SSH.
- **Host name**: the operator's name for a host. For the local host it is `machine.name`.
  For a remote host it is the entry's `name`, which defaults to the remote's own
  `machine.name`.
- `machine` remains the identity vocabulary: `[machine]`, `machine_id` and the `hm_`
  namespace are unchanged. `host` is the operator-facing noun for an addressable
  installation. The local identity stays in `[machine]` in the global `config.toml`, as
  decided in [ORB-12725]. This spec adds nothing there.

## Host file

Path: `~/.orbit/hosts.toml`. It is machine-global and owned by orbit-registry, beside
`workspaces.json`. It is never read from a workspace `config.toml`.

```toml
schema_version = 1

[[hosts]]
name = "dk-server-2"
machine_id = "hm_9ca6004473492f06"
ssh = "dk-server-2"
task_prefix = "ORB"
```

### Invariants

1. Every key is required. Unknown keys and an unsupported `schema_version` fail the load
   and leave the file's bytes intact.
2. `name` passes the `machine.name` validator. It is unique, compared case-insensitively,
   across all entries and the local `machine.name`.
3. `machine_id` passes `validate_machine_id`, is unique, and is never the local machine id.
4. `task_prefix` passes the task-prefix validator, is unique, and is never the local
   `machine.task_prefix`. Prefix uniqueness is what makes
   [host-routing](./host-routing.md) unambiguous. A hand edit that duplicates a prefix fails
   the load with `task_prefix_conflict`.
5. `ssh` follows the rules of the existing destination `ssh` key: an SSH alias or
   `user@host`.
6. No entry field records reachability, version, protocol, workspaces, last-seen time or
   health.
7. Writes validate a clone, serialize canonically (entries sorted by `name`) and replace the
   file atomically. A refused mutation leaves the prior bytes. Hand edits are allowed and go
   through the same validation on load.

## Identity probe

`host add`, `host list`, `host show` and `orbit doctor` read a host's identity from the host
itself. They open the session federated serve already uses
(`ssh -T -- <target> orbit mcp serve --remote-caller-machine-id <local id>`), within the
federated probe budget, and call `orbit.workspace.list`.

The v1 `orbit.workspace.list` envelope already carries `machine_id`. It gains four additive
fields:

| Field | Source on the remote |
|---|---|
| `machine_name` | `[machine] name` |
| `task_prefix` | `[machine] task_prefix` |
| `binary_version` | the running binary's version (the same value the drain probe reports) |
| `protocol_fingerprint` | `distributed_drain_protocol_fingerprint()` |

Rules:

- The probe is read-only and runs with the caller's ordinary session authority. It never
  adds `--operator`.
- A remote whose envelope lacks `task_prefix` predates this spec. `host add` refuses it with
  `host_too_old`, naming the remote's version. `host list` shows that row's missing fields as
  unknown.
- If a probe returns a `machine_id` or `task_prefix` that differs from the entry, the call
  fails with `host_identity_mismatch`, naming both values. The entry is never rewritten from
  a probe. A reinstalled or replaced host is removed and added again.

## Commands

Every command takes `--json`. `<host>` resolves by exact host name (case-insensitive) or
exact `machine_id`. Anything else is `unknown_host`, and there is no prefix or fuzzy
matching.

### `orbit host add <ssh-target> [--name <name>]`

Probes the target, validates the would-be entry against the invariants, and writes it.
Output is the entry plus the live probe summary. It refuses with an actionable error, and
writes nothing, when:

| Condition | Error |
|---|---|
| target does not answer within the probe budget | `unreachable_destination` |
| probe reports the local machine id or local prefix | `host_is_local` |
| `machine_id` already registered | `host_exists` (names the entry) |
| name already used by an entry or the local host | `host_name_conflict` |
| prefix already used by an entry or the local host | `task_prefix_conflict` |
| envelope lacks `task_prefix` | `host_too_old` |

Adding a host writes nothing on that host. Registration is one-directional: a follower needs
an entry for its owner, and the owner needs none for the follower.

### `orbit host list [--no-probe]`

The local host comes first, marked `local` and read in-process. Then each entry, sorted by
name, with: name, machine_id, ssh, task_prefix, reachable, binary_version,
protocol_fingerprint, a skew flag, and workspaces with their role on that host (`owner`, or
`replica` of `<owner machine_id>`).

- Hosts are probed in parallel, each within its own probe budget.
- An unreachable host is listed with its cached fields and the error class, never omitted.
- The skew flag is set when `binary_version` or `protocol_fingerprint` differs from the local
  host's.
- `--no-probe` prints cached fields only and opens no session.
- Exit status is 0 when the file loads, whatever the hosts' reachability, because the
  command is a report. `orbit doctor` is the gate.

### `orbit host show <host>`

One host, probed as in `list`. It also lists what on this machine depends on that host:
local replica checkouts whose owner is the host, and running pull drains whose destination
is the host. For the local host, it shows `[machine]` and the local workspaces.

### `orbit host rename <host> <new-name>`

Renames a remote entry. The new name must satisfy invariant 2. Renaming the local host is
refused with a pointer to `orbit config set --global machine.name <value>`.

### `orbit host remove <host> [--force]`

Refuses with `host_in_use` while a local replica checkout names the host as owner or a
running `orbit run auto --pull` drain targets it. The error lists the dependents. `--force`
removes the entry anyway and prints which dependents will lose their route. Removing the
local host is `host_is_local`.

## Dashboard

Orbit Web manages the serving host's host file with the same operations as the CLI.
Status: specified; implementation is [ORB-14451], which depends on [ORB-14448].

### API

| Route | Effect |
|---|---|
| `GET /api/hosts[?probe=false]` | Same rows and JSON shape as `orbit host list --json` |
| `GET /api/hosts/:host` | `orbit host show --json`, dependents included |
| `POST /api/hosts` with `{ssh, name?}` | `orbit host add` |
| `PATCH /api/hosts/:host` with `{name}` | `orbit host rename` |
| `DELETE /api/hosts/:host[?force=true]` | `orbit host remove [--force]` |

- Each route calls the orbit-registry operation the CLI calls. There is no second
  implementation of validation, migration or writes, and the typed errors are the CLI's.
- Mutating routes pass the existing origin guard (`api/origin.rs`) like every other dashboard
  mutation. The guard mitigates CSRF and DNS rebinding. Access control remains the loopback
  bind plus the SSH tunnel.
- The `ssh` value is checked by the host-file `ssh` validator before any process starts: a
  leading `-`, whitespace and shell metacharacters are refused. The target is passed after
  `--`. No route accepts a command or extra ssh options.
- Probes run off the async runtime, each within the federated probe budget, so one
  unreachable host never stalls other panels.

### View

The view is Settings › Hosts (`#config/hosts`).

- It shows the local host first, labelled as the host this dashboard edits, followed by every
  entry. Each row has reachability, version, protocol, the skew flag and workspace roles. An
  unreachable host shows its error class and is never hidden.
- Adding is an inline form (SSH target, optional name), and renaming is an inline editor.
  Removing asks for confirmation inline. The dashboard has no modal dialogs.
- A remove refused with `host_in_use` lists the dependents and offers an explicit force
  confirmation.
- Keyboard and focus behave as in [user-interface 2_design §6](../../user-interface/2_design.md#6-top-level-navigation).

### Freshness

The dashboard rereads the host file when it changes, using the generation-swap rule of the
registry snapshot ([2_design.md](../2_design.md) §6). A host added from the CLI appears on the
next refresh without a restart. A host file that fails to load shows an error banner on the
view, and the last valid snapshot stays in use, as the registry snapshot does.

A dashboard reached through `orbit web connect <host>` edits that host's host file, not the
caller's.

## Consumers

- **Federated serve.** The destinations are the host file's entries (`ssh`, `machine_id`)
  plus the implicit local destination. Selector, list and routing behavior in
  [federated-workspace-mcp](../../federated-mcp/specs/federated-workspace-mcp.md) is
  otherwise unchanged. Config-load `ambiguous_destination` is still enforced for hand edits.
- **Pull drains and replica worktree GC.** Their owner route resolves through the same
  loader.
- **Messages.** Every message that told the operator to add a row to
  `~/.orbit/mcp-destinations.toml` names `orbit host add <ssh-target>` instead. That
  includes worktree GC, follower admission, pull settlement, the settlement hint and
  `workspace init --role replica`. `workspace init --role replica` reports an owner
  `machine_id` with no entry and never adds one itself.
- **`orbit doctor`** gains a `hosts` row:
  - fail when the host file is invalid, when both files exist (see Migration), or when a
    host this machine pulls from or holds a replica of has a different
    `protocol_fingerprint`, or a version the pull admission ladder would refuse;
  - warn when an entry is unreachable, on any other version difference, and while only the
    legacy file exists.

## Migration from `mcp-destinations.toml`

One release of compatibility:

1. **Only the legacy file exists.** Federated serve, pull drains and worktree GC load its
   rows as today. `host list` shows them marked `legacy`, with live-probed fields. Prefix
   routing treats them as having no prefix, so they contribute nothing to the prefix table.
2. **First mutation.** The first `host add`, `rename` or `remove` probes every legacy row.
   If all of them answer, it writes `hosts.toml` with those rows, applies the mutation, and
   deletes the legacy file. Each migrated row is named after the remote's `machine.name`, or
   after its SSH target when that name is taken. If any row fails, the command refuses with `legacy_host_unreachable`,
   naming the row, and touches neither file.
3. **Both files exist.** Every consumer refuses with `host_file_conflict`, naming both paths.
   Orbit doesn't pick one, because they are two answers to the same question.
4. The next release drops the legacy reader and keeps the `host_file_conflict` check one
   release longer.

The legacy `~/.orbit/host.toml`, singular and folded into `[machine]` by [ORB-12725], is
unrelated to this file.

## Failure summary

| Condition | Result |
|---|---|
| host file malformed, unknown key, future schema | load fails, bytes kept, actionable error |
| duplicate name, machine_id or prefix (hand edit) | load fails (`host_name_conflict` / `ambiguous_destination` / `task_prefix_conflict`) |
| both host file and legacy file | `host_file_conflict` on every consumer |
| probe identity differs from entry | `host_identity_mismatch`; entry unchanged |
| remote too old to report `task_prefix` | `host add` refuses `host_too_old`; `list` shows unknown |
| host unreachable | `add` refuses; `list`/`show` report it; routed calls follow federated `unreachable_destination` |
| remove while depended on | `host_in_use` unless `--force` |

## Non-goals

- No presence heartbeat, background probe, cached health or last-seen state.
- No placement, failover, leases or leader election, and no choosing between hosts.
- No database tables. The legacy fleet-registry tables stay unread.
- No discovery: nothing scans SSH config, the tailnet or mDNS.
- No registration on the far side, and no credential storage. SSH authentication stays with
  ssh.
- No change to local identity, which stays in `[machine]`.

## Agent Signature

Specified by opus during on-call ORB-14441 at Daniel's direction (2026-10-07), for
implementation in [ORB-14448] and [ORB-14451] (dashboard).
