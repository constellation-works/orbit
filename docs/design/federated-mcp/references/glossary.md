---
type: design
summary: "Glossary: Federated MCP"
last_validated: 2026-09-19
title: Glossary — Federated MCP
owner: grok
status: Draft
feature: federated-mcp
tags: [federated-mcp, glossary]
related_features: [federated-mcp, host-registry, mcp-bridge]
related_artifacts: [ORB-11044, ORB-11023, ORB-11010, ORB-11009, ORB-11008]
---

# Glossary: Federated MCP

Vocabulary for the proposed federated MCP mux. Standard industry terms (proxy, mux, TTL) are included only when this feature gives them a specific meaning. Host-registry catalog terms (`machine_id`, owner checkout, replica checkout, checkout health) keep their v1 meanings; this table maps them into the federated surface rather than redefining them.

| Term | Meaning |
|------|---------|
| **Ambiguous destination** | Duplicate `machine_id` among configured destinations. Raised at **config load** as `ambiguous_destination`, not per call. A token that is not uniquely host-qualified (bare `ws_*`) is `unknown_selector`, not this class. [specs/federated-workspace-mcp.md](../specs/federated-workspace-mcp.md) |
| **Callers file** | Retired. `~/.orbit/mcp-callers.toml` declared a destination-side ceiling; removed in [ORB-12564] because an SSH login to a destination is ownership of it, so the ceiling lived in a file the caller could rewrite. A leftover file is ignored with one warning. [4_decisions.md](../4_decisions.md#an-ssh-login-to-a-destination-is-ownership-of-it) |
| **Capabilities** | Classes a destination holds for a workspace, at least `control_plane` and `execute`. Determined by the destination's local catalog role. List advertisement is a hint that may lag; Destination Core refusal is the correctness boundary. A workspace with absent `owner_machine_id` cannot advertise `control_plane`. [2_design.md §3](../2_design.md) |
| **Checkout health** | Repo-root presence at the destination (`active` / `invalid` / `unknown` when the host cannot be probed). Not SSH reachability. [specs/federated-workspace-mcp.md](../specs/federated-workspace-mcp.md) |
| **Control-plane authority** | The declared owner checkout (or a later cloud-offloaded store) that owns task issuance and the coordination store for that workspace. One per repository is operator configuration, not a mux invariant. [2_design.md §4](../2_design.md) |
| **Destination** | The accepting machine (implicit local membership) or an operator-configured SSH remote the mux may forward to. Not a host-registry fleet member. Local workspaces need no destinations-file row. [2_design.md §1](../2_design.md) |
| **Execute binding** | A replica checkout: it can run, log, and schedule on that host and must refuse control-plane tools. [2_design.md §3](../2_design.md) |
| **Delivery budget** | The budget a routed `tools/call` gets, stamped when its request is written. Separate from the probe budget that bounds SSH setup, the handshake, discovery, and `tools/list`, so classification never spends the tool's execution time. [2_design.md §6](../2_design.md) |
| **Fail-closed routing** | Live delivery with a single caller-facing precedence: `unknown_selector` → `ambiguous_destination` (config) → `unreachable_destination` → `stale_route` → `unhealthy_checkout` → `tool_not_on_this_host` → `capability_refused`. No local fallback, default workspace, or `ws_*` substitution. Cached list health does not decide the error. [specs/federated-workspace-mcp.md](../specs/federated-workspace-mcp.md) |
| **Federated namespace** | The caller-facing MCP surface that accepts host-qualified selectors and list-without-session. The only place the v1 no-relay rule is excepted. [mcp-bridge 3_vision.md §5](../../mcp-bridge/3_vision.md) |
| **Federated workspace list** | New session-unbound shape for `orbit_workspace_list`. Puts `machine_id` on each descriptor, not the v1 envelope, and does not inherit the v1 Active-and-locally-checked-out filter. [specs/federated-workspace-mcp.md](../specs/federated-workspace-mcp.md) |
| **Gateway** | The mux process that advertises the federated namespace. It does not own destination state and does not rewrite destinations. [1_overview.md](../1_overview.md) |
| **Granted authority** | Retired with the callers file [ORB-12564]. A destination declares no per-caller ceiling; it serves the authority the session's argv asks for. [4_decisions.md](../4_decisions.md#an-ssh-login-to-a-destination-is-ownership-of-it) |
| **Host reachability** | Whether the configured destination answers (reachable / unreachable). Separate from checkout health. [specs/federated-workspace-mcp.md](../specs/federated-workspace-mcp.md) |
| **Host-qualified selector** | Structured, caller-uninterpreted addressing token. Encoding `hm_<id>/ws_*` is normative. Not `host_id`, not a path, not a credential. Callers copy the list `selector` field; they must not parse or construct the token. [specs/federated-workspace-mcp.md](../specs/federated-workspace-mcp.md) |
| **Mux** | A configured forwarder of already-chosen destinations, not a registry and not automatic owner discovery. [2_design.md §1](../2_design.md) |
| **Outcome unknown** | A routed `tools/call` whose request was written and whose answer never arrived. Reported as `outcome_unknown`, never `unreachable_destination`: the remote work may have committed, so a retry could duplicate it. Post-dispatch, and therefore outside the fail-closed precedence ladder. [2_design.md §6](../2_design.md) |
| **Propagated authority** | The `--operator` a federated or remote-proxy client was started with, emitted into the argv of every SSH destination it opens. A statement, not a request: the destination serves it. A client running as an agent never propagates it. [4_decisions.md](../4_decisions.md#an-ssh-login-to-a-destination-is-ownership-of-it) |
| **Remote-originated session** | A served session that carries a forwarded `--remote-caller-machine-id`. It marks the transport as `ssh-mcp` and names the calling machine for audit; it changes no authorization. [4_decisions.md](../4_decisions.md#an-ssh-login-to-a-destination-is-ownership-of-it) |
| **Session authority** | The `agent` / `operator` capabilities an MCP session holds, governing `GOVERNED_OPERATIONS`. Resolved once from argv at server start, locally and remotely alike. Orthogonal to capability class: a call must clear both. [4_decisions.md](../4_decisions.md#an-ssh-login-to-a-destination-is-ownership-of-it) |
| **Stale route** | Destination is configured; a live probe shows the workspace is absent. Fails as `stale_route`. Distinct from `unknown_selector` (selector never valid). [specs/federated-workspace-mcp.md](../specs/federated-workspace-mcp.md) |
| **Tool class** | Assignment by behavior: coordination-store writes are `control_plane`; runs/logs/scheduler tools are `execute`; discovery/list tools are unclassified and not subject to `capability_refused`. Not a per-tool registry field. [specs/federated-workspace-mcp.md](../specs/federated-workspace-mcp.md) |
| **Tool-not-on-this-host** | The destination is identified but does not advertise the tool. Distinct from `unknown_selector`. [specs/federated-workspace-mcp.md](../specs/federated-workspace-mcp.md) |
| **Unclassified tool** | Discovery/list tool. Not subject to `capability_refused`. [specs/federated-workspace-mcp.md](../specs/federated-workspace-mcp.md) |
