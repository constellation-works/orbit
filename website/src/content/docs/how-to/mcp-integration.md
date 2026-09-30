---
title: Connect Your Agent
description: "Expose Orbit's safe MCP tool surface to Claude Code, Codex, Gemini, Antigravity, Grok Build, Cursor, VS Code, or Windsurf."
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

Or target clients explicitly and choose where the config is written:

```bash
orbit mcp init --claude
orbit mcp init --codex --gemini
orbit mcp init --client cursor --client vscode --scope home
orbit mcp init --all
```

`--client` accepts `claude`, `codex`, `gemini`, `antigravity`, `grok`, `cursor`,
`vscode`, and `windsurf`; each also has a flag of the same name, and `--all`
targets every supported client. `--scope workspace` (the default) writes
repo-local config; `--scope home` writes user-level config.

**Grok Build** reads the shared MCP config, so `orbit mcp init --grok` writes
`.mcp.json` in your workspace root, or `~/.claude.json` with `--scope home`. When
Grok's shared reader is unavailable, for example because `~/.grok/config.toml`
records that Grok already imported Claude's config or turns the Claude MCP
reader off, Orbit writes Grok's native `.grok/config.toml` instead.

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

`--operator` travels. Orbit is a single-user tool and an SSH login to a machine
is ownership of it, so a client started with `--operator` composes an operator
argv for every destination it opens, and each destination serves the session
that authority — the same way it would a local one. No file, forced command, or
per-destination setup is involved:

```bash
orbit mcp serve --mode federated --operator
orbit mcp serve --mode remote <ssh-host> --operator
```

Without `--operator` the remote sessions hold `agent`. A client that is itself
running as an agent — inside a managed run, or with an agent envelope in its
environment — never propagates operator, whatever the process that launched it
held. To deny a caller entirely, remove its key from the destination's
`~/.ssh/authorized_keys`.

## Serve

Start the MCP surface:

```bash
orbit mcp serve
```

Use `orbit tool list` to inspect the current local registry. MCP exposure is a
capability-filtered subset of that registry. The retired graph tools are not
exposed.

### Tool annotations

`tools/list` advertises MCP `annotations` on every built-in tool so a client can
decide what to auto-approve. Read-only tools (`orbit_task_list`,
`orbit_task_show`, `orbit_search`, `orbit_workspace_list`, and the other list
and show tools) carry `readOnlyHint: true`; mutating tools carry
`readOnlyHint: false` together with `destructiveHint`, `idempotentHint` and
`openWorldHint`. Tools that delete or overwrite data (`orbit_auto_task_delete`,
`orbit_friction_rehome`, `orbit_task_artifact_put`) are destructive, and tools
that start an agent or a process (`orbit_agent_invoke`, `orbit_command_exec`,
`orbit_workflow_ship`, `orbit_workflow_run_resume`) are open-world. A plugin tool
advertises only `readOnlyHint`, taken from its manifest's execution kind. The
hints describe behavior for the client's own prompts; they never grant or
withhold authority, which Orbit enforces on every call.

### Response shapes

`orbit_task_list` returns `{ tasks, total, truncated }`; read the `tasks` array.
Each task is a full record by default, descriptions and plans included, so a long
listing can be large. Pass `fields` (a string or array of `orbit_task_show` field
names, such as `["id", "title", "status"]`) to get objects holding only those
fields, then fetch details for the few tasks that matter with `orbit_task_show`.
The task-write tools — `orbit_task_add`, `orbit_task_update`, and
`orbit.task.reject` — omit `comments` and `history` unless you request them
with `fields` (or `field`).

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
connection. Pass `--workspace <selector>` to bind each accepted session to a
registered workspace by default, exactly as `orbit mcp serve --workspace` does;
without it each session names its own. The socket authenticates no client, so it
binds loopback; a wider bind requires `--allow-non-loopback` and a network path
you have restricted by other means.

## Remove

```bash
orbit mcp remove --all
orbit mcp remove --federated --all  # remove only the federated entry
```
