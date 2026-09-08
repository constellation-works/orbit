---
title: Orbit MCP — Overview
owner: codex
last_updated: 2026-09-07
last_validated: 2026-09-07
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
destinations, advertises host-qualified workspace selectors, and routes each
selected call to the encoded destination. Direct `--mode remote` remains the
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
It is a transport adapter only: it adds no broker, checkout preflight, placement
routing, or capability filter.

## Runtime rule

The machine accepting `orbit mcp serve` or `orbit mcp listen` is authoritative
for the call. It:

1. derives its process identity from its local registry;
2. uses definition scope to decide whether a workspace is required;
3. resolves any required workspace against its own registry and opens that
   server-local runtime;
4. sends every call, including an unknown raw name, through Orbit Core exactly
   once; and
5. records success, failure, or denial at that boundary.

This is the same rule for direct stdio, SSH-originated, and socket sessions. A
transport changes only how MCP bytes reach the server. The federated mode is
the explicit exception: its mux answers federated discovery and routes
host-qualified calls, while each destination still applies its own local
resolution, authorization, and Core dispatch.

## Audit context

Each tool call carries a fresh `trace_id`. The server also records:

- `caller_machine_id`: an audit-only label supplied by the direct SSH proxy, or
  the local process identity when available;
- `caller_ip`: the first field of `SSH_CONNECTION` for an SSH session, or the
  accepted peer's address for a listener session;
- `process_machine_id` and `process_host_id`: derived by the accepting server;
- `transport`: `local` or `ssh-mcp`. A listener session is `local`, because it
  is served by the same local process with the same envelope; `caller_ip` is
  what distinguishes it.

`host/local` is the fallback machine label when no persisted identity is
available. A forwarded caller label is audit-only under the self-asserted
Tier-1 path. The optional forced-command path binds the caller identity to a
key sshd authenticated and the destination's callers file then caps the
session; `caller_ip` remains observational metadata in both cases.

## Ownership

| Concern | Owner |
|---|---|
| MCP framing, tool discovery, server identity context, TCP listener, direct SSH stdio proxy, and the federated mux | `orbit-mcp` |
| Host identity and workspace-registry state | `orbit-registry` |
| Server composition and server-local runtime selection | `orbit-cli` |
| Domain validation, capability enforcement, sandboxing, audit persistence, and runtime authorization | `orbit-core` (with destination caller grants resolved by `orbit-mcp`) |
| Canonical builtin tool definitions | `orbit-tools` |
| HTTP UI and its own local-forward SSH connection | `orbit-web` |

`orbit-web` is a separate application surface. Its HTTP tunnel is not an MCP
transport and is not reused by MCP.

## V1 boundaries

Direct v1 deliberately has no shared broker, client-side checkout preflight,
owner-placement routing, or client-side capability filtering. The destination
does apply authorization: `~/.orbit/mcp-callers.toml` caps a remote session's
requested `agent`/`operator` authority, and Core enforces those effective
capabilities at the tool boundary. Tier 1 resolves a self-asserted forwarded
machine label; the optional Tier 2 forced-command path binds that identity to
the key sshd authenticated. The TCP listener remains a transport only and
authenticates no client, so it is hardcoded to agent authority and binds
loopback unless a wider bind is explicitly requested.

Federated mode adds the configured-destination mux as an explicit namespace
exception. It performs live discovery and host-qualified routing, and the
destination additionally enforces whether its checkout holds the tool's
`control_plane` or `execute` capability class.

Advertised definitions contain only schema plus global-versus-workspace-required
scope. `orbit.workspace.list` is the sole global tool. In direct mode it reports
active logical workspaces that have a checkout registered on the accepting
machine; federated mode replaces that response with live descriptors for the
accepting machine and configured destinations.

The executable contract and validation map live in
[`references/conformance-v1.yaml`](./references/conformance-v1.yaml). Detailed
request flow is in [`2_design.md`](./2_design.md), future work in
[`3_vision.md`](./3_vision.md), and the current decision set in
[`4_decisions.md`](./4_decisions.md).
