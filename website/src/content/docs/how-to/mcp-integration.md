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

### Workspaces with an external Orbit root

`orbit --root <dir> workspace init` registers a checkout whose Orbit data root
lives outside the repository, so nothing inside the checkout marks it as a
workspace. Select that root on the setup commands too — `orbit --root <dir> mcp
init --claude`, or `ORBIT_ROOT=<dir>` in the environment — and Orbit resolves
the registered checkout from that root's catalog: the client config is written
into the repository and bound to its `ws_*` workspace, and `orbit mcp remove`
takes it back out. Without a root selector there is no catalog to consult, and
both commands refuse rather than writing the config elsewhere.

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

### Attribute the tasks a session creates

A server can carry the orchestrator crew that its tasks are attributed to, so
each call does not have to remember it:

```bash
orbit mcp serve --workspace <selector> --orchestrator <crew>
```

The value is attribution only. Unlike `--operator` it grants no authority, and
it neither selects the crew a task executes under nor the model recorded for
that execution. A call that passes its own `orchestrator` wins; a call that
omits it inherits the session's; a server started without the flag attributes
nothing, exactly as before. The crew is resolved against the workspace the call
lands in, so an unconfigured name fails that call rather than falling back to
another crew, and only newly created tasks are affected — existing ones are
never rewritten.

Both client modes forward the value to the server that actually creates the
task:

```bash
orbit mcp serve --mode remote <ssh-host> --orchestrator <crew>
orbit mcp serve --mode federated --orchestrator <crew>
```

A destination whose `authorized_keys` pins a forced command composes its own
argv, so its configuration wins there — the same rule that already applies to
the authority a remote session asks for.

Treat the flag as configuration, not as evidence of which model is answering a
given call: an MCP connection commonly outlives a model switch on the client
side. Pass `orchestrator` on the individual call, or restart the connection
with a new value, when the orchestrating crew genuinely changes.

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
