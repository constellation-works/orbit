---
title: Open the MCP Apps control center
description: "Open Orbit's embedded control center from an MCP client, inspect a task or run, and know what its actions require."
---

Orbit's MCP server serves a small control center as an MCP Apps resource. A
client that renders MCP Apps shows it in or beside the conversation: the Tasks,
Runs, Auto-drain and Automation views of a workspace, without leaving the agent
session. The `orbit_ui` tool opens it.

It is separate from the [desktop app](/how-to/desktop/), which is a native
window around the web [dashboard](/how-to/dashboard/), and from the
[Claude Code mod](/how-to/claude-code/), which draws in the terminal.

:::caution[Rendering depends on the client]
Orbit serves the UI resource and marks the tool so a host can attach it.
Whether a client renders it is the client's choice, and this page names no
client that does. If yours does not, the tool still returns the same data as
JSON. See [When it does not render](#when-it-does-not-render).
:::

## Prerequisites

[Connect your agent](/how-to/mcp-integration/) first. The `ui` toolset must be
enabled for `orbit_ui` to appear in `tools/list` with the actions `open` and
`inspect`, and the session must also include the read-only task read. The
operator profile already includes task reads and writes, runs, dispatch (ship
and auto-drain) and routines. `ui` is opt-in. Two Automation actions need
further toolsets the operator profile does not include:

| Panel action | Tool | Toolset |
|---|---|---|
| Auto-tasks: list, enable or disable, mint one task | `orbit_auto_task` | `automation` |
| Run job | `orbit_pipeline_invoke` | `pipelines` |

This launch keeps those operator panel functions available:

```bash
orbit mcp serve --operator \
  --toolset ui,automation,pipelines
```

`orbit mcp serve --operator --toolset ui` still opens the panel. Tasks, Runs,
Auto-drain and Routines work. Auto-tasks and Run job stay unavailable: the
panel names the toolset to add and does not send the write. For an operator
launch that currently uses only `--toolset ui`, use the list shown above and
reopen the panel.

`machine.mcp.toolsets` is the host-wide default for `orbit mcp serve` launches
that do not pass `--toolset`. An explicit flag supplies that launch's toolsets
instead of the host list; the operator role still applies. Changing the host
setting will not add `automation` or `pipelines` to `--toolset ui`. A managed
run uses the toolsets Orbit stamped for that run. See the [toolset precedence
rules in MCP Integration](/how-to/mcp-integration/#choose-toolsets). Restart
the MCP server or client session to load the updated tool list.

A client that renders the panel reads the resource
`ui://orbit/control-center/v1/index.html` (MIME type
`text/html;profile=mcp-app`). The panel is static HTML. It loads no external
assets, opens no network connections, and reaches Orbit only through the
client's MCP bridge.

## Discover a workspace selector

The panel never infers a workspace from the session or the current directory.
List the registered workspaces and pass one:

```json
{
  "name": "orbit_workspace_list",
  "arguments": {}
}
```

| Session | What `workspace` accepts |
|---|---|
| Plain `orbit mcp serve` | A registered workspace name, a workspace ID (`ws_…`) or a registered absolute checkout path. |
| Federated (`orbit mcp init --federated`, see [Register the federated mux](/how-to/mcp-integration/#register-the-federated-mux)) | Only the opaque, host-qualified `selector` that `orbit_workspace_list` returns. Copy it unchanged. A bare ID, name or path is refused with `unknown_selector`. |

The examples use `ws_example`. Replace it with a value from your own list.

## Open the control center

With no arguments, `open` shows a workspace picker (host and workspace together
when federated):

```json
{
  "name": "orbit_ui",
  "arguments": { "action": "open" }
}
```

To open on one workspace, pass `workspace`:

```json
{
  "name": "orbit_ui",
  "arguments": { "action": "open", "workspace": "ws_example" }
}
```

To open on one task, add `id`. `kind` defaults to `task`:

```json
{
  "name": "orbit_ui",
  "arguments": { "action": "open", "workspace": "ws_example", "id": "EX-42" }
}
```

## Inspect one task or run

`inspect` requires `workspace` and `id`. `kind` is `task` (the default) or
`run`:

```json
{
  "name": "orbit_ui",
  "arguments": { "action": "inspect", "workspace": "ws_example", "id": "EX-42", "kind": "task" }
}
```

For a run, pass a run ID Orbit returned, for example from `orbit_run` action
`list`:

```json
{
  "name": "orbit_ui",
  "arguments": { "action": "inspect", "workspace": "ws_example", "id": "<run-id>", "kind": "run" }
}
```

Inspecting a run needs a session that may read runs, which means operator
authority (see [Connect your agent](/how-to/mcp-integration/#observe-the-run)).

The result is the data the panel starts from: a `task` object with `id`,
`title`, `status`, `updated_at`, `description` and `acceptance_criteria`, or a
bounded run read with `kind: "run"`. An empty `open` returns `workspace` and
`task` both `null`.

### Argument rules

- `id` requires `workspace`. `kind` requires both.
- Each of `workspace`, `id` and `kind` is a non-blank string of at most 2048
  UTF-8 bytes.
- Any other argument, such as `status` or `actor`, is refused.

A broken rule returns `invalid_input`. A task or run that does not exist in the
selected workspace is an error. Nothing falls back to another workspace.

## Navigate the views

The panel has a rail with a workspace selector and four views.

| View | What it shows |
|---|---|
| **Tasks** | Tasks grouped by status, with search and a priority filter. It opens on In progress, Review, Blocked and Backlog. **Active work** restores that set and **All** includes every status. A row opens its detail, with paged comments, history and artifact metadata. |
| **Runs** | Workflow runs with a state filter. A run's detail has its state and paged log excerpts. In-progress task rows link to their run. |
| **Auto-drain** | Backlog readiness, the current delivery window and its workers. |
| **Automation** | Routines and auto-tasks, with the next evaluation time when one is scheduled and their state otherwise. Job rows show their step count and parallel-run limit. Auto-tasks need the `automation` toolset. Run job needs the `pipelines` toolset. |

**Send context to chat** puts a short reference into the conversation: the
destination, the task or run ID, the observed revision and the title. It
carries no logs or history, so reread before you act on it.

## What opening does not grant

`orbit_ui` only reads. Opening the panel grants no authority: a session that
could not ship a task before opening it cannot ship it after. Every change the
panel makes goes through the ordinary Orbit tools, under the session's own
authority, workspace policy and audit.

| Area | What the panel can do | Needs |
|---|---|---|
| Tasks | **New task** files a proposed task with acceptance criteria. Nothing runs until it is dispatched. One edit can change title, description, acceptance criteria, priority and crew together. Status, a comment, and a review verdict are each a separate write. | An edit is refused if the task changed since it was read. |
| Review | Records criterion outcomes and evidence against the current revision and the run or pull-request head. Requesting changes and recording acceptance keep the task in review. **Accept and mark done** never merges, publishes or dispatches. | **Accept and mark done** needs fresh eligible state and operator authority. |
| Ship and auto-drain | Ship a backlog row, or start or stop an auto-drain window. A drain's **Complete automatically** option is off by default. Turning it on authorizes completion of every task admitted during that window. | Operator authority and an explicit workspace. Dispatch is in the operator profile. |
| Routines | Enable or disable a routine definition. | Operator authority and an explicit workspace. The routines toolset is in the operator profile. |
| Auto-tasks | List definitions, enable or disable one, or mint one task. Mint ignores the schedule and does not dispatch. | The `automation` toolset, operator authority and an explicit workspace. |
| Run job | Submit a job that accepts its default input. | The `pipelines` toolset, operator authority and an explicit workspace. |

An agent-only session (`orbit mcp init`) can read through the panel and make
the task edits an agent may make. It is refused the operator actions. A session
from `orbit workspace init --mcp` is operator. See
[Connect your agent](/how-to/mcp-integration/#connect).

A toolset that is not enabled is not a lost reply. The panel does not call
that tool, and it names the toolset to add. If the reply to a drain or
automation action is lost, the panel does not repeat the action. It disables
further writes of that kind for the workspace until you reopen it. Check Tasks
and Runs for what happened.

## When it does not render

If the client does not support MCP Apps, the call still succeeds and returns
the JSON described above, but nothing is drawn. Use the normal tools or another
surface:

| You want | Use |
|---|---|
| A task | `orbit_task_show`, `orbit_task_list` |
| Runs | `orbit_run` action `list`, `orbit_run` action `show` (operator) |
| Workspaces | `orbit_workspace_list` |
| A browser view | The [dashboard](/how-to/dashboard/) |
| A native window and tray | The [desktop app](/how-to/desktop/) |
| A terminal band and board | The [Claude Code mod](/how-to/claude-code/) |
