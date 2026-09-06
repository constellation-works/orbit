---
summary: "MCP Session Context — Design"
type: design
title: "MCP Session Context — Design"
owner: codex
last_updated: 2026-08-30
last_validated: 2026-08-30
status: Accepted
feature: mcp-session-context
doc_role: design
tags: ["mcp-session-context", "mcp", "workspace", "audit"]
paths: ["crates/orbit-common/src/types/tool.rs", "crates/orbit-mcp/src/**", "crates/orbit-cli/src/command/mcp/**", "crates/orbit-core/src/command/tool/**", "crates/orbit-store/src/sqlite/audit_event_store/**"]
related_features: ["mcp-session-context"]
related_artifacts: []
---

# MCP Session Context — Design

## 1. Field ownership

| Field | Source | Meaning |
|---|---|---|
| workspace | Tool input, initialize metadata, or the server's launch binding | Address selector, resolved per call against the accepting server's registry |
| workspace_id | Authoritative server resolution | Stable logical workspace selected for execution |
| caller_machine_id | Local identity or SSH proxy argument | Opaque audit correlation label |
| caller_host_id | Local accepting server | Local display label; unset for a remote caller |
| process_machine_id, process_host_id | Accepting server | Machine that executes the call |
| transport | Accepting server | local or ssh-mcp |
| caller_ip | Accepting SSH environment | Best-effort first field of SSH_CONNECTION |
| trace_id | MCP adapter | Fresh correlation ID for one tools/call |
| orchestrator | The server's launch binding | Crew name attributed to tasks the session creates; never an authorization or execution input |

Clients cannot populate trusted fields through initialize metadata or tool JSON. Initialize accepts only the workspace selector under _meta.orbit.workspace, plus the compatibility spelling _meta["orbit.workspace"].

A session may also be bound at launch: `orbit mcp serve --workspace <selector>` seeds the trusted envelope's workspace before any client connects. That binding is decided by whoever wrote the launch configuration, not by the connecting client, but it is still only a selector — it is resolved against the registry on every call and overridden by an explicit per-call workspace.

`orbit mcp serve --orchestrator <crew>` binds a second launch value the same way, and is deliberately the weaker of the two: it is attribution, so it contributes nothing to effective_capabilities, to any authorization decision, or to the crew and model a task executes under. It applies at task creation only — orbit.task.add fills an omitted `orchestrator` from it, an explicit one on the call wins, and orbit.task.update never reads it, so no existing record is backfilled. Like the workspace binding it is only a name: the workspace the call lands in resolves it against its own crews and rejects an unconfigured one rather than substituting another. Both client modes forward it into the destination's argv, since a routed session carries no context of its own; a destination running a forced command composes its own argv and its configuration wins there. It is configuration, not authenticated evidence of the model answering a call — a connection outlives a client-side model switch, so a changed orchestrator needs a per-call value or a restarted session.

## 2. Local and SSH sessions

A local server derives caller and process identity from its own host identity. If no machine identity exists, the machine label is host/local and the display label falls back to the OS hostname or local.

Remote mode starts one child:

    ssh -T -- <host> "orbit mcp serve --remote-caller-machine-id '<label>'"

SSH inherits stdin, stdout, and stderr. Orbit does not parse or rewrite MCP in the proxy. The hidden caller argument marks the accepting session as ssh-mcp; its value remains audit metadata. The remote server derives its own process identity and may record the source address exposed by SSH_CONNECTION.

There is no TCP listener or reusable network session in v1.

## 3. Workspace resolution

For a workspace-scoped tool, the authoritative server chooses the first non-empty selector:

1. workspace in the tool input;
2. workspace announced during initialize;
3. the workspace this server was launched for (`orbit mcp serve --workspace`, or a trusted managed child's `ORBIT_WORKSPACE` envelope when the flag is omitted);
4. otherwise, a missing-workspace error.

Steps 2 and 3 are one session field, resolved once at initialize: an announced workspace replaces the launch binding for that session, and a client that announces nothing falls back to the binding rather than clearing it. The fallback is the immutable launch value, so a re-initialize never inherits the previous client's claim. Most MCP clients cannot put _meta on their initialize at all, which is why a managed integration — what `orbit mcp init` and `orbit workspace init --mcp` generate — writes the binding into the argv it registers, using the logical `ws_*` ID so a linked worktree whose checkout identity diverged still names one workspace. A managed worker that launches `orbit mcp serve` without that argv still inherits the same logical selector from `ORBIT_WORKSPACE`, which is honored only together with managed-run provenance [ORB-11117]. A server the operator launched with no binding stays unbound, and every workspace-scoped call there must name its own workspace. An inherited `ORBIT_WORKSPACE` without that provenance is not a binding.

Process cwd is not an MCP fallback. The server resolves the selector against its registry, opens the selected local runtime, writes the resolved workspace_id into context, and normalizes an explicit workspace argument to the selected checkout path before Core dispatch.

Global tools do not require a workspace selector.

orbit.task.show is the one exception to the precedence above. Task IDs are a machine-global primary key in the coordination task registry, so a call carrying only {id} resolves the owning workspace from that registry and ignores the workspace announced at initialize — the announced workspace is ambient, like cwd, and is the right default for authoring but the wrong one for addressing an ID. A workspace passed in the tool input still wins and still filters: the call binds that workspace, and a task owned elsewhere is not found there. When the registry knows the ID but its owning checkout is unreadable or inactive, the error names that workspace rather than reporting the ID as unknown.

## 4. Adapter and tool surface

OrbitToolServer holds one context for its stdio session. Initialize may replace only the workspace selector. For every tools/call, the adapter clones the session context and mints one fresh trace_id without writing it back.

tools/list comes from the authoritative host on every request. Each definition carries a ToolSchema and one McpToolScope: Global or WorkspaceRequired. Scope controls only workspace-selector injection and server dispatch; it is not authorization metadata.

Because tools/list is answered per session, the injected selector documents the session the caller is actually in: optional in a bound session, required in an unbound one. Both spellings describe the same server rule; only the obligation on the caller differs.

## 5. Core dispatch and audit

The server passes a resolved workspace call to Core through execute_tool_command_dispatch_with_session_context. Global calls use Core's global in-process dispatch seam. Unknown or unadvertised raw names use that same global seam and produce one denied row. Every tools/call therefore crosses one Core audit boundary exactly once with its per-call context.

Audit records include resolved workspace when applicable, caller/process metadata, transport, trace ID, and caller IP when present.

Model-authored fields with names resembling audit fields do not override the supplied ToolSessionContext.

## 6. Explicitly deferred

MCP performs no lease validation, placement routing, broker negotiation, or Orbit principal authentication.

Capability authorization is no longer deferred: Core's tool chokepoint authorizes a governed operation from the session's effective capabilities alone, and the serving process decides those once at startup — `orbit mcp serve` grants agent, `orbit mcp serve --operator` grants agent and operator ([ORB-10916], [ORB-10927]). McpCapability is still not MCP exposure metadata; `tools/list` advertises the same surface to every session.
