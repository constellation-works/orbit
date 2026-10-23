---
title: Orbit MCP — Overview
owner: codex
last_updated: 2026-10-20
last_validated: 2026-10-20
status: Draft
feature: mcp-bridge
doc_role: overview
type: design
summary: One authoritative Orbit MCP server, reached by local stdio, a byte-transparent direct SSH stdio proxy, or a loopback-default TCP listener.
tags: [mcp, ssh, remote-access, registry, audit]
paths: ["crates/orbit-mcp/**", "crates/orbit-registry/**", "crates/orbit-core/**", "crates/orbit-cli/src/command/mcp/**"]
related_features: [host-registry, mcp-session-context, remote-access, federated-mcp]
---

# Orbit MCP — Overview

Orbit exposes the same MCP server over three transports:

```text
local:  MCP client <-> stdio <-> orbit mcp serve  <-> Orbit Core
remote: MCP client <-> stdio <-> SSH <-> orbit mcp serve <-> Orbit Core
socket: MCP client <-> TCP  <-> orbit mcp listen <-> Orbit Core
```

`orbit mcp serve --mode federated` is a separate stdio mode, not a fourth
transport. It includes the accepting machine plus operator-configured SSH
destinations and advertises host-qualified workspace selectors. Workspace-scoped calls that supply a selector route to its encoded
destination. For
id-routed task tools, an id-only call instead routes by the configured task-ID
prefix to the host that holds that task. Direct `--mode remote` remains the
byte-transparent one-host proxy described above.

The remote side is intentionally just direct SSH stdio. The local proxy starts a
non-interactive SSH process whose remote command is:

```text
ssh -T <host> orbit mcp serve --remote-caller-machine-id <audit-label>
```

The proxy inherits stdin, stdout, and stderr. It does not parse MCP frames, open a
checkout, resolve a workspace, filter tools, make authorization decisions, or
forward the call through another machine.

`orbit mcp listen` is the socket form of the same server, for deployments that
need one — typically reached through an SSH tunnel. It binds loopback unless a
wider bind is asked for explicitly, because the socket authenticates no client.
Local processes can still connect to loopback without authentication. A browser
page can also send HTTP to loopback, so the listener closes connections whose
first byte is not a JSON object start before rmcp sees any message. It is a
transport adapter only: it adds no broker, checkout preflight, placement
routing, or client-side capability filter. The listener's server context is
agent-only, as described below.

## Runtime rule

The local server's Orbit data root is selected by `--root` first, then a
non-empty `ORBIT_ROOT`, then the default `~/.orbit`. This applies to stdio,
TCP, and the local destination of `--mode federated`. Without an explicit
override, managed children retain their dispatching `ORBIT_REGISTRY_ROOT`.
The selected root supplies machine identity, workspace discovery, and tool
state; workspace selection still happens per session or tool call. The direct
SSH proxy (`--mode remote`) refuses local root overrides: configure the data
root on the SSH destination instead.

The machine accepting `orbit mcp serve` or `orbit mcp listen` is authoritative
for the call. It:

1. derives its process identity from its local registry;
2. uses definition scope to decide whether a workspace is required;
3. resolves any required workspace against its own registry and opens that
   server-local runtime;
4. sends each host-dispatched tool call, including global discovery, unknown
   raw names, and server-resolution failures, through Orbit Core exactly once;
   and
5. records outcomes for calls that cross this boundary.

Adapter-owned UI presentation entrypoints may return open, empty-state, or
input-validation results locally. Selected task and run reads delegate to the
backing read tool through Core.

This is the same rule for direct stdio, SSH-originated, and socket sessions. A
transport changes only how MCP bytes reach the server. The federated mode is
the explicit exception: its mux answers federated discovery and routes
host-qualified calls, while each destination still applies its own local
resolution, authorization, and Core dispatch.

## Audit context

Each Core-dispatched tool call carries a fresh `trace_id`. The Core audit row
records:

- `caller_machine_id`: an audit-only label supplied by the direct SSH proxy, or
  the local process identity when available;
- `caller_ip`: the first field of `SSH_CONNECTION` for an SSH session, or the
  accepted peer's address for a listener session;
- `process_machine_id` and `process_machine_name`: derived by the accepting
  server. `process_host_id` is accepted as a legacy input alias for the latter;
- `transport`: `local` or `ssh-mcp`. A listener session is `local`, because it
  is served by the same local process with the same envelope; `caller_ip` is
  what distinguishes it.

`host/local` is the fallback machine label when no persisted identity is
available. A forwarded caller label is audit-only: it marks the session's
transport as `ssh-mcp` and names the calling machine, and it contributes to no
authorization decision [ORB-12564]. `caller_ip` is likewise observational.

## Ownership

| Concern | Owner |
|---|---|
| MCP framing, tool discovery, server identity context, TCP listener, direct SSH stdio proxy, and the federated mux | `orbit-mcp` |
| Host identity and workspace-registry state | `orbit-registry` |
| Server composition and server-local runtime selection | `orbit-cli` |
| Domain validation, capability enforcement, sandboxing, audit persistence, and runtime authorization | `orbit-core` (with the session envelope composed by `orbit-mcp`) |
| Canonical builtin tool definitions | `orbit-tools` |
| HTTP UI and its own local-forward SSH connection | `orbit-web` |

`orbit-web` is a separate application surface. Its HTTP tunnel is not an MCP
transport and is not reused by MCP.

## V1 boundaries

Direct v1 deliberately has no shared broker, client-side checkout preflight,
owner-placement routing, or client-side capability filtering. The destination
resolves session authority from the argv it was started with — for an
SSH-originated session exactly as for a local one, because an SSH login to the
destination is ownership of it [ORB-12564] — and Core enforces those effective
capabilities at the tool boundary. The client is what decides: a proxy started
with `--operator` composes an operator argv for its destination, and a client
running as an agent never does. The TCP listener remains a transport only and
authenticates no client, so it is hardcoded to agent authority and binds
loopback unless a wider bind is explicitly requested.

Federated mode adds the configured-destination mux as an explicit namespace
exception. It performs live discovery and host-qualified routing, and the
destination additionally enforces whether its checkout holds the tool's
`control_plane` or `execute` capability class.

Each tool definition's global-versus-workspace-required scope decides only
whether a workspace selector is required; it does not encode caller capability,
placement, or authorization. `orbit.workspace.list` is the sole global discovery
tool in the MCP-owned server surface; plugins may declare global tools through
their own scope. In direct mode it reports active logical workspaces with a
registered checkout on the accepting machine; federated mode replaces that
response
with live descriptors for the accepting machine and configured destinations.

The executable contract and validation map live in
[`references/conformance-v1.yaml`](./references/conformance-v1.yaml). Detailed
request flow is in [`2_design.md`](./2_design.md), future work in
[`3_vision.md`](./3_vision.md), and the current decision set in
[`4_decisions.md`](./4_decisions.md).
