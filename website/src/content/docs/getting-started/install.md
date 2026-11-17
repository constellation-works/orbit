---
title: Install Orbit
description: "Install the Orbit CLI, set up a repository with the orbit-setup skill or by hand, and keep the install current."
---

Install the CLI, then let your agent's `orbit-setup` skill set up the
repository, or run each step yourself.

## Prerequisites

- macOS or Linux, on x64 or arm64. On Windows, use [WSL2](#windows).
- Node 18 or newer, for the npm install.
- At least one signed-in agent CLI, such as Claude Code or Codex. Orbit runs
  every agent step through one. The
  [setup explorer](/concepts/agents/#set-up-an-executor) lists the supported
  CLIs.
- The GitHub CLI (`gh`), signed in, if you want pull requests.

## Install

```bash
npm install -g @orbit-tools/cli
orbit init
```

`orbit init` asks for a machine name and a task-ID prefix, detects your agent
CLIs, links Orbit's skills into your agents, and on Linux prepares the
sandbox.

[GitHub releases](https://github.com/constellation-works/orbit/releases) may
also attach a desktop app, a native window for the dashboard, as `.deb` and
`.AppImage` bundles for Linux. macOS bundles require signing and notarization
credentials. The app needs this CLI install. See [Use the desktop app](/how-to/desktop/).

## Windows

Native Windows is not supported: the npm install fails there with
`EBADPLATFORM`. Open a WSL2 Linux shell and install inside it:

```bash
npm install -g @orbit-tools/cli
```

WSL2 has not yet been verified on a Windows host. The
[Windows WSL2 guide](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/windows-wsl2.md)
covers setup.

## Install as an agent plugin

The plugin gives one agent Orbit's MCP tools and skills without `orbit` on
your `PATH`. It launches the pinned npm CLI. The dashboard and setup across
several agents still need the CLI install above.

```text
# Claude Code
/plugin marketplace add constellation-works/orbit
/plugin install orbit

# Codex CLI
codex plugin marketplace add constellation-works/orbit --ref agent-main
codex plugin add orbit@orbit

# Cursor, from a checkout of the Orbit repository
mkdir -p ~/.cursor/plugins/local && ln -sfn "$(pwd)/plugin" ~/.cursor/plugins/local/orbit
```

In Claude Code, the plugin also adds a live task band, an interactive board,
and a ship view. See [Use the Claude Code mod](/how-to/claude-code/).

## Let your agent set it up

Open your agent in the repository and ask it to **set up Orbit for this
repo**.

The `orbit-setup` skill registers the repository, connects your agent over
MCP, checks your agent CLIs and the sandbox, and runs `orbit doctor`. It asks
only for what it cannot infer, such as the branch pull requests should target.
When it finishes, start a fresh agent session so the Orbit tools load.

Use the same skill later to add a provider, schedule work, upgrade, or fix a
failing `orbit doctor`.

## Set up by hand

The steps the skill runs:

```bash
orbit init
cd <repo>
orbit workspace init --mcp
orbit doctor
```

### Initialize the machine

`orbit init` sets up this machine under `~/.orbit/`. It asks for:

- a **machine name**, which you can change later;
- a **task-ID prefix** of 2–5 uppercase letters, such as `ABC`, which can
  never change. `ORB` and `ADR` are reserved.

For an unattended setup, pass both:

```bash
orbit init --non-interactive --machine-name build-01 --task-prefix ABC
```

`orbit init` also adds a [human crew](/reference/config/#human-crews) for
tasks Orbit tracks for you but never hands to an agent. Its name defaults to
your OS user name; pass `--human-name <name>` to choose another.

### Register the repository

`orbit workspace init` registers the repository and keeps its state under
`.orbit/`. Its options:

| Option | Effect |
|---|---|
| `--mcp` | Connects your agent clients with **operator** authority, so they can ship tasks and run governed operations. For an agent-only connection, see [Connect your agent](/how-to/mcp-integration/). |
| `--base-branch <branch>` | Sets the branch pull requests target. The default is the branch checked out when you run the command, or `main`. |
| `--ship-mode local` | Merges in place instead of opening pull requests. |
| `--preset <file>` | Applies a [workspace preset](/reference/config/#workspace-presets) that a distribution supplies. |

### Prepare the sandbox

Orbit runs each agent in an OS-level sandbox. macOS needs no setup. On Linux
the sandbox is Bubblewrap, and runs stay blocked until
`orbit doctor providers` reports it ready.

`orbit init` prepares it: if Bubblewrap is missing or does not work for your
account, it installs one and asks for your password. Run `orbit init` as the
account that will run Orbit, not through `sudo`.

An unattended `orbit init` never waits for a password, so it prepares the
sandbox only as root or with passwordless `sudo`. Otherwise it warns and sets
up the rest. Fix the cause the warning names, then run
`orbit init --host-prerequisites-only`.

A run that fails with `bwrap: setting up uid map: Permission denied` points to
this step, not your task. The
[Linux sandbox runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/linux-sandbox.md)
has the fix for each failure.

### Configure and check

`orbit init` writes `~/.orbit/config.toml` with the built-in crews, enables
those whose agent CLI it finds, and picks a default from them. If it finds
none, enable a crew before you dispatch.
[What `orbit init` seeds](/reference/config/#what-orbit-init-seeds) lists the
other values it sets.

To use a different default crew in this workspace:

```bash
orbit config set --seed-from-global workflow.default_crew opus
```

`--seed-from-global` starts a workspace config that does not exist yet from
the global one.

`orbit doctor` checks the workspace, and `orbit doctor providers` shows
whether each agent CLI and the sandbox are ready.
[Configuration](/reference/config/) lists every setting.

## Stay current

Ask your agent to upgrade Orbit; the `orbit-setup` skill checks for running
Orbit processes first. To upgrade by hand, stop running Orbit sessions and
`orbit web serve`, then run:

```bash
npm install -g @orbit-tools/cli@latest
orbit workspace sync --all
```

- `orbit update --check` tells you whether a newer release is out. With an npm
  install, `orbit update` prints the npm command instead of replacing the
  binary.
- `orbit workspace sync --all` brings every registered workspace's managed
  defaults up to date. Run it once on each host. See
  [Synchronize shipped defaults](/how-to/workspace-maintenance/#synchronize-shipped-defaults).
- Required state migrations apply the next time Orbit opens a workspace.
  `orbit migrate` lists the others, and `orbit migrate --confirm` applies
  them.

:::caution
Upgrade every host before any of them opens a shared workspace: a workspace
that a newer build has upgraded refuses older clients. Restart long-lived MCP
servers too.
:::
