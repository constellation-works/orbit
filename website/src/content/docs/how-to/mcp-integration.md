---
title: Connect your agent
description: "Connect Claude Code, Codex, Cursor, and other agent clients to Orbit over MCP, and choose which tools each session gets."
---

Connect your agent to Orbit over MCP so it can file tasks, ship them, and run
drains. The second half of this page is reference: tool families, toolsets,
roles, operator tools, and response shapes.

:::tip
Ask your agent to **set up Orbit for this repo**. The `orbit-setup` skill
registers Orbit with the clients you use. It also handles other machines,
remote dashboards, and a second repository.
:::

## Connect

```bash
orbit workspace init --mcp
```

This registers Orbit with every agent client it finds. Start a fresh agent
session afterwards so the tools load.

The registration carries **operator** authority. To limit an agent, or to
pick clients, use `orbit mcp init`:

| Command | Authority | The agent can |
|---|---|---|
| `orbit workspace init --mcp`, or `orbit mcp init --operator` | Operator | File and update tasks, ship them, run drains, resume runs, and run governed commands. |
| `orbit mcp init` | Agent-only | File and update tasks. It cannot dispatch runs. |

```bash
orbit mcp init --auto                   # every detected client
orbit mcp init --claude --codex         # specific clients
orbit mcp init --all --scope home       # user-level config, every client
```

Supported clients: `claude`, `codex`, `gemini`, `antigravity`, `grok`,
`cursor`, `vscode`, and `windsurf`. `--scope workspace`, the default, writes
config into the repository; `--scope home` writes it for your user.

An existing operator entry keeps its authority when you run `orbit mcp init`
again. To make it agent-only, [remove](#remove) it first. Agents that Orbit
launches for a run always get agent-only authority.

Grok Build reads the shared `.mcp.json` (`~/.claude.json` with
`--scope home`). When its shared reader is off, Orbit writes
`.grok/config.toml` instead.

If the workspace's Orbit data lives outside the repository, pass
`orbit mcp init` the same `--root <dir>` (or `ORBIT_ROOT`) you used for
`orbit workspace init`.

## Register the federated mux

A federated server puts this machine's workspaces and those on SSH remotes
under one MCP server. Register each remote host once, by its SSH alias or
`user@host`:

```bash
orbit host add orbit-owner
orbit host list
```

`orbit host add` records the host in `~/.orbit/hosts.toml`. `orbit host list`
shows each host's reachability, Orbit version, and workspaces, and flags a
version that differs from this machine's. `orbit host rename` and
`orbit host remove` manage the entries.

Then register the federated server with a client:

```bash
orbit mcp init --federated --client codex --scope home
```

This adds an `orbit-federated` entry beside any existing `orbit` one. In that
session:

- `orbit_workspace_list` returns host-qualified selectors such as
  `hm_alpha/ws_orbit`. Pass one unchanged as the `workspace` of a call.
- A call that addresses one task by ID, such as `orbit_task_show` or
  `orbit_task_update`, can omit `workspace`. It goes to the host that the ID's
  prefix names.
- There is no failover. A call reaches only the machine you select or the one
  its task ID names.

A plain `orbit mcp serve` session does not relay. Given another host's task ID
without `workspace`, it answers `task_prefix_remote` and names that host.

### Authority on remote hosts

A client started with `--operator` has operator authority on every host it
reaches, because an SSH login to a machine is ownership of it. Without
`--operator`, and always for a client inside a managed run, remote sessions
get agent-only authority. To shut a caller out, remove its key from that
machine's `~/.ssh/authorized_keys`.

### If a command reports `host_file_conflict`

An older `~/.orbit/mcp-destinations.toml` is still present. Move it aside, run
`orbit host add <ssh-target>` for each of its `ssh` rows, then delete it.

## Attribute tasks to an orchestrator

```bash
orbit mcp serve --workspace <selector> --orchestrator <crew>
```

Tasks the session creates are recorded as filed by that crew, unless a call
passes its own `orchestrator`. This is attribution only: it grants no
authority and does not choose the crew that executes a task. The flag also
works with `--mode remote <ssh-host>` and `--mode federated`.

## Serve on a socket

```bash
orbit mcp listen              # 127.0.0.1:7879
```

The listener serves the same tools over TCP, one session per connection, for
setups such as a server-side Orbit reached through an SSH tunnel.

It accepts a connection only from the user that runs the server, so a remote
TCP client is refused: reach the listener through an SSH tunnel as that user.
It binds loopback unless you pass `--allow-non-loopback`, and that flag does
not admit other users.

## Remove

```bash
orbit mcp remove --all
orbit mcp remove --federated --all   # only the federated entry
```

## Tool families

`orbit tool list` shows the available tools; `--all` adds inactive ones.
`orbit tool run` names a tool with dots (`orbit.command.exec`). MCP advertises
the same tool with underscores (`orbit_command_exec`).

Related operations share one tool, and its required `action` field picks the
operation:

| Family | Actions |
|---|---|
| `orbit.run` | `list`, `show`, `observe`, `resume` |
| `orbit.workflow` | `ship`, `land`, `auto` (its sub-command goes in `mode`) |
| `orbit.task.recover` | `requeue`, `review_reset`, `retry_handoff`, `revoke_handoff`, `recover_claim`, `repilot`, `reconcile_review` (its sub-command goes in `mode`) |
| `orbit.task.artifact` | `get`, `put` |
| `orbit.friction` | `add`, `update` |
| `orbit.decision` | `list`, `show`, `raise` |
| `orbit.auto_task` | `list`, `add`, `update`, `mint` |
| `orbit.codebase` | `show`, `related`, `analytics` |
| `orbit.ui` | `open`, `inspect` |

The `action` description in each schema lists the fields every action
requires. A call that omits one is refused with `invalid_input`. The CLI calls
a family the same way:

```bash
orbit tool run orbit.run --input '{"action":"show","id":"<run-id>"}'
```

Grants, `required_tools`, and activity allow and deny lists name a whole
family (`orbit.run`) or one action as `family:action` (`orbit.run:show`).

Every tool advertises MCP annotations (`readOnlyHint`, `destructiveHint`, and
so on) so a client can decide what to auto-approve. Orbit still checks
authority on every call, whatever the session lists.

## Toolsets

Every tool schema a server lists costs context in each agent session. Each
tool belongs to one toolset, and a server lists only the toolsets it enables.
`orbit tool show <tool>` names a tool's toolset, and `orbit tool list --json`
reports it as `mcp_toolset`.

| Toolset | Tools |
|---|---|
| `core` (always on) | `orbit.workspace.list` |
| `tasks-read` | `orbit.task.show`, `orbit.task.list`, `orbit.task.eligible`, `orbit.task.artifact` action `get`, `orbit.attention` |
| `tasks-write` | `orbit.task.add`, `orbit.task.update`, `orbit.task.artifact` action `put`, `orbit.friction` |
| `task-repair` | `orbit.task.block_triage`, `orbit.task.recover` |
| `decisions` | `orbit.decision` |
| `runs` | `orbit.run` actions `list`, `show`, `observe` |
| `dispatch` | `orbit.workflow`, `orbit.run` action `resume`, `orbit.pipeline.invoke` |
| `automation` | `orbit.auto_task`, `orbit.routine.control`, `orbit.automation.explain` |
| `codebase` | `orbit.codebase` |
| `search` | `orbit.search` |
| `exec` | `orbit.command.exec` |
| `plugins` | Plugin tools |
| `experiments` | `orbit.experiment.*` |
| `analytics` | `orbit.agents.*` |
| `decision-channel` | `orbit.decision.submit`, `orbit.decision.digest`. Only the bound channel plugin or the operator may call them. |
| `notice` | `orbit.notice.post`, `close`, `list`, `show`: fleet notices |
| `resident` | `orbit.resident.*`, and `orbit.evolution.add`, `list`, `show`: resident orchestrator mandates and the evolution ledger |
| `ui`, `leases` | None. Their tools were removed; the names still load in a selection. |
| `local` | Tools no MCP server lists, such as `proc.spawn`, `github.*`, and the drain tools. `orbit tool run` and activities reach them. |

A family whose actions span toolsets, such as `orbit.run`, is listed when any
of its actions is enabled.

### Choose toolsets

Without a role, a server lists `core`, `tasks-read`, `tasks-write`, and
`search`. The name `default` stands for that set, and `all` enables every
toolset. Add toolsets for one server with `--toolset`, or for every server on
the host with the global `machine.mcp.toolsets` key. The flag replaces the
key.

```bash
orbit mcp serve --toolset experiments,analytics
orbit config set --global machine.mcp.toolsets '["experiments"]'
```

`--mode remote` asks the destination for the same role and toolsets, and
`--mode federated` applies them to every destination.

### If a call returns `toolset_not_enabled`

The tool or action is outside the server's toolsets. The error names the
toolset, the roles that include it, and the flag that enables it.
`orbit tool run`, activities, and a decision channel plugin's callback session
reach every tool regardless.

## Roles

A role is a named set of toolsets for one kind of session:

| Role | Toolsets (beside `core`) |
|---|---|
| `operator` | `tasks-read`, `tasks-write`, `task-repair`, `decisions`, `runs`, `dispatch`, `automation`, `codebase`, `search`, `ui`, `exec`, `plugins` |
| `manager` | `tasks-read`, `runs`, `decisions`, `notice`, `resident`, `search` |
| `resident` | `tasks-read`, `tasks-write`, `runs`, `dispatch`, `decisions`, `notice`, `resident`, `search` |
| `agent` | `tasks-read`, `tasks-write`, `decisions`, `codebase`, `search`, `ui`, `leases`, `notice` |
| `reader` | `tasks-read`, `runs`, `codebase`, `search` |

```bash
orbit mcp serve --role reader
orbit mcp serve --role manager --toolset analytics   # the role plus analytics
orbit config set --global machine.mcp.role resident
orbit tool list --role agent                         # the role's tools and size
```

`--role` replaces the global `machine.mcp.role` for one server, and
`--toolset` adds to the role. `orbit tool list --role <role>` prints the
role's tool count and the approximate token size of its schemas.

A role grants no authority. It limits only what MCP lists and dispatches.
Operator-only tools still need `--operator` under every role, including
`operator`.

## Operator tools

Command execution and job submission need an operator session, such as
`orbit mcp serve --operator`. For the other tools, use these guides:

| Tools | Guide |
|---|---|
| `orbit.task.*`, `orbit.search`, `orbit.friction` | [Tasks and lifecycle](/concepts/tasks/) and [First task](/getting-started/first-task/) |
| `orbit.agents.*` | [Agents and crews](/concepts/agents/) |
| `orbit.workflow`, `orbit.run` | [Delivery workflows](/getting-started/workflows/) and [Run a delivery window](/how-to/continuous-delivery/) |
| `orbit.pipeline.invoke` | [Activities and jobs](/concepts/activities-jobs/), [Write an activity](/how-to/write-activity/), and [Activity and Job YAML](/reference/activity-job-yaml/) |
| `orbit.routine.control`, `orbit.auto_task` | [Routines and auto-tasks](/concepts/scheduling/) and [Schedule recurring work](/how-to/recurring-work/) |
| `orbit.decision`, `orbit.decision.digest`, `orbit.decision.submit` | [Decision channels](/how-to/decision-channels/). Agents cannot answer requests. |
| `orbit.codebase` | [Use the dashboard](/how-to/dashboard/) |
| `orbit.ui` | [Open the MCP Apps control center](/how-to/mcp-apps/) |
| Plugin tools marked `agent_callable`, once the plugin is granted `agent_tools` | [Install and grant plugins](/how-to/plugins/) |

The requests below are MCP `tools/call` bodies. Replace `ws_example` with a
selector from `orbit_workspace_list`, and the paths, job names, and run IDs
with your own. Requests take no `model` or `agent` argument: attribution comes
from the session.

### Execute a bounded command

`orbit.command.exec` runs an `argv` array, program first, in an explicit
`working_directory`. No shell interprets it: pipes, redirection, variable
expansion, and `&&` are not available. Pass each argument as its own string,
without shell quoting.

```json
{
  "name": "orbit_command_exec",
  "arguments": {
    "workspace": "ws_example",
    "argv": ["git", "status", "--short"],
    "working_directory": "/home/operator/example",
    "timeout_ms": 30000
  }
}
```

| Aspect | Rule |
|---|---|
| Working directory | Absolute and existing, inside the workspace's checkout or a linked worktree under `.orbit/state/worktrees/`. Orbit never infers it. |
| Environment | An allowlist, not the server's environment: `HOME`, `PATH`, `USER` and similar, git author and committer identity, and Orbit's execution variables. `SSH_AUTH_SOCK` and other names are absent. |
| Deadline | `timeout_ms` defaults to 120000 and is capped at 600000. On timeout, Orbit kills the command and its process group and returns `timed_out: true` with no exit code and no captured output. |
| Output | `success`, `stdout`, `stderr`, `exit_code`, `duration_ms`, and `timed_out`. Each stream is capped at 4 MiB. |
| Workspace claim | If another operator holds the workspace claim, pass its `claim_token`, or set `ORBIT_WORKSPACE_CLAIM_TOKEN` in the server's environment. Otherwise the call is refused and names the holder and expiry. |
| Managed runs | Cannot call this tool, even when it appears in their tool list. |

A completed tool call is not a successful command: read `success` and
`exit_code`.

### Submit a durable job

`orbit.pipeline.invoke` submits a registered job and returns at once with a
durable `run_id`. The job has been accepted, not finished. Pass `input` as a
JSON object that matches the job's input contract, even when it is `{}`. This
example assumes a job named `example_check` that accepts `directory`:

```json
{
  "name": "orbit_pipeline_invoke",
  "arguments": {
    "workspace": "ws_example",
    "job_name": "example_check",
    "input": { "directory": "src" },
    "priority": "low"
  }
}
```

`priority` is optional: `low`, `medium`, `high`, or `critical`. Managed runs
may submit only a child pipeline that the host has authorized; other
submissions from them are refused.

### Submit with catalog defaults

Some enabled jobs can run with no input. To list them, call `orbit.run`
action `list` with `view: "bounded"` and `include_catalog: true`. Then submit
one with its defaults:

```json
{
  "name": "orbit_pipeline_invoke",
  "arguments": {
    "workspace": "ws_example",
    "job_name": "example_check",
    "default_input": true
  }
}
```

This form takes exactly `workspace`, `job_name`, and `default_input`; omit
`input` and `priority`. It always refuses managed runs. Subroutines, disabled
jobs, and task-delivery jobs are not eligible: ship tasks with the delivery
tools.

### Observe the run

Keep the returned `run_id` and fetch it with `orbit.run` action `show`:

```json
{
  "name": "orbit_run",
  "arguments": {
    "action": "show",
    "workspace": "ws_example",
    "id": "<returned-run-id>",
    "view": "bounded",
    "limit": 20
  }
}
```

The bounded detail holds the run and one page of its log; page through the
log with `log_offset` and `limit`. Keep observing until the run is terminal,
then read its outcome. Listing and showing runs need operator capability.

:::caution
If a submission reply is lost, inspect recent runs before you submit again.
The first call may already have created a run.
:::

## Response shapes

| Tool | Returns |
|---|---|
| `orbit_task_list` | `{ tasks, total, truncated }` with compact rows: `id`, `title`, `status`, `priority`, `type`, `dependencies`, `resolved_dependencies`, `implemented_by`, `not_before`, `created_at`, `updated_at`, `workspace`. Pass `fields` to choose fields, including long text such as `description` or `plan`. |
| `orbit_task_eligible` | `{ tasks, total, truncated }`: the `backlog` and `proposed` tasks whose files overlap no running or in-review task. Rows hold `id`, `title`, `type`, `status`, `priority`, and `complexity`; there is no `fields` option. With `explain: true`, `conflicting` lists each held-back task, the overlapping file, and the task holding it. |
| `orbit_task_show` | The full record, or the fields named in `fields`. It includes `not_before`, `started_at` (the first entry into `in-progress`), and `scheduled_hold` (whether a backlog task is still held by a future schedule). |
| `orbit_task_add`, `orbit_task_update` | The task without `comments` and `history`, unless you name them in `fields`. |

Schedule filters on `orbit_task_list`:

| Field | Effect |
|---|---|
| `not_before_from`, `not_before_until` | Inclusive bounds, as RFC 3339 instants with offsets. Use one or both. Tasks without a schedule do not match. |
| `scheduled: true` | Lists future schedules, soonest first, including `proposed` tasks. Other filters and `limit` still apply. |

Set a schedule with `not_before` on `orbit_task_add` or `orbit_task_update`.
On update, an empty string clears it and null leaves it unchanged. A schedule
is the earliest start, not an exact time.
