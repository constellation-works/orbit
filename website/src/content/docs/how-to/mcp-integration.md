---
title: Connect your agent
description: "Connect Claude Code, Codex, Cursor, and other agent clients to Orbit over MCP: let the orbit-setup skill do it, or run one command."
sidebar:
  order: 5
---

Your agent reaches Orbit through an MCP server. Once connected, it can file
tasks, ship them, and run drains.

:::tip[Recommended]
Ask your agent to **set up Orbit for this repo**. The `orbit-setup` skill
registers Orbit with the clients you use, and it also handles the setups
further down this page: other machines, remote dashboards, and a second
repository.
:::

## Connect

```bash
orbit workspace init --mcp
```

This registers Orbit with every agent client it finds, with **operator**
authority, so your agent can ship tasks and run drains as well as file them.
Start a fresh agent session afterwards so the tools load.

The registration launches `orbit mcp serve --operator`. Operator sessions can
dispatch workflows, resume runs, and run governed commands; agents launched by
Orbit get agent-only authority. Each session's capabilities filter the tools
advertised by `tools/list`; authorization is also checked when a tool is called.

`orbit mcp init` registers Orbit with the narrower **agent-only** authority:
the agent can file and update tasks but cannot dispatch runs. Use it to pick
clients or to limit an agent:

```bash
orbit mcp init --auto                   # every detected client
orbit mcp init --claude --codex         # specific clients
orbit mcp init --all --scope home       # user-level config, every client
```

Supported clients: `claude`, `codex`, `gemini`, `antigravity`, `grok`,
`cursor`, `vscode`, and `windsurf`. `--scope workspace`, the default, writes
config into the repository; `--scope home` writes it for your user.

Grok Build reads the shared `.mcp.json` (or `~/.claude.json` with
`--scope home`); when its shared reader is off, Orbit writes
`.grok/config.toml` instead.

If the workspace's Orbit data lives outside the repository, pass
`orbit mcp init` the same `--root <dir>` (or `ORBIT_ROOT`) you used for
`orbit workspace init`.

## Register the federated mux

A federated server puts this machine's workspaces and those on SSH remotes
under one namespace. Register each remote host once, by its SSH alias or
`user@host`:

```bash
orbit host add orbit-owner
orbit host list
```

`orbit host add` reads the host's machine ID, name and task prefix from the
host itself and records them in `~/.orbit/hosts.toml`. `orbit host list` shows
every host with its live reachability, Orbit version, pull protocol and
workspaces, and flags a version or protocol that differs from this machine's.
`orbit host rename` and `orbit host remove` manage the entries. An older
`~/.orbit/mcp-destinations.toml` is still read until the first
`orbit host add`, `orbit host rename` or `orbit host remove` migrates it.

Then register the federated server with a client:

```bash
orbit mcp init --federated --client codex --scope home
```

This adds a separate `orbit-federated` entry beside any existing `orbit` one.
In that session, `orbit_workspace_list` returns host-qualified selectors such
as `hm_alpha/ws_orbit`; pass one unchanged as the `workspace` of a call. Task
reads go to the owner's selector. A call that addresses one task by ID, such
as `orbit_task_show` or `orbit_task_update`, can omit `workspace`: it goes to
the host the ID's prefix names. There is no automatic failover: a call reaches
only the machine you select or the one its task ID names. A plain
`orbit mcp serve` session does not relay. Given another host's task ID without
`workspace`, it answers `task_prefix_remote` and names that host.

Operator authority travels over SSH. A client started with `--operator` serves
operator on every destination it opens, because an SSH login to a machine is
ownership of it. Without `--operator`, and always for a client running inside
a managed run, remote sessions get agent-only authority. To shut a caller out,
remove its key from that machine's `~/.ssh/authorized_keys`.

## Attribute tasks to an orchestrator

```bash
orbit mcp serve --workspace <selector> --orchestrator <crew>
```

Tasks the session creates are recorded as filed by that crew, unless a call
passes its own `orchestrator`. This is attribution only: it grants no
authority and does not choose the crew that executes a task. The same flag
works with `--mode remote <ssh-host>` and `--mode federated`.

## Toolsets

Every tool schema a server lists costs context in each agent session, so
`orbit mcp serve` lists a default toolset and keeps occasional groups opt-in:

| Toolset | Tools |
|---|---|
| `default` (always on) | Tasks, workflows and runs, `orbit.pipeline.invoke`, `orbit.agent.invoke` (listed only to a server a managed run launched), routines and auto-tasks, `orbit.automation.explain`, `orbit.decision.list` and `orbit.decision.show`, codebase, friction, search, the UI and visual tools, `orbit.command.exec` and workspace discovery. |
| `leases` | `orbit.lease.*` |
| `experiments` | `orbit.experiment.*` |
| `analytics` | `orbit.agents.*`, `orbit.orchestrator.show`, `orbit.orchestrator.scorecard` and `orbit.crew.ladder` |
| `decision-channel` | `orbit.decision.submit` and `orbit.decision.digest`, which only the bound channel plugin or the operator may call |

Enable groups per server with `--toolset`, or for every server on the host
with the global `machine.mcp.toolsets` config key; the flag replaces the
config. `all` enables every toolset.

```bash
orbit mcp serve --toolset leases,analytics
orbit config set --global machine.mcp.toolsets '["experiments"]'
```

A toolset changes only what that server lists and dispatches. Calling a tool
outside it returns a `toolset_not_enabled` error naming the toolset and the
flag to enable it, so a long-lived client learns how to reach it. The tools
stay registered: `orbit tool run`, activities that require them, and a
decision channel plugin's callback session reach every tool. `--mode remote`
asks the destination for the same toolsets, and `--mode federated` applies
them to every destination.

## Serve on a socket

```bash
orbit mcp listen              # 127.0.0.1:7879
```

The listener serves the same tools over TCP, one session per connection, for
setups such as a server-side Orbit reached through an SSH tunnel. It does not
authenticate clients, so it binds loopback unless you pass
`--allow-non-loopback`.

The listener allows 64 concurrent sessions. Each accepted connection has five
seconds to send a complete initialization request and receive its response;
silent peers and incomplete messages are closed and release their slots.
Established sessions remain connected while idle, until the client disconnects.

## Operator tools

Tool names such as `orbit.command.exec` use dots in `orbit tool run`; MCP
advertises them with underscores, such as `orbit_command_exec`. Read the
session's `tools/list` for the registered input schemas. A listed tool still
requires the caller's authority and workspace policy to admit the call.

Use the existing guides for the other public families:

| Tools | Guide |
|---|---|
| `orbit.task.*`, `orbit.search`, `orbit.friction.*` | [Tasks and lifecycle](../../concepts/tasks/) and [your first task](../../getting-started/first-task/). |
| `orbit.agent.invoke`, `orbit.agents.*` | [Agents and crews](../../concepts/agents/). |
| `orbit.workflow.ship`, `orbit.workflow.auto`, `orbit.workflow.run.*` | [Delivery workflows](../../getting-started/workflows/) and [delivery windows](../continuous-delivery/). |
| `orbit.pipeline.invoke` | [Activities and jobs](../../concepts/activities-jobs/), [write an activity](../write-activity/), and [Activity and Job YAML](../../reference/activity-job-yaml/). |
| `orbit.routine.control`, `orbit.auto_task.*` | [Routines and Auto-Tasks](../../concepts/scheduling/) and [schedule recurring work](../recurring-work/). |
| `orbit.decision.list`, `orbit.decision.show`, `orbit.decision.digest`, `orbit.decision.submit` | [Decision channels](../decision-channels/) covers read-only inspection, operator registration and authenticated plugin callbacks. Agents cannot answer requests. |
| `orbit.codebase.*` | [Dashboard views](../dashboard/). |
| `orbit.ui.open`, `orbit.ui.inspect` | [Open the MCP Apps control center](../mcp-apps/). |
| Plugin tools marked `agent_callable`, once the plugin is granted `agent_tools` | [Install and grant plugins](../plugins/). |

The requests below are illustrative MCP `tools/call` bodies, not setup steps.
Replace `ws_example` with an unchanged selector from `orbit_workspace_list`
(host-qualified in a federated session), and replace paths, job names and run
IDs with values from that workspace. Public command execution and job
submission require an operator session, such as `orbit mcp serve --operator`.
The `model` field records provenance; it grants no authority.

### Execute a bounded command

`orbit.command.exec` runs a non-empty `argv` array, program first, in the
explicit `working_directory`. Each argument is a separate string. No shell
interprets it: pipes, redirection, variable expansion and `&&` are not shell
syntax here. Do not put a whole command line in one string or add shell
quoting around an argument containing spaces.

```json
{
  "name": "orbit_command_exec",
  "arguments": {
    "workspace": "ws_example",
    "argv": ["git", "status", "--short"],
    "working_directory": "/home/operator/example",
    "timeout_ms": 30000,
    "model": "codex"
  }
}
```

The directory must exist and be absolute, inside the selected workspace's
checkout or a linked worktree under `.orbit/state/worktrees/`. Orbit never
infers it from the caller's current directory.

The workspace claim is an optional exclusive coordination hold, separate
from operator authority. An unclaimed workspace admits the command. If a
claim is held, supply its matching `claim_token` argument or set
`ORBIT_WORKSPACE_CLAIM_TOKEN` in the executing Orbit server's environment.
A mismatched or missing token refuses the command and identifies the holder
and expiry. Claim management is not exposed over MCP; coordinate with the
operator holding the claim. A token does not grant operator authority.

The default deadline is 120000 ms; requested values are clamped to
1–600000 ms. Orbit caps each of stdout and stderr at 4 MiB. The result
includes `success`, `stdout`, `stderr`, `exit_code`, `duration_ms` and
`timed_out`. On timeout, Orbit kills the command and its process group,
returns `timed_out: true` with no exit code, and discards captured command
output. Inspect the result rather than treating a completed tool call as a
successful command.

Managed runs cannot call this tool, even if it appears in their tool list:
a leaf agent cannot use general command execution to bypass its tool policy.

### Submit a durable job

`orbit.pipeline.invoke` submits a registered v2 job and returns immediately
with a durable `run_id`; acceptance is not completion. With ordinary input,
pass a JSON object matching the job's input contract. For example, after
registering a job named `example_check` that accepts `directory`:

```json
{
  "name": "orbit_pipeline_invoke",
  "arguments": {
    "workspace": "ws_example",
    "job_name": "example_check",
    "input": { "directory": "src" },
    "priority": "low",
    "model": "codex"
  }
}
```

`priority` is optional: `low`, `medium`, `high` or `critical`. Ordinary
submission requires `input`, even when it is `{}`. This schema does not
advertise `claim_token`; do not copy command-execution arguments into it.

Managed runs may invoke only a host-authorized child pipeline admission.
That admission comes from trusted host context, not caller-supplied JSON;
ordinary leaf submissions are refused.

### Submit with catalog defaults

To run an enabled job that the catalog permits for no-input submission,
use `default_input: true` and an explicit `workspace`. First inspect the
no-input job catalog alongside recent runs:

```json
{
  "name": "orbit_workflow_run_list",
  "arguments": {
    "workspace": "ws_example",
    "view": "bounded",
    "include_catalog": true,
    "limit": 10
  }
}
```

After selecting an eligible job, submit it with its defaults. Here
`example_check` stands for that catalog job:

```json
{
  "name": "orbit_pipeline_invoke",
  "arguments": {
    "workspace": "ws_example",
    "job_name": "example_check",
    "default_input": true,
    "model": "codex"
  }
}
```

This variant accepts only `workspace`, `job_name`, `default_input` and the
optional `model`. Omit both `input` and `priority`. It requires trusted
operator authority and always refuses managed runs, including child
admissions. Not every registered job is eligible: subroutines, disabled jobs
and task-delivery jobs do not belong on this route. Use the delivery tools
for task shipping.

### Observe the run

Keep the returned `run_id` and fetch it with `orbit.workflow.run.show`:

```json
{
  "name": "orbit_workflow_run_show",
  "arguments": {
    "workspace": "ws_example",
    "id": "<returned-run-id>",
    "view": "bounded",
    "limit": 20
  }
}
```

Run listing and detail require operator capability; agent-only sessions are
refused. The bounded detail includes the run and a bounded log page; use
`log_offset` and `limit` to page through logs. Continue observing until the
run is terminal, then inspect its outcome and failure details. An unfinished
run is still durable after the submission call returns.

If a submission reply is lost, inspect recent runs and their details before
submitting again. Never automatically replay a default-input submission:
the first call may already have created a run.

## Response shapes

`orbit tool list` shows available tools; `--all` also includes inactive tools.
Each one advertises MCP annotations
(`readOnlyHint`, `destructiveHint`, and so on) so a client can decide what to
auto-approve; Orbit still enforces authority on every call.

- `orbit_task_list` and `orbit_task_eligible` return
  `{ tasks, total, truncated }`. `orbit_task_list` returns compact task rows by
  default (id, title, status, priority, type, dependencies,
  resolved_dependencies, implemented_by, not_before, created_at, updated_at, and workspace).
  `not_before` is null when unset. Pass `fields` to select only named fields,
  including long-text fields such as `description` or `plan`.
- `orbit_task_list` accepts `not_before_from` and `not_before_until` as RFC 3339
  instants with offsets, individually or together. Both endpoints are inclusive;
  tasks without a schedule do not match. Malformed bounds name the input in the
  error. `scheduled: true` lists future schedules soonest first, including
  proposed tasks; other filters and the limit still apply.
- `orbit_task_show` exposes `not_before` and derived `started_at` (the first
  recorded entry into in-progress), also available through `fields`.
  `scheduled_hold` reports whether a backlog task is still held by its future
  schedule, accounting for operator overrides. A task's
  schedule does not guarantee an exact execution time. Set `not_before` through
  `orbit_task_add` or `orbit_task_update`; an empty string clears it on update,
  while null leaves it unchanged.
- `orbit_task_eligible` rows are minimal: id, title, type, status, priority and
  complexity. The tool takes no `fields` projection. To read a full record, use
  `orbit_task_show`; to add long-text fields to a listing, use `orbit_task_list`
  with `fields`.
- `orbit_task_eligible` lists, in dispatch order, the `backlog` and `proposed`
  tasks whose files overlap no running or in-review task. With
  `explain: true`, `conflicting` also lists the held-back tasks, each with the
  overlapping file and the task holding it.
- The task-write tools leave out `comments` and `history` unless you name them
  in `fields`.

## Remove

```bash
orbit mcp remove --all
orbit mcp remove --federated --all   # only the federated entry
```
