---
title: Install Orbit
description: "Install the Orbit CLI, then let the orbit-setup skill in your agent finish the setup. Each step is also here to do by hand."
sidebar:
  order: 2
---

Install the CLI, then let your agent do the rest: Orbit ships an `orbit-setup`
skill that sets up your repository for you. Every step is also here to do by
hand.

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

The `orbit-setup` skill takes it from there. It checks what is already in
place, registers the repository, connects your agent over MCP, checks your
agent CLIs and the sandbox, and runs `orbit doctor`. It asks only for choices it
cannot infer, such as the branch pull requests should target. Start a fresh
agent session when it finishes, so the Orbit tools load.

Go back to the same skill for later changes: adding a provider, scheduling
work, upgrading, or fixing a failing `orbit doctor`.

## Set up by hand

The skill runs these same steps. Use this section to do them yourself or to
see what the skill did.

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
[Connect Your Agent](../../how-to/mcp-integration/). Add
`--base-branch <branch>` when pull requests should target a branch other than
`main`, or `--ship-mode local` to merge in place instead of opening pull
requests.

### Prepare the sandbox

Orbit runs each agent in an OS-level sandbox: `sandbox-exec` on macOS, which
needs no setup, and Bubblewrap on Linux, which fails closed without a trusted
`/usr/bin/bwrap`.

On Linux, `orbit init` prepares the sandbox for you. It probes Bubblewrap as
your account and, only if that fails, installs it through the distribution's
package manager, asking for your password. On Ubuntu 24.04 it also loads the
packaged AppArmor rule. Run `orbit init` as the account that will run Orbit,
not through `sudo`.

A run failing with `bwrap: setting up uid map: Permission denied` is this
step, not your task. The
[Linux sandbox runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/linux-sandbox.md)
covers which distributions are prepared automatically, container images, and
the fix for each failure.

### Configure and check

`orbit init` writes a working `~/.orbit/config.toml` with a crew for each agent
CLI it found. Pick the default crew, then check the workspace:

```bash
orbit config set workflow.default_crew opus
orbit doctor
```

`orbit doctor` checks config, database, disk, indexes, locks, and runs, and
`orbit doctor providers` shows whether each agent CLI and the sandbox are ready.
[Configuration](../../reference/config/) lists every setting.

## Stay current

```bash
npm install -g @orbit-tools/cli@latest
```

`orbit update --check` tells you whether a newer release is out. npm owns this
install, so upgrade through npm; `orbit update` names the right command for
however Orbit was installed. Pending state migrations apply the next time Orbit
opens the workspace. Stop running Orbit sessions and `orbit web serve` before
upgrading, or ask your agent to upgrade Orbit and let the skill check for them.
