---
title: Install Orbit
description: "Install the Orbit CLI, then let the orbit-setup skill in your agent finish the setup. Each step is also here to do by hand."
sidebar:
  order: 2
---

Install the CLI, then let your agent's `orbit-setup` skill set up your
repository. Every step is also below to do by hand.

## Prerequisites

- macOS or Linux, on x64 or arm64. On Windows, run Orbit inside WSL2.
- Node 18 or newer, for the npm install.
- At least one signed-in agent CLI, such as Claude Code or Codex. Orbit runs
  every agent step through one. The
  [setup explorer](../../concepts/agents/#set-up-an-executor) lists the
  supported CLIs.
- The GitHub CLI (`gh`), signed in, if you want pull requests.

## Install

```bash
npm install -g @orbit-tools/cli
orbit init
```

The package downloads the matching native binary and puts `orbit` on your
`PATH`. `orbit init` asks for a machine name and a task-ID prefix, detects
your agent CLIs, links Orbit's skills into your agents, and on Linux prepares
the sandbox.

## Let your agent set it up

:::tip[Recommended]
Open your agent in the repository and ask it to **set up Orbit for this repo**.
:::

The `orbit-setup` skill checks what is already in place, registers the
repository, connects your agent over MCP, checks your agent CLIs and the
sandbox, and runs `orbit doctor`. It asks only for choices it cannot infer, such
as the branch pull requests should target. When it finishes, start a fresh
agent session so the Orbit tools load.

Use the same skill for later changes: adding a provider, scheduling work,
upgrading, or fixing a failing `orbit doctor`.

## Set up by hand

These are the steps the skill runs. Use them to set up by hand, or to see what
the skill did.

### Initialize state

```bash
orbit init
cd <repo>
orbit workspace init --mcp
```

`orbit init` sets up this machine under `~/.orbit/`. It asks for a **machine
name**, which you can rename later, and a **task-ID prefix** of 2–5 uppercase
letters, such as `ABC`, which can never change (`ORB` and `ADR` are reserved).
For an unattended setup, pass both:

```bash
orbit init --non-interactive --machine-name build-01 --task-prefix ABC
```

`orbit workspace init` registers the repository and keeps its state under
`.orbit/`. `--mcp` connects your agent clients with **operator** authority,
which lets them ship tasks and run governed operations. Plain `orbit mcp init`
registers an agent-only connection instead; see
[Connect Your Agent](../../how-to/mcp-integration/).

By default, pull requests target the branch checked out when you run
`orbit workspace init`, or `main` if none is. Add `--base-branch <branch>` to
choose another, or `--ship-mode local` to merge in place instead of opening
pull requests.

### Prepare the sandbox

Orbit runs each agent in an OS-level sandbox: `sandbox-exec` on macOS, which
needs no setup, and Bubblewrap on Linux, which fails closed without a trusted
`/usr/bin/bwrap`.

On Linux, `orbit init` prepares the sandbox. It probes Bubblewrap as your
account and, only if that fails, installs it through the distribution's
package manager, asking for your password. On Ubuntu 24.04 it also loads the
packaged AppArmor rule. Run `orbit init` as the account that will run Orbit,
not through `sudo`.

A run that fails with `bwrap: setting up uid map: Permission denied` points to
this step, not your task. The
[Linux sandbox runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/linux-sandbox.md)
covers which distributions are prepared automatically, container images, and
the fix for each failure.

### Configure and check

`orbit init` writes a working `~/.orbit/config.toml` with a crew for each agent
CLI it found, and picks a default crew. To pick a different one, set
`workflow.default_crew`. Then check the workspace:

```bash
orbit config set workflow.default_crew opus
orbit doctor
```

`orbit doctor` checks config, database, disk, indexes, locks, and runs, and
`orbit doctor providers` shows whether each agent CLI and the sandbox are ready.
[Configuration](../../reference/config/) lists every setting.

## Stay current

Ask your agent to upgrade Orbit; the `orbit-setup` skill checks for running
Orbit processes first. To upgrade by hand, stop running Orbit sessions and
`orbit web serve`, then run:

```bash
npm install -g @orbit-tools/cli@latest
```

`orbit update --check` tells you whether a newer release is out. npm owns this
install, so `orbit update` prints the npm command instead of replacing the
binary. Pending state migrations apply the next time Orbit opens the
workspace.
