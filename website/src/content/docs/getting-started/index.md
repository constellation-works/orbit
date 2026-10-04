---
title: Quickstart
description: "Install Orbit, let the orbit-setup skill in your agent set up a repository, open the dashboard, and ship a first task to a pull request."
sidebar:
  order: 1
---

Install Orbit, let your agent set up the repository, and ship a first task from
the dashboard.

## Before you begin

- macOS or Linux, on x64 or arm64. On Windows, run Orbit inside WSL2.
- Node 18 or newer, for the npm install.
- At least one signed-in agent CLI, such as Claude Code or Codex.
- The GitHub CLI (`gh`), signed in, so Orbit can open pull requests.

## 1. Install the CLI

```bash
npm install -g @orbit-tools/cli
orbit init
```

`orbit init` asks for a machine name and a task-ID prefix of 2–5 uppercase
letters, such as `ABC`. The prefix cannot change later on this machine. It also
detects your agent CLIs, links Orbit's skills into your agents, and on Linux
prepares the sandbox.

## 2. Let your agent set up the repository

:::tip[Recommended]
Open your agent in the repository and ask it to **set up Orbit for this repo**.
:::

The `orbit-setup` skill registers the repository, connects your agent over
MCP, and runs `orbit doctor`, asking only for what it cannot infer, such as the
branch pull requests should target. Start a fresh agent session when it
finishes, so the Orbit tools load.

Rather do it yourself? Run `orbit workspace init --mcp` and `orbit doctor` in
the repository; [Install Orbit](./install/#set-up-by-hand) explains each step.

## 3. Open the dashboard

```bash
orbit web serve
```

The dashboard opens at `http://127.0.0.1:7878` and serves every workspace on
this machine. Keep it running in its own terminal:

| Section | What you do there |
|---|---|
| **Tasks** | Approve proposed tasks, ship backlog tasks, edit a task's details, and review finished work. |
| **Runs** | Follow a run's steps and events, and cancel, resume, or replay it. |
| **Health** and **Audit** | See failures, policy denials, and every recorded step. |
| **Automation** | Turn routines and auto-tasks on or off, file an auto-task now, and run a delivery window. |
| **Settings** | Inspect and edit configuration and crews. |

It listens on loopback only; `orbit web connect <ssh-host>` opens one on
another machine. See [Use the Dashboard](../how-to/dashboard/).

## 4. Ship your first task

1. **Ask your agent for a small change**, such as documenting one function. It
   files a task with acceptance criteria. The task appears in **Tasks** under
   **Awaiting approval**.
2. **Click Approve.** The task moves to the backlog.
3. **Click Ship.** Orbit reserves the task's files, runs the agent in an
   isolated worktree, and plans, executes, and reviews the change. Follow it
   from **View run**.
4. **Review the pull request.** The run stops with the task in review and the
   pull request open. Merge it on GitHub, then click **Approve** on the task to
   close it.

Your agent can do steps 2 and 3 for you when you tell it to. The dashboard
shows the same task either way.

:::note[Two gates stay yours]
A new task waits in `proposed` until you approve it, and a ship run stops at
`review` with the pull request open. Merging the pull request and closing the
task are separate decisions.
:::

Prefer the terminal? [First Task](./first-task/) walks through the same flow
with `orbit task add`, `orbit task update --approve`, and `orbit run ship`.

## Then what

Once one task has shipped, choose how much Orbit runs without you. Each of
these also has a home in the dashboard.

<div class="orbit-card-grid orbit-card-grid-3">
  <a class="orbit-card" href="../how-to/dashboard/">
    <h3>Get more from the dashboard</h3>
    <p>Edit tasks, read run traces, and reach a remote host over SSH.</p>
  </a>
  <a class="orbit-card" href="../how-to/continuous-delivery/">
    <h3>Run a delivery window</h3>
    <p>Drain an approved backlog for a set time, several tasks at once.</p>
  </a>
  <a class="orbit-card" href="../how-to/recurring-work/">
    <h3>Schedule recurring work</h3>
    <p>Run jobs on a schedule and let auto-tasks file routine chores.</p>
  </a>
</div>
