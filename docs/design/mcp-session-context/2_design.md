---
summary: "MCP Session Context — Design"
type: design
title: "MCP Session Context — Design"
owner: codex
last_updated: 2026-10-31
last_validated: 2026-10-31
status: Implemented
feature: mcp-session-context
doc_role: design
tags: ["mcp-session-context", "mcp", "workspace", "audit"]
paths: ["crates/orbit-types/src/tool/**", "crates/orbit-mcp/src/**", "crates/orbit-cli/src/command/mcp/**", "crates/orbit-core/src/adapter/command/**", "crates/orbit-store/src/driver/sqlite/audit_event_store/**", "crates/orbit-agent/src/providers/codex/**", "crates/orbit-engine/src/activity_job/cli_runner/**", "crates/orbit-common/src/security/child_env.rs"]
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
| caller_machine_name (legacy read alias `caller_host_id`) | Local accepting server | Local display label; unset for a remote caller |
| process_machine_id, process_machine_name (legacy read alias `process_host_id`) | Accepting server | Machine that executes the call |
| transport | Accepting server | local or ssh-mcp |
| caller_ip | Accepting SSH environment | Best-effort first field of SSH_CONNECTION |
| trace_id | MCP adapter | Fresh correlation ID for one tools/call |
| orchestrator | The server's launch binding | Crew name attributed to tasks the session creates; never an authorization or execution input |
| effective_capabilities | Serving process startup mode | Complete session grants used by Core authorization and tools/list filtering; never client-supplied |
| origin_session_id | MCP adapter | Server-minted ID stable for one MCP session and used for audit correlation |
| mcp_call_id | Federated call boundary | Destination/tool/request correlation ID used when a remote outcome is unknown [ORB-14656] |
| worker_invocation | Managed Orbit runtime | Validated task/claim/run binding carried through the runtime session, not editable job input [ORB-12496] |
| worker_host_call | Runtime or destination SSH launch path | Marks an internal worker projection; initialize metadata and tool arguments cannot set it |
| self_reported_actor | Initialize metadata | Normalized self-claim for audit only; it does not authenticate the caller |

Clients cannot populate trusted fields through initialize metadata or tool JSON. Initialize accepts a workspace selector under _meta.orbit.workspace (plus the compatibility spelling _meta["orbit.workspace"]) and an untrusted self-reported actor claim; caller/process/transport/correlation fields remain server-owned.

A session may also be bound at launch: `orbit mcp serve --workspace <selector>` seeds the trusted envelope's workspace before any client connects. That binding is decided by whoever wrote the launch configuration, not by the connecting client, but it is still only a selector — it is resolved against the registry on every call and overridden by an explicit per-call workspace.

`orbit mcp serve --orchestrator <crew>` binds a second launch value the same way, and is deliberately the weaker of the two: it is attribution, so it contributes nothing to effective_capabilities, to any authorization decision, or to the crew and model a task executes under. It applies at task creation only — orbit.task.add fills an omitted `orchestrator` from it, an explicit one on the call wins, and orbit.task.update never reads it, so no existing record is backfilled. The task records that crew's configured model as `orchestrator_model` at the same moment, unless the call passes `orchestrator_model`; the session binding never rewrites it on an existing task. Like the workspace binding it is only a name: the workspace the call lands in resolves it against its own crews and rejects an unconfigured one rather than substituting another. Both client modes forward it into the destination's argv, since a routed session carries no context of its own; a destination running a forced command composes its own argv and its configuration wins there. It is configuration, not authenticated evidence of the model answering a call — a connection outlives a client-side model switch, so a changed orchestrator needs a per-call value or a restarted session.

## 2. Local and SSH sessions

A local server derives caller and process identity from its own host identity. If no machine identity exists, the machine label is host/local and the display label falls back to the OS hostname or local.

Remote mode starts one child:

    ssh -T -- <host> "orbit mcp serve --remote-caller-machine-id '<label>'"

SSH inherits stdin, stdout, and stderr. Orbit does not parse or rewrite MCP in the proxy. The hidden caller argument marks the accepting session as ssh-mcp; its value remains audit metadata. The remote server derives its own process identity and may record the source address exposed by SSH_CONNECTION.

The stdio modes do not use a TCP listener or a reusable network session. The separate `orbit mcp listen` command accepts TCP connections, creates an independent `OrbitToolServer` session for each peer, and records the peer address as `caller_ip`; it binds loopback by default and supplies no authentication policy [ORB-10690] [ORB-10806]. The federated stdio mux is another distinct mode: it routes workspace calls to configured local or SSH destinations and routes task IDs through the configured task-prefix table [ORB-11009].

## 3. Workspace resolution

For a workspace-scoped tool, the authoritative server chooses the first non-empty selector:

1. workspace in the tool input;
2. workspace announced during initialize;
3. the workspace this server was launched for (`orbit mcp serve --workspace`, or a trusted managed child's `ORBIT_WORKSPACE` envelope when the flag is omitted);
4. otherwise, a missing-workspace error.

Steps 2 and 3 are one session field, resolved once at initialize: an announced workspace replaces the launch binding for that session, and a client that announces nothing falls back to the binding rather than clearing it. The fallback is the immutable launch value, so a re-initialize never inherits the previous client's claim. Most MCP clients cannot put _meta on their initialize at all, which is why a managed integration — what `orbit mcp init` and `orbit workspace init --mcp` generate — writes the binding into the argv it registers, using the logical `ws_*` ID so a linked worktree whose checkout identity diverged still names one workspace. A managed worker that launches `orbit mcp serve` without that argv still inherits the same logical selector from `ORBIT_WORKSPACE`, which is honored only together with managed-run provenance [ORB-11117]. A server the operator launched with no binding stays unbound, and every workspace-scoped call there must name its own workspace. An inherited `ORBIT_WORKSPACE` without that provenance is not a binding.

Process cwd is not an MCP fallback. The server resolves the selector against its registry, opens the selected local runtime, writes the resolved workspace_id into context, and normalizes an explicit workspace argument to the selected checkout path before Core dispatch.

For Codex, `orbit mcp init` writes `env_vars` on the local `[mcp_servers.orbit]` entry. The list forwards the managed binding names `ORBIT_MANAGED_RUN_CONTEXT`, `ORBIT_RUN_ID`, `ORBIT_SESSION_ID`, `ORBIT_WORKSPACE`, and `ORBIT_REGISTRY_ROOT`, the agent identity names `ORBIT_AGENT_NAME` and `ORBIT_AGENT_MODEL`, and the policy names `ORBIT_TASK_ACTOR_KIND`, `ORBIT_ACTIVITY_TOOLS`, `ORBIT_ACTIVITY_TOOL_POLICY`, `ORBIT_ACTIVITY_TOOLS_DENY`, `ORBIT_ACTIVITY_NAME`, `ORBIT_ACTIVITY_DEADLINE_UNIX_MS`, `ORBIT_ACTIVITY_FS_PROFILE`, `ORBIT_PROC_ALLOWED_PROGRAMS`, `ORBIT_PROC_PROGRAM_POLICY`, and `ORBIT_PROC_DISALLOWED_PROGRAMS`, the activity toolset name `ORBIT_ACTIVITY_MCP_TOOLSETS`, the timeout and build-budget names `ORBIT_ACTIVITY_TIMEOUT_MS` and `ORBIT_ACTIVITY_BUILD_BUDGET_DIR`, and the plugin broker locator `ORBIT_PLUGIN_BROKER`, from Codex's environment [ORB-13268] [ORB-13269] [ORB-13315] [ORB-13316] [ORB-13317]. The broker locator names a socket and grants nothing: the broker authenticates each connection by the kernel's peer identity against the run it serves, so forwarding it lets a nested server reach brokered tools (the claimed before-PR review bridge among them) without widening authority. A managed Codex launch overrides that list and supplies a complete enabled orbit MCP entry: `command` is the selected `ORBIT_BIN` and `args` are `mcp serve`. This works when the user's Codex config has no orbit entry and does not put managed envelope values in argv. A job run exports `ORBIT_RUN_ID`; a source inspection exports `ORBIT_SESSION_ID` instead. Either identity, together with the managed marker, allows `ORBIT_WORKSPACE` to bind the nested server. The forwarded identity and activity names make the nested server record agent attribution and use the same tool policy (allowlist, or deny-mode disallow list), filesystem profile, process program policy, and `proc.spawn` timeout budget (the activity's deadline) as the managed run. The selector remains subject to registry resolution and per-call override.

Global tools do not require a workspace selector.

orbit.task.show and orbit.task.artifact.get are the ID-resolved exceptions to the precedence above. Task IDs are a machine-global primary key in the coordination task registry, so a call carrying only {id} resolves the owning workspace from that registry and ignores the workspace announced at initialize — the announced workspace is ambient, like cwd, and is the right default for authoring but the wrong one for addressing an ID. A workspace passed in the tool input still wins and still filters: the call binds that workspace, and a task owned elsewhere is not found there. When the registry knows the ID but its owning checkout is unreadable or inactive, the error names that workspace rather than reporting the ID as unknown.

## 4. Adapter and tool surface

OrbitToolServer holds one context for its stdio session. Initialize may replace the workspace selector and the untrusted self-reported actor claim. For every tools/call, the adapter clones the session context and mints one fresh trace_id without writing it back.

tools/list is derived from the authoritative host and its validated result is cached per session/selector. Each definition carries a ToolSchema, one McpToolScope (Global or WorkspaceRequired) and, for a plugin tool that declares one, its own input schema, which is advertised as written instead of the schema derived from the ToolSchema parameters ([plugins §4.2](../plugins/1_scope.md#42-execution-protocol)). Scope controls only workspace-selector injection and server dispatch; it is not authorization metadata.

Workspace-bound schema decoration follows the same session selector. In particular, the `orbit.friction.add` and `orbit.friction.update` tag schemas enumerate the bound workspace's operator-owned taxonomy with descriptions. Before a workspace is bound, the schema falls back to the shipped taxonomy and explicitly notes that `.orbit/frictions/tags.yaml` may extend it. The tools-list cache key includes the normalized session selector so re-initializing a connection for another workspace cannot retain the previous workspace's vocabulary.

Because tools/list is answered per session, the injected selector documents the session the caller is actually in: optional in a bound session, required in an unbound one. The JSON schema also marks `workspace` required for an unbound workspace-scoped call. Federated tools/list requires its host-qualified selector. `orbit.task.show` keeps its v1 ID-only exemption. `orbit.search` additionally advertises that at least one of `query` or `tag` must be supplied; its MCP surface does not expose the CLI `path` filter.

For a plugin tool that does not declare `workspace` in its input schema, the MCP adapter moves that argument into the per-call routing context before dispatch. The plugin receives only its declared input. If the plugin itself declares `workspace`, the adapter preserves it in the plugin input.

The host selects toolsets at startup with `--toolset` or `machine.mcp.toolsets`; the default exposes tasks, workflows, runs, automation and decision tools, while optional sets include leases, experiments, analytics, decision-channel and agent-invoke. A call outside the selected set is refused as disabled. The default omits `agent-invoke`; managed explorer runs can opt in when configured [ORB-20234] [ORB-20382] [ORB-20414].

## 5. Core dispatch and audit

The server passes a resolved workspace call to Core through execute_tool_command_dispatch_with_session_context. Global calls use Core's global in-process dispatch seam. Unknown or unadvertised raw names use that same global seam and produce one denied row. Every tools/call therefore crosses one Core audit boundary exactly once with its per-call context.

Audit records include resolved workspace when applicable, caller/process metadata, transport, trace ID, and caller IP when present.

The managed MCP audit row records `job_run_id` from `ORBIT_RUN_ID` for a job run or `session_id` from `ORBIT_SESSION_ID` for a source inspection. The session ID is read only when the managed marker and a run or inspection identity are present.

Model-authored fields with names resembling audit fields do not override the supplied ToolSessionContext.

## 6. Explicitly deferred

MCP performs no lease validation, placement routing, broker negotiation, or Orbit principal authentication.

Claimed worker identity is now carried through a runtime-owned invocation binding, which provides the task/claim/run fence independently of editable job input [ORB-12496]. The nested MCP server still depends on the provider CLI forwarding activity policy, filesystem profile and process-policy environment, along with the managed workspace and attribution values [ORB-13268] [ORB-13269] [ORB-13315] [ORB-13316] [ORB-13317]. Replacing that policy propagation with host-owned state would be a separate authorization change.

Capability authorization is no longer deferred: Core's tool chokepoint authorizes a governed operation from the session's effective capabilities alone, and the serving process decides those once at startup — `orbit mcp serve` grants agent, `orbit mcp serve --operator` grants agent and operator ([ORB-10916], [ORB-10927]). McpCapability is still not MCP exposure metadata; `tools/list` is filtered by each session's effective capabilities, and calls remain subject to authorization when made.

## Task References

- [ORB-11117] — bound managed MCP sessions to the resolved workspace selector.
- [ORB-10916] — made Core authorization use the session's effective capabilities.
- [ORB-10927] — added MCP capability filtering while retaining call-time authorization.
- [ORB-10690] — added the explicit TCP MCP listener.
- [ORB-10806] — added the policy-free `orbit mcp listen` command.
- [ORB-11009] — established federated MCP routing and task-prefix lookup.
- [ORB-13268] — forwarded managed-run identity and activity policy into Codex MCP sessions.
- [ORB-13269] — enforced the activity tool allowlist and actor kind in Codex MCP sessions.
- [ORB-13315] — added the deny-list tool policy and its MCP environment mode.
- [ORB-13316] — moved shipped activities to the deny-list tool policy.
- [ORB-13317] — added process-program deny-list policy for shipped activities.
- [ORB-12496] — carried runtime-owned worker invocation bindings through coordination sessions.
- [ORB-14656] — made malformed federated replies surface an unknown outcome with call identity.
- [ORB-20234] — added opt-in MCP toolsets.
- [ORB-20382] — hid agent invocation from operator sessions.
- [ORB-20414] — moved agent invocation into the opt-in `agent-invoke` toolset.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
