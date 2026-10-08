---
title: Run Orbit across hosts
description: "Register another Orbit host, route tasks by prefix, select remote workspaces, and prepare a follower for a pull drain."
sidebar:
  order: 8
---

Register a remote host once on the machine that needs to reach it. The host
registry supplies routes for task commands, federated MCP, pull drains, and
replica worktree cleanup. SSH supplies authentication.

## Add another host

Install and initialize Orbit on both machines. Give each a different task
prefix when running `orbit init`: a prefix identifies the machine that mints
those tasks and cannot be changed with `orbit config set`.

On the calling machine, make sure SSH logs in without a prompt and `orbit` is
on the remote `PATH`. Then register an SSH alias or `user@host`:

```bash
orbit host add orbit-owner --name owner
orbit host list
orbit host show owner
```

Here `orbit-owner` is your SSH target and `owner` is the local name for that
host. Omit `--name` to use the remote's `machine.name`. Registration reads the
remote's machine ID and task prefix and writes
[`~/.orbit/hosts.toml`](../../reference/config/#host-registry) on this machine.
It writes nothing on the remote. Registering the owner on a follower does
not register the follower on the owner.

`host list` shows this machine first, then each registered host's
reachability, Orbit version, pull protocol, and workspaces with their owner
or replica role. **SKEW** means its version or protocol differs from this
machine's. Unreachable hosts stay in the list with their typed error.

```bash
orbit host list --no-probe     # stored fields; no SSH sessions
orbit host list --json        # machine-readable report
orbit host show owner --json  # one host and its local dependents
```

`show` also lists replica checkouts and running pull drains on this machine
that use the host. Host arguments accept an exact name, ignoring case, or
an exact `machine_id`; they do not use partial matching.

## Route a task by its prefix

Suppose the registered owner mints IDs with prefix `BOX`. From the other
machine:

```bash
orbit task show BOX-123
```

With no explicit `--workspace`, a task command that addresses one ID routes
to the host that owns its prefix. This machine's prefix runs locally; a
registered remote prefix goes over SSH; an unregistered prefix returns
`unknown_task_prefix`. This applies to `show`, `update`, `artifact get` and
`put`, `review-reset`, and `reconcile-review`, and to their ID-routed tools
through `orbit tool run`.

An explicit workspace takes precedence. To read a local mirror, knowing it
may be behind:

```bash
orbit task show BOX-123 --workspace project
```

If the prefix's host is unreachable, the ID-only call returns
`owner_unreachable`. It never silently falls back to a mirror.

[Federated MCP](../mcp-integration/#register-the-federated-mux) follows the
same prefix rule for ID-only calls. Plain local and remote MCP servers do
not relay; they return `task_prefix_remote` for another registered host's ID.

## Select a workspace with `--host`

On a routable task command, pair `--host` with `--workspace`:

```bash
orbit task show BOX-123 --host owner --workspace project
```

Orbit reads the named host's live workspace list and uses the selector it
reports for `project`. You can use a workspace name or `ws_*` ID. A full
host-qualified selector from discovery also remains valid.

Pass `--host` after the task subcommand. It is accepted on the routable task
commands above, on `orbit tool run`, and with `orbit run auto --pull`.
Host-local commands such as `orbit workspace`, `orbit config`, `orbit doctor`,
`orbit task list`, and `orbit task add` do not accept it. Run those on the
remote over SSH, or use the workspace-scoped task tools through the
federated server.

## Prepare a follower and pull work

A **follower** executes tasks for another machine's backlog. Its repository
checkout is registered as a **replica** of the owner's workspace. The owner
keeps the task store, admits work, and lands the resulting pull requests.
Adding a host alone does not make a checkout a follower.

On the owner, `orbit config get machine.id` prints the owner machine ID.
On the follower, from a checkout of the same repository:

```bash
cd /path/to/repo
orbit workspace init --role replica --owner <owner-machine-id>
orbit host show owner
orbit doctor
```

Before pulling, check:

- The owner is registered on the follower and reachable over SSH.
- Both machines run the same Orbit version and distributed-drain protocol.
- Both declare the same `workflow.required_validation_commands`, and the
  follower can run those commands and access the repository's forge.
- The follower has an enabled crew, its provider CLI installed and signed
  in, and the tools the task needs. If the owner enables before-PR review,
  the follower can also run the owner's `operation.review_crew`.

Start a bounded window on the replica checkout:

```bash
orbit run auto --pull project --host owner --for 8h --concurrency 3
```

`project` is the owner's workspace name or `ws_*` ID. Without `--host`,
`--pull` requires the full host-qualified selector from discovery. It must
name this replica's own owner and workspace. The owner admits only tasks
whose crew and OS the follower can run. Each leaf implements, validates,
and opens a pull request, then hands the candidate back to the owner.

For the admission probe, before-PR review setup, and stop and recovery
procedures, follow [Set Up a Distributed Drain](../distributed-drain/).

## Rename or remove a route

```bash
orbit host rename owner build-box
orbit host show build-box
orbit host remove build-box
```

Rename changes the entry's local name. Its SSH target, machine ID, and task
prefix stay the same. Remove refuses with `host_in_use` while a local
replica checkout or running pull drain depends on the route. Re-home those
dependents first, or use `orbit host remove build-box --force` deliberately:
the reported dependents will lose their route.

The local host is always implicit. Rename it with
`orbit config set --global machine.name <value>`; it cannot be removed.
You can also manage remote entries in
[Settings › Hosts](../dashboard/#hosts) on the dashboard.

## Troubleshoot by error code

Run `orbit host list`, `orbit host show <host>`, and `orbit doctor` on the
calling machine. `list` and `show` are reports: an unreachable row does not
by itself make them exit with failure. Doctor's **hosts** row is the health
check. It fails for an invalid or conflicting host file, an identity
mismatch, or version/protocol skew on a replica's owner; it warns for
unreachable hosts, other skew, missing owner routes, and legacy-only files.

| Code | What to do |
|---|---|
| `unknown_host` | Use an exact name or machine ID from `orbit host list`, or register the target with `orbit host add`. |
| `unknown_task_prefix` | Register the machine that minted the task, or select a workspace explicitly to read a mirror. Legacy rows have no prefix until migrated. |
| `task_prefix_remote` | Use federated MCP or connect to the host named by the error. This MCP server does not relay. |
| `unreachable_destination` / `owner_unreachable` | Check the SSH target, non-interactive login, and remote `orbit` on `PATH`. Restore the route; prefix routing has no mirror fallback. |
| `host_too_old` | Upgrade the remote to a build that reports its task prefix. |
| `host_exists` | The machine ID is already registered. Inspect or rename its existing entry. |
| `host_name_conflict` | Choose another `--name` when adding, or another new name when renaming. Names must be unique across remote entries and this machine. |
| `task_prefix_conflict` | Two machines claim one prefix. Verify their identities; prefixes are fixed at initialization, so a display-name change cannot resolve this. |
| `host_is_local` | The target is this machine. Its local row already exists. |
| `host_identity_mismatch` | Verify that SSH still reaches the intended installation. If it was replaced, remove and add the route again after addressing its dependents. |
| `host_in_use` | Inspect `orbit host show <host>` and re-home the listed replica checkouts or stop their pull drains before removal. |
| `stale_route` / `unknown_selector` | The host does not list that workspace, or its name matches more than one. Check its live list and use the exact workspace ID. |
| `legacy_host_unreachable` / `host_file_conflict` | Follow the [legacy migration procedure](../../reference/config/#legacy-migration). |
