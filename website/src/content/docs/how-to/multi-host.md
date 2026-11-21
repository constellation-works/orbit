---
title: Run Orbit across hosts
description: "Register another Orbit host, reach its tasks by ID prefix or with --host, and fix routing errors."
---

Register a remote host once on the machine that needs to reach it. Task
commands, federated MCP, pull drains and the dashboard's host picker then
route to it over SSH. SSH supplies the authentication.

## Add another host

Install and initialize Orbit on both machines. Give each a different task
prefix at `orbit init`: the prefix identifies the machine that mints a task,
and `orbit config set` cannot change it.

On the calling machine, make sure SSH logs in without a prompt and `orbit` is
on the remote `PATH`. Then register an SSH alias or `user@host`:

```bash
orbit host add orbit-owner --name owner
orbit host list
orbit host show owner
```

`orbit-owner` is the SSH target and `owner` is your local name for the host.
Without `--name`, the host keeps the remote's `machine.name`. Registration
reads the remote's machine ID and task prefix and writes
[`~/.orbit/hosts.toml`](/reference/config/#host-registry) on this machine. It
writes nothing on the remote, so registering A on B does not register B on A.

`host list` shows this machine first, then each registered host with its
reachability, Orbit version, pull protocol and workspaces. **SKEW** marks a
host whose version or protocol differs from this machine's. `host show` adds
the replica checkouts and running pull drains on this machine that use the
host.

```bash
orbit host list --no-probe    # stored fields only; contacts no host
orbit host list --json
```

Host arguments take an exact name (case-insensitive) or a `machine_id`.

## Route a task by its prefix

Suppose the registered owner mints IDs with the prefix `BOX`. From the other
machine:

```bash
orbit task show BOX-123
```

A task command that names one ID goes to the host that owns its prefix. This
machine's prefix runs locally and a registered remote prefix goes over SSH.
The rule covers `show`, `update`, `artifact get` and `put`, `review-reset`
and `reconcile-review`, and the same tools through `orbit tool run`.
[Federated MCP](/how-to/mcp-integration/#register-the-federated-mux) routes
ID-only calls the same way.

If the owning host is unreachable, the call fails with `owner_unreachable`.
It never falls back to a local mirror. To read a mirror, knowing it may be
behind, name the workspace:

```bash
orbit task show BOX-123 --workspace project
```

## Select a workspace with `--host`

Pair `--host` with `--workspace` to address a workspace on a named host:

```bash
orbit task show BOX-123 --host owner --workspace project
```

`project` is a workspace name or `ws_*` ID on that host. Pass `--host` after
the subcommand. The prefix-routed task commands, `orbit tool run` and
`orbit run auto --pull` accept it.

Host-local commands such as `orbit workspace`, `orbit config`, `orbit doctor`,
`orbit task list` and `orbit task add` do not. Run those on the remote over
SSH, or use the task tools through the federated MCP server.

## See another host in the dashboard

A registered host appears in the dashboard's **Host** picker. Select it to
see its tasks, runs and logs without opening a second dashboard. See
[Switch hosts](/how-to/dashboard/#switch-hosts).

## Pull work from another host

Registering a host does not make this machine work for it. To run tasks from
another machine's backlog, register a checkout as a replica and start a pull
drain: follow [Set up a distributed drain](/how-to/distributed-drain/).

## Rename or remove a host

```bash
orbit host rename owner build-box
orbit host remove build-box
```

Rename changes only the local name. Remove refuses with `host_in_use` while a
replica checkout or a running pull drain on this machine depends on the host.
`orbit host show` lists those dependents. `--force` removes the entry anyway
and leaves them without a route.

This machine is always listed and cannot be removed. Rename it with
`orbit config set --global machine.name <value>`. You can also manage hosts
in the dashboard's [Observe › Fleet](/how-to/dashboard/#fleet) page.

## Troubleshoot by error code

Start with `orbit host list` and `orbit doctor` on the calling machine. `host
list` is a report: an unreachable host does not make it fail. The `hosts` row
in `orbit doctor` is the health check: it fails on an identity mismatch or on
skew with a replica's owner, and warns on unreachable hosts.

| Code | What to do |
|---|---|
| `unknown_host` | Use an exact name or machine ID from `orbit host list`, or register the host with `orbit host add`. |
| `unknown_task_prefix` | Register the machine that minted the task, or pass `--workspace` to read a mirror. |
| `task_prefix_remote` | A plain local or remote MCP server does not relay to other hosts. Use federated MCP, or connect to the host the error names. |
| `unreachable_destination` / `owner_unreachable` | Check the SSH target, non-interactive login and `orbit` on the remote `PATH`. |
| `host_too_old` | Upgrade Orbit on the remote. |
| `host_exists` | That machine ID is already registered. Inspect or rename the existing entry. |
| `host_name_conflict` | Choose another name. Names are unique across remote entries and this machine. |
| `task_prefix_conflict` | Two machines claim one prefix. Verify which machine each entry reaches. A prefix is fixed at initialization, so renaming a host does not resolve this. |
| `host_is_local` | The target is this machine, which is already listed. |
| `host_identity_mismatch` | SSH now reaches a different installation. Check the target; if the host was replaced, remove and add it again. |
| `host_in_use` | Run `orbit host show <host>`, then move the listed replica checkouts or stop their pull drains. |
| `stale_route` / `unknown_selector` | The host lists no such workspace, or the name matches more than one. Use the exact `ws_*` ID from its live list. |
| `host_file_conflict` | Follow the [retired destinations file recovery](/reference/config/#legacy-migration). |
