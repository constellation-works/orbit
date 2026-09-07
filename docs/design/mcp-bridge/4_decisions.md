---
title: Orbit MCP — Decisions
owner: codex
last_updated: 2026-09-07
last_validated: 2026-09-07
status: Draft
feature: mcp-bridge
doc_role: decisions
type: design
summary: Current decisions for the direct-SSH MCP v1, stated without historical design chains.
tags: [mcp, ssh, remote-access, registry, audit]
paths: ["crates/orbit-mcp/**", "crates/orbit-registry/**", "crates/orbit-core/**", "crates/orbit-web/**"]
related_features: [host-registry, mcp-session-context]
---

# Orbit MCP — Decisions

This file records only the decisions that define the current implementation.

## The accepting machine is authoritative

**Context.** Client-side checkout and routing logic duplicated server knowledge
and could be bypassed by calling the server directly.

**Decision.** A client selects an SSH destination. The accepting machine resolves
its own registry and runtime, performs validation through Core, and owns the audit
record. It never asks the caller to prove local checkout placement.

**Consequences.** Local and remote calls have one correctness boundary. A caller
that selects the wrong host receives a server-side error rather than being relayed.

## Remote MCP is direct SSH stdio

**Context.** MCP already supports stdio, and the supported remote environment
already provides SSH.

**Decision.** The local proxy starts one `ssh -T` process running remote
`orbit mcp serve`. The child inherits stdin, stdout, and stderr. The proxy does not
parse frames or retry calls.

**Consequences.** The remote path needs no port-forward tunnel, shared broker, or
third-machine relay. SSH owns transport security and shell access, while the
destination's callers policy and Core enforce Orbit's session authority after
the accepting process receives the bytes.

## A socket deployment gets its own command, not a mode of `serve`

**Context.** Some deployments need the server on a socket — typically a
server-side Orbit reached through an SSH tunnel — but overloading the stdio
server with a transport flag hid which server a given invocation was.

**Decision.** `orbit mcp listen` is a separate command that serves the same host
over TCP; `orbit mcp serve` stays stdio-only. The listener binds loopback unless
`--allow-non-loopback` is passed, applies that policy before opening the socket,
and serves each accepted connection as an independent session carrying the peer's
IP as audit metadata.

**Consequences.** A transport is chosen by naming it. The listener adds no
capability filter, placement rule, owner routing, or checkout authority; a socket
call reaches the same host and the same single Core dispatch and audit boundary a
stdio call does. Because the socket authenticates no client, restricting who can
reach it stays a deployment responsibility, and the default bind is the one that
cannot be reached off-box.

## Every tool call crosses Core once

**Context.** Audit and validation become unreliable when discovery or failure paths
bypass the normal dispatcher.

**Decision.** In the direct server, every `tools/call`, including global
discovery, unknown or unadvertised raw names, and workspace setup failures,
enters Core's dispatch and audit seam exactly once with the per-call session
context. Server-local projections and pre-runtime denials use Core's global
in-process dispatch hook. Federated mode is the explicit namespace exception:
its mux answers federated discovery and routes calls, while each destination's
direct server applies this rule.

**Consequences.** Successes and failures share one audit model without adding a
Core dependency on MCP or the registry crate.

## Caller metadata and destination policy

**Context.** The proxy can supply a machine label and the SSH server exposes a
source IP. A forwarded label is self-asserted, while a destination can also
receive a forced-command identity tied to the key sshd authenticated.

**Decision.** Record the caller label or destination-composed caller identity,
best-effort SSH caller IP, accepting process identity, transport, and a fresh
trace ID. Use `host/local` when no machine identity is available. Resolve remote
session authority from the destination's `~/.orbit/mcp-callers.toml`; the
forwarded label and IP alone do not grant authority, and a forced-command caller
identity is recorded as key-bound when sshd authenticated its key.

**Consequences.** Records support correlation and make the destination's
authority decision inspectable. Tier 1 remains a self-asserted identity, while
the optional Tier 2 path establishes a key-bound remote caller without treating
the network address as a credential.

## Authorization is enforced by the destination and Core

**Context.** A UI or proxy check can always be bypassed by invoking the server.

**Decision.** The destination resolves `~/.orbit/mcp-callers.toml` at SSH session
establishment. The caller's requested `agent`/`operator` authority is intersected
with that destination grant, and Core enforces the effective capabilities and
governed operations after server-side workspace resolution. The optional forced
command binds a caller identity to the key sshd authenticated; `agent_invoke`
remains a separate workspace-scoped grant. The TCP listener authenticates no
client and therefore serves agent authority only.

**Consequences.** Direct local, SSH, and socket calls share Core's operation
boundary, while remote authority is capped by the destination that executes the
call. Federated destinations apply their own policy independently, so the mux
cannot grant authority on a destination's behalf.

## Crates follow present responsibilities

**Context.** A broad remote feature layer accumulated unrelated registry, protocol,
routing, and UI concerns.

**Decision.** Keep MCP protocol, direct SSH support, destination caller policy,
and federated routing in `orbit-mcp`; host and workspace state in
`orbit-registry`; domain execution, capability enforcement, and audit in
`orbit-core`; canonical builtin definitions in `orbit-tools`; and HTTP UI
behavior in `orbit-web`.

**Consequences.** Dependency direction follows the data and execution boundaries.
There is no general remote layer between MCP and Core.

## Web and MCP transports stay separate

**Context.** The HTTP UI and MCP both use SSH in some workflows but expose different
protocols and lifecycles.

**Decision.** `orbit-web` owns its local-forward tunnel. `orbit-mcp` owns its direct
stdio SSH process. Neither transport is a shared abstraction.

**Consequences.** The UI can evolve independently without turning its loopback HTTP
listener into an MCP dependency.

## Tool schemas are composed, not copied

**Context.** Repeated schema declarations drift from actual dispatch behavior.

**Decision.** Compose the advertised MCP surface from canonical builtin definitions
and MCP-owned discovery definitions. Each definition carries only its schema and
global-versus-workspace-required scope.

**Consequences.** Discovery, dispatch lookup, snapshots, and documentation describe
one surface.
