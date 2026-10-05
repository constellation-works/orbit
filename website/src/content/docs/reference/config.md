---
title: Configuration
description: "config.toml locations, crews, and the settable configuration keys."
sidebar:
  order: 5
---

## File locations

| Path | Scope |
|---|---|
| `~/.orbit/config.toml` | Global |
| `.orbit/config.toml` | Workspace, per user (gitignored) |

Each key resolves on its own. A workspace value overrides the global value, a
global value fills a workspace omission, and built-in defaults fill the rest.
Tables merge setting by setting; scalars and arrays replace the global value
whole. Named crews merge by crew name and field, so a workspace can override
one crew's `model` without restating the crew.

Three security keys never inherit from global once a workspace file exists:
`execution.codex.sandbox`, `execution.codex.approval_policy`, and
`execution.env.pass`. A workspace file that omits one gets its built-in
default. `orbit config show` labels every value with its source.

`orbit init` seeds the global file. `orbit init --force` resets the global root
first.

```bash
orbit config path              # the config.toml in effect
orbit config show              # values with their source, plus derived paths
orbit config get workflow.default_crew
orbit config set workflow.default_crew opus
orbit config keys              # every settable key, with type and description
```

`orbit config set` writes the workspace file; `--global` writes the global one.
If the workspace file does not exist yet, add `--fresh` to start it empty or
`--seed-from-global` to start from a copy of the global file.

## Crews

A **crew** is a named provider and model. A task runs on its own `crew`, or on
`[workflow] default_crew` when it names none.

```toml
[crews.sol]
provider = "codex"
model = "gpt-6.1-sol"
effort = "high"
description = "Systems implementation"
tags = ["implementation", "review"]

[crews.opus]
provider = "claude"
model = "opus"

[crews.gemini]
enabled = false
provider = "gemini"
model = "gemini-3.8-flash"

[workflow]
base_branch = "main"
default_crew = "sol"
```

| Field | Purpose |
|---|---|
| `provider` | Required. Agent CLI family: `claude`, `codex`, `antigravity`, `gemini`, `grok`, `copilot`, `cursor`, `pi`, or `opencode`. |
| `model` | Required. Model ID passed to the provider CLI. |
| `enabled` | Default `true`. `false` keeps the crew listed but refuses to run it. See [Disabled crews](#disabled-crews). |
| `effort` | Reasoning effort. See [Reasoning effort](#reasoning-effort). |
| `description` | Short summary for people. |
| `tags` | Discovery labels. |

`ollama` and `openai_compat` are recognized provider IDs with no shipped
executor: a crew naming either one loads, then fails at dispatch. See
[Providers](../../concepts/agents/#providers). `gemini` is the legacy Google
CLI; `orbit init` prefers `antigravity` when `agy` is installed. For deprecated
provider aliases and a starter crew per executor, see
[Set up an executor](../../concepts/agents/#set-up-an-executor).

At dispatch, the first crew that is set wins: an explicit crew on the run or
activity input, then the task's `crew`, then `[workflow] default_crew`.

`default_crew` must name a defined crew. If you define crews and leave it
unset, Orbit falls back to an `opus` (or legacy `claude`) crew, and otherwise
refuses to load.

### Disabled crews

`orbit init` writes every built-in crew. It sets `enabled = true` on the crews
whose provider CLI it detects and `enabled = false` on the rest. To turn a
provider on later:

```bash
orbit config set crews.gemini.enabled true
```

A crew table without `enabled` is enabled. A disabled crew:

- stays listed: `orbit config show` (`ENABLED` column), `orbit config get
  crews.<name>.enabled`, `orbit.workspace.list` with `include: ["crews"]`
  (`enabled` per crew, schema version 3), and the dashboard all show it;
- is never drawn from a complexity pool. A pool whose members are all disabled
  acts as an empty pool, so the task gets `default_crew`;
- is refused, never substituted, when a task's `crew`, an explicit crew,
  `default_crew`, `system_crew`, or a job step's `crew: system` selects it. The
  error names the crew and the `orbit config set … enabled true` command;
- is reported by `orbit doctor` when `default_crew` or `system_crew` names it.
  Config still loads.

With no supported agent CLI detected, every crew is seeded disabled and
`default_crew` and `system_crew` stay unset, so nothing dispatches until you
enable a crew.

### Reasoning effort

`effort` is unset by default, which keeps the provider's own default. A
supported value is passed through the provider's documented flag.

| Provider | Accepted values | Rendered as |
|---|---|---|
| `claude` | `low`, `medium`, `high`, `xhigh`, `max` | `--effort` |
| `codex` | `low`, `medium`, `high`, `xhigh`, `max` | `--config model_reasoning_effort` |
| `pi` | `low`, `medium`, `high`, `xhigh`, `max` | `--thinking` |
| `antigravity` | `low`, `medium`, `high` | `--effort` |
| `opencode` | `high`, `max` | `--variant` |
| `grok` with `grok-4.6` or `grok-4.7` | `low`, `medium`, `high`, `xhigh` | `--reasoning-effort` |
| `grok` with `grok-4.5` | `low`, `medium`, `high` | `--reasoning-effort` |
| others | None | — |

An invalid or unsupported `effort` is **ignored for that crew**, as if unset,
so a slip such as `effort = "hard"` (a task-complexity word) cannot stop every
command. Loading logs a warning with the config path, crew, value, and accepted
values, and `orbit doctor` lists each ignored value with the fix. Orbit never
remaps an unsupported value onto a nearby one. `orbit config set` refuses to
write a value that loading would drop. A missing `provider` or `model`, a
retired backend, or an unknown pool reference still fails config loading.

The narrower sets follow each CLI. `agy --effort` defines no `xhigh` or `max`.
OpenCode forwards `--variant` verbatim to whichever model provider `--model`
selects and publishes no provider-independent values, so Orbit accepts only
`high` and `max`. Grok effort is verified only for `grok-4.5`, `grok-4.6`, and
`grok-4.7`; on any other Grok model it is ignored with a warning. The provider
can still reject a value for a particular model.

Choose capability with the model first, then use `effort` to tune the reasoning
budget within it.

### The system crew

`[workflow] system_crew` (default `system`) names the crew for system
activities that Orbit creates at runtime, which have no job step to name a
crew. The main one is step-failure recovery. `orbit init` points it at the
cheapest enabled crew of the preferred detected family (`luna` when Codex is
present, else `sonnet`, `grok`, …); interactive init offers those crews by
name. Shipped job steps that name `crew: system` run on this crew unless you
define a `[crews.system]` table. System work never inherits a failed task's
crew or the workspace default.

## Settable keys

`orbit config set` accepts these keys. `orbit config keys` prints the same list.

| Key | Type | Default | Purpose |
|---|---|---|---|
| `workflow.base_branch` | string | `main` | Base branch for ship, auto, and the task pilot when the registered workspace has none. |
| `workflow.default_crew` | string | See [Crews](#crews) | Crew for a task that names none and gets no override. |
| `workflow.system_crew` | string | `system` | Crew for system activities such as step-failure recovery and the task pilot. |
| `workflow.auto_ship` | bool | `false` | Opt this workspace in to `orbit run ship-sweep`. The seeded `ship-sweep` routine does not read it; its `enabled:` flag is its switch. |
| `workflow.low_complexity_crews` | array&lt;string&gt; | `[]` | Crew pool for low-complexity tasks with no crew. |
| `workflow.medium_complexity_crews` | array&lt;string&gt; | `[]` | Crew pool for medium-complexity tasks with no crew. |
| `workflow.hard_complexity_crews` | array&lt;string&gt; | `[]` | Crew pool for hard-complexity tasks with no crew. |
| `workflow.xhard_complexity_crews` | array&lt;string&gt; | `[]` | Crew pool for xhard-complexity tasks with no crew. |
| `workflow.final_recovery_crews` | array&lt;string&gt; | `["sol:100", "opus:20"]` | Crew pool for final recovery, drawn once per run after step recovery is exhausted. The default keeps only members you define; `[]` disables final recovery. |
| `workflow.required_validation_commands` | array&lt;string&gt; | `[]` | Commands every delivered candidate must pass. Owner PR and local deliveries run them before push or merge; a distributed claim must pass them before this owner accepts its handoff. Empty means no required check on any path: nothing runs and a claimed handoff carries no validation logs. |
| `workflow.distributed_completion` | string | `review` | How far this owner takes an accepted distributed handoff: `review` waits for an operator's Approve handoff; `done` lands it through the owner's landing job, as `orbit run auto --complete` does. |
| `workflow.task_pilot_freshness.material_fields` | array&lt;string&gt; | `title`, `description`, `criteria`, `plan`, `selectors` | Task fields whose edit makes an accepted task-pilot assessment stale. Also accepts `tags`, `crew`, `tools`, `type`, `complexity`, `relations`, `dependencies`, `instructions`. |
| `workflow.task_pilot_freshness.source_sensitivity` | string | `ignore` | Whether a branch-head move makes an accepted task-pilot assessment stale: `ignore`, `context_files` (only when the move changed a path the task's selectors name), or `any`. |
| `workflow.resource_throttle.enabled` | bool | `true` | Start no new task while host CPU, memory, or disk pressure stays high. Disabled, pressure is still reported. |
| `workflow.resource_throttle.cpu_high_percent` | integer | `90` | CPU high-water mark. |
| `workflow.resource_throttle.cpu_resume_percent` | integer | `85` | Resume below this CPU percentage. |
| `workflow.resource_throttle.memory_high_percent` | integer | `90` | Memory high-water mark. |
| `workflow.resource_throttle.memory_resume_percent` | integer | `85` | Resume below this memory percentage. |
| `workflow.resource_throttle.disk_high_percent` | integer | `90` | Disk high-water mark, per observed filesystem. |
| `workflow.resource_throttle.disk_resume_percent` | integer | `85` | Resume below this disk percentage. |
| `machine.name` | string | Set by `orbit init` | Global only. This machine's display name, and the one `[machine]` value you can change. |
| `machine.worker_containment` | bool | `true` | Global only. Run each detached pipeline worker in its own systemd user scope, bounded by the `machine.worker_*` limits. Needs Linux with a user manager; otherwise workers run uncontained. |
| `machine.worker_containment_strict` | bool | `false` | Global only. Refuse to launch a detached worker when no systemd user scope is available. Requires `machine.worker_containment = true`. |
| `machine.worker_memory_high` | string | `40%` | Global only. Worker scope `MemoryHigh=` (throttle point): a size such as `6G`, a percentage of RAM, or `infinity`. |
| `machine.worker_memory_max` | string | `50%` | Global only. Worker scope `MemoryMax=` (OOM point), same format. |
| `machine.worker_tasks_max` | integer | `4096` | Global only. Worker scope `TasksMax=`. |
| `tasks.id_start` | integer | Unset | Floor for this machine's task-ID allocator. Moves only forward, so machines can hold disjoint ID ranges. |
| `automation.stall_window_minutes` | integer | `60` | Minutes a delivery-automation deferral may persist before Orbit logs a warning and files one friction (1–1440). |
| `scoring.enabled` | bool | `true` | Record scoreboard metrics for task runs. |
| `pr.task_url_template` | string | Unset | URL template that links a task ID in PR descriptions. |
| `execution.env.pass` | array&lt;string&gt; | `HOME`, `PATH`, `CODEX_HOME`, `TMPDIR`, `USER` | Environment variables passed to agent subprocesses. Replaces the default list rather than extending it. macOS also passes `__CF_USER_TEXT_ENCODING` by default. |
| `execution.codex.sandbox` | string | `workspace-write` | Codex sandbox mode: `read-only`, `workspace-write`, or `danger-full-access`. The global file `orbit init` writes sets `danger-full-access`. |
| `execution.codex.approval_policy` | string | Unset | Codex approval policy: `untrusted`, `on-request`, or `never`. |
| `plugin.legacy_callback_identity` | bool | `false` | Deprecated; removed in the next release. Also accept the environment token and process ancestry as a plugin callback credential. |
| `runtime.log_max_file_mb` | integer | `100` | Roll the active JSONL log past this size. At least 1 and at most `runtime.log_max_total_mb`. |
| `runtime.log_max_total_mb` | integer | `500` | Total size budget for JSONL log archives; the oldest are pruned first. |
| `runtime.log_retention_days` | integer | `7` | Delete JSONL log archives older than this. |
| `review.before_pr` | bool | `false` | Before-PR review: hold PR creation for a fresh reviewer that fixes what it finds. Refused for local-only delivery. A run keeps the value it was submitted with. |
| `review.minutes` | integer | `30` | Time limit for one candidate's before-PR review (1–1440). Each candidate gets one review; a changed candidate is a new one. |
| `operation.review_crew` | string | Unset | Crew for automatic review: the before-PR reviewer and every review task the after-landing auto-task mints. Unset, after-landing review uses that auto-task's template crew. |

Notes:

- Pool entries are `name` or `name:weight`, all bare or all weighted. An empty
  pool sends that tier to `default_crew`.
- Each resource-throttle percentage is 1–100, and each resume mark must be
  lower than its high-water mark.
- `orbit run ship --strict-worker-containment` and
  `orbit run auto --strict-worker-containment` turn on strict containment for
  one run, whatever `machine.worker_containment_strict` says.
- After-landing review is not a config key. Turn it on with
  `orbit auto-task toggle delivery-code-review on`; it reviews landed
  deliveries in batches, observing `origin/<branch>` when that remote exists.
  `orbit config show` and `orbit doctor` report both review switches, and
  doctor is not ok while the observed commit trails the remote-tracking head
  past the batch's maximum wait.
- `operation.review_policy` and `operation.review_minutes` are deprecated and
  still load with a warning: `before-pr` becomes `review.before_pr = true`,
  `after-landing` turns on the `delivery-code-review` auto-task until you
  toggle it yourself, and `none` turns neither on. A later release rejects
  them.
- `operation.review_reviewer_starts` and `operation.review_repair_cycles` are
  retired and ignored with a warning.
- `orbit config keys` also lists `machine.id` and `machine.task_prefix`.
  `orbit init` writes both once; neither is settable.

Crew fields are settable as `crews.<name>.<field>`, where the field is
`model`, `provider`, `enabled`, `effort`, `description`, or `tags`; for
example, `orbit config set crews.sol.effort high`. `orbit config set` cannot
create a crew: first add a `[crews.<name>]` table with `model` and `provider`.
An installed plugin's declared settings are settable as `plugins.<ns>.<key>`.

## Root override

Most commands accept the global `--root` option, which overrides the Orbit root
directory:

```bash
orbit --root /path/to/orbit-root task list
```

## Retired backend selection

Agent activities always run through the provider's CLI. The `--backend` flag,
`ORBIT_BACKEND`, `[runtime] backend`, and `[crews.<name>] backend` were
removed, since there is no backend left to select.

Orbit still recognizes old declarations, so nothing is silently reinterpreted:
`cli` is accepted and ignored, while `http` and `auto` are rejected with a
migration message. Remove the setting.

## Workspace state

Workspace state lives in the repository's `.orbit/` directory, including
routine definitions in `.orbit/routines/` and the auto-task definitions that
`orbit auto-task` manages. Global state lives under `~/.orbit/`, created by
`orbit init`. See [Scoping Rules](../scoping/).
