---
title: Use the Claude Code mod
description: "Review tasks, approve proposals, ship backlog items, follow PR pipeline runs, and rescue blocked work directly inside Claude Code."
sidebar:
  order: 2
---

The Orbit plugin for Claude Code includes a hooks module (mod) that brings your
Orbit workspace into the terminal session. It draws task counts and run status
around the prompt, responds to slash commands, and opens interactive panes to
manage tasks, run preflight checks, and follow ships through the pull request
pipeline.

Other agent clients (such as Codex CLI, Cursor, or Gemini CLI) load Orbit's MCP
tools and workflow skills directly, and clients or terminals that cannot render
the mod fall back to the standard MCP tool and skill interface without loss of
functionality.

## Prerequisites and fallback

To use the mod:

- Install the Orbit plugin in Claude Code:
  ```bash
  /plugin marketplace add constellation-works/orbit
  /plugin install orbit
  ```
- Have the `orbit` CLI installed. The mod searches for `orbit` on your `PATH`,
  then in `~/.orbit/bin`.
- Open Claude Code inside an initialized Orbit workspace, a replica checkout,
  or a directory configured with an [owner host](#plugin-options).
- Use a terminal with sufficient width. Interactive panes (Board and Ship) open
  when terminal width permits; on narrower screens, commands notify you that
  the pane will open once the terminal is wide enough.

If your client or terminal cannot render the interactive mod, the session
gracefully falls back: Orbit's full MCP tool surface and bundled workflow skills
(`orbit-setup`, `orbit-orchestrate`) remain available through normal tool calls
and prompts.

## Slash commands

The mod registers three slash commands in Claude Code:

| Command | Argument | Action |
|---|---|---|
| `/orbit-board` | `[id]` | Opens the Orbit board pane. Passing a task ID (such as `/orbit-board ABC-12`) opens that task's lane and selects its card. |
| `/orbit-ship` | `[id]` | Opens the Ship pane. Passing a task ID runs its preflight checks and stages it for launch. |
| `/orbit-band` | None | Toggles visibility of the prompt band on or off. |

## In-session interface

The mod surfaces workspace activity without requiring you to switch windows.

### Prompt band

The band renders above your input prompt:

- **Workspace counts**: shows open task counts across `blocked`, `in-progress`,
  `review`, `proposed`, and `backlog`.
- **Active task**: highlights the task currently assigned to this session.
- **Flight tracker**: while a ship is running, shows its current step and
  progress along the pull request pipeline.
- **Blocked work**: when a task is blocked, surfaces the first blocked task with
  immediate **Open** and **Rescue here** buttons.
- **Compact mode**: if the terminal window is narrower than 80 columns, or if
  the `band` plugin setting is set to `compact`, the band renders as a single
  line.

The band refreshes every `refreshMinutes` (default 3 minutes) and after any turn
that finishes at least 30 seconds after the last read. You can hide or restore it
anytime with `/orbit-band`.

### Status line

The status line displays high-priority state:

1. **Ship in flight**: `orbit ⇡ ABC-12 · review 7/12`
2. **Active task**: `orbit ◉ ABC-12 · in-progress`
3. **Blocked tasks**: count of blocked tasks awaiting attention.

### Toasts

Toasts notify you when a task enters `blocked`, `review`, or `done`, or when a
ship finishes landing.

### Task cards

When you mention a known task ID (like `ABC-12`) in your prompt, the mod
automatically injects that task's card—including status, priority, and acceptance
criteria—into Claude's prompt context. You do not need to copy and paste task
details into the conversation.

## The Board pane

Open the board with `/orbit-board` or `/orbit-board <id>`.

The board organizes open tasks into collapsible lanes ordered by attention:

1. **Blocked**: tasks requiring intervention or diagnosis.
2. **In-progress**: tasks actively being worked.
3. **Review**: tasks with completed work awaiting review acceptance.
4. **Proposed**: newly filed tasks awaiting triage or approval.
5. **Backlog**: approved tasks ready to be worked or shipped.
6. **Done today**: tasks completed recently.

Each lane displays up to 20 tasks. When a lane exceeds 20 tasks, a footer
indicates additional tasks (`+N more · /orbit-board <id> opens one`). Opening a
task directly with `/orbit-board <id>` brings that task into view even if it is
outside the first 20. Collapsing a lane clears any active task selection within it.

### Task actions and confirmations

Selecting a task expands its details, showing its complexity, dependencies, last
run ID, acceptance criteria, and status-specific action buttons:

| Status | Available actions | Confirmation behaviour |
|---|---|---|
| **Proposed** | **Approve** (`a`), **Reject** (`x`) | **Approve** runs immediately (`orbit task update <id> --approve`), moving the task to `backlog`. **Reject** prompts for confirmation before marking the task `rejected`. |
| **Backlog** | **Ship…** (`s`), **Work here** (`w`) | **Ship…** switches to the Ship pane and runs preflight readiness checks. **Work here** assigns the task to this session and submits an instruction to Claude. |
| **In-progress** | **Track run** (`t`), **Work here** (`w`) | **Track run** opens the Ship view to follow the execution. **Work here** assigns the task to this session if it is not already active. |
| **Review** | **Accept → done** (`a`), **Work here** (`w`) | **Accept → done** prompts for confirmation (`Accept <id> and mark it done?`) before marking the task `done`. **Work here** prompts Claude to make further changes. |
| **Blocked** | **Rescue here** (`r`), **Open run** (`t`) | **Rescue here** assigns the task to this session and prompts Claude to diagnose and repair the issue. **Open run** inspects run logs. |

### Work here and Rescue here

- **Work here**: marks the task as active in the session, updates the band and
  status line, and submits a prompt to Claude to read the task, start work using
  the `orbit` skill, and bring it to review.
- **Rescue here**: marks the blocked task as active and instructs Claude to
  diagnose the failed run using the `orbit-orchestrate` skill, repair the issue
  in the current session, and hand the candidate back to review.

Neither button dispatches unattended background workers; both route the task
directly into your active Claude session.

## The Ship pane

Open the ship pane with `/orbit-ship` or `/orbit-ship <id>`.

If no task is staged, the pane lists ready backlog tasks. Selecting one or
passing an ID initiates preflight checks.

### Preflight checks

Before shipping, the mod runs `orbit run readiness <id>` to verify:

1. **Backlog status**: the task is approved in `backlog`.
2. **Dependencies**: all required dependency tasks are marked `done`.
3. **No concurrent ship**: no other ship run is currently in progress for the task.
4. **File locks**: workspace files touched by the task are not locked by other active runs.

If checks pass, the **Launch** button (`l`) activates. If any check fails, the
pane highlights the blocker in red and prevents launching.

### Launch confirmation and tracking

Pressing **Launch** opens a confirmation prompt (`Ship <id> through the PR pipeline?`).
Confirming starts `orbit run ship <id> --json`, which launches the ship coordinator.

The view then tracks the run through the twelve steps of the pull request pipeline:

- **Visualization**: On desktop and VS Code surfaces, progress displays as an
  SVG trajectory curve. In standard terminals, it displays step glyphs and labels
  across columns.
- **Step indicators**:
  - `●` (accent/color): Completed step.
  - Active: Cyan highlight.
  - `✗` (red): Failed step.
  - `■` (yellow): Stopped, held, or cancelled step.
  - `↷`: Skipped step (does not increment the completed step count).
- **Terminal outcomes**:
  - **Landed**: The pipeline passed, the pull request was opened against the base
    branch, and the task moved to `review`.
  - **Failed**: The run halted; the pane displays the failure reason and provides
    a **Rescue here** button.
  - **Held**: Execution paused waiting on external review evidence; the task
    remains in its current state.
  - **Skipped**: The run was skipped and tracking ends.

While a ship is in flight, you can safely continue conversation in Claude Code;
the prompt band and status line maintain live progress indicators until the run
settles.

## Automatic commit trailers

When you work on an active task (via **Work here**, **Rescue here**, or setting a
task in-progress), the mod inspects shell commands and automatically adds a
`--trailer 'Task: <id>'` argument to `git commit`.

- The trailer is appended to Git commits within chained commands (`&&`, `;`, `|`),
  subshells, or conditional statements (`if`, `while`).
- It ignores `--amend` commits, commits that already specify a `Task:` trailer,
  plumbing commands like `git commit-tree`, and commit messages in heredocs.
- You can disable this behaviour by setting the `commitTrailer` plugin option to
  `false`.

## Plugin options

Configure the mod through Claude Code plugin settings (`/plugin`, choose **orbit**,
then **configure**):

| Option | Type | Default | Description |
|---|---|---|---|
| `ownerHost` | string | `""` | SSH host or alias for remote workspaces. When set, all `orbit` CLI commands execute over `ssh -o BatchMode=yes <ownerHost>`. When empty, the mod falls back to hosts registered with `orbit host add`. |
| `refreshMinutes` | number | `3` | Refresh frequency in minutes (minimum 1). The mod also refreshes after turns that finish at least 30 seconds after the last refresh. |
| `band` | string | `"on"` | Display mode for the prompt band: `"on"` (standard multi-line), `"compact"` (always single-line), or `"off"` (hidden). |
| `commitTrailer` | boolean | `true` | When `true`, automatically adds `--trailer 'Task: <id>'` to `git commit` commands executed while working on a task. |

## Remote workspace routing

Every operation executed by the mod is an `orbit` CLI invocation scoped with
`--workspace`. When Claude Code runs in a directory, the mod determines how to
route calls:

1. **Local checkouts**: If the checkout is an initialized local workspace,
   commands run locally.
2. **Replica or unregistered checkouts**: If the directory is a replica or an
   unregistered clone named after a workspace:
   - Commands route to `ownerHost` over SSH if configured.
   - Otherwise, the mod queries the federated host registry (`~/.orbit/hosts.toml`
     or legacy `~/.orbit/mcp-destinations.toml`, configured via `orbit host add`)
     and selects the first host that owns the workspace.

All remote executions use non-interactive batch mode (`ssh -o BatchMode=yes`),
ensuring your Claude session remains responsive without hanging on credential
prompts.
