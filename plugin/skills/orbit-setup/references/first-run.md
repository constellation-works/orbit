# Setting Orbit up

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
retired `danieljhkim/tap/orbit` formula installed conflicts with the canonical
one — `orbit update` on that machine detects it and reports the exact
migration sequence (uninstall the legacy formula, then install the canonical
one) instead of an ambiguous `brew upgrade orbit`; do not improvise a
different migration. Agent plugins provide the MCP
integration and bundled skill through their own package distribution, and do
not require a source checkout. Source builds are optional for customization;
follow the published README for toolchain and build instructions if that is
what the user requested. Respect the user's install destination and existing
installation; ask only for missing choices or required host permissions.

Only when agent execution is requested, verify an authenticated supported agent CLI on the execution
host. PR mode also needs an authenticated `gh` client. On Linux, `/usr/bin/bwrap`
must pass its namespace/mount probe; Ubuntu's AppArmor restrictions may require
the packaged narrow Bubblewrap profile. Complete the [Linux sandbox setup](linux-sandbox.md)
before dispatch. Do not disable host protection or enable sandbox fallback to hide
a failed probe.

To add a provider or deterministic executor to Orbit itself, work in a source
checkout and follow the [executor onboarding runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/executor-onboarding.md).
Do not change a production login or workspace configuration merely to test a
new executor.

## Step 3 — Initialize the machine

```bash
orbit init
```

This creates `~/.orbit/` and, on a fresh machine, establishes host identity. Two
values are asked for and both matter:

**Host name** — how this machine is named in run ownership, task claims, and
Orbit's own output. Defaults to the OS hostname. Renameable later with
`orbit host rename`.

**Task prefix** — 2–5 uppercase ASCII letters that namespace every task ID this
machine allocates. **Chosen once and never changed.** Its whole purpose is to
keep two machines that share a repository from allocating the same ID, so on a
second machine pick a *different* prefix — see [multi-host.md](multi-host.md)
before initializing one.

Non-interactive runs must pass both explicitly:

```bash
orbit init --non-interactive --host-name <name> --task-prefix <PREFIX>
```

## Step 4 — Initialize the workspace

For agent-assisted PR delivery, from the repository root (omit `--mcp` if no client registration is wanted):

```bash
orbit workspace init --base-branch <integration-branch> --ship-mode pr --mcp
```

This registers the checkout, creates `.orbit/`, and seeds the default skills,
jobs, activities, policies, routines, and auto-task definitions. `--mcp`
auto-detects installed agent clients and registers Orbit's MCP server with
them as `orbit mcp serve --operator --workspace <ws_id>` — **operator
authority**, so the registered integration can dispatch workflows
(`orbit.workflow.ship`), observe/resume runs, and run `orbit.command.exec`.
This is the deliberate bootstrap path for a human-facing orchestrator; bare
`orbit mcp serve` and any worker/agent-launched MCP session stay agent-only.
`--workspace` binds the session to the workspace being registered, so
workspace-scoped tools route without an explicit selector on every call —
most MCP clients cannot announce one at initialize.

`orbit mcp serve --orchestrator <crew>` binds a second session default, this
one purely descriptive: tasks the session creates are attributed to that crew
unless the call passes its own `orchestrator`. It is independent of
`--operator` and grants nothing, does not select the crew or model a task
executes under, and never rewrites an existing task. The crew is resolved
against the workspace the call lands in, so an unconfigured name fails that
call instead of silently selecting another crew. Because a connection outlives
a model switch in the client, pass `orchestrator` per call or restart the
session when the orchestrating crew changes.

Choose the real integration branch; do not assume the product default `main`
is the repository's landing branch. `--ship-mode local` selects worktree-based
local merge delivery instead of opening PRs. For another host's workspace, use
`--role replica --owner <owner-machine-id>` rather than creating another owner.
See [multi-host.md](multi-host.md).

Three things to know about what it seeded and how to finalize:

- **Every routine and auto-task ships disabled.** The automation layer is
  installed but dark until someone reviews and opts in. Do not assume a fresh
  workspace schedules anything. → [automation.md](automation.md)
- **Definitions belong in git; state does not.** `orbit workspace init` seeds a
  `.gitignore` pattern that ignores `.orbit/` and then re-includes the versioned
  definition directories. Keep it.
- **Finalize generated files before local shipping.** `orbit workspace init`
  updates `.gitignore` and creates untracked definitions in `.orbit/auto_tasks/`
  and `.orbit/routines/`. Orbit intentionally does not auto-commit, stash, or
  discard operator modifications. When using local delivery (`--ship-mode local`),
  the landing base checkout must be clean before running workflows. Review and
  commit the generated onboarding files to finalize setup safely:

  ```bash
  git add .gitignore .orbit/auto_tasks .orbit/routines
  git commit -m "chore: initialize Orbit workspace definitions"
  ```

Ordinary `orbit mcp init` installs agent-only authority, unlike the operator
bootstrap above. Re-registering a client is not a way to preserve or grant
operator authority implicitly. For federated setup use `mcp init --federated`
and [remote-access.md](remote-access.md).

Targeting specific clients, or a second pass later:

```bash
orbit mcp init --client claude --client codex        # repeatable
orbit mcp init --all --scope home                    # user-level rather than repo-local
```

Supported clients: `claude`, `codex`, `gemini`, `antigravity`, `grok`, `cursor`,
`vscode`, `windsurf`. `--scope workspace` (the default) writes repo-local config;
`--scope home` writes user-level config. The current Google terminal CLI is
`agy` (Antigravity). Gemini CLI remains available for enterprise / API-key
deployments; individual Gemini CLI accounts stopped on 2026-06-18.

## Step 5 — Verify

```bash
orbit --version
orbit workspace show
orbit tool run orbit.task.list --input '{"model":"<agent-family>","workspace":"<workspace-id>"}'
orbit doctor
```

`orbit doctor` is the real check — it inspects config, database, disk, indexes,
locks, and runs. Report its output. Distinguish failures affecting the requested workflow from optional capabilities
not configured. Resolve relevant failures or report the specific blocker; a
passing `--version` alone is not sufficient.

If semantic search is requested, its local companion requires a model download.
Use existing authorization or confirm this addition before downloading:

```bash
orbit semantic install
orbit semantic stats
```

Lexical search works without it. → [search.md](../../orbit/references/search.md)

## Step 6 — Hand off

- **First real task** — route through [task-authoring.md](../../orbit/references/task-authoring.md).
  Create it when requested; setup alone does not authorize unrelated work.
- **Feature tour** — read the README's feature section and summarize against the
  user's stated goal, not generically.

## What to set up next

Offer only additions relevant to the user's goal; none is required merely because
initial setup succeeded:

1. **A docs corpus** — register the markdown the repo already has, so agents
   retrieve by concept instead of filename. → [docs-corpus.md](../../orbit/references/docs-corpus.md)
2. **Crews and base branch** — point `workflow.base_branch` at the branch task
   PRs should target, and set a default crew. → [configuration.md](configuration.md)
3. **The scheduler** — host clock, then routines, in the documented order.
   → [automation.md](automation.md)
4. **Recurring chores** — QA sweeps, friction curation, anything periodic.
   → [auto-tasks.md](auto-tasks.md)
5. **Task publication** — bind a dedicated snapshot repository and verify one
   publish/inspect cycle before relying on recovery. → [publication.md](publication.md)
6. **Upgrade convergence** — use `orbit workspace sync --check`, then
   `orbit workspace sync` to refresh managed defaults. → [maintenance.md](maintenance.md)

## Anti-patterns

- Check the installed release and effective resource catalog when commands
  differ from these examples; do not rebuild Orbit just to access a task.
- Don't pick a task prefix casually on a second machine. It cannot be changed.
- Don't enable every seeded routine at once. Enable worktree GC before, not
  after, scheduling ship traffic.
- A semantic model download needs authorization; lexical search remains available without it.
