# Tools: serving MCP and toolsets

Contents:

- [Serving MCP: `orbit mcp serve` flags](#serving-mcp-orbit-mcp-serve-flags)
- [MCP toolsets](#mcp-toolsets)

## Serving MCP: `orbit mcp serve` flags

`orbit mcp serve --help` keeps one line per flag; these are its rules.

- `--mode remote` proxies one chosen SSH destination and requires it as an
  argument. `--mode federated` includes this machine automatically and muxes
  every host registered with `orbit host add`; it takes no argument.
  - A
    federated server binds to no single workspace: it lists each destination's
    workspaces as live descriptors, probing remotes on every call and including
    remotes that are unreachable right now rather than hiding them.
    Workspace-scoped tools take the host-qualified `selector` copied from that
    list; a registered name, a bare `ws_*` or `--workspace` is not a federated
    selector.
- **Omit `--operator` for any server an agent launches.** Without it a session
  holds the agent capability only, and governed operations (such as
  dispatching a workflow or deleting a task) are refused. The
  flag is the deliberate act: `ORBIT_OPERATOR` in the server's environment is
  ignored on the MCP surface, so an operator shell cannot grant operator
  authority to an agent's server by accident.
- With `--mode remote` or `--mode federated`, `--operator` is also the operator
  statement for every SSH destination the client opens: Orbit is a single-user
  tool and an SSH login to a machine is ownership of it, so the destination
  serves the authority this argv asks for. A client that is itself running as
  an agent never propagates it.
- `--workspace <SELECTOR>` takes a workspace name, a logical `ws_*` ID or an
  absolute registered checkout path. Most MCP clients cannot announce
  `_meta.orbit.workspace` on initialize, so without `--workspace` a
  workspace-scoped tool must repeat the selector on every call.
  - Local
    `orbit mcp init` and `orbit workspace init --mcp` write it into the
    integrations they generate; a managed child that launches `orbit mcp serve`
    without the flag still inherits `ORBIT_WORKSPACE` from the trusted execution
    envelope. The federated init path is session-unbound. A client that does
    announce a workspace, and any explicit per-call `workspace`, still take
    precedence. The selector is resolved against the accepting server's
    registry per call, never from the server process cwd.
- `--orchestrator <CREW>` is attribution only. Unlike `--operator` it grants
  nothing: a session still holds exactly the capabilities it was served with,
  and the crew named neither selects the crew a task executes under nor the
  model recorded for that execution. When the call is not brokered for a job
  and this is not a managed run with `ORBIT_AGENT_MODEL`, the named crew's
  configured model is the audit `session_model`.
  - The name is resolved against the crews
    of whichever workspace the call lands in, so an unconfigured crew fails that
    call rather than quietly selecting another one. It applies only when a task
    is created, and only to that task, unless the call names an orchestrator
    itself; an existing task's orchestrator is never rewritten from this
    default.
  - Treat it as configuration, not as evidence of who is calling. A
    long-lived MCP connection outlives a model switch in the client, so pass
    `orchestrator` on the individual call, or restart the connection with a new
    value, when the orchestrating crew actually changes.
- `--role <ROLE>` serves only one role's toolsets: `operator` (the operator profile),
  `manager`, `resident`, `agent` or `reader`. It replaces the
  `machine.mcp.role` host config. A role limits what is advertised and
  callable over MCP; it grants no authority. `orbit tool list --role <ROLE>`
  lists a role's tools and size.
- Only `core` is always advertised. Without a configured role, the default
  tier (tasks, artifacts, friction, search and attention) is advertised.
  `--operator` selects the operator profile and authority; `--role` can
  select a narrower profile. Managed workers use core plus their admitted
  activity allowlist, required and granted tools, independent of host
  configuration. `decision-channel` tools may be called only by the bound
  channel plugin or the operator.
- `--toolset` takes a comma-separated list (`automation`, `codebase`,
  `pipelines`, `task-triage`, `ui`, `experiments`, `analytics`,
  `decision-channel`, `notice`, `resident`, `tasks-write`, `dispatch`,
  `routines`; `default` names the default surface and `all` every toolset)
  advertised beside the role's, or beside the default surface without a role.
  - It replaces the `machine.mcp.toolsets` host config for this server.
    `--mode remote` asks the destination for the same toolsets, and
    `--mode federated` applies them to every destination. `orbit tool list`
    shows each tool's toolset; what a toolset changes is under MCP toolsets
    below.

## MCP toolsets

`orbit mcp serve` and `orbit mcp listen` advertise the selected role (or the
`default` tier without one) plus any opt-in toolsets the server or host enables.
Opt-in toolsets include:

| Toolset | Tools | Notes |
|---|---|---|
| `automation` | `orbit_auto_task`, `orbit_automation_explain` | Opt-in in the operator profile (`orbit_routine_control` stays in operator default via `routines`). CLI: `orbit auto-task`, `orbit automation explain`. |
| `codebase` | `orbit_codebase` | Opt-in in the operator profile; included in `agent` and `reader`. CLI: `orbit codebase analytics\|show\|related`. |
| `pipelines` | `orbit_pipeline_invoke` | Opt-in in the operator profile (`orbit_workflow` and `orbit_run` stay in operator default via `dispatch`/`runs`); included in `resident`. CLI: `orbit run job`. |
| `task-triage` | `orbit_task_block_triage` | Opt-in in the operator profile (`orbit_task_recover` stays in operator default via `task-repair`). CLI: `orbit tool run orbit.task.block_triage`. |
| `ui` | `orbit_ui` | Opt-in in the operator profile; included in `agent`. |
| `experiments` | `orbit_experiment_*` | Experiment definitions, lifecycle and readouts. |
| `analytics` | `orbit_agents_*` | Agent scorecards and CI failure breakdowns. |
| `decision-channel` | `orbit_decision_submit`, `orbit_decision_digest` | Decision channel callbacks (`orbit_decision` list/show/raise use `decisions`). |
| `notice` | `orbit_notice_*` | Fleet notices; included in `manager`, `resident` and `agent`. |
| `resident` | `orbit_resident_*`, `orbit_evolution_*` | Resident mandates and evolution ledger; included in `manager` and `resident`. Widening resident authority from MCP raises a decision request for the human. |

Enable one or more opt-in toolsets per server with `--toolset`, or for the host
with `machine.mcp.toolsets` (the flag replaces the config for that server):

```bash
# Enable one toolset (for example, pilot-warning triage only):
orbit mcp serve --mode federated --operator --toolset task-triage

# Enable all six operator opt-in tools:
orbit mcp serve --mode federated --operator \
  --toolset automation,codebase,pipelines,task-triage,ui

# Or persist the opt-in in host config:
orbit config set --global machine.mcp.toolsets \
  '["automation","codebase","pipelines","task-triage","ui"]'
```

Example `.mcp.json` entry opting an operator session into all six tools (or pass
only the toolsets you need, such as `"task-triage"`):

```json
{
  "mcpServers": {
    "orbit": {
      "command": "orbit",
      "args": [
        "mcp",
        "serve",
        "--mode",
        "federated",
        "--operator",
        "--toolset",
        "automation,codebase,pipelines,task-triage,ui"
      ]
    }
  }
}
```

A call to a tool outside the server's toolsets is refused
`toolset_not_enabled`, naming the tool's `toolset`, the `--toolset` flag and
the `machine.mcp.toolsets` config key. A federated mux filters every row the
same way. The toolset of each tool is in `orbit tool show <name> --json`
(`mcp_toolset`).

Toolsets change what an MCP server lists and dispatches. `orbit tool run`, CLI commands,
activity `required_tools` and task grants, and a decision-channel plugin's
callback session reach every tool regardless.
