---
title: Orbit MCP — Design
owner: codex
last_updated: 2026-09-07
last_validated: 2026-09-07
status: Draft
feature: mcp-bridge
doc_role: design
type: design
summary: Implemented v1 request flow for local MCP stdio, direct SSH stdio, and the TCP listener, with server-side resolution and one Core audit boundary.
tags: [mcp, ssh, remote-access, registry, audit]
paths: ["crates/orbit-mcp/**", "crates/orbit-registry/**", "crates/orbit-core/**", "crates/orbit-cli/src/command/mcp/**", "crates/orbit-tools/**"]
related_features: [host-registry, mcp-session-context]
---

# Orbit MCP — Design

## 1. Invariants

The v1 design rests on six invariants:

1. The accepting machine is authoritative for registry and runtime state.
2. A remote session is one direct SSH hop; no Orbit process relays it onward.
3. The client-side proxy is byte-transparent and policy-free.
4. Every tools/call enters Core's dispatch and audit boundary exactly once,
   including global discovery, unknown raw names, and setup failures.
5. A forwarded caller label is audit metadata; destination-side caller policy,
   not that label, grants remote authority. A forced-command SSH acceptance can
   bind the label to the key sshd authenticated.
6. Direct local, remote, and socket calls use the same server implementation;
   federated mode is a separate mux that delivers to those destination servers.

## 2. Components

### `orbit-mcp`

The framing kernel owns MCP framing, advertised-name translation, structured
responses, canonical surface composition, per-call trace creation, server
identity context, and the TCP listener. The crate also owns the direct SSH
stdio proxy, destination-side caller policy, and the explicit federated mux.
Its `McpHost` boundary accepts canonical tool calls with a trusted session
context; runtime opening remains in `orbit-cli`, while Core enforces effective
capabilities and governed operations.

### `orbit-registry`

Owns persisted host identity and workspace-registry state. The accepting process
uses it to derive its own identity and resolve server-local workspaces.

### `orbit-cli`

Composes the concrete MCP host once and serves it over whichever transport was
asked for. It loads the accepting machine's registry, selects a workspace when
required, opens the corresponding runtime, and hands the operation to Core.

### `orbit-core`

Owns domain execution, sandbox validation, capability enforcement, governed
operation authorization, and invocation audit persistence. It is the single
execution boundary reached by direct local and remote calls after server-side
resolution.

### `orbit-tools`

Owns canonical builtin tool definitions. `orbit-mcp` composes those definitions
with its small machine-local discovery surface rather than redeclaring schemas.

## 3. Local request flow

The client starts `orbit mcp serve` and speaks MCP over the process's stdio.

1. The server resolves the global Orbit root, its own process identity, and the
   session authority its argv asked for. An SSH-originated session is resolved
   the same way as a local one [ORB-12564]; the forwarded caller label only
   marks the transport and names the calling machine.
2. MCP initialization may establish a workspace selector for the session.
3. Tool discovery returns the canonical composed surface.
4. For each `tools/call`, the adapter creates a fresh `trace_id` and combines it
   with server-established session context. Audit fields in tool input are not
   trusted context.
5. An unknown or unadvertised raw name enters Core's global audit seam and is
   recorded as denied without opening a workspace runtime.
6. A workspace-scoped call selects `workspace` from tool input first, then from
   MCP initialization metadata. Missing selection fails clearly.
7. The server resolves that selector against its own registry and opens that
   checkout's runtime.
8. The operation and its context enter Core once; Core enforces effective
   capabilities and returns or records the outcome at the same audit boundary.

Global tools do not require a workspace selector, but they still enter Core once.
Their server-local registry projection is supplied through Core's in-process
dispatch seam so discovery is audited like every other MCP call.

## 4. Remote and socket request flows

### Direct SSH stdio

A client may register the local command:

```text
orbit mcp serve --mode remote <ssh-host>
```

That process resolves its persisted machine ID when possible, falls back to
`host/local`, and starts the equivalent of:

```text
ssh -T <ssh-host> orbit mcp serve --remote-caller-machine-id <audit-label>
```

`-T` prevents PTY allocation. The SSH child inherits all three standard streams,
so the local proxy never sees or rewrites an MCP message. SSH handles transport,
host verification, encryption, and access to the remote shell.

The remote `orbit mcp serve` process then follows the local request flow. It also
marks the session as `ssh-mcp` and reads the first field of `SSH_CONNECTION` as a
best-effort caller IP. The supplied machine label and the observed IP are audit
data; the session's authority comes from the argv the proxy composed, which
carries `--operator` when the proxy itself was started with it [ORB-12564].

If SSH cannot start or exits unsuccessfully, the proxy reports that transport
failure. It does not retry or replay tool calls because it cannot know whether a
request crossed the process boundary.

### TCP listener

```text
orbit mcp listen [ADDR] [--allow-non-loopback]
```

The listener binds before it accepts, so the bind policy is applied and the
assigned address is known before any client can arrive. A non-loopback address is
refused unless the operator asked for it explicitly, because the socket
authenticates no client: whoever reaches it reaches the accepting machine's full

Each accepted connection is served on its own task with its own server instance.
That isolation is load-bearing rather than defensive. The adapter's session state
is written during `initialize`, so two clients sharing one instance would race on
the announced workspace selector, and the loser would receive a *successful*
response computed against the other client's workspace. Every session also mints
its own origin session id, since a listener-wide id would collapse concurrent
clients into one audit identity.

Past that point the session follows the direct local request flow exactly: the
same host, the same workspace resolution, and the same single Core dispatch and
audit boundary. The listener adds no broker, checkout preflight, placement
decision, capability filter, or authorization step of its own; it is hardcoded
to agent authority because it authenticates no client.

## 5. Workspace authority

The client selects only an SSH destination. It does not inspect a checkout or a
registry to decide where an operation belongs.

On the accepting server:

- `orbit.workspace.list` reads the machine-local registry and returns active
  logical workspaces with a checkout registered on that machine, including
  locally registered replicas;
- workspace-scoped tools accept a stable workspace selector from the call or
  initialization metadata;
- the selector is resolved to a server-local checkout;
- the resolved runtime performs the same Core validation used by non-MCP entry
  points.

No client-side ownership, placement, capability, or authorization check is part
of correctness. UI checks may improve ergonomics, but they cannot establish
server authority.

## 6. Tool discovery and dispatch

The canonical surface is assembled in `orbit-mcp` from builtin definitions in
`orbit-tools` and MCP-owned discovery definitions. Definitions are sorted and
validated before advertisement. One source therefore drives both advertised
schemas and dispatch lookup.

Each definition contains a schema and an `McpToolScope`. Scope controls only
whether the server injects and resolves a workspace selector. It does not encode
caller capability, placement, or authorization.

The concrete server classifies known tools before opening a runtime. Known global
tools use Core's global in-process seam; known workspace tools use the resolved
runtime seam. Unknown and unadvertised raw names use the global seam and return
`tool_not_found`. All three paths cross Core exactly once, so audit coverage does
not depend on recognition or outcome.

## 7. Audit envelope

| Field | Source | V1 meaning |
|---|---|---|
| `trace_id` | MCP adapter, fresh per call | Correlates one invocation |
| `caller_machine_id` | Local server identity, or the SSH proxy's forwarded label | Audit correlation only |
| `caller_ip` | First field of `SSH_CONNECTION`, or the accepted peer address | Best-effort network observation |
| `process_machine_id` | Accepting machine registry | Machine executing the call |
| `process_host_id` | Accepting machine registry | Host executing the call |
| `transport` | Accepting server mode | `local` or `ssh-mcp`; a listener session is `local` |
| `effective_capabilities` | The accepting server's `--operator` argv, resolved once at start | Capabilities Core may enforce for this call |

The adapter prevents caller-supplied tool input from replacing trusted session
context. A forwarded caller label and caller IP are not credentials and are not
read as any part of an authorization decision.

## 8. Authorization boundary

For direct SSH sessions, the destination serves the authority the argv asks for,
exactly as it does locally [ORB-12564]. Orbit is a single-user tool and an SSH
login to a destination is ownership of it: anyone who can start
`orbit mcp serve --operator` there can equally set `ORBIT_OPERATOR=1` on any
other command, so a destination-side ceiling would only be a file the caller can
rewrite. The client decides — a proxy or mux started with `--operator`
propagates it, and one running as an agent never does. `orbit_agent_invoke` is
admitted on `operator` like every other governed operation.

Core enforces the resulting capabilities and governed operations after the
accepting server has resolved the session. A proxy or UI may not grant authority,
because either can be bypassed by reaching the server directly. The TCP listener
is the one place that ignores its process's authority and serves `agent` only:
it authenticates no client, so a socket peer has not demonstrated the SSH login
the argv rule rests on. Federated destinations additionally enforce the mux's
destination and checkout-class checks.

## 9. Separate web transport

`orbit-web` serves the HTTP UI and owns the local-forward SSH mechanism used by
that UI. MCP does not use that listener or tunnel. Sharing application state does
not require sharing transport code.

## 10. Verification

[`references/conformance-v1.yaml`](./references/conformance-v1.yaml) maps the
contract to source and focused tests. The important behavioral gates are:

- exact tool-surface snapshot;
- protocol and production MCP round trips;
- direct SSH command construction and inherited stdio;
- the listener bind policy, and a loopback listener round trip that shows the
  accepted peer's IP reaching the audit context;
- server identity and SSH caller-IP parsing;
- discovery and unknown-name denial through one Core audit boundary; and
- crate dependency-direction checks.
