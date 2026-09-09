---
summary: "MCP Session Context — Overview"
type: design
title: "MCP Session Context — Overview"
owner: codex
last_updated: 2026-09-09
last_validated: 2026-09-09
status: Accepted
feature: mcp-session-context
doc_role: overview
tags: ["mcp-session-context", "mcp", "workspace", "audit"]
paths: ["crates/orbit-types/src/tool/**", "crates/orbit-mcp/src/**", "crates/orbit-cli/src/command/mcp/**", "crates/orbit-core/src/adapter/tool_host/**"]
related_features: ["mcp-session-context"]
related_artifacts: []
---

# MCP Session Context — Overview

ToolSessionContext is Orbit's transport-to-Core invocation envelope. It keeps caller-supplied addressing separate from facts constructed by the accepting Orbit server.

## Live v1 flow

1. A client selects a workspace in tool input or MCP initialize metadata.
2. The accepting server resolves that address to a registered workspace and local runtime.
3. The server supplies caller, process, and transport audit metadata.
4. The MCP adapter mints a fresh trace_id for each tools/call request.
5. Core dispatch executes the tool and writes the authoritative audit event from the trusted context.

The external workspace value is addressing input, not a trusted workspace identity. The resolved workspace_id is written only after server-side resolution.

## Identity and transport

- caller_machine_id is an opaque audit label. It is not an authenticated principal.
- A direct SSH proxy forwards the caller's persisted machine ID when available and host/local otherwise.
- caller_ip is best-effort audit data taken from SSH_CONNECTION when an SSH server exposes it.
- process_machine_id and process_host_id describe the machine accepting and executing the call.
- transport is local or ssh-mcp.

## Current boundary

MCP v1 still has one authoritative server host and stdio framing, either local or carried byte-for-byte through SSH. The current surface also has an explicit TCP listener transport and an opt-in federated stdio mux that routes to configured SSH destinations. Tool definitions carry global-versus-workspace-required scope; the accepting server and Core resolve capabilities and authorization. Caller machine and network labels remain audit evidence rather than authenticated principals, except where destination-owned SSH acceptance supplies key-bound caller evidence.

See [2_design.md](./2_design.md) for the concrete path, [3_vision.md](./3_vision.md) for evolution gates, and [4_decisions.md](./4_decisions.md) for current design choices.
