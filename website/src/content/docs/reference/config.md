---
title: Configuration
description: "config.toml locations, the host registry, crews, and the settable configuration keys."
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
first, including the machine identity. A reset creates a new machine ID;
non-interactive resets require both `--machine-name` and `--task-prefix`.
Missing non-interactive inputs or invalid supplied identity flags are refused
before deleting the existing root.

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

## Unknown settings

Unknown keys and tables load with a warning naming their file and path, plus a
nearby known key when available. Each file and path warns once per process.
Unknown tables, including empty ones, are reported as a whole. Known section
headers, plugin settings and registered table-valued settings stay under their
own validation. Unknown `[operation]` and `[review]` fields also warn; known
values still validate normally.

`orbit doctor` lists unknown settings alongside retired keys.
`orbit doctor --fix-retired-config-keys` backs up each affected file and removes
those entries, preserving known settings and translated review preferences.
Check the list first when sharing config with a newer binary: that version
may still use an entry this version does not recognise.

## Host registry

`~/.orbit/hosts.toml` registers the remote Orbit installations this machine
can reach over SSH. It is machine-global, beside `workspaces.json`; workspace
configuration does not override or merge it. The local host's identity stays
in `[machine]` in the global `config.toml`, outside this file.

Manage entries with `orbit host add`, `rename`, and `remove`, or the
dashboard's [Settings › Hosts](../../how-to/dashboard/#hosts). For setup and
prefix routing, see [Run Orbit across hosts](../../how-to/multi-host/).

The following illustrates the schema written by `orbit host add`; use the
command to discover the remote identity rather than inventing its values:

```toml
schema_version = 1

[[hosts]]
name = "owner"
machine_id = "hm_example_owner"
ssh = "orbit-owner"
task_prefix = "BOX"
```

| Field | Meaning and validation |
|---|---|
| `schema_version` | Required integer. This build accepts only `1`. |
| `hosts` | Array of remote entries (`[[hosts]]`); omission means no remote hosts. |
| `name` | Required host name, defaulting to the remote's `machine.name`. Non-empty, at most 128 bytes, with no surrounding whitespace, control characters, or path separators. Unique ignoring case, including against this machine's name. |
| `machine_id` | Required remote identity: `hm_` followed by ASCII letters, digits, `_`, or `-`, at most 128 bytes overall. Unique and different from this machine's ID. |
| `ssh` | Required SSH alias or `user@host`. No leading `-`, whitespace, control characters, or shell metacharacters. |
| `task_prefix` | Required task namespace. Two to five uppercase ASCII letters, unique across registered hosts and this machine. Stored legacy `ORB` remains valid; fresh initialization reserves `ORB`, `ADR`, `L`, and `F`. |

Unknown keys, malformed entries, unsupported schema versions, and duplicate
identities or prefixes fail loading. Orbit keeps the file's bytes intact on
a load failure. Mutations validate the resulting entries, sort them by name,
and replace the file atomically. A refused mutation preserves the prior file.

Reachability, Orbit version, protocol, and workspaces are read live by
`orbit host list` and `show`; none is persisted in this file. Registration is
one-way and stores no SSH credentials.

### Legacy migration

`~/.orbit/mcp-destinations.toml` is the legacy destination file. Its reader
remains for one release. Use `orbit host` to migrate it; do not add new rows
to the legacy file.

- **Only the legacy file exists:** federated MCP, pull drains, and replica
  worktree cleanup still read it. `orbit host list` marks rows **legacy**.
  They have no stored task prefix, so they cannot supply prefix routes.
- **First add, rename, or remove:** Orbit probes every retained legacy host,
  writes `hosts.toml`, applies the operation, and deletes the legacy file.
  `orbit host add <existing-ssh-target>` migrates an already-listed host
  successfully, preserving its SSH target and chosen migration name;
  `--name` applies only to a new host. Migrated names use the remote's
  `machine.name`, falling back to its SSH target when the name is taken.
- **A retained host does not answer:** `legacy_host_unreachable` refuses the
  mutation and neither file changes. Restore SSH access or remove the
  decommissioned row with `orbit host remove <ssh-target-or-machine-id>`.
  The removed legacy host is never contacted; all remaining hosts must
  answer. `--force` only overrides replica and pull-drain dependents.
- **Both files exist:** every consumer, including `orbit host`, refuses
  with `host_file_conflict`. The diagnostic compares them by machine ID
  and lists missing routes with their `orbit host add` commands. Preserve
  a backup, retire the legacy file, then run those commands to register
  any missing hosts. Orbit does not choose one file for you.

`orbit doctor` warns while only the legacy file exists and gives an
`orbit host add` command using an existing SSH target. The next release
drops the legacy reader and retains the both-files conflict check for one
additional release. The older singular `host.toml` identity file is
unrelated to this migration.

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
| `provider` | Required. Agent CLI family: `claude`, `codex`, `antigravity`, `gemini`, `grok`, `copilot`, `cursor`, `pi`, `opencode`, or `muse`. |
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
- is refused, never substituted, when a task's `crew`, an explicit crew, or
  `default_crew` selects it. The error names the crew and the
  `orbit config set … enabled true` command;
- is reported by `orbit doctor` when `default_crew` names it. Config still
  loads.

With no supported agent CLI detected, every crew is seeded disabled and
`default_crew` stays unset, so nothing dispatches until you
enable a crew.

`orbit init` also seeds the four `workflow.*_complexity_crews` pools from the
detected providers. Claude only: `haiku` / `sonnet` / `opus` / `opus`. Codex
only: `luna` / `sol` / `sol` / `astra`. Both: `haiku, luna` / `sol, sonnet` /
`opus` / `opus, astra`. Grok adds `grok` to `medium`; a Google CLI adds
`antigravity` (when `agy` is present) or else `gemini` to `low`. Other
providers leave the pools empty, which routes to `default_crew`. Init only
seeds a new file; an existing config is not rewritten.

### Reasoning effort

`effort` is unset by default, which keeps the provider's own default. A
supported value is passed through the provider's documented flag.

| Provider | Accepted values | Rendered as |
|---|---|---|
| `claude` | `low`, `medium`, `high`, `xhigh`, `max` | `--effort` |
| `codex` | `low`, `medium`, `high`, `xhigh`, `max` | `--config model_reasoning_effort` |
| `pi` | `low`, `medium`, `high`, `xhigh`, `max` | `--thinking` |
| `muse` | `low`, `medium`, `high`, `xhigh`, `max` | `--reasoning-effort` |
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

### System work

Orbit's own work draws its crew from the complexity pools, like a task with no
crew, until you give a purpose a pool of its own (see
[purpose pools](#purpose-pools)). Shipped auto-task templates name no crew, so
each minted task draws from the pool for its complexity; a `template.crew`
setting, an explicit template crew, and `operation.review_crew` for
`delivery-code-review` still win. While their purpose pools are unset, the
task pilot draws from `low`, step-failure and PR conflict recovery from
`medium`, and repairs authorized by `orbit run auto --auto-fix`, conflict
repairs included, from `low`. Repairs are narrowed only by `--allow-crew`, and a
pool the run cannot use stops the repair rather than escalating. A run draws
once per pool, keeping the pool's weights and skipping disabled and
usage-limited crews, and records the draw in its state. An empty pool falls
back to `default_crew`, and the recorded draw says so. Recovery never inherits
a failed task's crew.

### Purpose pools

Each purpose of work can name a weighted pool of its own under
`[workflow.purpose_crews]`, written like the complexity pools:

```toml
[workflow.purpose_crews]
task_pilot = ["luna:3", "sonnet:1"]
failure_recovery = ["sol"]
baseline_red_repair = ["opus"]
```

The purposes are `task_pilot`, `failure_recovery`, `auto_fix`,
`baseline_red_repair`, `ci_failure_sweep`, `merged_head_repair` and
`dependabot`. Final recovery and automatic review keep their existing keys,
`workflow.final_recovery_crews` and `operation.review_crew`. A crew is chosen
in this order:

1. An explicit crew on the task, auto-task template or activity input, used as
   it is.
2. The purpose's pool, when it has an enabled member. A provider usage limit
   or failure hold on the assigned member redraws from the pool's other
   enabled members, keeping their weights, before the route below.
3. The route the purpose had before purpose pools: the complexity pool named
   above, else `default_crew`.

`--allow-crew` narrows a purpose draw the same way it narrows a complexity
draw. A task's purpose comes from the tag it was filed with
(`baseline-red-repair`, `ci-failure-sweep`, the Dependabot and code-scanning
tags, a merged-head follow-up tag), or from a `crew-purpose:<purpose>` tag an
auto-task template adds. `orbit config show` lists each purpose's resolved
pool and where it comes from, and `orbit run show` names the rule behind each
activity crew a run drew.

`[workflow] system_crew` is deprecated: it still loads for one release with a
warning, but is ignored. Orbit no longer defines a built-in `system` crew, so a
template or job still naming `crew: system` routes through the pools with a
warning. A `[crews.system]` table you define is an ordinary crew.

## Settable keys

`orbit config set` accepts these keys, except the two read-only `machine.*` identity keys. `orbit config keys` prints the same list, and the docs test suite fails when this table and that list diverge.

| Key | Type, default and purpose |
|---|---|
| `workflow.base_branch` | string · **Default:** `main`<br>Base branch for ship, auto, and the task pilot when the registered workspace has none. |
| `workflow.default_crew` | string · **Default:** See [Crews](#crews)<br>Crew for a task that names none and gets no override. |
| `workflow.auto_ship` | bool · **Default:** `false`<br>Opt this workspace in to `orbit run ship-sweep`. The seeded `ship-sweep` routine does not read it; its `enabled:` flag is its switch. |
| `workflow.low_complexity_crews` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for low-complexity tasks with no crew, and for the task pilot and PR auto-fix while their purpose pools are unset. |
| `workflow.medium_complexity_crews` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for medium-complexity tasks with no crew, and for step-failure and PR conflict recovery while their purpose pool is unset. |
| `workflow.hard_complexity_crews` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for hard-complexity tasks with no crew. |
| `workflow.xhard_complexity_crews` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for xhard-complexity tasks with no crew. |
| `workflow.final_recovery_crews` | array&lt;string&gt; · **Default:** `["sol:100", "opus:20"]`<br>Crew pool for final recovery, drawn once per run after step recovery is exhausted. The default keeps only members you define; `[]` disables final recovery. |
| `workflow.purpose_crews.auto_fix` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for repairs authorized by `orbit run auto --auto-fix`. Unset or `[]`, they draw from `workflow.low_complexity_crews`. An explicit crew still wins, and `--allow-crew` still applies. See [purpose pools](#purpose-pools). |
| `workflow.purpose_crews.baseline_red_repair` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for the tasks the red-base re-check files. Unset or `[]`, they draw from their complexity pool. An explicit crew still wins, and `--allow-crew` still applies. See [purpose pools](#purpose-pools). |
| `workflow.purpose_crews.ci_failure_sweep` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for the tasks the CI failure sweep files. Unset or `[]`, they draw from their complexity pool. An explicit crew still wins, and `--allow-crew` still applies. See [purpose pools](#purpose-pools). |
| `workflow.purpose_crews.dependabot` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for the tasks the Dependabot and code-scanning sweeps file. Unset or `[]`, they draw from their complexity pool. An explicit crew still wins, and `--allow-crew` still applies. See [purpose pools](#purpose-pools). |
| `workflow.purpose_crews.failure_recovery` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for step-failure and PR conflict recovery. Unset or `[]`, they draw from `workflow.medium_complexity_crews`. An explicit crew still wins, and `--allow-crew` still applies. See [purpose pools](#purpose-pools). |
| `workflow.purpose_crews.merged_head_repair` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for the merged-head repairs a review reconciliation files. Unset or `[]`, they draw from their complexity pool. An explicit crew still wins, and `--allow-crew` still applies. See [purpose pools](#purpose-pools). |
| `workflow.purpose_crews.task_pilot` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for the task pilot. Unset or `[]`, it draws from `workflow.low_complexity_crews`. An explicit crew still wins, and `--allow-crew` still applies. See [purpose pools](#purpose-pools). |
| `workflow.provider_limit_max_used_pct` | integer · **Default:** `90`<br>Delivery admission skips a crew while a live usage window of its provider, or of its model, is exhausted or used at or above this percent. `100` skips only on exhaustion. |
| `workflow.provider_limit_overrides` | array&lt;string&gt; · **Default:** `[]`<br>Per-provider thresholds replacing `workflow.provider_limit_max_used_pct`, each `provider:percent` or `provider:off` (record readings without gating admission), such as `["claude:80", "grok:100", "codex:off"]`. |
| `workflow.provider_limit_budgets` | array&lt;string&gt; · **Default:** `[]`<br>Rolling budgets for providers that report no usage, each `provider:<amount><usd\|tokens>/<n><h\|d>`, such as `["grok:30usd/5h"]`. Counts this host's Orbit runs only. |
| `workflow.provider_limit_explicit_crews` | string · **Default:** `wait`<br>A task whose explicit crew is limited waits in the backlog (`wait`), or is drawn from the unlimited members of its complexity pool (`pool`). A purpose-sourced crew is not an explicit pin; it redraws within its purpose pool first. |
| `workflow.required_validation_commands` | array&lt;string&gt; · **Default:** `[]`<br>Commands every delivered candidate must pass. Owner PR and local deliveries run them before push or merge; a distributed claim must pass them before this owner accepts its handoff. Empty means no required check on any path: nothing runs and a claimed handoff carries no validation logs. |
| `workflow.distributed_completion` | string · **Default:** `review`<br>How far this owner takes an accepted distributed handoff: `review` waits for an operator's Approve handoff; `done` lands it through the owner's landing job, as `orbit run auto --complete` does. |
| `workflow.task_pilot_freshness.material_fields` | array&lt;string&gt; · **Default:** `title`, `description`, `criteria`, `plan`, `selectors`<br>Task fields whose edit makes an accepted task-pilot assessment stale. Also accepts `tags`, `crew`, `tools`, `type`, `complexity`, `relations`, `dependencies`, `instructions`. |
| `workflow.task_pilot_freshness.source_sensitivity` | string · **Default:** `ignore`<br>Whether a branch-head move makes an accepted task-pilot assessment stale: `ignore`, `context_files` (only when the move changed a path the task's selectors name), or `any`. |
| `workflow.mid_run_steering.enabled` | bool · **Default:** `false`<br>Record in each run's state the newest task comment every agent prompt with task context included, the run's steering watermark, and lead the run's later prompts with the operator comments written after it. The shipped delivery pipelines do not commit while one is undelivered. Claimed leaves read steering from their owner and fail closed before commit when it cannot answer. |
| `workflow.mid_run_steering.max_rounds` | integer · **Default:** `2`<br>Steering rounds a run may spend at the promotion gate before it holds as `steering_unconsumed` (`1`–`10`). |
| `workflow.resource_throttle.enabled` | bool · **Default:** `true`<br>Start no new task while host CPU, memory, disk space, or (on Linux) disk I/O pressure stays high. Disabled, pressure is still reported. |
| `workflow.resource_throttle.cpu_source` | string · **Default:** `"load"`<br>`load` or `utilization`. Linux utilization uses busy CPU tick deltas excluding idle and I/O wait; macOS uses Mach utilization for either choice. Both use the existing ten-second sustain window; I/O pressure remains independently gated. The active source appears in readiness and doctor. |
| `workflow.resource_throttle.cpu_high_percent` | integer · **Default:** `90`<br>CPU high-water mark. With the default `cpu_source = "load"`, Linux uses load average divided by online CPUs, includes the run queue and I/O wait, and can exceed 100%; this differs from `machine.auto_concurrency.admit_cpu_percent`, which measures forecast utilization. |
| `workflow.resource_throttle.cpu_resume_percent` | integer · **Default:** `85`<br>Resume below this CPU percentage. |
| `workflow.resource_throttle.cpu_light_leaves` | integer · **Default:** `2`<br>While CPU alone holds admissions, a local drain still starts up to this many `no-diff-expected` auto-tasks (reviews, curation). Memory, disk and I/O pressure hold them too; `0` holds them with everything else. |
| `workflow.resource_throttle.memory_high_percent` | integer · **Default:** `90`<br>Memory high-water mark. It and `machine.auto_concurrency.admit_memory_percent` measure non-reclaimable memory; the global auto-concurrency threshold must stay below this global high mark. |
| `workflow.resource_throttle.memory_resume_percent` | integer · **Default:** `85`<br>Resume below this memory percentage. |
| `workflow.resource_throttle.disk_high_percent` | integer · **Default:** `90`<br>Disk high-water mark, per observed filesystem. |
| `workflow.resource_throttle.disk_resume_percent` | integer · **Default:** `85`<br>Resume below this disk percentage. |
| `workflow.resource_throttle.io_full_high_percent` | integer · **Default:** `20`<br>Linux only. High-water mark for disk I/O stall: `full avg60` in `/proc/pressure/io`, the share of the last minute in which every runnable task was stalled on I/O. `full`, not `some`, because `some` fires whenever any one process waits on disk, which is normal on a busy build host. Hosts without pressure stall information are not gated. |
| `workflow.resource_throttle.io_full_resume_percent` | integer · **Default:** `10`<br>Resume below this `full avg60` percentage. |
| `workflow.validation_env.login_shell` | bool · **Default:** `true`<br>Resolve the `PATH` (and allowlisted toolchain locators such as `CARGO_HOME`) that required validation and `local_shell` steps see from the owner's shell, using `-i -l -c` and falling back to `-l -c`. `false` never probes the shell. Each probe is bounded to 10 seconds and its outcome is cached for two minutes. |
| `workflow.validation_env.interactive` | bool · **Default:** `true`<br>Try an interactive login shell (`-i -l -c`) so toolchains exported in rc files are found; on startup failure, nonzero exit, timeout, or a missing marker it falls back to `-l -c`. `false` probes only `-l -c`. Ignored when `login_shell` is `false`. |
| `workflow.validation_env.path` | array&lt;string&gt; · **Default:** `[]`<br>`PATH` entries for required validation and `local_shell` steps, combined with the resolved `PATH` per `path_mode`. A leading `~/` expands to `HOME`. Empty adds nothing. |
| `workflow.validation_env.path_mode` | string · **Default:** `prepend`<br>How `workflow.validation_env.path` combines with the resolved `PATH`: `prepend` puts it first, `replace` makes it the whole `PATH`. |
| `workflow.validation_env.build_output_env` | array&lt;string&gt; · **Default:** `[]`<br>Environment variable names bound to managed host output for required validation and host evidence. Configured bindings replace inherited values; commands must honor them. Empty allocates nothing. See [Workspace build output](#workspace-build-output). |
| `workflow.validation_env.cargo_incremental` | bool · **Default:** `false`<br>When a host validation target is bound, force `CARGO_INCREMENTAL=0` on required validation, host evidence, baseline reruns and recovery. `true` leaves the variable untouched. No binding does not set it. |
| `machine.name` | string · **Default:** Set by `orbit init`<br>Global only. This machine's display name, and the one `[machine]` value you can change. |
| `machine.id` | string · **Default:** Set by `orbit init`<br>Global only, read-only. This machine's stable generated identity (`hm_…`), written once by `orbit init` and never reused. `orbit config set` refuses it. |
| `machine.task_prefix` | string · **Default:** Set by `orbit init`<br>Global only, read-only. The task-ID namespace for IDs minted on this machine (2–5 uppercase ASCII letters), chosen once by `orbit init`. `orbit config set` refuses it. |
| `machine.host_shutdown_hold_minutes` | integer · **Default:** `180`<br>Global only. Hold new unattended admissions only when a scheduled host shutdown or reboot is this many minutes away or sooner (`>= 1`). A later schedule is still reported and admission continues. |
| `machine.worker_containment` | bool · **Default:** `true`<br>Global only. Run each detached pipeline worker in its own systemd user scope, bounded by the `machine.worker_*` limits. Needs Linux with a user manager; otherwise workers run uncontained. |
| `machine.worker_containment_strict` | bool · **Default:** `false`<br>Global only. Refuse to launch a detached worker when no systemd user scope is available. Requires `machine.worker_containment = true`. |
| `machine.worker_memory_high` | string · **Default:** `40%`<br>Global only. Worker scope `MemoryHigh=` (throttle point): a size such as `6G`, a percentage of RAM, or `infinity`. |
| `machine.worker_memory_max` | string · **Default:** `50%`<br>Global only. Worker scope `MemoryMax=` (OOM point), same format. |
| `machine.worker_tasks_max` | integer · **Default:** `4096`<br>Global only. Worker scope `TasksMax=`. |
| `machine.worker_cpu_quota` | integer · **Default:** unset<br>Global only. Worker scope `CPUQuota=` as a percentage of one core (`400` = four cores); `0` or unset sets no CPU limit. Linux only. |
| `machine.auto_concurrency.mode` | string · **Default:** `off`<br>Global only. Forecast admission mode: `off`, `shadow`, or `enforce`. `off` leaves admission unchanged. `shadow` records what gap fit would admit in each local drain pass and admits as `off`. `enforce` admits local candidates whose profile fits under the forecast. Under `orbit run auto --pull`, `enforce` sends the fit list with each pull request and requests nothing while nothing fits; `shadow` records that list and requests as with `off`. |
| `machine.auto_concurrency.admit_cpu_percent` | integer · **Default:** `90`<br>Global only. Forecast CPU utilization as a percentage of host cores (1–100); distinct from the default Linux load-based throttle signal (`workflow.resource_throttle.cpu_source = "load"`). |
| `machine.auto_concurrency.admit_memory_percent` | integer · **Default:** `75`<br>Global only. Forecast non-reclaimable memory as a percentage of host memory (1–100), strictly below global `workflow.resource_throttle.memory_high_percent`. |
| `machine.auto_concurrency.profile_window` | integer · **Default:** `50`<br>Global only. Most recent completed deliveries retained per profile key; at least `min_profile_deliveries`. |
| `machine.auto_concurrency.min_profile_deliveries` | integer · **Default:** `10`<br>Global only. Minimum completed deliveries before using a profile key rather than falling back. |
| `machine.auto_concurrency.horizon_cap_minutes` | integer · **Default:** `120`<br>Global only. Maximum forecast horizon for a candidate, in minutes. |
| `machine.auto_concurrency.head_max_wait_minutes` | integer · **Default:** `30`<br>Global only. Maximum queue-head wait before the starvation guard stops backfilling, in minutes. |
| `machine.auto_concurrency.head_stuck_passes` | integer · **Default:** `20`<br>Global only. Consecutive protected passes whose miss has not improved before a drain reports the queue head stuck: an `enforce` drain files one friction, and `orbit run show` and `orbit run readiness` print a `Stuck head:` line while it lasts. Admission is unchanged. At least `1`. |
| `tasks.id_start` | integer · **Default:** Unset<br>Floor for this machine's task-ID allocator. Moves only forward, so machines can hold disjoint ID ranges. |
| `ci_failure.operator_suppression_hours` | integer · **Default:** `6`<br>Hours an archived/rejected CI sweep finding without `covered_by` holds its exact failure key (0–720). Explicit task/PR covers hold while open or until a failing checkout contains the landed fix; operator holds report their owner and reason. |
| `automation.stall_window_minutes` | integer · **Default:** `60`<br>Minutes a delivery-automation deferral may persist before Orbit logs a warning and files one friction (1–1440). |
| `pr.close_on_terminal` | bool · **Default:** `true`<br>Close a task's open Orbit-authored pull requests, including preservation PRs for blocked tasks, when the task lands, is rejected, or is archived, with a comment naming the landing or decision. Branches are kept; a forge error is a warning, never a failure. |
| `pr.delivery_authors` | array&lt;string&gt; · **Default:** `[]`<br>Forge logins whose pull requests count as Orbit-authored for `pr.close_on_terminal` and for settling a task to done from its delivery pull request merged outside Orbit. Empty means the login the forge CLI is authenticated as on this machine. |
| `pr.task_url_template` | string · **Default:** Unset<br>URL template that links a task ID in PR descriptions. |
| `execution.env.pass` | array&lt;string&gt; · **Default:** `HOME`, `PATH`, `CODEX_HOME`, `TMPDIR`, `USER`<br>Environment variables passed to agent subprocesses. Replaces the default list rather than extending it. macOS also passes `__CF_USER_TEXT_ENCODING` by default. A name you add that the launching shell does not set reaches no agent: drains, `orbit run ship` and `orbit run job` warn on stderr and record it on the run, and `orbit doctor` reports it (start drains from a login shell). |
| `execution.codex.sandbox` | string · **Default:** `workspace-write`<br>Codex sandbox mode: `read-only`, `workspace-write`, or `danger-full-access`. The global file `orbit init` writes sets `danger-full-access`. |
| `execution.codex.approval_policy` | string · **Default:** Unset<br>Codex approval policy: `untrusted`, `on-request`, or `never`. |
| `execution.proc_spawn_max_timeout_minutes` | integer · **Default:** `45`<br>Longest timeout one `proc.spawn` call may use inside a managed activity, also capped by the activity's remaining wall-clock budget (1–1440). Outside an activity the ceiling stays 60 seconds. |
| `execution.agent_stall_minutes` | integer · **Default:** `30`<br>Minutes a provider process may produce no output and use no CPU before its step fails as stalled and is retried. `0` disables the check and leaves only the wall-clock timeout. At most 1440. |
| `plugin.legacy_callback_identity` | bool · **Default:** `false`<br>Deprecated; removed in the next release. Also accept the environment token and process ancestry as a plugin callback credential. |
| `runtime.log_max_file_mb` | integer · **Default:** `100`<br>Roll the operational `orbit.jsonl` past this size (MiB). At least 1 and at most `runtime.log_max_total_mb`. |
| `runtime.log_max_total_mb` | integer · **Default:** `500`<br>Operational `orbit.jsonl` archive budget in MiB; the oldest are pruned first. |
| `runtime.log_retention_days` | integer · **Default:** `7`<br>Delete archives in both operational and agent feeds older than this. |
| `retention.audit_days` | integer · **Default:** `60`<br>Days `orbit gc audit` keeps audit rows (1–36500). Older command-audit rows, this workspace's run-audit rows, and run-audit rows whose workspace id is no longer registered become reclaimable, along with audit blobs no remaining row names. Another registered workspace keeps its own window. Nothing is deleted until you run `orbit gc audit --apply` or enable the `store-gc` routine. |
| `retention.runs_days` | integer · **Default:** `60`<br>Days after a run finishes before `orbit gc runs` may drop its pipeline state (1–36500). The run, its steps and its summary stay. Unregistered workspace ids are included. On a large store, 14 reclaims much more terminal run state than 60. |
| `retention.journal_days` | integer · **Default:** `14`<br>Days after a task commit is applied before `orbit gc journal` may delete its commit journal row (1–36500). Recovery replays only prepared and committed rows, so an applied row is kept only for diagnosis; prepared, committed and aborted rows, and each task's newest applied row, are never deleted. Unregistered workspace ids are included. Nothing is deleted until you run `orbit gc journal --apply`. |
| `retention.store_warn_bytes` | integer · **Default:** `1073741824`<br>Warn from `orbit doctor` when the host store file is larger than this many bytes and no store-gc routine is enabled (1 GiB). Zero warns for any non-empty file. The check stays ok under the threshold or while the routine is enabled. |
| `worktree.reclaim` | array&lt;string&gt; · **Default:** `["target"]`<br>Rebuildable paths in kept terminal run worktrees. Relative globs use `*` within a component and whole-component `**` across components. An explicit list replaces earlier layers; `[]` disables reclamation. Absolute, parent and root-matching patterns fail config load. Only ignored or untracked paths without tracked content are deleted, after registration, confinement, symlink and worker checks. Inspect with `orbit gc worktrees --reclaim`, apply with `--confirm`. |
| `worktree.reclaim_below_free_mib` | integer · **Default:** Unset<br>Reclaim during admission when free MiB under the state directory falls below this threshold, oldest terminal worktrees first. Active runs and live or undecidable workers stay protected. |
| `worktree.parked_reclaim_after_hours` | integer · **Default:** `336`<br>Hours a task must stay parked in `someday`, counted from that status change, before worktree GC removes a terminal run's worktree serving it (0–876000). A live worker, an unsettled claim or a Git worktree lock keeps it. A clean worktree whose run branch holds commits no other ref reaches and `origin` lacks stays, branch included, as `skipped:unpublished_commits`. |
| `security_alert_sweep.min_severity` | string · **Default:** `moderate`<br>Lowest severity the security alert sweep files for Dependabot and code-scanning alerts: `low`, `moderate`, `high`, or `critical`. Run input overrides the workspace value, which overrides the global one. Secret-scanning alerts are always filed. |
| `review.before_pr` | bool · **Default:** `false`<br>Before-PR review: hold PR creation for a fresh reviewer that fixes what it finds. Refused for local-only delivery. A run keeps the value it was submitted with. |
| `review.before_landing` | bool · **Default:** `false`<br>Before-landing review: open the PR first, review it while hosted CI runs, and merge only the head the review settled. Any outcome but an approve leaves the PR open with the task in `review`. Refused for local-only delivery. Orbit fails to load config while this and `review.before_pr` are both on: there is one review layer before landing. |
| `review.before_landing_hosts` | array&lt;string&gt; · **Default:** `[]`<br>Owner policy for the distributed drain: machine ids (`hm_…`) whose claimed leaves run before-landing review while this owner's own deliveries keep `review.before_landing` off. The owner resolves it for the caller's machine when a follower probes or pulls, and the claim captures the result. Orbit fails to load config while `review.before_pr` is on and the list is non-empty. The machine label is chosen by the caller, so this is policy, not a security boundary. |
| `review.minutes` | integer · **Default:** `30`<br>Time limit for one candidate's before-PR or before-landing review (1–1440). Each candidate gets one review; a changed candidate is a new one. |
| `review.baseline_commands` | array&lt;string&gt; · **Default:** `[]`<br>Commands before-PR review may rerun on the host to confirm that a failed required check fails the same way on the pinned base. `workflow.required_validation_commands` always count. A confirmed claim holds the task in the backlog until the base passes; a claim about any other command settles the review incomplete. A listed command's failure cannot be recorded as a diagnostic: it needs a passing record or a confirmed claim. The list is captured when a delivery or claim is admitted. |
| `review.verify_findings` | bool · **Default:** `false`<br>Review findings must be verified before approval. When on, a review-filed bug (tagged `code-review`, `delivery-code-review`, `security-review` or `qa-sweep`) is refused at approval and promotion and kept out of drains until `finding_verification_pipeline` confirms it. When off, verification state is ignored. Read each time the gate is checked; toggle it with `orbit config set` or the dashboard's Settings › Effective switch. Turning it on does not enable the `finding_verification` routine. |
| `operation.review_crew` | string or array · **Default:** Unset<br>Crew for automatic review: the before-PR or before-landing reviewer and every review task the after-landing auto-task mints. Unset, after-landing review uses that auto-task's template crew. An array is a pool written like the complexity pools (`["sol", "grok"]` or `["sol:3", "grok:1"]`, all bare or all weighted): each review draws one crew by weight, skipping crews that are disabled or at a provider usage limit and then preferring one that did not implement the reviewed work, each step falling back to the pool when it would leave none. A distributed owner resolves its pool to one crew per claim. `[]` unsets it; a crew the registry does not define fails the load. |

Agent stdout/stderr relay uses `~/.orbit/state/logs/orbit-agent.jsonl` with a separate 200 MiB archive budget and 50 MiB file limit. Its output cannot evict operational events from `orbit.jsonl`. The dashboard log dock, `/api/log`, `/api/log/stream`, and `orbit log tail` merge the two active feeds; per-run captures remain available separately. `runtime.log_retention_days` applies to both feeds. Size limits still take precedence over age when either feed's own budget is exhausted; seven days of operational history requires its non-relay volume to fit the operational budget.

Log snapshots retain the numeric operational `offset` and add `agent_offset`. Resume SSE with `from` and `agent_from`; split-feed event IDs carry both byte cursors as `operational:agent`. Custom log paths with a filename other than `orbit.jsonl` read that file alone.

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

Crew fields are settable as `crews.<name>.<field>`, where the field is
`model`, `provider`, `enabled`, `effort`, `description`, or `tags`; for
example, `orbit config set crews.sol.effort high`. `orbit config set` cannot
create a crew: first add a `[crews.<name>]` table with `model` and `provider`.
An installed plugin's declared settings are settable as `plugins.<ns>.<key>`.

The scheduled worktree GC also reclaims declared output in every kept terminal
worktree. Candidate commits, branches, tracked files and unmatched `.orbit/tmp`
evidence remain; resumed runs rebuild reclaimed output. Collection never runs a
cleanup command: checkout-authored Cargo configuration can redirect `cargo clean`
outside the worktree, and Make or npm scripts can execute code with host authority.
Declare only rebuildable paths; evidence matching a declared pattern is reclaimable.

## Workspace build output

Configure the output locator your workspace's build commands consume:

```toml
[workflow.validation_env]
build_output_env = ["CARGO_TARGET_DIR"] # Rust workspace
```

A non-Rust workspace can use `build_output_env = ["DIST_DIR"]` and have its
validation command invoke `cmake -S . -B "$DIST_DIR"` followed by
`cmake --build "$DIST_DIR"`. Orbit has no toolchain-specific output default.

The built-in list is empty. A workspace list replaces the global list, including
an explicit `[]`. Empty configuration allocates no managed output and leaves
inherited variables unchanged. Names are trimmed, deduplicated and sorted;
they must be shell environment identifiers. Execution identity variables
(`PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `TMPDIR`, `TMP`, `TEMP`, `PWD`,
`OLDPWD`) and `ORBIT_` names are reserved, ignoring case.

After environment resolution, every configured variable points to the same
absolute host-owned directory, outside worker-writable build areas:
`<primary-repository>/.orbit/state/host-validation-targets/<sha256(canonical-source-checkout)>`.
Different source checkouts have separate output; repeated host checks for one
source can reuse it. Commands must honor the locator to preserve that isolation.
Bindings apply to required validation, host replay and detached evidence,
including baseline and recovery checks, on every platform. They do not apply
to agent sessions or ordinary `local_shell` steps. When a binding is in
effect, those commands also receive `CARGO_INCREMENTAL=0` unless
`workflow.validation_env.cargo_incremental` is `true`. Logs record the resolved
`build_output` environment, directory, source and host context.

Managed cleanup retains active, leased or ambiguous output and can reclaim
eligible inactive output even after its checkout is gone. Existing
`host-validation-targets` remain attributable and reclaimable when bindings
are disabled. Enable the workspace's output locator before rolling out a
runtime that removes implicit Cargo routing; keep active clients' output
contexts intact. Configuration examples describe an operator rollout,
not an automatic change to the owning workspace's file.

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
