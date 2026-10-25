---
title: Open the MCP Apps control center
description: "Open Orbit's embedded control center from an MCP client: global open, inspect a task or run, local and federated selectors, views, guarded actions, and fallback."
sidebar:
  order: 4
---

Orbit's MCP server can serve a small control center as an
MCP Apps resource.
A client that renders MCP Apps shows it inside the conversation or beside it:
the Tasks, Runs, Auto-drain and Automation views of a workspace, without
leaving the agent session. Two read-only tools open it.

This is a separate surface from the other two:

- The [desktop app](../desktop/) is a native window and tray around the web
  [dashboard](../dashboard/). It supervises its own `orbit web serve` process
  and does not use MCP.
- The [Claude Code mod](../claude-code/) draws a band, status line and panes in
  the terminal. It does not render this control center.

:::caution[Rendering depends on the client]
Orbit serves the UI resource and marks the tools with the metadata a host needs
to attach it. Whether a client renders it is that client's choice. Orbit's own
protocol and DOM tests cover the resource, but no check in an installed desktop
client has been recorded, so this page does not name a client that renders it. If yours
does not, the tools still return the same data as JSON and every other Orbit
tool works unchanged. See [When it does not render](#when-it-does-not-render).
:::

## Prerequisites

Connect your agent first with [Connect your agent](../mcp-integration/). The
two tools appear in `tools/list` as `orbit_ui_open` and `orbit_ui_inspect`.
They are listed only when the server can serve a read-only task read, so a
session that hides that tool does not list them either.

A client that wants to render the panel reads the resource
`ui://orbit/control-center/v1/index.html` (MIME type
`text/html;profile=mcp-app`). It is static HTML with a restrictive content
security policy: it loads no external assets and opens no network connections.
The panel reaches Orbit only through the client's MCP bridge.

## Discover a workspace selector

The panel never infers a workspace from the session or the current directory.
You pass one explicitly. Ask for the registered workspaces:

```json
{
  "name": "orbit_workspace_list",
  "arguments": {}
}
```

In a plain `orbit mcp serve` session, `workspace` accepts a registered
workspace name, a logical workspace ID (`ws_…`) or a registered absolute
checkout path. In a federated session
(`orbit mcp init --federated`, see
[Register the federated mux](../mcp-integration/#register-the-federated-mux)),
`orbit_workspace_list` returns an opaque, host-qualified `selector` for each
workspace on this machine and on every SSH remote. Copy it unchanged. A bare
workspace ID, name or path is refused with `unknown_selector`; do not build a
selector by hand. The examples below use `ws_example`; replace it with a value
from your own list.

## Open the control center

`orbit_ui_open` with no arguments opens the control center globally. The panel
then discovers the available workspaces itself and shows a workspace picker
(host and workspace together when federated):

```json
{
  "name": "orbit_ui_open",
  "arguments": {}
}
```

To open it on one workspace, pass `workspace`:

```json
{
  "name": "orbit_ui_open",
  "arguments": { "workspace": "ws_example" }
}
```

Opening on a workspace carries no task or run. The panel rediscovers the
selector and reads data itself, so the workspace you passed is a starting
point, not a data read.

To open a specific task, pass `workspace` and `id` together. `kind` defaults to
`task`:

```json
{
  "name": "orbit_ui_open",
  "arguments": { "workspace": "ws_example", "id": "EX-42" }
}
```

`id` requires `workspace`, and `kind` requires both.

## Inspect one task or run

`orbit_ui_inspect` is the explicit form: `workspace` and `id` are required,
and `kind` is `task` (the default) or `run`.

```json
{
  "name": "orbit_ui_inspect",
  "arguments": { "workspace": "ws_example", "id": "EX-42", "kind": "task" }
}
```

For a run, pass the run ID Orbit returned, for example from
`orbit_workflow_run_list`:

```json
{
  "name": "orbit_ui_inspect",
  "arguments": { "workspace": "ws_example", "id": "<run-id>", "kind": "run" }
}
```

Both tools reject any other argument, such as a `status` or `actor`, with
`invalid_input`. A missing `workspace` or `id`, an empty string or a value over
2048 characters is refused the same way. A task or run that does not exist in
the selected workspace is an error; nothing falls back to another workspace.
Inspecting a run needs a session that may read runs, which means operator
authority (see [Connect your agent](../mcp-integration/#observe-the-run)).

The tool result is the data the panel starts from: a `task` object with `id`,
`title`, `status`, `updated_at`, `description` and `acceptance_criteria` for a
task, or a bounded run read with `kind: "run"` for a run. An empty open returns
a panel with `workspace` and `task` both `null`.

## Navigate the views

The panel has a rail with a workspace selector and four views.

| View | What it shows |
|---|---|
| **Tasks** | Tasks grouped by status, with search and a priority filter. It opens on In progress, Review, Blocked and Backlog; **Active work** restores that set and **All** includes every status. Selecting a row opens its detail with comments, history and artifact metadata, each paged. |
| **Runs** | Workflow runs with a state filter. A run's detail has its state and paged log excerpts. In-progress task rows link to their run. |
| **Auto-drain** | Readiness of the backlog, the current delivery window and its workers. |
| **Automation** | Routines, auto-tasks and jobs defined in the workspace, with their next scheduled slot. |

The selected host, workspace and task or run stay visible. Switching
workspace or view discards late replies from the previous one, so a slow read
never lands under the wrong destination. In a narrow host the rail becomes
horizontal navigation, and the panel follows the dashboard's dark and light
palettes.

**Send context to chat** puts a short reference (destination, task or run ID,
observed revision and title, not logs or history) into the conversation. Treat
it as a pointer: reread before you act on it.

## What opening does not grant

`orbit_ui_open` and `orbit_ui_inspect` only read. They are annotated read-only,
idempotent and non-destructive, and opening grants no authority: a session that
could not ship a task before opening it cannot ship it after. Neither the
widget, a click nor a model-context message upgrades the session.

Everything the panel can change goes through the ordinary Orbit tools, under
the session's own authority, workspace policy and audit:

- **Tasks.** **New task** files a proposed task that needs acceptance criteria;
  nothing runs until it is dispatched. Edit changes one field at a time, and
  comments are appended separately. Edits carry the revision they were read at
  and are refused if the task changed. A retried write with the same request ID
  does not repeat the mutation.
- **Review.** A review records criterion outcomes and evidence against the
  current revision and the run or pull-request head. Requesting changes and
  recording acceptance keep the task in review. **Accept and mark done**
  needs fresh eligible state and operator authority, and it never merges,
  publishes or dispatches.
- **Ship, drain and automation.** Ship on a backlog row, starting or stopping
  an auto-drain window, and toggling routines or auto-tasks need operator
  authority and an explicit workspace. A drain's **Complete automatically**
  option is off by default; turning it on authorizes completion of every task
  admitted during that window.

Drain and automation actions have no retry receipts. If a reply is lost, the
panel does not repeat the action: it disables further writes for that
workspace and category until you reopen it, and you check Tasks and Runs for
what happened.

An agent-only session (`orbit mcp init`) can read through the panel and make
the task edits an agent may make, but is refused the operator actions above. A
session from `orbit workspace init --mcp` is operator. See
[Connect your agent](../mcp-integration/#connect).

## When it does not render

If the client does not support MCP Apps, `tools/call` still succeeds and returns
the JSON described above, but nothing is drawn. Use the normal tools or the
dashboard instead:

| You want | Use |
|---|---|
| A task | `orbit_task_show`, `orbit_task_list` |
| Runs | `orbit_workflow_run_list`, `orbit_workflow_run_show` (operator) |
| Workspaces | `orbit_workspace_list` |
| A browser view | The [dashboard](../dashboard/) |
| A native window and tray | The [desktop app](../desktop/) |
| A terminal band and board | The [Claude Code mod](../claude-code/) |
