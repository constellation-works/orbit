# Setting Orbit up: initialize the workspace

## Contents

- [Step 4 — Initialize the workspace](#step-4--initialize-the-workspace)
  - [Grok MCP registration](#grok-mcp-registration)
  - [Workspace presets](#workspace-presets)

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
(`orbit.workflow` action `ship`), observe/resume runs, and run `orbit.command.exec`.

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
call instead of silently selecting another crew.

Because a connection outlives
a model switch in the client, pass `orchestrator` per call or restart the
session when the orchestrating crew changes.

Choose the real integration branch; do not assume the product default `main`
is the repository's landing branch. `--ship-mode local` selects worktree-based
local merge delivery instead of opening PRs; change it later with
`orbit workspace ship-mode local|pr`. For another host's workspace, use
`--role replica --owner <owner-machine-id>` rather than creating another owner.
See [multi-host.md](multi-host.md).

Three things to know about what it seeded and how to finalize:

- **Three shipped routines are seeded enabled when selected by the seeding
  policy:** `worktree-gc`, `task-pilot` and one dormant experiment routine
  that mints nothing unless a running experiment has a memory arm. The
  `host-health` auto-task is seeded disabled, like every other shipped
  auto-task, until someone opts in with `orbit auto-task toggle host-health on`. A host clock is still needed
  for unattended runs.

  When enabled, the `host-health` auto-task files a weekly host-health review on the workspace
  owner once the host clock is installed; `orbit auto-task toggle host-health
  off` turns it off, and `orbit routine disable <routine-name>` turns a routine
  off (operator only). → [automation-scheduler.md](automation-scheduler.md), [auto-tasks-authoring.md](auto-tasks-authoring.md)
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
and [remote-access-mcp.md](remote-access-mcp.md).

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

### Grok MCP registration

Grok normally loads Orbit's MCP server from the project `.mcp.json` shared with
Claude Code. `orbit workspace init --mcp` auto-detects installed clients and
writes the project registration; to target Grok explicitly, run
`orbit mcp init --scope workspace --grok`. For a home registration, run
`orbit mcp init --scope home --grok`; the shared Claude-compatible home file is
`~/.claude.json`.

Re-running Orbit MCP initialization migrates an older Orbit-generated Grok
entry out of `.grok/config.toml`, preserving other Grok settings and servers.
When `[claude_compat] imported = true` in `~/.grok/config.toml`, Grok ignores
the project `.mcp.json`, so initialization keeps the native `.grok/config.toml`
target. For home scope, initialization also keeps that native target when
`[compat.claude]` has `mcps = false` or `GROK_CLAUDE_MCPS_ENABLED=0`.

Run `grok inspect` to see which configuration source Grok loaded. The project
registration is shared: removing Orbit's entry through Claude Code or Grok
removes it for both clients. Gemini CLI is the neighboring client and uses
`.gemini/settings.json`.

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
  a shipped default. The three shipped routines seeded enabled—`worktree-gc`,
  `task-pilot` and the dormant experiment routine—are on without an opt-in only when the seeding policy selects them.
  The `host-health` auto-task seeds disabled.
  The experiment routine still mints nothing unless a running experiment
  has a memory arm.

  Every other
  shipped routine and auto-task stays disabled unless someone opts in.

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

Continue with [Step 5 — Verify](first-run-verify.md#step-5--verify).
