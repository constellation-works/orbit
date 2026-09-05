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

The workspace config **replaces** the global config when present — it does not
merge. Move settings into the workspace file, or rely on the global file alone.

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
| `provider` | Agent family. CLI-executable families are `claude`, `codex`, `gemini`, `grok`, `copilot`, and `cursor`. |
| `model` | Model identifier passed to the provider CLI. |
| `effort` | Optional reasoning effort. See below. |
| `description` | Optional human-facing summary. |
| `tags` | Optional discovery labels. |

Crew precedence at dispatch is: an explicit crew on the activity input, then the
task's `crew` field, then `[workflow] default_crew`.

### Reasoning effort

`effort` is omitted by default, which leaves the provider's own model default
alone. When set, Orbit validates it while loading `config.toml` and passes it
through the provider's documented argument — Codex receives
`model_reasoning_effort`, Claude receives `--effort`, and Grok receives
`--reasoning-effort`.

| Provider | Accepted values |
|---|---|
| `claude`, `codex` | `low`, `medium`, `high`, `xhigh`, `max` |
| `grok` with `grok-4.6` | `low`, `medium`, `high`, `xhigh` |
| `grok` with `grok-4.5` | `low`, `medium`, `high` |
| others | Not supported — configuring `effort` is rejected. |

Orbit rejects an unsupported provider/model/effort combination outright rather
than silently downgrading or ignoring it. Availability can still depend on the
selected model on the provider's side.

Choose capability with the model first, then use `effort` to adjust the
reasoning budget inside it.

### The system crew

`[workflow] system_crew` (default: `system`) names the crew for system
activities that are synthesized at runtime and so have no job step to name a
crew on — principally step-failure recovery. Interactive `orbit init` asks which
detected bounded family should back `[crews.system]`. System work never inherits
a failed task's crew or the workspace default.

## Settable keys

These are the keys `orbit config set` accepts:

| Key | Type | Purpose |
|---|---|---|
| `workflow.base_branch` | string | Default base branch for ship workflows. |
| `workflow.default_crew` | string | Crew used when a task declares none and no override is given. |
| `workflow.system_crew` | string | Crew used by system activities such as step-failure recovery and failed-run triage. |
| `workflow.auto_ship` | bool | Opt in to unattended ship dispatch via the routine/sweep scheduler. |
| `routines.role` | string | Opt in to the routine scheduler. The only supported value is `source`. |
| `tasks.id_start` | integer | Floor for this machine's task-id allocator. Forward-only; lets machines hold disjoint ID ranges. |
| `scoring.enabled` | bool | Whether scoreboard metrics are recorded for task runs. |
| `pr.task_url_template` | string | URL template used to link a task ID in PR descriptions. |
| `execution.env.pass` | array&lt;string&gt; | Environment variable names allow-listed for passthrough into agent subprocesses. |
| `execution.codex.sandbox` | string | Codex sandbox mode: `read-only`, `workspace-write`, or `danger-full-access`. |
| `execution.codex.approval_policy` | string | Codex approval policy: `untrusted`, `on-request`, or `never`. |
| `runtime.log_max_file_mb` | integer | Roll the active JSONL log past this size. Must be ≥ 1 and ≤ `runtime.log_max_total_mb`. |
| `runtime.log_max_total_mb` | integer | Total size budget across JSONL log archives; oldest pruned first. |
| `runtime.log_retention_days` | integer | Delete JSONL log archives older than this. |

Crews are edited in the file rather than through `orbit config set`.

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
