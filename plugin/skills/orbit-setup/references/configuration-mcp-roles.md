# Configuration: MCP roles and toolsets

## MCP roles and toolsets

`machine.mcp.role` and `machine.mcp.toolsets` are global-only host settings
used by `orbit mcp serve` and `orbit mcp listen`. A role selects the MCP
toolsets the server advertises and accepts calls for. The `core` toolset,
including workspace discovery, is always present. Roles are presets:

| Role | Included toolsets |
|---|---|
| `operator` | `tasks-read`, `tasks-write`, `task-repair`, `decisions`, `runs`, `dispatch`, `routines`, `search`, `exec`, and `plugins` |
| `manager` | Task and run reads, decisions, notices, resident mandates and search |
| `resident` | The manager surface plus task writes, dispatch and `pipelines` |
| `agent` | Task reads and writes, decisions, codebase, search, UI and notices |
| `reader` | Task and run reads, codebase and search |

The `operator` role selects `tasks-read`, `tasks-write`, `task-repair`,
`decisions`, `runs`, `dispatch`, `routines`, `search`, `exec`, and `plugins`
but grants no operator authority.

Six rarely used tools are opt-in in the
operator profile (`automation` for `orbit_auto_task` and
`orbit_automation_explain`, `codebase` for `orbit_codebase`, `pipelines` for
`orbit_pipeline_invoke`, `task-triage` for `orbit_task_block_triage`, and `ui`
for `orbit_ui`) via `--toolset` or `machine.mcp.toolsets`; `orbit_workflow`
(`dispatch`), `orbit_run` (`runs`/`dispatch`), `orbit_routine_control`
(`routines`) and `orbit_task_recover` (`task-repair`) stay in the default.

`--operator` is a separate server choice, and selects the operator profile when
no `--role` is given; tools that require operator capability still require that
capability under every role.

Set and read back the global defaults with:

```bash
orbit config set --global machine.mcp.role reader
orbit config set --global machine.mcp.toolsets '["experiments"]'
orbit config get machine.mcp.role
orbit config get machine.mcp.toolsets
orbit config show --scope global
```

The configured toolset names are additions to the chosen role. At server
startup, `--role <role>` replaces `machine.mcp.role`, and `--toolset <list>`
replaces `machine.mcp.toolsets`; each flag overrides only its corresponding
setting. Passing both flags replaces both configured values. The requested
toolsets are then added to the selected role, or to the core profile
when no role is selected.

With no selected role, that core profile (`core`
plus `tasks-read`, `tasks-write`, and `search`) remains the base;
configured or command-line toolsets can extend it. `default` names
the core profile and `all` names every toolset. Restart a long-lived MCP
server or listener after changing the host defaults; an open session keeps the
selection it started with.

Check `orbit --version` before relying on role support. Inspect
`orbit mcp serve --help`, `orbit mcp listen --help`, `orbit tool list --help`
and `orbit config keys` from that same installed version. A source checkout or
newer documentation does not add settings and flags to an older installed
binary; upgrade to a version that includes role support before using this
guidance.

For the built-in role baseline and its size, run
`orbit tool list --role <role>` with that matching-version binary. This shows
the role's tools, count and approximate token size; it does not include
configured or command-line additions. To inspect the exact surface of a
server, use an MCP client connected to that server, initialize the session,
send `notifications/initialized`, then call `tools/list`. The response is the
effective advertised surface, including any additional toolsets.

For a local
check, have a disposable foreground stdio client start `orbit mcp serve` with
the intended selection, use a short timeout, request `tools/list`, and exit
after the response. Do not add `--operator` just to inspect the tool list.
