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
retired Homebrew formula installed conflicts with the canonical
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
(or Orbit's bundled Bubblewrap, where the host's lacks `--bind-fd`) must pass
its namespace/mount probe; Ubuntu's AppArmor restrictions may require
the packaged narrow Bubblewrap profile. Complete the [Linux sandbox setup](linux-sandbox.md)
before dispatch. Do not disable host protection or enable sandbox fallback to hide
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

## Step 4 — Initialize the workspace

For agent-assisted PR delivery, from the repository root (omit `--mcp` if no client registration is wanted):

```bash
orbit workspace init --base-branch <integration-branch> --ship-mode pr --mcp
```

Fresh registration refuses an origin containing URL credentials before writing
workspace state. Remove credentials from the origin and use Git's credential
helper or SSH authentication instead. Credential-free local origins remain
accepted; discovery redacts remote credentials in older registrations.

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
local merge delivery instead of opening PRs; change it later with
`orbit workspace ship-mode local|pr`. For another host's workspace, use
`--role replica --owner <owner-machine-id>` rather than creating another owner.
See [multi-host.md](multi-host.md).

Three things to know about what it seeded and how to finalize:

- **Only `crew_performance_refresh` is seeded enabled.** Every other routine and
  auto-task ships disabled: the automation layer is installed but dark until
  someone reviews and opts in. `crew_performance_refresh` no-ops while
  `workflow.crew_performance` is `off`, but it still records a job run each hour,
  so a fresh workspace that seeds the default routines schedules it. → [automation.md](automation.md)
- **`.orbit/` is per-user state.** `orbit workspace init` seeds a `.gitignore`
  that ignores the whole of `.orbit/` with no re-includes. Keep it. Seeded
  routines, auto-tasks, and resources come from the binary, not from git.
- **Generated definition files are ignored.** `orbit workspace init` updates
  `.gitignore` and writes files under `.orbit/auto_tasks/` and
  `.orbit/routines/`. Those paths do not dirty the checkout. Orbit
  intentionally does not auto-commit, stash, or discard operator
  modifications. Review the managed `.gitignore` entry; if git still tracks
  an older `.orbit/` tree, `orbit doctor` names `git rm -r --cached .orbit`.

Ordinary `orbit mcp init` installs agent-only authority, unlike the operator
bootstrap above. Re-registering a client is not a way to preserve or grant
operator authority implicitly. For federated setup use `mcp init --federated`
and [remote-access.md](remote-access.md).

Targeting specific clients, or a second pass later:

```bash
orbit mcp init --client claude --client codex        # repeatable
orbit mcp init --all --scope home                    # user-level rather than repo-local
```

Existing operator registrations keep their complete server entry, including
`--operator` and their workspace binding, on both explicit and automatic setup.
To replace one with agent authority, deliberately run `orbit mcp remove` for the
same client and scope, then `orbit mcp init`. New `mcp init` entries use agent
authority; `workspace init --mcp` creates operator registrations.

Every client config write or removal emits an `mcp_config_write` event to
`~/.orbit/state/logs/orbit.jsonl` with the config path (and symlink target),
source caller, process ID, working directory, invocation arguments and Orbit
server arguments. These records use the `orbit::mcp_config` tracing target,
enabled in the default file filter; an explicit `RUST_LOG` overrides that filter.

Supported clients: `claude`, `codex`, `gemini`, `antigravity`, `grok`, `cursor`,
`vscode`, `windsurf`. `--scope workspace` (the default) writes repo-local config;
`--scope home` writes user-level config. The current Google terminal CLI is
`agy` (Antigravity). Gemini CLI remains available for enterprise / API-key
deployments; individual Gemini CLI accounts stopped on 2026-06-18.

### Workspace presets

A preset is a TOML file a distribution ships to set a workspace up in one step.
Apply it with `--preset` when registering the workspace, or with `--force` on a
workspace that is already registered:

```bash
orbit workspace init --base-branch <integration-branch> --ship-mode pr --preset <distribution>.toml
orbit workspace init --force --preset <distribution>.toml   # already registered
```

A preset file has one required key and three optional tables:

```toml
name = "<distribution>"                # required: 1-64 ASCII letters, digits, "-", "_" or "."

[seeding]                              # which shipped defaults this workspace seeds
routines = "none"                      # "all" (the default), "none", or a list of shipped names
auto_tasks = ["<shipped-auto-task>"]

[enable]                               # switch on definitions an enabled plugin already seeded
routines = ["<plugin-routine>"]
auto_tasks = []

[config]                               # workspace keys that `orbit config set` accepts
"<section>.<key>" = "<value>"          # dotted keys or nested tables
```

- **`[seeding]`** sets which shipped routines and auto-tasks are seeded, through
  the `seeding.routines` and `seeding.auto_tasks` keys. A list seeds exactly the
  named shipped defaults, so a later release's additions are not seeded.
- **`[config]`** sets workspace config keys. `seeding.*` keys belong in
  `[seeding]`, not here.
- **`[enable]`** switches on routines and auto-tasks that an enabled plugin
  already seeded into this workspace. It applies after seeding. It never enables
  a shipped default, so `crew_performance_refresh` (seeded enabled) is the only
  shipped default on without an opt-in; every other shipped routine and
  auto-task stays disabled unless someone opts in.

**Validation and limits.** Orbit validates the whole file against this
workspace before it writes anything. Nothing is written or registered when any
of these fails:

- an unknown field in any table;
- a `[config]` key or value that `orbit config set` refuses for this workspace;
- a blank `[enable]` name, or one that no plugin seeded here;
- an `[enable]` definition a plugin seeded whose file is missing or does not load;
- a file larger than 256 KiB.

**No plugin consent.** A preset never grants plugin permissions and never
enables a plugin. Fields named `grant`, `grants`, `permissions`, `plugin`,
`plugins`, `enable_plugins` or `plugin_enablement` are refused at the top level
or under `[enable]`, and so is any `plugin_enablement*` key under `[config]`.
To use `[enable]`, enable the plugin first, then apply the preset:

```bash
orbit plugin enable <plugin>
orbit workspace init --force --preset <distribution>.toml
```

**Verify.** `orbit workspace init` prints the preset's name, its `sha256:`
digest of the file bytes, and any definitions it enabled. Afterwards,
`orbit workspace show` reports the last applied preset, and its JSON output has
a `preset` key. Check the enabled routines and auto-tasks with
`orbit routine list` and `orbit auto-task list`.

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

## Step 6 — Hand off

- **First real task** — route through [task-authoring.md](../../orbit/references/task-authoring.md).
  Create it when requested; setup alone does not authorize unrelated work.
- **Feature tour** — read the README's feature section and summarize against the
  user's stated goal, not generically.

## What to set up next

Offer only additions relevant to the user's goal; none is required merely because
initial setup succeeded:

1. **Crews and base branch** — register the workspace with `--base-branch`
   at the branch task PRs should target (`workflow.base_branch` is only the
   config fallback), and set a default crew. → [configuration.md](configuration.md)
2. **The scheduler** — host clock, then routines, in the documented order.
   → [automation.md](automation.md)
3. **Recurring chores** — QA sweeps, friction curation, anything periodic.
   → [auto-tasks.md](auto-tasks.md)
4. **Task publication** — bind a dedicated snapshot repository and verify one
   publish/inspect cycle before relying on recovery. → [publication.md](publication.md)
5. **Upgrade convergence** — use `orbit workspace sync --check`, then
   `orbit workspace sync` to refresh managed defaults. → [maintenance.md](maintenance.md)
6. **A replica execution host** — collapse to one owner, match binaries and
   protocol, make sure the follower can run the owner's captured reviewer from
   `workflow.purpose_crews.pre_landing_review` when non-empty, otherwise
   `workflow.purpose_crews.review`. Check this when the owner's
   `review.timing` enables before-PR or before-landing review, then probe.
   Installation is not pull enablement.
   → [distributed-drain.md](../../orbit/references/setup/distributed-drain.md)

## Anti-patterns

- Check the installed release and effective resource catalog when commands
  differ from these examples; do not rebuild Orbit just to access a task.
- Don't pick a task prefix casually on a second machine. It cannot be changed.
- Run `orbit init` before creating any task. Tasks created first (for example after
  `orbit workspace init` alone) mint `ORB-` ids, which fixes the prefix: a later
  `orbit init --task-prefix <PREFIX>` is refused before it writes anything.
- Don't enable every seeded routine at once. Enable worktree GC before, not
  after, scheduling ship traffic.
- Task search uses the local SQLite index; no model download is needed.
