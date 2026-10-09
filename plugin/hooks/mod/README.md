# Orbit mod for Claude Code

The Orbit plugin's hooks module. In Claude Code it shows the workspace's
tasks around the prompt and lets you act on them without leaving the
session. Other agents load the plugin's skills and MCP server and ignore
this folder.

## What it draws

- **Band**, above the prompt. Shows running, blocked, review, backlog and
  proposed counts, and the task this session is working on. While a ship
  is in flight, it shows the ship's position in the PR pipeline. The first
  blocked task gets **Open** and **Rescue here** buttons. Below 80 columns,
  or with `band` set to `compact`, the band is a single line.
- **Status line**: the ship in flight (`orbit ⇡ ORB-12 · review 7/12`),
  else the active task (`orbit ◉ ORB-12 · in-progress`), else the blocked
  count when there is one.
- **Toasts**: a task moves to blocked, review or done, or a ship lands.
- **Orbit pane**, with three tabs:
  - **Board.** One lane per status. Pick a card to see its acceptance
    criteria and the actions its status allows:
    - proposed: approve, reject
    - backlog: ship, work here
    - in-progress: track the run
    - review: accept to done
    - blocked: rescue here
  - **Ship.** First a preflight checks that the task is in backlog, its
    dependencies are done, no other ship is running, and its files are
    free of other tasks' locks (`orbit run readiness`). Then
    `orbit run ship` launches a coordinator. The view follows its recorded
    child dispatches to the task's `task_pr_pipeline` run for the 12-step
    track, retrying while delivery is waiting to start. The coordinator's
    terminal state decides whether the ship landed or failed. On desktop
    and VS Code the trajectory is drawn as an SVG.
- **Task cards.** A prompt that mentions a known task id, such as
  `ORB-123`, carries that task's card (status, priority, criteria) as
  context.
- **Commit trailer.** While the session works a task (after **Work here**,
  **Rescue here**, or an `orbit_task_update` to in-progress), a Bash
  `git commit` gains `--trailer 'Task: <id>'`. A commit that already has
  a trailer, or uses `--amend`, is left alone.

Approve runs as soon as you press it. Accept, reject and ship ask for
confirmation first. **Work here** and **Rescue here** submit a prompt to
this session. Nothing the mod does dispatches work by itself.

## Commands

| Command | Does |
|---|---|
| `/orbit-board [id]` | Opens the board, with the task selected when an id is given |
| `/orbit-ship [id]` | Opens the ship view, with the task's preflight when an id is given |
| `/orbit-band` | Shows or hides the band |

## Options

Set these in the plugin's settings (`/plugin`, then Orbit, then configure).

| Option | Default | Meaning |
|---|---|---|
| `ownerHost` | empty | SSH host that owns this workspace, for a checkout that is a replica or isn't registered on this machine. Reads and writes then run `orbit` there over `ssh -o BatchMode=yes`. Empty falls back to the hosts Orbit's federated MCP uses (registered with `orbit host add` in `~/.orbit/hosts.toml`, or the legacy `~/.orbit/mcp-destinations.toml`); with several, the first that answers for the workspace. |
| `refreshMinutes` | 3 | How often the band refreshes. It also refreshes after a turn that ends at least 30 s after the last read. |
| `band` | `on` | `on`, `compact` (always one line), or `off` |
| `commitTrailer` | true | Add the `Task:` trailer to commits made while working a task |

## How it reads Orbit

Every read and write is an `orbit` CLI call scoped with `--workspace`.
The mod looks for the binary on `PATH`, then in `~/.orbit/bin`. It finds
the workspace with `orbit workspace show --format json` run in the
session's directory:

- An owner checkout is read locally.
- A replica, or an unregistered checkout whose git root is named after a
  workspace, is read through `ownerHost`, or through the federated MCP's
  destinations when `ownerHost` is empty.

## Developing

```bash
cd plugin
claude plugin validate .
claude plugin test .
```

The tests in `tests/` answer `orbit` with a fake workspace and drive the
band and pane on the terminal and desktop surfaces. Every function that
takes `$` lives in `register.tsx`, because the engine follows `$` only
into functions declared in the same file. `model.ts` and `cli.ts` are
pure. `views/` draws from plain data and an `Actions` table.
