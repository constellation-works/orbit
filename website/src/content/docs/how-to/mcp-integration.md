---
title: Set Up MCP
description: "Expose Orbit's safe MCP tool surface to Claude Code, Codex, Gemini, or Grok Build."
sidebar:
  order: 5
---

## Initialize

Use auto-detection:

```bash
orbit mcp init --auto
```

This registers the **agent-only** tool surface — the same authority as bare
`orbit mcp serve`. If you want an agent to be able to dispatch workflows and run
governed operations, register the operator-authorized integration during
workspace setup instead:

```bash
orbit workspace init --mcp
```

Or target a client explicitly:

```bash
orbit mcp init --claude
orbit mcp init --codex
orbit mcp init --gemini
orbit mcp init --grok
```

**Grok Build** uses the native `.grok/config.toml` format (similar to how Claude Code can use a config file). `orbit mcp init --grok` will create or update `.grok/config.toml` in your workspace root (or `~/.grok/config.toml` for global).

## Register the federated mux

Federated MCP presents one namespace over this machine's workspaces plus any
SSH remotes in the machine-global `~/.orbit/mcp-destinations.toml`. Local
workspaces need no destination row; a missing or empty file still serves a
useful local-only federated session. Additional remotes are declared as SSH
destinations:

```toml
[[destinations]]
ssh = "orbit-owner"
machine_id = "hm_alpha"

[[destinations]]
ssh = "operator@orbit-build"
machine_id = "hm_beta"
```

Register it with a client (Codex shown here):

```bash
orbit mcp init --federated --client codex --scope home
```

This adds a separate `orbit-federated` entry that launches `orbit mcp serve
--mode federated`; an existing v1 `orbit` entry is left unchanged. Use another
`--client` value or `--auto` to target a different installed client.
Remove only that entry later with `orbit mcp remove --federated` and the same
client/scope selection.

In that federated MCP session, list destinations, copy an owner row's
host-qualified `selector`, and pass it unchanged to a workspace-scoped call:

```text
orbit_workspace_list({})
  -> {"workspaces":[{"selector":"hm_alpha/ws_orbit", ...}]}

orbit_task_list({"workspace":"hm_alpha/ws_orbit"})
```

There is no placement, implicit failover, or competing-Owner detection;
availability is the availability of the selected destination. Task reads are
owner-only, so `orbit_task_list` and `orbit_task_show` must use the owner
selector. A replica selector returns `capability_refused`.

## Serve

Start the MCP surface:

```bash
orbit mcp serve
```

Use `orbit tool list` to inspect the current local registry. MCP exposure is a
capability-filtered subset of that registry. The retired graph tools are not
exposed.

## Listen on a socket

Deployments that need the server on a port — a server-side Orbit reached through
an SSH tunnel, for example — use the listener instead:

```bash
orbit mcp listen              # binds 127.0.0.1:7879
orbit mcp listen 127.0.0.1:9000
```

It serves the same tool surface as `orbit mcp serve`, one independent session per
connection. The socket authenticates no client, so it binds loopback; a wider bind
requires `--allow-non-loopback` and a network path you have restricted by other
means.

## Remove

```bash
orbit mcp remove --all
orbit mcp remove --federated --all  # remove only the federated entry
```
