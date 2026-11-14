---
title: Agents and crews
description: "Which provider CLIs Orbit can run, how to write a crew for one, and how each run is sandboxed."
---

Orbit runs an agent by spawning its provider's CLI. Use this page to pick a
provider, write a crew for it in `config.toml`, and see what confines the run.

## Runtime path

Orbit spawns the official provider CLI as a supervised subprocess, bounded by
the activity's filesystem profile (`fsProfile`) and tool policy. The CLI talks
to its provider. There is no other execution path: the old `backend` selector
is retired; see
[Retired backend selection](/reference/config/#retired-backend-selection).

## Providers

A **provider** is an agent family id, such as `claude` or `codex`. An
**executor** is the shipped recipe Orbit uses to spawn that family's CLI: its
command, static flags, output format and sandbox backend.

| Provider | Executor command | Reasoning `effort` | Status |
|---|---|---|---|
| `claude` | `claude` | `low`–`max`, via `--effort` | Active |
| `codex` | `codex exec` | `low`–`max`, via `--config model_reasoning_effort` | Active |
| `antigravity` | `agy` | `low`, `medium`, `high`, via `--effort` | Active |
| `grok` | `grok` | Model-specific, via `--reasoning-effort` | Active |
| `copilot` | `copilot` | Not supported | Active |
| `muse` | `muse exec` | `low`–`max`, via `--reasoning-effort` | Active on macOS and Linux |
| `gemini` | `gemini` | Not supported | Legacy |
| `ollama` | — | Not supported | No shipped crew or executor |
| `openai_compat` | — | Not supported | No CLI runtime; dispatch fails |

A crew that names `ollama` or `openai_compat` loads, but dispatch refuses it.
Orbit never re-points the run at another family.

Orbit also ships a **`local-shell`** executor, which is not a provider. It
runs the deterministic commands of `local_shell` steps, carries no model or
prompt, and no crew names it.

### Deprecated aliases

These legacy vendor spellings still resolve, each with a deprecation warning:

| Alias | Resolves to |
|---|---|
| `anthropic` | `claude` |
| `openai`, `chatgpt` | `codex` |
| `google` | `gemini` |
| `xai` | `grok` |

`openai-compat` is an accepted spelling of `openai_compat`. Any other string
is rejected as an unknown provider.

### Retired providers

`cursor`, `pi` and `opencode` are retired. A crew that names one fails to
load, and `orbit doctor` reports it; pick a supported provider instead.
Stored records that name them still display. The Cursor editor MCP client
(`orbit mcp init --cursor`) is a separate surface and still works.

## Set up an executor

:::tip[Let your agent set it up]
Ask your agent to add a provider or crew. The `orbit-setup` skill handles
provider and crew configuration.
:::

To set one up by hand:

1. Install the provider's CLI so its command is on `PATH`, and sign in to it
   with the vendor's instructions. `copilot` is the npm package
   `@github/copilot`.
2. Add a crew for it to `config.toml` and, if you want it as the fallback,
   name it in `default_crew`.

The starter crews below are examples, one per executor. Copy the ones you
need. Orbit passes `model` to the CLI verbatim; whether your account can run
it is up to the provider.

```toml
[workflow]
default_crew = "opus"

[crews.opus]
provider = "claude"
model = "opus"
effort = "high"              # optional: low, medium, high, xhigh, max

[crews.sol]
provider = "codex"
model = "gpt-6.1-sol"        # effort: low, medium, high, xhigh, max

[crews.muse-spark]
provider = "muse"
model = "muse-spark-1.3"     # effort: low, medium, high, xhigh, max

[crews.gemini-flash]
provider = "antigravity"
model = "gemini-3.8-flash-high"  # effort: low, medium, high

[crews.grok]
provider = "grok"
model = "grok-4.7"           # effort: low, medium, high, xhigh

[crews.copilot]
provider = "copilot"
model = "claude-sonnet-5"    # no effort

[crews.gemini]
provider = "gemini"          # legacy lane
model = "gemini-3.8-flash"   # no effort
```

Omit `effort` to keep the model's default; Orbit then passes no effort
argument. What to know per executor:

| Executor | Notes |
|---|---|
| `codex` | Sandbox mode and approval policy are separate settings: [`execution.codex.*`](/reference/config/#settable-keys). |
| `muse` | Runs on macOS and Linux. |
| `antigravity` | Model ids carry an effort suffix and must come from `agy models`; a bare Gemini CLI id such as `gemini-3.8-flash` is rejected at config load. `xhigh` and `max` are not accepted, although `agy --effort` advertises them. When `agy` is detected, `orbit init` also seeds an `argon` crew (`gemini-4-argon-high`) into the `hard` complexity pool. |
| `grok` | Effort depends on the model: `grok-4.7` and `grok-4.6` accept `low`–`xhigh`, `grok-4.5` accepts `low`–`high`. With any other model, or none, `effort` is ignored. |
| `copilot` | The retired `gh-copilot` gh extension is a different tool and is not an Orbit executor. |
| `gemini` | Legacy Google lane. Individual Gemini CLI accounts stopped on 2026-06-18; enterprise Gemini Code Assist and API-key authentication still work. For new setups use `antigravity`, which `orbit init` picks when `agy` is installed. |

To pass credentials, list environment variable names (never secret values) in
`config.toml`, for example in a global crew's `env` list.

Before your first dispatch you also need, on Linux, a working sandbox. See
[Install Orbit](/getting-started/install/#prerequisites).

## Crews

A **crew** is a named provider and model, with an optional reasoning
`effort`. The selected crew overrides an activity's own provider, model and
effort.

```toml
[crews.sol]
provider = "codex"
model = "gpt-6.1-sol"
effort = "high"
```

To pick a task's crew yourself:

```bash
orbit task update <id> --crew <name>
```

### How a crew is chosen

A run resolves its crew at dispatch, first match wins:

1. An explicit crew on the activity input.
2. The task's `crew` field.
3. `[workflow] default_crew`.

A task created without a crew gets one at creation: a draw from the pool for
its purpose when one is configured, otherwise from the pool for its complexity
(`workflow.<complexity>_complexity_crews`), otherwise `default_crew`. The
task's history records which. Automatic admission can redraw a pool
assignment when a provider limit excludes the crew; a crew you set afterwards
is kept.

System activities have no crew of their own. The task pilot draws from
`workflow.purpose_crews.task_pilot`, falling back to the `low` complexity
pool. Step-failure and PR conflict recovery draw from
`workflow.purpose_crews.failure_recovery`, falling back to `medium`, and never
inherit the failed task's crew. See
[Purpose pools](/reference/config/#purpose-pools).

To tune the draws, see [Configuration](/reference/config/#crews):

- `workflow.crew_selection = "usage_pressure"` favours crews whose weekly
  allowance would otherwise go unused.
- `workflow.task_types` with `workflow.type_complexity_crews` gives a kind of
  work its own pool per tier.

Compare how crews do with `orbit agents scorecard --role implementers --by-type`,
or rank implementer models with `orbit agents rank`.

### Effort

`effort` is checked against the provider and model when the config loads. An
unsupported value is ignored for that crew with a warning, and `orbit doctor`
lists it. Orbit never substitutes the nearest supported value, and
`orbit config set` refuses a value that load would drop. The per-provider
sets are in [Configuration](/reference/config/#reasoning-effort).

### Crews with their own account

A crew with an `env` list has its own provider account for usage limits,
holds and budgets. Crews without one share the provider's default account.
Allowlisting a crew on the default account does not also allow an
environment-backed crew.

## Tool policy

An `agent_loop` activity sets which Orbit tools its agent may call, in one of
two modes:

- **Allowlist** (`tools`): only the listed tools are callable.
- **Deny** (`tool_disallow_list`): every registered agent-facing tool is
  callable except the listed ones. The shipped agent activities use this mode.

```yaml
spec:
  type: agent_loop
  tools:
    - orbit.task.show
    - orbit.search
```

Declare one or the other. An empty `tools:` with no `tool_disallow_list` is
deprecated and loads with a warning.

Orbit's MCP server and `orbit tool run` refuse a call outside the policy, and
each run records its effective tool list in the audit trail. The provider
CLI's own built-in tools stay with that CLI, inside the OS sandbox.

## Platform support

Orbit wraps each spawned agent CLI in an OS sandbox scoped by the activity's
filesystem profile. `orbit init` writes the right backend for your host into
the shipped executors.

| Host | Sandbox | What it does |
|---|---|---|
| macOS | `sandbox-exec` | Applies the profile. If `sandbox-exec` is missing, dispatch fails unless the executor sets `allow_fallback: true`. If it exists but cannot apply the profile (exit 71, `sandbox_apply: Operation not permitted`), the run fails and no fallback applies. |
| Linux | Bubblewrap (`bwrap`) | Confines writes to the profile and hides well-known credential locations such as `~/.ssh` and `~/.aws`. Other host reads and network access stay open. If `bwrap` is missing or its probe fails, dispatch fails unless the executor sets `allow_fallback: true`. |
| Other | None | Release binaries ship for macOS and Linux only (x64 and arm64). A source build elsewhere seeds its executors with no OS sandbox; process supervision and tool policy still apply. |

The `local-shell` executor declares no sandbox on any platform. For the Linux
prerequisites, see
[Install Orbit](/getting-started/install/#prepare-the-sandbox).

To change a shipped executor's sandbox, fallback or the hosts it may run on,
edit its machine-level YAML, not the crew. See
[Executor YAML](/reference/executor-yaml/).
