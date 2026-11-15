---
title: Use the Claude Code mod
description: "Review tasks, approve proposals, ship backlog items, follow PR pipeline runs and rescue blocked work inside Claude Code."
---

The Orbit plugin for Claude Code includes a mod that shows your workspace's
tasks around the prompt and lets you approve, ship and rescue them without
leaving the session. Other agent clients load the plugin's MCP tools and skills
and ignore the mod.

## Set up

1. Install the plugin in Claude Code:

   ```text
   /plugin marketplace add constellation-works/orbit
   /plugin install orbit
   ```

2. Install the `orbit` CLI. The mod looks in `~/.orbit/bin` first, then your
   `PATH`.
3. Open Claude Code in an initialized Orbit workspace, or in a replica checkout
   whose owner the mod can reach (see [Remote workspaces](#remote-workspaces)).

The panes need a wide enough terminal. On a narrow one, the command tells you
the pane opens once the terminal is wide enough. If your client or terminal
cannot draw the mod at all, Orbit's MCP tools and skills still work through
normal tool calls.

## Slash commands

| Command | Action |
|---|---|
| `/orbit-board [id]` | Opens the Board pane. With a task ID, opens that task's lane and selects its card. |
| `/orbit-ship [id]` | Opens the Ship pane. With a task ID, runs that task's preflight. |
| `/orbit-band` | Shows or hides the band above the prompt. |

## What the mod shows

| Element | Shows |
|---|---|
| **Band**, above the prompt | Counts of running, blocked, review, backlog and proposed tasks, and the task this session is working on. While a ship runs, its step in the pull request pipeline. The first blocked task gets **Open** and **Rescue here** buttons. Below 80 columns, or with `band` set to `compact`, the band is one line. |
| **Status line** | The ship in flight (`orbit ⇡ ABC-12 · review 7/12`), else the active task (`orbit ◉ ABC-12 · in-progress`), else the blocked count. |
| **Toasts** | A task moved to `blocked`, `review` or `done`, or a ship ended. |
| **Task cards** | When your prompt mentions a known task ID such as `ABC-12`, the task's status, priority and acceptance criteria are added to Claude's context. |

The band refreshes every `refreshMinutes` (default 3), and after any turn that
ends at least 30 seconds after the last read.

## The Board pane

The board has one collapsible lane per status, most urgent first: **Blocked**,
**In-progress**, **Review**, **Proposed**, **Backlog** and **Done today**.

Each lane shows up to 20 tasks, then a footer
(`+N more · /orbit-board <id> opens one`). `/orbit-board <id>` brings a task
into view even when it is outside the first 20.

Select a task to see its complexity, dependencies, last run ID and acceptance
criteria, with the actions its status allows:

| Status | Actions |
|---|---|
| **Proposed** | **Approve** (`a`) runs `orbit task update <id> --approve` at once and moves the task to `backlog`. **Reject** (`x`) asks for confirmation, then marks it `rejected`. |
| **Backlog** | **Ship…** (`s`) opens the Ship pane and runs preflight. **Work here** (`w`). |
| **In-progress** | **Track run** (`t`) follows the task's run in the Ship pane, when it has one. **Work here** (`w`) when the task is not already active in this session. |
| **Review** | **Accept → done** (`a`) asks `Accept <id> and mark it done?`, then marks it `done`. **Work here** (`w`). |
| **Blocked** | **Rescue here** (`r`). **Open run** (`t`) shows the task's run in the Ship pane, when it has one. |

**Work here** and **Rescue here** make the task active in this session and
submit a prompt to Claude:

- **Work here** asks Claude to read the task, start it with the `orbit` skill
  and work it to review.
- **Rescue here** asks Claude to diagnose the task's last run with the
  `orbit-orchestrate` skill, finish the work in this session and hand it to
  review.

Neither dispatches a background worker.

## The Ship pane

Open it with `/orbit-ship`, or `/orbit-ship <id>` to go straight to a task's
preflight. With no task chosen, the pane lists backlog tasks to pick from.

### Preflight checks

The pane computes the preflight once when it loads. It does not re-run the
checks before launch.

| Check | Passes when | Gates Launch |
|---|---|---|
| **Task is in backlog** | The task's status is `backlog`. | Yes |
| **Dependencies are done** | Every dependency is `done`. | Yes |
| **No ship already running** | Shown for context. It always passes for a backlog task. | No |
| **Files are free to lock** | No other task holds a conflicting lock. Otherwise the detail names the holder, such as `held by ABC-9`. | No |
| **Ready to start** | `orbit run readiness <id>` reports the task eligible. Otherwise the detail gives the reason. | No |

The last two rows come from `orbit run readiness`. When it reports nothing for
the task, the pane shows one passing **Readiness** row instead:
`not reported; the run waits for locks if needed`.

### Launch and track

**Launch** (`l`) appears when the first two checks pass. It asks
`Ship <id> through the PR pipeline?`, then runs `orbit run ship <id> --json`.

The pane then follows the run through the twelve steps of the pull request
pipeline. Desktop and VS Code surfaces also draw the track as an SVG curve.

| Glyph | Step state |
|---|---|
| `●` | Completed. |
| `◉` | Active. |
| `✗` | Failed. |
| `■` | Stopped, held or cancelled. |
| `↷` | Skipped. Not counted as completed. |

| Outcome | Meaning |
|---|---|
| **Landed** | The pull request is open against the base branch and the task moves to `review`. |
| **Failed** | The pane shows the failure reason and a **Rescue here** button. |
| **Held** | Delivery is waiting on external review evidence. The run is over and the task stays as it is. |
| **Skipped** | The run was skipped. Tracking ends. |

You can keep working in the session while a ship is in flight. The band and
status line follow the run until it ends.

## Automatic commit trailers

While the session works a task (after **Work here**, **Rescue here**, or setting
a task in-progress), the mod adds `--trailer 'Task: <id>'` to `git commit`
commands Claude runs.

- It also catches commits inside chained commands (`&&`, `;`, `|`), subshells
  and conditions (`if`, `while`).
- It skips `--amend`, commits that already carry a trailer, `git commit-tree`
  and text inside heredocs.
- Set the `commitTrailer` option to `false` to turn it off.

## Plugin options

Set these in Claude Code's plugin settings (`/plugin`, choose **orbit**, then
**configure**):

| Option | Type | Default | Description |
|---|---|---|---|
| `ownerHost` | string | `""` | SSH host or alias that owns this workspace. See [Remote workspaces](#remote-workspaces). |
| `refreshMinutes` | number | `3` | How often the mod refreshes, in minutes (minimum 1). |
| `band` | string | `"on"` | `"on"`, `"compact"` (always one line) or `"off"` (hidden). |
| `commitTrailer` | boolean | `true` | Add the `Task:` trailer to commits made while working a task. |

## Remote workspaces

Every read and write is an `orbit` CLI call scoped with `--workspace`.

- In an initialized local workspace, the calls run locally.
- In a replica, or an unregistered clone named after a workspace, the calls run
  on the owner over `ssh -o BatchMode=yes`. The owner is `ownerHost` when set.
  Otherwise the mod tries the hosts registered with `orbit host add`
  (`~/.orbit/hosts.toml`) and uses the first that answers for the workspace.

Batch mode means SSH never stops the session to ask for a password.
