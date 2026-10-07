---
title: Connect Your Agent
description: "Connect Claude Code, Codex, Cursor, and other agent clients to Orbit over MCP: let the orbit-setup skill do it, or run one command."
sidebar:
  order: 5
---

Your agent reaches Orbit through an MCP server. Once connected, it can file
tasks, ship them, and run drains.

:::tip[Recommended]
Ask your agent to **set up Orbit for this repo**. The `orbit-setup` skill
registers Orbit with the clients you use, and it also handles the setups
further down this page: other machines, remote dashboards, and a second
repository.
:::

## Connect

```bash
orbit workspace init --mcp
```

This registers Orbit with every agent client it finds, with **operator**
authority, so your agent can ship tasks and run drains as well as file them.
Start a fresh agent session afterwards so the tools load.

`orbit mcp init` registers Orbit with the narrower **agent-only** authority:
the agent can file and update tasks but cannot dispatch runs. Use it to pick
clients or to limit an agent:

```bash
orbit mcp init --auto                   # every detected client
orbit mcp init --claude --codex         # specific clients
orbit mcp init --all --scope home       # user-level config, every client
```

Supported clients: `claude`, `codex`, `gemini`, `antigravity`, `grok`,
`cursor`, `vscode`, and `windsurf`. `--scope workspace`, the default, writes
config into the repository; `--scope home` writes it for your user.

Grok Build reads the shared `.mcp.json` (or `~/.claude.json` with
`--scope home`); when its shared reader is off, Orbit writes
`.grok/config.toml` instead.

If the workspace's Orbit data lives outside the repository, pass
`orbit mcp init` the same `--root <dir>` (or `ORBIT_ROOT`) you used for
`orbit workspace init`.

## Register the federated mux

A federated server puts this machine's workspaces and those on SSH remotes
under one namespace. Register each remote host once, by its SSH alias or
`user@host`:

```bash
orbit host add orbit-owner
orbit host list
```

`orbit host add` reads the host's machine ID, name and task prefix from the
host itself and records them in `~/.orbit/hosts.toml`. `orbit host list` shows
every host with its live reachability, Orbit version, pull protocol and
workspaces, and flags a version or protocol that differs from this machine's.
`orbit host rename` and `orbit host remove` manage the entries. An older
`~/.orbit/mcp-destinations.toml` is still read until the first of these
commands migrates it.

Then register the federated server with a client:

```bash
orbit mcp init --federated --client codex --scope home
```

This adds a separate `orbit-federated` entry beside any existing `orbit` one.
In that session, `orbit_workspace_list` returns host-qualified selectors such
as `hm_alpha/ws_orbit`; pass one unchanged as the `workspace` of a call. Task
reads go to the owner's selector. There is no automatic failover: a call
reaches only the machine you select.

Operator authority travels over SSH. A client started with `--operator` serves
operator on every destination it opens, because an SSH login to a machine is
ownership of it. Without `--operator`, and always for a client running inside
a managed run, remote sessions get agent-only authority. To shut a caller out,
remove its key from that machine's `~/.ssh/authorized_keys`.

## Attribute tasks to an orchestrator

```bash
orbit mcp serve --workspace <selector> --orchestrator <crew>
```

Tasks the session creates are recorded as filed by that crew, unless a call
passes its own `orchestrator`. This is attribution only: it grants no
authority and does not choose the crew that executes a task. The same flag
works with `--mode remote <ssh-host>` and `--mode federated`.

## Serve on a socket

```bash
orbit mcp listen              # 127.0.0.1:7879
```

The listener serves the same tools over TCP, one session per connection, for
setups such as a server-side Orbit reached through an SSH tunnel. It does not
authenticate clients, so it binds loopback unless you pass
`--allow-non-loopback`.

The listener allows 64 concurrent sessions. Each accepted connection has five
seconds to send a complete initialization request and receive its response;
silent peers and incomplete messages are closed and release their slots.
Established sessions remain connected while idle, until the client disconnects.

## Response shapes

`orbit tool list` shows every tool. Each one advertises MCP annotations
(`readOnlyHint`, `destructiveHint`, and so on) so a client can decide what to
auto-approve; Orbit still enforces authority on every call.

- `orbit_task_list` and `orbit_task_eligible` return
  `{ tasks, total, truncated }`. Task records are full by default; pass
  `fields` (for example `["id", "title", "status"]`) to keep a listing small.
- `orbit_task_eligible` lists, in dispatch order, the `backlog` and `proposed`
  tasks whose files overlap no running or in-review task. With
  `explain: true`, `conflicting` also lists the held-back tasks, each with the
  overlapping file and the task holding it.
- The task-write tools leave out `comments` and `history` unless you name them
  in `fields`.

## Remove

```bash
orbit mcp remove --all
orbit mcp remove --federated --all   # only the federated entry
```
