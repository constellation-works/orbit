# Setting Orbit up: detect, install and initialize the machine

Get to a workspace supporting the user's requested workflow. Task tracking and
search can stand alone; configure execution only when wanted. Use
this when `.orbit/` is absent, when the user is still deciding whether to adopt
Orbit, or when a second machine or repository needs onboarding.

This reference works without an Orbit source checkout. For release-specific
installation details, consult the [published README](https://github.com/constellation-works/orbit#quick-start)
and compare the version with `orbit --version`. Do not assume a newer website
or a locally modified resource catalog matches the installed binary.

## Step 1 — Detect current state

Inspect the requested machine/repository; reuse existing setup rather than reinstalling:

```bash
command -v orbit          # is the binary on PATH?
test -d .orbit            # is this workspace initialized?
test -d ~/.orbit          # is this machine initialized?
```

Use these results and the user's goal to choose the needed steps. Skip installation
when the existing executable is suitable; skip MCP registration if no agent client is wanted.

## Step 2 — Install

A prebuilt CLI gives you setup, dashboard, and administration commands:

```bash
curl -sSf https://raw.githubusercontent.com/constellation-works/orbit/main/install.sh | sh
# Alternative when Homebrew is the chosen package manager:
brew install constellation-works/tap/orbit
```

Choose one installation method; do not run both. A machine that still has the
retired Homebrew formula installed conflicts with the canonical
one — `orbit update` on that machine detects it and reports the exact
migration sequence (uninstall the legacy formula, then install the canonical
one) instead of an ambiguous `brew upgrade orbit`; do not improvise a
different migration.

Agent plugins provide the MCP
integration and bundled skill through their own package distribution, and do
not require a source checkout. Source builds are optional for customization;
follow the published README for toolchain and build instructions if that is
what the user requested. Respect the user's install destination and existing
installation; ask only for missing choices or required host permissions.

Only when agent execution is requested, verify an authenticated supported agent CLI on the execution
host. PR mode also needs an authenticated `gh` client. On Linux, `/usr/bin/bwrap`
(or Orbit's bundled Bubblewrap, where the host's lacks `--bind-fd`) must pass
its namespace/mount probe; Ubuntu's AppArmor restrictions may require
the packaged narrow Bubblewrap profile. Complete the [Linux sandbox setup](linux-sandbox.md)
before dispatch.

Do not disable host protection or enable sandbox fallback to hide
a failed probe. On Windows, install and run everything inside a WSL2
distribution: follow [windows-wsl2.md](windows-wsl2.md) alongside these steps.

To add a provider or deterministic executor to Orbit itself, work in a source
checkout and follow the [executor onboarding runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/executor-onboarding.md).
Do not change a production login or workspace configuration merely to test a
new executor.

## Step 3 — Initialize the machine

```bash
orbit init
```

This creates `~/.orbit/` and, on a fresh machine, writes the `[machine]`
identity into the global `config.toml`. Two values are asked for and both
matter:

**Machine name** — how this machine is named in run ownership, task claims, and
Orbit's own output. Defaults to the OS hostname. Renameable later with
`orbit config set --global machine.name <new-name>`.

**Task prefix** — 2–5 uppercase ASCII letters that namespace every task ID this
machine allocates. **Chosen once and never changed.** Its whole purpose is to
keep two machines that share a repository from allocating the same ID, so on a
second machine pick a *different* prefix — see [multi-host.md](multi-host.md)
before initializing one.

Non-interactive runs must pass both explicitly:

```bash
orbit init --non-interactive --machine-name <name> --task-prefix <PREFIX>
```

Init also seeds the operator's human crew (`[crews.<name>] kind = "human"`) for work Orbit tracks but never dispatches. It uses the OS user name, offered at a prompt and taken as-is non-interactively; pass `--human-name <name>` to choose another. A re-run keeps an existing one.

Continue with [Step 4 — Initialize the workspace](first-run-workspace.md#step-4--initialize-the-workspace).
