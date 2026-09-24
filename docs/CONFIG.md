---
type: context
summary: Orbit Configuration
last_validated: 2026-09-23
---

# Orbit Configuration

Operator reference for Orbit's `config.toml`: every fixed key, its default, and what it does. `orbit config keys` prints the same key list with descriptions. The annotated template `orbit init` writes is [`crates/orbit-config/assets/default-config.toml`](../crates/orbit-config/assets/default-config.toml); treat it as a reference, not a file to copy wholesale. A shorter overview lives on the website under [Reference › Configuration](https://orbit-cli.com/reference/config/).

To add a new execution lane rather than configure a shipped one, see the [executor onboarding runbook](runbooks/executor-onboarding.md).

## Where config lives

| Path | Scope | Created by |
|---|---|---|
| `<workspace>/.orbit/config.toml` | Workspace-local (per user, gitignored) | Hand-authored, or `orbit config set --fresh` / `--seed-from-global` |
| `~/.orbit/config.toml` | Global | `orbit init` (only when absent, or under `--force`) |

Ordinary settings inherit per key: workspace values override global values, global values fill omissions, and built-in defaults fill remaining gaps.

- **Tables** layer down to individual settings. **Scalars and arrays** replace the matching global value.
- **Named crews** layer by crew name and field, so `[crews.sol]` with only `model = "gpt-5.6-terra"` in the workspace file overrides one field of the global `sol` crew.
- **Security exceptions.** `execution.codex.sandbox`, `execution.codex.approval_policy` and `execution.env.pass` never inherit from global once a workspace file exists. If the workspace file omits one, its built-in default applies.
- **Global-only.** A `[machine]` table in a workspace file is refused at load, naming the file.

The workspace identity file `.orbit/config.yaml` (it stores `workspace_id`) is a separate artifact and not runtime config.

## Inspecting and editing

| Command | What it does |
|---|---|
| `orbit config show` | Effective merged view, grouped as Machine, Delivery (`workflow.*`), Crews, Execution, Review (`operation.*`), Housekeeping and Paths. `--all` expands sections whose keys are all unset. `--json` adds a `provenance` object. |
| `orbit config get <key>` | One value. |
| `orbit config set <key> <value>` | Write the workspace file (`--global` for the global one). The value is parsed as a TOML literal, falling back to a string. If the workspace file does not exist, pass `--fresh` (start empty) or `--seed-from-global`. |
| `orbit config keys` | Every fixed settable key with type, section, and description. |
| `orbit config path` | The resolved `config.toml` path. |

Each value in `show` reports one state: `workspace`, `global` or `environment` (the layer that set it), `default` (built-in value in force), or `unset` (no value and no default). When a lower layer also defines the key, the row says so: `(overrides global: main)`, or `(global sets danger-full-access — not inherited)` for a security key the workspace file did not restate. A registered checkout also gets a `Workspace` line with the registry's base branch and ship mode, which is what delivery uses.

In `--json`, each key's provenance carries `scope`, `path`, `section`, `description`, `state` (`set`/`default`/`unset`) and `shadowed_by` (`[{layer, value, reason}]`, reason `overridden`, `not-inherited` or `preset-reset`). A top-level `workspace_binding` reports the registered base branch and ship mode, or `null`.

`--scope global` or `--scope workspace` resolves one file in isolation, still filling built-in defaults for omitted keys. In scoped `config get --json`, `exists` says whether the key is present in that file. In scoped `config show --json`, `source.exists` says whether the file exists.

---

## `[machine]` — who this machine is

Global-only. `orbit init` writes the identity keys once.

```toml
[machine]
id          = "hm_9ca6004473492f06"
name        = "dk-server-1"
task_prefix = "ORB"
```

| Key | Default | Settable | What it is |
|---|---|---|---|
| `machine.id` | written by init | No | Opaque `hm_…` identity. It names run ownership, workspace ownership and federated routing. |
| `machine.name` | written by init | Yes (`--global`) | Display label. |
| `machine.task_prefix` | written by init | No | 2–5 uppercase ASCII letters used for task IDs minted here. Fixed for the life of the local task store. |
| `machine.worker_containment` | `true` | Yes | Run each detached pipeline worker in its own transient systemd user scope (`orbit-worker-<run_id>-<nonce>.scope`), so a runaway run is throttled or OOM-killed inside it. Without a user manager (macOS, containers) or with `false`, workers run in the caller's cgroup and each Orbit process logs one warning. |
| `machine.worker_memory_high` | `40%` | Yes | Scope `MemoryHigh=` (throttle point). Bytes with optional `K`/`M`/`G`/`T`, a percentage of physical RAM, or `infinity`. |
| `machine.worker_memory_max` | `50%` | Yes | Scope `MemoryMax=` (OOM point). Same grammar. |
| `machine.worker_tasks_max` | `4096` | Yes | Scope `TasksMax=` (processes plus threads), at least 1. |

- `orbit config set` refuses `machine.id` and `machine.task_prefix`: changing either would orphan or renumber records minted under it.
- Hand edits fail closed. A `[machine]` table missing any identity key is an error. A `task_prefix` that contradicts the local task allocator, or an `id` that contradicts a workspace record naming this machine as owner, is refused. Nothing falls back to the hostname.
- A legacy `~/.orbit/host.toml` is folded into `[machine]` on first load and removed. If both exist and disagree, Orbit refuses to start and names both paths. Delete the stale one.
- A run that fails after hitting a worker limit carries error code `worker_resource_limit` in `orbit run show`. Inspecting scopes: [operational logs › Worker Resource Containment](../crates/orbit-core/assets/skills/orbit-setup/references/operational-logs.md#worker-resource-containment).

---

## `[workflow]` — branch and crew defaults

```toml
[workflow]
base_branch = "main"
default_crew = "opus"
system_crew = "luna"
low_complexity_crews = []
medium_complexity_crews = []
hard_complexity_crews = []
xhard_complexity_crews = []
```

| Key | Default | What it does |
|---|---|---|
| `workflow.base_branch` | `main` | Fallback base branch for ship, auto and pilot when the workspace registry has none. The registry value (`orbit workspace show`) wins, and `--base <branch>` overrides both. For a two-branch repo, register with `--base-branch agent-main`. |
| `workflow.default_crew` | see [resolution](#resolution-precedence) | Crew for a task with no `crew`. Must name a defined crew. |
| `workflow.system_crew` | `system` | Crew for runtime-synthesized system work such as step-failure recovery. |
| `workflow.low_complexity_crews`, `medium_…`, `hard_…`, `xhard_…` | `[]` | Crew pools a crew-less task draws from at creation, by complexity. Empty means "use `default_crew`". See [pools](#automatic-crew-pools-by-complexity). |
| `workflow.auto_ship` | `false` | Opt in to unattended ship dispatch from the sweep/routine scheduler. While `false`, the ship sweep skips with `auto_ship_disabled`. |
| `workflow.required_validation_commands` | `[]` | Commands a distributed-drain claim must pass on its exact candidate before this owner accepts its handoff. Empty refuses every claimed handoff. |

**The `system` name.** Shipped job steps such as `task_pilot_pipeline` name `crew: system` directly. At load that name is aliased onto the crew `workflow.system_crew` names, so `system_crew = "luna"` runs the task pilot on Luna. A user-authored `[crews.system]` table wins over the alias. Older configs without `system_crew` fall back to an existing `[crews.qa]`, then to the default crew. An unknown custom name is not substituted and fails at dispatch. A missing or unusable system crew leaves the original failed step failed, with a diagnostic naming `workflow.system_crew`.

**What `orbit init` seeds.** Only the global file, and only when it is absent (or under `--force`):

- the [built-in crews](#crewsname--which-provider-model-runs-the-task) for each detected provider CLI,
- `default_crew` set to the default crew of the first detected family in preference order,
- `system_crew` set to the first detected of `luna`, `sonnet`, `grok`, `antigravity`, `gemini`, `copilot`, `cursor`, `pi`, `opencode` (cheapest tier first),
- all four pools as `[]`.

Interactive init asks for both crews by name, and skips the question when there is only one candidate. With no supported CLI detected, it writes an empty `[crews]` table (so the built-in crews are not used) and leaves both keys unset. Init never writes `[crews.system]`, `[crews.custom]` or `[crews.qa]`.

---

## `[crews.<name>]` — which provider-model runs the task

A crew is one provider-model assignment. An activity uses the crew named in its rendered input, otherwise the run's resolved crew.

| Field | Required | Values |
|---|---|---|
| `provider` | Yes | `claude`, `codex`, `antigravity`, `gemini`, `grok`, `copilot`, `cursor`, `pi`, `opencode`. See [provider identity](#provider-identity-and-resolution). |
| `model` | Yes | Model ID passed to the provider CLI. |
| `effort` | No | Reasoning effort; see the table below. Omitted leaves the provider's default. |
| `description` | No | Summary. Trimmed; blank becomes absent. |
| `tags` | No | Discovery labels. Trimmed, blanks dropped, sorted and deduplicated. |

```toml
[crews.sol]
model = "gpt-6-sol"
provider = "codex"
effort = "high"
description = "Systems implementation"
tags = ["implementation", "review"]
```

**Built-in crews.** `orbit init` seeds a family's crews when it detects that family's binary. A config with no `[crews]` table at all uses the full set, plus a built-in `system` crew (`claude`, `sonnet`). Families are listed in init's preference order. Existing explicit model pins are kept as written.

| Family | Binary | Crews (model) | Default crew |
|---|---|---|---|
| `claude` | `claude` | `opus` (`opus`), `sonnet` (`sonnet`), `fable` (`fable`) | `opus` |
| `codex` | `codex` | `astra` (`gpt-6-astra`), `sol` (`gpt-6-sol`), `terra` (`gpt-5.6-terra`), `luna` (`gpt-6-luna`) | `astra` |
| `antigravity` | `agy` | `antigravity` (`gemini-3.8-flash-high`) | `antigravity` |
| `gemini` | `gemini` | `gemini` (`gemini-3.8-flash`) | `gemini` |
| `grok` | `grok` | `grok` (`grok-4.7`) | `grok` |
| `copilot` | `copilot` | `copilot` (`claude-sonnet-5`) | `copilot` |
| `cursor` | `cursor-agent` | `cursor` (`gpt-5`) | `cursor` |
| `pi` | `pi` | `pi` (`sonnet`) | `pi` |
| `opencode` | `opencode` | `opencode` (`anthropic/claude-sonnet-4-5`) | `opencode` |

**Reasoning effort.** Choose capability with the model first, then tune `effort` inside it.

| Provider | Accepted `effort` | Rendered as |
|---|---|---|
| `claude`, `codex`, `pi` | `low`, `medium`, `high`, `xhigh`, `max` | `--effort` · `model_reasoning_effort` · `--thinking` |
| `antigravity` | `low`, `medium`, `high` | `--effort` |
| `opencode` | `high`, `max` | `--variant` |
| `grok` (`grok-4.7`, `grok-4.6`) | `low`, `medium`, `high`, `xhigh` | `--reasoning-effort` |
| `grok` (`grok-4.5`) | `low`, `medium`, `high` | `--reasoning-effort` |
| others | none | — |

An invalid or unsupported `effort` (for example `max` on Grok, or `effort = "hard"`) is ignored for that crew with a warning naming the file, crew, value and accepted values. It is never remapped. `orbit doctor` lists ignored properties, and `orbit config set` refuses to write a value load would drop. A selected crew's provider, model and effort all override an activity's inline baseline.

**Editing crews.** Crew fields are addressable as `crews.<name>.<field>`:

```bash
orbit config set crews.sol.effort high
orbit config get crews.sol.effort
```

`config set` refuses invalid values, unsupported provider/model combinations and misspelled fields before writing. It cannot create a crew: add a `[crews.<name>]` table with `model` and `provider` first. `orbit.crew.list` returns the normalized crews of the selected checkout's effective config.

**Validation.**

- `model` and `provider` must be non-empty.
- `default_crew` must name a defined crew. If you define crews but leave `default_crew` unset, Orbit uses `opus` (or a legacy `claude` crew) when defined, and otherwise refuses to load.
- A crew name may not contain `:`, because the pool grammar reserves it. See [repair](#repairing-a-config-that-already-names-a-crew-with-a-colon).
- An Antigravity crew must use an `agy models` slug. A bare Gemini CLI ID such as `gemini-3.8-flash` fails with migration guidance.

**Retired crew shapes.** These fail load with migration guidance:

- `planner` / `implementer` / `reviewer` sub-tables: rewrite as flat `model` and `provider`.
- `backend`: `cli` is accepted and ignored, while `http` and `auto` are refused. The same applies to `ORBIT_BACKEND` and `[runtime] backend`. Remove the key.
- `[agent.<role>]` tables: migrate to `[crews.<name>]` plus `workflow.default_crew`.

### Repairing a config that already names a crew with a colon

A colon-named crew makes every command refuse to load, including `orbit config set`, so fix the file by hand. The error names the file and table:

```
error: invalid input: [crews]: crew name 'gpt-5:codex' must not contain ':'; ...
Edit '~/.orbit/config.toml' and rename or remove the [crews."gpt-5:codex"] table, then rerun the command
```

Rename the table to a colon-free name (or delete it), update anything that referenced the old name (`default_crew`, `system_crew`, pool entries, a task's `crew`), and rerun. Each layer is checked before merging, so the message names the file that actually defines the crew.

---

## Provider identity and resolution

Every `provider` string, whether in a crew, an activity's inline `provider`, or setup detection, goes through one canonical parser.

### Canonical providers

| ID | CLI binary | Notes |
|---|---|---|
| `claude` | `claude` | |
| `codex` | `codex` | |
| `gemini` | `gemini` | Legacy Gemini CLI (enterprise / API-key deployments). |
| `grok` | `grok` | |
| `copilot` | `copilot` | [GitHub Copilot CLI](#github-copilot-cli) |
| `cursor` | `cursor-agent` | [Cursor Agent CLI](#cursor-agent-cli) |
| `pi` | `pi` | [Pi CLI](#pi-cli) |
| `antigravity` | `agy` | [Antigravity CLI](#antigravity-cli) |
| `opencode` | `opencode` | [OpenCode CLI](#opencode-cli) |
| `ollama` | — | Recognized but unsupported at the Orbit CLI entry point. Selecting it fails with `provider.unsupported`. |
| `openai_compat` (`openai-compat`) | — | HTTP-only, with no CLI runtime. Selecting it fails. |

- Parsing is case- and whitespace-insensitive.
- Deprecated aliases resolve with an `orbit.config.crew` warning: `anthropic` → `claude`, `openai`/`chatgpt` → `codex`, `google` → `gemini`, `xai` → `grok`. Update the config.
- `copilot`, `cursor`, `pi`, `antigravity` and `opencode` have no aliases. The model vendor a lane runs (a Claude model through Copilot, say) never changes the provider identity.
- The cross-repo Worker executor runs only `claude`, `codex`, `gemini` and `grok`. A Worker-routed step naming another lane is refused rather than re-pointed.

### Resolution precedence

**Which crew a task dispatches.** The first tier that is set wins:

1. **explicit**: `--crew` or run-input `crew`.
2. **task_config**: `task.crew`. Tasks normally get this at creation (see [pools](#automatic-crew-pools-by-complexity)).
3. **workspace_default**: `workflow.default_crew`.
4. **environment_default**: `CONSTELLATION_DEFAULT_PROVIDER`, a provider ID or alias. `claude` selects `opus` and `codex` selects `sol` when defined, and any other ID selects the same-named crew. It never overrides a configured `default_crew`.
5. **system_default**: the `opus` crew, or a legacy `claude` crew.

**Which crew an activity uses.** A non-empty `crew` in the activity's rendered input, otherwise the run's crew. Activity and job assets that declare `role` are rejected.

**Crew over inline baseline.** For each of `provider`, `model` and `effort`, the selected crew's value wins when present, and otherwise the activity's inline `agent_loop` value stands. An unrecognized crew `provider` is logged and falls back to the inline provider, so a typo never moves dispatch onto a different runtime. A provider already recorded on a run is reused verbatim on reconciliation.

### No silent fallback

Explicit selections that cannot run fail with a stable diagnostic and never fall back to another runtime:

- `provider openai_compat is unsupported by the Orbit CLI entry point (HTTP-only)`
- `provider ollama is unsupported by the Orbit CLI entry point`
- `unknown provider '<x>'; expected one of claude, codex, gemini, grok, copilot, ollama, openai_compat, cursor, pi, antigravity, opencode`
- A selected lane whose binary is missing fails with a permanent diagnostic naming the binary.

---

## Provider CLI notes

Common to every lane below: Orbit passes `--model` from the crew, sends the prompt on **stdin** (never argv, which is visible in process listings and audit), and treats the Orbit OS sandbox as the filesystem boundary. Credentials reach the agent only through [`[execution.env].pass`](#executionenv--the-agent-subprocess-environment), and Orbit never puts a key on argv. Provider-specific write grants apply only while that provider is running. Lanes without native MCP (or without Orbit-managed MCP config) reach Orbit tools with `orbit tool run <tool> --input '<json>'` through their shell tool, under the same grants.

## GitHub Copilot CLI

| | |
|---|---|
| Install | `npm install -g @github/copilot`, then `copilot --version`. The retired `gh copilot` extension is not supported. |
| Auth | `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, else the `copilot /login` session (macOS keychain item `github-copilot-app`). Orbit forwards none of the token variables unless you list one in `pass`. The recommended Mac setup is `/login` once. |
| Model | `--model` is always passed, so `COPILOT_MODEL` and the CLI's saved `/model` are ignored. List IDs with `/model` inside `copilot`, and recheck pins after upgrading the CLI. |
| Flags | `--allow-all-tools --no-ask-user --output-format json`. Never `--allow-all`/`--yolo`, which would widen paths and URLs. |
| Sandbox | Write: `COPILOT_HOME` (default `~/.copilot`) and `$XDG_CACHE_HOME/copilot`. macOS: read `~/Library/Keychains`. No access to `~/.config/gh`. |

An account without Copilot entitlement fails with `Error: Authentication failed`. Third-party MCP servers disabled by org policy produce a `session.warning` (`warningType: "policy"`). Both are fixed by the GitHub org admin, not in Orbit. A run with no assistant message fails the step.

## Cursor Agent CLI

| | |
|---|---|
| Install | `curl https://cursor.com/install -fsS \| bash`, then `cursor-agent --version`. Put `~/.local/bin` on `PATH`. Cloud agents are not used. |
| Auth | `cursor-agent login` (check with `cursor-agent status`), stored in the macOS login keychain by default. A file-store login (`AGENT_CLI_CREDENTIAL_STORE=file` at login) also needs `pass = ["AGENT_CLI_CREDENTIAL_STORE"]`. Alternatively `pass = ["CURSOR_API_KEY"]`. |
| Model | `cursor-agent models` (or `--list-models`). |
| Flags | `--print --force --output-format json`. The terminal `{"type":"result","subtype":"success","is_error":false,"result":…}` object is validated before the envelope is read. |
| Sandbox | Write: `~/.cursor`. macOS: read `~/Library/Keychains`. |

## Pi CLI

| | |
|---|---|
| Install | `npm install -g @earendil-works/pi-coding-agent`, then `pi --version`. |
| Auth | `/login` in an interactive `pi` session (stored under `$PI_CODING_AGENT_DIR`, default `~/.pi/agent`), or a vendor key via `pass`. Orbit never renders `--api-key`. |
| Model | `--model` takes a pattern that may carry a vendor prefix (`openai/gpt-4o`). List with `pi --list-models`. `effort` renders as `--thinking <level>`. |
| Flags | `--mode json --no-session --no-approve --offline`: ephemeral sessions, no inherited project trust (so project-local `.pi` extensions don't run, while `AGENTS.md`/`CLAUDE.md` still load), and no startup network calls. |
| Sandbox | Write: `$PI_CODING_AGENT_DIR`, else `~/.pi`. |

Pi has no MCP client, and `orbit mcp setup` offers none. Keep Pi's `bash` tool available, because Orbit tools go through it. Orbit reads completion only from the final assistant `message_end` frame, and reports no provider token usage for Pi runs. Raw output is still kept in the audit blob store.

## Antigravity CLI

`antigravity` launches `agy`, Google's current terminal agent. It is a separate lane from `gemini` (the legacy Gemini CLI), with different flags, MCP config and model slugs. `gemini-*` models still attribute to the Gemini model family.

| | |
|---|---|
| Install | See the [Antigravity CLI docs](https://www.antigravity.google/docs/cli/headless/), then `agy --version` and `agy models`. Init prefers it over `gemini` when both are present. |
| Auth | One interactive `agy` login, cached under `~/.gemini/antigravity-cli/`. An unauthenticated headless run exits with `authentication required`. |
| Model | A slug from `agy models`. `effort` accepts `low`/`medium`/`high` only. To migrate a `provider = "gemini"` crew, change the provider and switch to an `agy models` slug. |
| Flags | `--input-format stream-json --output-format stream-json --dangerously-skip-permissions`, plus `--print-timeout` set to the activity deadline minus 30 s (a shorter custom value is kept). Don't add `agy --sandbox` or Gemini CLI flags. |
| Sandbox | Write: `~/.gemini` (shared with the Gemini CLI). |
| MCP | `~/.gemini/config/mcp_config.json` or `.agents/mcp_config.json`, not `.gemini/settings.json`. |

A terminal `result` with `status: "SUCCESS"` completes the step. On a non-zero exit with a terminal `ERROR`, Orbit surfaces the bounded, redacted `error` string.

## OpenCode CLI

| | |
|---|---|
| Install | `opencode --version`, then `opencode models`. Served modes (`serve`, `--attach`, `web`) are not used. |
| Auth | `opencode auth login` (stored in `auth.json` under `$XDG_DATA_HOME/opencode`), or a vendor key via `pass`. |
| Model | Must be a fully qualified `<vendor>/<model>`, for example `anthropic/claude-sonnet-4-5`. `effort` renders as `--variant` and accepts `high`/`max` only. |
| Flags | `run --format json --auto`. `--auto` is required unattended (without it every permission request is auto-rejected) and is not a security boundary. `--continue`, `--session` and `--share` are never passed. |
| Sandbox | Write: `$XDG_DATA_HOME/opencode`, the config root (`$OPENCODE_CONFIG_DIR`, else `$XDG_CONFIG_HOME/opencode`), `$XDG_STATE_HOME/opencode` and `$XDG_CACHE_HOME/opencode`. |

Orbit does not write OpenCode's `opencode.json` MCP config, and `orbit mcp setup` has no OpenCode target. Orbit tools go through OpenCode's shell tool. Only assistant `text` parts are read, and a terminal `error` event or a non-zero exit fails the step. No provider token usage is reported.

---

## Per-task crew override

`workflow.default_crew` is only the fallback. Every task has an optional `crew` field, and `orbit run ship` resolves each task's crew by the [resolution precedence](#resolution-precedence), so one ship can mix crews. Each child run records its crew (`orbit run show` → `resolved_crew`). A single child pipeline whose tasks name different crews, or mix set and unset crews, fails rather than falling back to the default.

### Automatic crew pools by complexity

```toml
[workflow]
low_complexity_crews = ["luna"]
medium_complexity_crews = ["grok", "terra"]
hard_complexity_crews = ["astra"]
xhard_complexity_crews = ["fable", "astra"]
```

- **Crew is fixed when the task is created.** A task created without `crew` (via `orbit task add`, `orbit.task.add`, an auto-task mint with no template crew, or an import) draws from the pool for its complexity, falling back to `default_crew`, and stores the result in `task.crew`. A `crew_assigned` history entry records the source: `explicit`, `pool:<complexity>` or `default`. A workspace with no crews configured leaves the field unset.
- **Nothing re-routes afterward.** Status transitions never change `task.crew`, and neither does changing `--complexity`. Only [`task update --crew ""`](#setting-taskcrew) draws again.
- **Tiers:** `low`, `medium`, `hard`, `xhard`. Unset or `unassessed` complexity uses the default chain. The task pilot never demotes a task out of `xhard`.
- **Empty pool** (`[]`, the init scaffold) means no pool, so the task gets `default_crew`. Blank entries and unknown crew names fail before dispatch.
- **Pools are preferences, not allowlists.** An explicit `task.crew`, an explicit run crew, and system, review and preparation jobs keep the crew they name.
- **Legacy tasks.** At admission (drain or ship), a task still without a crew is routed through the pools as a fallback, and nothing is written back to the task.
- **Run overrides.** `orbit run auto --low-complexity-crews …` (and `--medium-…`, `--hard-…`, `--xhard-…`) replaces that one pool for one drain, and the flag with no names disables it. `orbit run ship` has no override flags.

#### Weighting a pool

```toml
[workflow]
low_complexity_crews    = ["luna:50", "sonnet:50"]
medium_complexity_crews = ["grok:70", "opus:10", "sol:20"]
hard_complexity_crews   = ["opus", "sol"]          # bare = uniform
```

- Weights are relative non-negative integers, so `["grok:7", "sol:3"]` gives the same odds as `["grok:70", "sol:30"]`.
- A pool is either all bare or all weighted. Mixing them, or a suffix like `grok:-1` or `grok:2.5`, is an error. The same checks run at load, in `orbit config set`, and on the CLI flags.
- Bare duplicates collapse to one ticket. A weighted pool names each crew once.
- Weight `0` parks a crew, which is never drawn. At least one entry must weigh more than 0.
- The draw walks the pool in canonical name order over `[0, total_weight)`.

**Allowlists.** With `--allow-crew`, the draw renormalizes over the permitted members, preserving their ratios. A pool with no permitted positive-weight member is disjoint, and `orbit run readiness --allow-crew <crew>` reports `crew_not_allowed`. The allowlist is checked against `task.crew`, and still applies to system and review activities at dispatch.

**Frozen per run.** The admitting run captures the effective pools in run input `auto_crew_pools`, and descendants inherit that copy. Each admitted leaf records `crew` and `crew_selection` (task ID, complexity, source, and the eligible `[{name, weight}]` after renormalization), shown as `Crew Selection:` in `orbit run show`. Retries and resumes keep the admitted selection.

### Setting `task.crew`

| Surface | How |
|---|---|
| Dashboard | The crew dropdown on each task card. The label `default: <crew>` means the task has no `crew` and inherits `default_crew`. |
| CLI | `orbit task add --crew <name>`, or `orbit task update <id> --crew <name>`. Passing `--crew ""` to `update` re-draws for the current complexity. |
| MCP | The `crew` parameter on `orbit.task.add` / `orbit.task.update`. An empty string on update re-draws. |

### What "ran" vs what "was selected"

`orbit.task.show` returns `crew` (the task's selection) and, once a run exists, `resolved_crew` plus `crew_model` (what was dispatched, read from the persisted run record). `task.crew` is validated on write. If you later delete a crew that tasks still name, `orbit run ship` fails at run start, before any agent dispatches.

---

## Sandbox write grants for the shared Cargo caches

Both OS sandboxes (macOS `sandbox-exec`, Linux Bubblewrap) let workers share one Cargo cache:

| Path | Inside a worker |
|---|---|
| `$CARGO_HOME/registry`, `$CARGO_HOME/git` | read-write |
| `$CARGO_HOME/.package-cache`, `.package-cache-mutate` | read-write (cargo's download locks, so concurrent workers stay serialized) |
| `$CARGO_HOME/bin` | read and execute, never write |
| `$CARGO_HOME/credentials.toml`, `$CARGO_HOME/credentials` | read-denied |
| `$CARGO_HOME`, `$CARGO_HOME/.global-cache` | read-only |

- `$CARGO_HOME` is the child's variable if you pass it through `[execution.env].pass`, else `~/.cargo`.
- The grant applies only to profiles that already grant some write (`implementer`, `unrestricted`, `docs_writer`, …). Read-only profiles such as `reviewer` and `pure_compute` get none.
- On Linux, a cache path that doesn't exist on the host is skipped rather than created.

## Sandbox pseudo-tty allocation (macOS)

The macOS profile allows `pseudo-tty`, `/dev/ptmx` (read, write, ioctl), and read, write and ioctl on `/dev/ttys[0-9]+`, so `openpty`/`posix_openpt` work inside a worker (for example, a test that drives a CLI through a real terminal). Access to other PTYs remains subject to normal OS checks. Linux needs no equivalent rule.

---

## `[execution.env]` — the agent subprocess environment

Every agent subprocess (bare, Bubblewrap or `sandbox-exec`) starts from a **cleared** environment and receives only:

| Group | Contents |
|---|---|
| Baseline | `HOME`, `LANG`, `LC_ALL`, `LOGNAME`, `PATH`, `SHELL`, `TERM`, `TMPDIR`, `TZ`, `USER` |
| `pass` | Names listed in `execution.env.pass`. Default: `HOME`, `PATH`, `CODEX_HOME`, `TMPDIR`, `USER`, plus `__CF_USER_TEXT_ENCODING` on macOS. |
| Provider extras | Variables the selected provider runtime declares it needs. |
| Orbit envelope | Named `ORBIT_*` execution variables: run, task and session identity (`ORBIT_RUN_ID`, `ORBIT_TASK_ID`, `ORBIT_SESSION_ID`, …), locators (`ORBIT_WORKSPACE`, `ORBIT_WORKTREE_ROOT`, `ORBIT_BIN`, `ORBIT_REGISTRY_ROOT`, …) and activity bindings (`ORBIT_ACTIVITY_*`, `ORBIT_STEP_INDEX`, …). Privilege-bearing names (`ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN`) are not admitted, and an inherited `ORBIT_ROOT` is removed. |

This is an allowlist, not a secret filter. A benignly named credential such as `DATABASE_URL` never reaches an agent unless you name it. Agents keep network access, so this is the boundary against accidental exfiltration.

```toml
[execution.env]
pass = ["HOME", "PATH", "CODEX_HOME", "TMPDIR", "USER", "GITHUB_TOKEN"]
```

- `pass` replaces the default instead of extending it, so restate the baseline names you want.
- A listed name that is unset is absent, not empty. Names must be valid identifiers.
- `pass` is a security key: a workspace file that omits it gets the built-in default, not the global value.
- Inheriting the full environment is not configurable. A stale `execution.env.inherit` key is ignored.

---

## Other sections

| Key | Default | What it does |
|---|---|---|
| `execution.codex.sandbox` | `workspace-write` | Codex sandbox mode: `read-only`, `workspace-write` or `danger-full-access`. The file `orbit init` seeds sets `danger-full-access` globally. Security key, not inherited by a workspace file. |
| `execution.codex.approval_policy` | unset | `untrusted`, `on-request` or `never`. Security key. |
| `operation.review_policy` | `none` | Automatic review: `none`, `before-pr` (hold PR creation for a fresh reviewer, refused for local-only delivery) or `after-landing` (minted by the `delivery-code-review` auto-task with its own template crew). See [review-gate design](design/review-gate/2_design.md). |
| `operation.review_crew` | unset | Reviewer crew for `before-pr`. |
| `operation.review_reviewer_starts` | `2` | Fresh reviewer invocations per delivery candidate lineage (1–10). |
| `operation.review_repair_cycles` | `2` | Repair/validation cycles per lineage (0–10). |
| `operation.review_minutes` | `30` | Before-PR review, repair and final-validation minutes per lineage (1–1440). |
| `tasks.id_start` | unset | Forward-only floor for this machine's task-ID allocator, raised on every runtime build and never lowered, so machines can hold disjoint ranges. For the first seed prefer `orbit workspace init --task-id-start N`. See [task migration](design/task-migration/1_overview.md). |
| `automation.stall_window_minutes` | `60` | How long a delivery-automation consumer may sit on a stuck deferral (`history_diverged`, `repository_changed`, `provider_identity_missing`, `state_missing`) before a warning and one deduped friction (1–1440). Transient backpressure never escalates. See [auto-tasks](../plugin/skills/orbit-setup/references/auto-tasks.md). |
| `scoring.enabled` | `true` | Record per-agent scoreboard metrics for task runs. |
| `pr.task_url_template` | unset | URL template linking a task ID in PR descriptions. |
| `runtime.log_retention_days` | `7` | Delete `~/.orbit/state/logs/orbit.jsonl` archives older than N days (≥ 1). |
| `runtime.log_max_total_mb` | `500` | Total archive budget in MiB, pruned oldest first (≥ 1). |
| `runtime.log_max_file_mb` | `100` | Roll the active log past N MiB (≥ 1, ≤ `log_max_total_mb`). |
| `plugin.legacy_callback_identity` | `false` | Deprecated. Also accept the environment token and process ancestry as a plugin callback credential. Removed next release. |

Full log rotation runs in long-lived processes (`orbit mcp serve`, `orbit clock tick`/`orbit sweep`, `orbit web serve`). Short-lived commands roll only an oversized active file. `[operation]` keys resolve built-in → global → workspace, and unknown `[operation]` keys fail load.

## Plugins — `.orbit/plugins.yaml` and `[plugins.<ns>]`

Plugins install once per machine (`~/.orbit/plugins/<ns>/<version>/`). Enable state and grants are host-local. A repository commits only its pin file, and `orbit plugin sync` installs what the pins name. The full operator guide is the orbit-setup [plugins reference](../crates/orbit-core/assets/skills/orbit-setup/references/plugins.md), and the manifest spec is the [plugin standard](design/plugins/1_scope.md).

```yaml
# .orbit/plugins.yaml — committed
schemaVersion: 1
plugins:
  - name: graph
    version: "^0.4.1"                # optional version or semver range
    source: git+https://github.com/constellation-works/orbit-graph#v0.4.1
    enabled: true
  - name: chart
    source: https://github.com/constellation-works/orbit-chart/releases/download/v1.2.0/orbit-chart-1.2.0.tar.gz
    digest: sha256:0a1b2c3d…         # required for, and only allowed on, https:// archives
    enabled: true
```

- **Sources:** a directory outside the current repo, `git+<url>#<ref>`, a local `.tar.gz`/`.tgz`/`.tar`/`.zip`, or an `https://` archive pinned by `sha256` digest (no trust-on-first-use). Sources containing symlinks are refused.
- **Permissions are requested, never implied.** `spec.permissions` in the manifest is a request. Only `--grant` on `plugin add --enable`, `enable`, `upgrade` or `sync` grants it, and an ungranted plugin's tools register inactive. An upgrade that widens the request disables the plugin until you re-grant it.
- **`[plugins.<ns>]`** holds the plugin's own settings, validated against its `spec.config.schema` with its defaults underneath. `orbit config get`/`set plugins.<ns>.<key>` accepts only declared keys, and values layer workspace over global. An invalid value disables only that plugin. A section for a plugin this machine hasn't installed is warned about and ignored.

---

## Validation and errors

Config is parsed at startup, and invalid entries fail loud. Common errors:

| Message | Fix |
|---|---|
| `crew '<x>' is not defined in [crews.*]` | Name a crew that exists. |
| `[workflow].default_crew must be set when defining [crews.*]` | Set `default_crew`, or define an `opus` crew. |
| `[crews.<name>].<field> must not be empty` | Give the crew a `model` and `provider`. |
| `config schema no longer supports [agent.<role>] tables` | Migrate to `[crews.<name>]`. |
| `execution.codex.sandbox has invalid value '<x>'` | Use `read-only`, `workspace-write` or `danger-full-access`. |
| `[operation] has unknown key '<x>'`, `operation.review_policy has invalid value '<x>'` | The review keys are a closed set. |
| `[task] artifact_store is no longer supported` | Remove the key. |

**Retired keys that warn and are ignored.** Delete them. `orbit config get`/`set` reject them with migration notes.

| Retired | Note |
|---|---|
| `operation.preset`, `completion`, `preparation`, `preparation_due_seconds`, `promotion`, `leaf_ceiling`, `recovery`, `recovery_episodes_per_task`, `recovery_minutes_per_task`, `delivery_cap` | Operation mode was removed. `[operation]` keeps only the review keys. |
| `[docs]` | The docs corpus was removed. |
| `[semantic]`, `search.model` | Search is lexical (SQLite FTS5) and needs no model. The legacy `semantic.db` path remains the lexical search database. |
| `workflow.pilot_max_complexity` | Route a tier with `workflow.<tier>_complexity_crews`, or pin `crew` on the task. |
| `[duel]`, `[duel.models]` | Retired. |
| `[routines]` (`role = "source"`) | Every registered owner checkout is a routine source. |
| `knowledge.task_id_pattern` | Deprecated. |
| `execution.env.inherit` | Inheritance is fixed off. |

Start with a minimal workspace file that holds only genuine overrides.
