---
title: Quickstart
description: "Install Orbit, set up this machine and one repository, connect your agent, and ship a first task to a pull request."
sidebar:
  order: 1
---

Set up Orbit in one repository, connect the agent you already use, and ship one
task end to end. You finish with a pull request waiting for your review.

## Before you begin

- macOS or Linux, on x64 or arm64.
- Node 18 or newer, for the npm install.
- At least one signed-in agent CLI, such as Claude Code or Codex. `orbit doctor
  providers` lists the ones Orbit supports and whether this machine can launch
  each.
- The GitHub CLI (`gh`), signed in, so Orbit can open pull requests.
- On Ubuntu 24.04 and similar, the Linux sandbox prepared before your first run.
  See [Prepare the sandbox](./install/#prepare-the-sandbox).

## 1. Install the CLI

```bash
npm install -g @orbit-tools/cli
orbit --version
```

The package downloads the matching native binary and puts `orbit` on your
`PATH`. [Install Orbit](./install/) covers the other install methods.

## 2. Set up this machine

```bash
orbit init
```

`orbit init` asks for a machine name and a task-ID prefix of 2–5 uppercase
letters, such as `ABC`. Neither can change later on this machine. It also
detects your agent CLIs and seeds a crew for each.

## 3. Register a repository and connect your agent

```bash
cd your-repo
orbit workspace init --mcp
orbit doctor
```

`--mcp` registers Orbit with your agent clients, so they can file and ship
tasks. [Connect Your Agent](../how-to/mcp-integration/) explains what each
client gets.

## 4. Ship your first task

Ask your agent for a small change, such as documenting one function. It files a
task, asks you to approve it, ships it, and reports the pull request.

To do the same from the CLI, [First Task](./first-task/) walks through each
command:

```bash
TASK_ID=$(orbit task add --title "Document the fsProfile lookup" \
  --acceptance-criteria "The fsProfile lookup has a doc comment." \
  --complexity low --status proposed)
orbit task update "$TASK_ID" --approve
orbit run ship "$TASK_ID"
```

:::note[Two gates stay yours]
A new task waits in `proposed` until you approve it, and a ship run stops at
`review` with the pull request open. Merging the pull request, and closing the
task with `orbit task update "$TASK_ID" --approve`, are separate decisions.
:::

## Then what

Once one task has shipped, choose how much Orbit runs without you.

<div class="orbit-card-grid orbit-card-grid-3">
  <a class="orbit-card" href="../how-to/continuous-delivery/">
    <h3>Run a delivery window</h3>
    <p>Drain an approved backlog for a set time, several tasks at once.</p>
  </a>
  <a class="orbit-card" href="../how-to/recurring-work/">
    <h3>Schedule recurring work</h3>
    <p>Run jobs on a schedule and let auto-tasks file routine chores.</p>
  </a>
  <a class="orbit-card" href="../how-to/dashboard/">
    <h3>Use the dashboard</h3>
    <p>Watch tasks, runs, and errors in a browser, locally or over SSH.</p>
  </a>
</div>
