---
summary: "MCP Session Context — Overview"
type: design
title: "MCP Session Context — Overview"
owner: codex
last_updated: 2026-10-31
last_validated: 2026-11-01
status: Implemented
stage: shipped
next: "—"
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

1. A workspace-scoped call may select a workspace in tool input or MCP initialize
   metadata; without one, the server may use its launch workspace. An id-routed
   task call can identify its owner by task ID.
2. The accepting server resolves the workspace address or task ID to a registered
   workspace and local runtime under the applicable local or federated routing
   rules.
3. The server supplies caller, process, and transport audit metadata.
4. The MCP adapter mints a fresh trace_id for each tools/call request.
5. Core dispatch executes the tool and writes the authoritative audit event from the trusted context.

The external workspace value is addressing input, not a trusted workspace identity. The resolved workspace_id is written only after server-side resolution. Design approval is recorded with the Host Registry and MCP Bridge design [ORB-10228].

## Identity and transport

- caller_machine_id is an opaque audit label. It is not an authenticated principal.
- A direct SSH proxy forwards the caller's persisted machine ID when available and host/local otherwise.
- caller_ip is best-effort audit data taken from SSH_CONNECTION when an SSH server exposes it.
- process_machine_id and process_machine_name describe the machine accepting and executing the call.
- transport is `local` or `ssh-mcp`; TCP listener sessions are recorded as
  `local`, with their peer address in `caller_ip`.

## Current boundary

Local and SSH modes use stdio framing; the SSH proxy carries those bytes through unchanged. The separate `orbit mcp listen` TCP listener gives each accepted connection its own server session and records the peer address as best-effort `caller_ip`; it is loopback-only by default [ORB-10690] [ORB-10806]. The opt-in federated stdio mux routes to configured local and SSH destinations [ORB-11009]. Workspace selectors apply to workspace-scoped calls; federated id-routed task calls use the task ID and configured task-prefix table. Tool definitions carry global-versus-workspace-required scope; the accepting server and Core resolve capabilities and authorization. Caller machine and network labels remain audit evidence rather than authenticated principals; a destination serves the authority the session's argv asks for, because an SSH login to it is ownership of it [ORB-12564].

See [2_design.md](./2_design.md) for the concrete path, [3_vision.md](./3_vision.md) for evolution gates, and [4_decisions.md](./4_decisions.md) for current design choices.

## Task References

- [ORB-10228] — accepted the Host Registry and MCP Bridge design.
- [ORB-10690] — added the explicit TCP MCP listener.
- [ORB-10806] — defined the policy-free `orbit mcp listen` entry point.
- [ORB-11009] — established the federated MCP routing contract.
- [ORB-12564] — propagated operator authority through the federated destination's SSH policy.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
