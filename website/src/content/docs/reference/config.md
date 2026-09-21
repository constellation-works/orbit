---
title: Configuration
description: "config.toml locations, crews, and the settable configuration keys."
sidebar:
  order: 5
---

## File locations

| Path | Scope |
|---|---|
| `~/.orbit/config.toml` | Global defaults |
| `.orbit/config.toml` | Workspace-local |

Settings layer per key: a workspace value overrides the global value, the
global value fills a workspace omission, and built-in defaults fill the rest.
Tables layer down to individual settings; scalars and arrays replace the
matching global value; named crews layer by crew name and field, so a workspace
can override one crew's `model` without restating the crew.

Three security-sensitive keys never inherit from global once a workspace file
exists: `execution.codex.sandbox`, `execution.codex.approval_policy`, and
`execution.env.pass`. Omit one of them in the workspace file and its built-in
default applies. `orbit config show` annotates every value with its source.

`orbit init` seeds the global config with crews for the provider CLIs it
detects. `orbit init --force` resets the global root before initialization.

```bash
orbit config path              # which file is in effect
orbit config show              # values, with source provenance and derived paths
orbit config get workflow.default_crew
orbit config set workflow.default_crew opus
orbit config keys              # every settable key
```

## Crews

A **crew** is one provider-model assignment. A task selects a crew, and
otherwise `[workflow] default_crew` applies.

```toml
[crews.sol]
provider = "codex"
model = "gpt-5.6-sol"
effort = "high"
description = "Systems implementation"
tags = ["implementation", "review"]

[crews.opus]
provider = "claude"
model = "opus"

[workflow]
base_branch = "main"
default_crew = "sol"
```

| Field | Purpose |
|---|---|
| `provider` | Agent family. CLI-executable families are `claude`, `codex`, `antigravity`, `gemini`, `grok`, `copilot`, `cursor`, `pi`, and `opencode`. |
| `model` | Model identifier passed to the provider CLI. |
| `effort` | Optional reasoning effort. See below. |
| `description` | Optional human-facing summary. |
| `tags` | Optional discovery labels. |

`ollama` and `openai_compat` are recognized provider ids with no shipped
executor or crew: a crew naming either one loads and fails at dispatch — see
[Providers](../../concepts/agents/#providers). `gemini` is the legacy Google
lane; `orbit init` prefers `antigravity` when `agy` is installed. For the
deprecated vendor aliases and a starter crew per executor, see
[Set up an executor](../../concepts/agents/#set-up-an-executor).

Crew precedence at dispatch is: an explicit crew on the activity input, then the
task's `crew` field, then `[workflow] default_crew`.

Defining any `[crews.*]` table makes `[workflow] default_crew` mandatory, and
it must name one of those crews.

### Reasoning effort

`effort` is omitted by default, which leaves the provider's own model default
alone. When set to a supported value, Orbit passes it through the provider's
documented argument. An invalid or provider-unsupported `effort` is **ignored
for that crew** (treated as unset) so a slip such as `effort = "hard"` — the
task-complexity word — cannot take every command down. Load emits a warning
naming the config path, crew, property, offending value, and accepted values;
`orbit doctor` lists each ignored property with the corrective edit.
`orbit config set` still refuses to persist a value that admission would
drop. Missing `provider`/`model`, a retired backend, and unknown pool
references still fail closed.

| Provider | Accepted values | Rendered as |
|---|---|---|
| `claude` | `low`, `medium`, `high`, `xhigh`, `max` | `--effort` |
| `codex` | `low`, `medium`, `high`, `xhigh`, `max` | `--config model_reasoning_effort` |
| `pi` | `low`, `medium`, `high`, `xhigh`, `max` | `--thinking` |
| `antigravity` | `low`, `medium`, `high` | `--effort` |
| `opencode` | `high`, `max` | `--variant` |
| `grok` with `grok-4.6` | `low`, `medium`, `high`, `xhigh` | `--reasoning-effort` |
| `grok` with `grok-4.5` | `low`, `medium`, `high` | `--reasoning-effort` |
| others | Not supported — configuring `effort` is ignored with a warning. | — |

The narrower sets are narrow on purpose. `antigravity` omits `xhigh`/`max`
because `agy --effort` does not define them; `opencode` omits everything but
`high`/`max` because `--variant` is forwarded verbatim to whichever model
provider `--model` selected and OpenCode publishes no provider-independent
vocabulary. Grok is the only model-specific case, and effort is verified only
for `grok-4.5` and `grok-4.6` — any other Grok model with `effort` set is
ignored with a warning rather than remapped.

Orbit does not silently downgrade or remap an unsupported
provider/model/effort combination onto a nearby value. The optional key is
dropped for that crew and a warning records what was ignored. Availability can
still depend on the selected model on the provider's side.

Choose capability with the model first, then use `effort` to adjust the
reasoning budget inside it.

### The system crew

`[workflow] system_crew` (default: `system`) names the crew for system
activities that are synthesized at runtime and so have no job step to name a
crew on — principally step-failure recovery. `orbit init` points it at the
cheapest seeded crew of the preferred detected family (`luna` when Codex is
present, else `sonnet`, `grok`, …); interactive init offers those crews by
name. Shipped job steps that name `crew: system` resolve onto this crew unless
a user-authored `[crews.system]` table exists. System work never inherits a
failed task's crew or the workspace default.

## Settable keys

These are the keys `orbit config set` accepts, as printed by `orbit config keys`:

| Key | Type | Purpose |
|---|---|---|
| `workflow.base_branch` | string | Config fallback for ship/auto/pilot base branch when no registered workspace `base_branch` is bound. |
| `workflow.default_crew` | string | Crew used when a task declares none and no override is given. |
| `workflow.system_crew` | string | Crew used by system activities such as step-failure recovery and the task pilot. |
| `workflow.auto_ship` | bool | Opt in to unattended ship dispatch via the routine/sweep scheduler. |
| `workflow.low_complexity_crews` | array&lt;string&gt; | Weighted crew pool for unassigned low-complexity tasks in drains and ships; entries are `name` or `name:weight` (all bare or all weighted); empty disables the pool. |
| `workflow.medium_complexity_crews` | array&lt;string&gt; | Weighted crew pool for unassigned medium-complexity tasks in drains and ships; entries are `name` or `name:weight` (all bare or all weighted); empty disables the pool. |
| `workflow.hard_complexity_crews` | array&lt;string&gt; | Weighted crew pool for unassigned hard-complexity tasks in drains and ships; entries are `name` or `name:weight` (all bare or all weighted); empty disables the pool. |
| `workflow.xhard_complexity_crews` | array&lt;string&gt; | Weighted crew pool for unassigned xhard-complexity tasks in drains and ships; entries are `name` or `name:weight` (all bare or all weighted); empty disables the pool. |
| `workflow.required_validation_commands` | array&lt;string&gt; | Commands a distributed execution claim must pass on its exact candidate before this owner accepts its delivery handoff; empty means no claimed handoff can be accepted. |
| `tasks.id_start` | integer | Floor for this machine's task-id allocator. Forward-only; lets machines hold disjoint ID ranges. |
| `automation.stall_window_minutes` | integer | Minutes a deferred delivery-automation reason may persist before the evaluator logs a warning and files one friction (1–1440). |
| `scoring.enabled` | bool | Whether scoreboard metrics are recorded for task runs. |
| `pr.task_url_template` | string | URL template used to link a task ID in PR descriptions. |
| `execution.env.pass` | array&lt;string&gt; | Environment variable names allow-listed for passthrough into agent subprocesses. |
| `execution.codex.sandbox` | string | Codex sandbox mode: `read-only`, `workspace-write`, or `danger-full-access`. |
| `execution.codex.approval_policy` | string | Codex approval policy: `untrusted`, `on-request`, or `never`. |
| `runtime.log_max_file_mb` | integer | Roll the active JSONL log past this size. Must be ≥ 1 and ≤ `runtime.log_max_total_mb`. |
| `runtime.log_max_total_mb` | integer | Total size budget across JSONL log archives; oldest pruned first. |
| `runtime.log_retention_days` | integer | Delete JSONL log archives older than this. |
| `operation.review_policy` | string | Automatic review timing: `none` (default), `before-pr`, or `after-landing`. `before-pr` holds PR creation for a fresh reviewer and is refused for local-only delivery. |
| `operation.review_crew` | string | Crew for before-PR automatic review. After-landing review uses its delivery auto-task's template crew. |
| `operation.review_reviewer_starts` | integer | Fresh reviewer invocations allowed per delivery candidate lineage (1–10, default 2). |
| `operation.review_repair_cycles` | integer | Reviewer repair/validation cycles allowed per delivery candidate lineage (0–10, default 2). |
| `operation.review_minutes` | integer | Before-PR reviewer, repair, and final-validation wall-time minutes per delivery candidate lineage (1–1440, default 30). |

Named crew fields are also settable as `crews.<name>.<field>` (`model`,
`provider`, `effort`, `description`, `tags`). Example:
`orbit config set crews.sol.effort high`. Creating a crew still requires a
`[crews.<name>]` table with `model` and `provider`; `config keys` lists only
the fixed settings above.

## Root override

Most commands accept the global `--root` option to override the Orbit root
directory:

```bash
orbit --root /path/to/orbit-root task list
```

## Retired backend selection

`agent_loop` execution runs through the provider's CLI agent. The `--backend`
flag, `ORBIT_BACKEND`, `[runtime] backend`, and `[crews.<name>] backend` were
removed: there is no backend left to select.

Existing declarations are recognized rather than ignored, so nothing is silently
reinterpreted. `cli` is accepted and inert; `http` and `auto` are rejected with a
migration message. Remove the setting.

## Workspace state

Workspace-local state lives under `.orbit/` in the repository — including
routine definitions in `.orbit/routines/` and auto-task definitions managed by
`orbit auto-task`. Global state is initialized by `orbit init`, usually under
`~/.orbit/`.
