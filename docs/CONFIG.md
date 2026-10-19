---
type: context
summary: Orbit Configuration
last_validated: 2026-10-17
---

# Orbit Configuration

Reference for Orbit's `config.toml`: every fixed key, its default and what it does. `orbit config keys` prints the same keys with descriptions. The annotated template `orbit init` writes is [`crates/orbit-config/assets/default-config.toml`](../crates/orbit-config/assets/default-config.toml); use it as a reference, not a file to copy. The website has a shorter overview under [Reference › Configuration](https://orbit-cli.com/reference/config/). To add an execution lane rather than configure a shipped one, see the [executor onboarding runbook](runbooks/executor-onboarding.md).

## Where config lives

| Path | Scope | Created by |
|---|---|---|
| `<workspace>/.orbit/config.toml` | Workspace-local (per user, gitignored) | Hand-authored, or `orbit config set --fresh` / `--seed-from-global` |
| `~/.orbit/config.toml` | Global | `orbit init` (only when absent, or under `--force`) |

Settings inherit per key: workspace overrides global, global fills omissions, built-in defaults fill the rest. Keep a workspace file to genuine overrides.

Both config files must be regular files. Runtime loads and workspace plugin-toggle polling refuse symlink leaves (including dangling links), directories, FIFOs and other non-regular leaves. Symlinked parent directories are supported and resolved before opening the file. A missing file or parent directory contributes no layer, so global settings and built-in defaults still apply.

- **Tables** layer down to individual settings; **scalars and arrays** replace the global value.
- **Named crews** layer by crew name and field: a workspace `[crews.sol]` holding only `model = "gpt-6-astra"` overrides that one field of the global `sol` crew.
- **Security keys** (`execution.codex.sandbox`, `execution.codex.approval_policy`, `execution.env.pass`) never inherit from global once a workspace file exists; an omitted one takes its built-in default. A workspace file holding only `[plugin_enablement]` does not count, so those keys keep inheriting.
- `[machine]` is global-only and `[plugin_enablement]` workspace-only; either in the wrong file is refused at load, naming the file.

`.orbit/config.yaml` (it stores `workspace_id`) is the workspace identity file, not runtime config.

## Inspecting and editing

| Command | What it does |
|---|---|
| `orbit config show` | Effective merged view. Identity (`machine.id`, `machine.task_prefix`) leads, then Machine, Delivery (`workflow.*`), Crews, Execution, Review and Housekeeping. Each row's description is one sentence; defaults, ranges and interactions are in this reference. Unconfigured advanced keys are hidden with a count per section. `--all` (alias `--advanced`) lists every key, expands unset sections, and appends Paths, Crew purposes and Review, which restate rows above. `--json` adds `provenance` and always includes `crew_purposes` and `review`. |
| `orbit config get <key>` | One value. |
| `orbit config set <key> <value>` | Write the workspace file (`--global` for the global one). The value parses as a TOML literal, else a string. Without a workspace file, pass `--fresh` (start empty) or `--seed-from-global`. |
| `orbit config keys` | Every fixed settable key with type, section and description. |
| `orbit config path` | The resolved `config.toml` path. |

**Advanced** keys are shown whenever any layer sets them, including a global security value that is not inherited. JSON always includes every key. Scoped text views apply the same tier filter to the selected file. The tier affects display only: loading, `config get`, and `config set` are unchanged.

Each `show` value has one state: `workspace`, `global` or `environment` (the layer that set it), `default` or `unset`. `machine.id` and `machine.task_prefix` are not config keys in the text view: they render under Identity with state `identity`. JSON `state` stays `set`, `default` or `unset` for every key, including those two. A row names a shadowed layer: `(overrides global: main)`, or `(global sets danger-full-access — not inherited)` for a security key the workspace omits. A registered checkout also gets a `Workspace` line with the registry's base branch and ship mode, which delivery uses. In `--json`, each key's provenance has `scope`, `path`, `section`, `tier` (`standard`/`advanced`), `description`, `state` (`set`/`default`/`unset`) and `shadowed_by` (`[{layer, value, reason}]`, reason `overridden` or `not-inherited`); top-level `workspace_binding` is the registered base branch and ship mode, or `null`.

`--scope global|workspace` shows one file's values plus built-in defaults, without inheriting the other layer. Crew references are still validated against both layers' crews (partial workspace overrides included), here and in the dashboard's Workspace file view; a crew in neither is reported by name and key. A dashboard file validation error names the file and asks you to fix it before reloading. Scoped JSON reports `exists` (`config get`: key in that file) and `source.exists` (`config show`: file exists).

The dashboard's Settings payloads (`GET /api/config/effective`, `GET /api/config/file?scope=…`) add per-key comparison facts to each row. `built_in` is `{state, value}`: the value Orbit ships with, `state` `unset` with a `null` value when there is none. `fallback` is `{layer, value, state}`: what the key resolves to if the layer that sets it stopped setting it, or `null` when nothing can be removed or no admitted fallback can be predicted. Removing a workspace security key falls to the built-in value, not the global one, while a workspace file exists. `equals` is `{global, built_in}`: whether the row's value restates the global file's value (`null` when the global file does not set the key, and on the Global file view itself) and the built-in default, by plain JSON equality, so `opus` and `opus:100` differ. On the Global file view, `reach` says what a set value does for the bound workspace: `{state: "in-effect"}`, `{state: "overridden", workspace_value, equals_global}` or `{state: "not-inherited"}`. On the file views, `fallback`, `equals` and `reach` are `null` for rows the file does not set, and also when the two layers do not load together. Every row also says what `orbit config set` would refuse: `read_only_reason` is the refusal note for a key no write may change (`null` otherwise), and `global_only` marks a key only the global file may carry, such as `machine.*`. File views do not build crew rows. They report `crews_enumerated: false` and list the file's `[crews.<name>]` tables in `crew_tables`, so an empty `crews` list does not mean no crew is defined.

For crew fields, the library’s `EffectiveConfigValue.fallback` describes removal of one field while its table survives. A workspace field inherits a lower global field when the resulting crew can be admitted. Otherwise optional fields use the configured entry’s defaults (`enabled = true`, empty `tags`, absent `description` and `effort`), even for a crew with a shipped name. Removing `model` or `provider` without a valid lower file value leaves the table invalid, so the prediction is `None`. The separate `built_in` value still describes the shipped crew of that name; shipped crew fields do not fill omissions in configured tables.

For registered settings, fallback values and global comparisons use the config layer's admitted JSON in the current workspace's settings and crew context. For example, a global `workflow.purpose_crews.review = "sol"` supplies `["sol"]`, and complexity pools are compared in canonical crew order with duplicate bare members collapsed. `shadowed_by.value` keeps the raw file projection, including a scalar crew or the written pool order. If a shadowed global setting cannot be admitted in that context, its comparison is `null`; its removal fallback is also `null` rather than predicting a successful reload. Replace-only security settings still fall back to built-in values.

---

## `[machine]` — who this machine is

Global-only. `orbit init` writes the identity keys once.

```toml
[machine]
id          = "hm_0123456789abcdef"
name        = "example-host"
task_prefix = "ORB"
```

| Key | Default | Settable | What it is |
|---|---|---|---|
| `machine.id` | written by init | No | Opaque `hm_…` identity for run ownership, workspace ownership and federated routing. |
| `machine.name` | written by init | Yes (`--global`) | Display label: `executed_on.machine_name` on new local and claimed runs, `run_context.machine_name` in pull admission, and dashboard execution labels (full id in the tooltip). |
| `machine.task_prefix` | written by init | No | 2–5 uppercase ASCII letters for task IDs minted here; fixed for the life of the local task store. |
| `machine.host_shutdown_hold_minutes` | `180` | Yes (`--global`) | **Advanced.** Hold new unattended admissions only when a scheduled host shutdown or reboot is this many minutes away or sooner (`>= 1`). A later schedule is still reported and admission continues. |
| `machine.worker_containment` | `warn` | Yes | `warn` runs each detached pipeline worker in a transient systemd user scope (`orbit-worker-<run_id>-<nonce>.scope`), so a runaway run is throttled or OOM-killed inside it, and launches it uncontained with a warning when no scope is available; `strict` refuses that launch; `off` never uses a scope. A boolean still loads with a deprecation warning (`true` is `warn`, `false` is `off`). See [worker containment](#worker-containment). |
| `machine.worker_memory_high` | `40%` | Yes | **Advanced.** Scope `MemoryHigh=` (throttle point): bytes with optional `K`/`M`/`G`/`T`, a percentage of physical RAM, or `infinity`. |
| `machine.worker_memory_max` | `50%` | Yes | **Advanced.** Scope `MemoryMax=` (OOM point). Same grammar. |
| `machine.worker_tasks_max` | `4096` | Yes | **Advanced.** Scope `TasksMax=` (processes plus threads), at least 1. |
| `machine.worker_cpu_quota` | `0` (unset) | Yes | **Advanced.** Scope `CPUQuota=` as a percentage of one core (`400` = four cores); `0` sets no CPU limit. Linux only. |

### `[machine.auto_concurrency]` — host forecast admission

Global-only host thresholds for forecast-based delivery admission in local
and pull drains. `off`, the default, leaves admission unchanged. `shadow`
makes each local drain pass record in `drain_last_pass.admission_forecast`
what gap fit would admit, while it admits as `off` and reserves nothing.
`enforce` admits local candidates whose profile fits under the forecast
within `admit_cpu_percent` and `admit_memory_percent`, after the drain's
ceiling, the resource throttle and the conflict checks. While the forecast
holds no running delivery or reservation, the first candidate is admitted
even when it does not fit, so work larger than the thresholds still runs on
an idle host; its admission records the miss as `idle_host`.

For one local drain, `orbit run auto --auto-concurrency off|shadow|enforce`
overrides only its forecast mode. Omission follows the configured host mode on
each admission pass, including on pull drains. Explicit overrides and their
`input.auto_concurrency_source = "explicit"` provenance survive restart and
resume; configured drains retain `"configured"` provenance and follow host edits.

Local and pull drains refresh `[machine.auto_concurrency]` and the effective
`workflow.provider_limit_*` policy at the start of each admission pass. Global
and workspace `config.toml` mtimes cache resolution; unchanged files are not
parsed again. Host settings remain global-only, while provider policy retains
normal workspace-over-global precedence. A parse or validation error logs a
warning and keeps both previous policies until a later valid edit. Running
leaves continue; no drain restart is needed.

The dashboard's **Auto-concurrency mode** control offers **Inherit host**,
**Off**, **Shadow** and **Enforce** in an operator session. MCP
`orbit.workflow.auto` accepts `auto_concurrency` on `action: start`; HTTP
`POST /api/workflows/auto` accepts the same field. Start receipts carry an
`auto_concurrency` object with `mode`, `source` and current `configured_mode`,
with explicit overrides read from durable run input. Live drain views and
readiness resolve inherited mode from the current last-known-good host policy.
CLI/MCP/HTTP historical run projections retain the submitted mode under `run`
in CLI and HTTP detail JSON. Unknown values, non-string modes and settings on stop or
resize are refused before effects. CLI `--stop` and `--pull` conflict with
`--auto-concurrency`: pull overrides are unsupported, and pull drains
continue using their executor host configuration.

CLI submissions report the captured mode in a separate text line and the
`auto_concurrency` JSON field; submission warnings and notes keep their
existing meanings. HTTP stop requests require operator authority even when
their bodies are malformed; authorized incompatible requests are refused
without stopping admissions.

For a pull drain (`orbit run auto --pull`), under `enforce` each
`orbit.task.pull` request names the work this host's load forecast has room
for, the owner grants only a task inside it, and the drain requests nothing
while nothing fits (`forecast_no_headroom`). On an idle host every kind of
work fits, as in a local drain. A recurring `auto-task:` template this host
has a profile of is judged by that profile. A template it has no profile of
yet is judged by the template's base class (`no-diff` when its task carries
`no-diff-expected`, otherwise `change`), through the profile fallback chain
its claim is reserved by, so the host can claim a template before it has
ever run one. `shadow` records that list on
the drain as `pull_admission_fit` and requests as with `off`. A host without
resource profiles requests as with `off`. Claims are reserved before the
next request is forecast. `profile_window`, `min_profile_deliveries` and
`horizon_cap_minutes` also shape the resource profiles `orbit run profiles`
reports. See [auto-concurrency §6](design/auto-concurrency/2_design.md#6-pull-drains).

```toml
[machine.auto_concurrency]
mode = "off"
admit_cpu_percent = 90
admit_memory_percent = 75
profile_window = 50
min_profile_deliveries = 10
horizon_cap_minutes = 120
head_max_wait_minutes = 30
head_stuck_passes = 20
```

| Key | Default | What it is |
|---|---|---|
| `machine.auto_concurrency.mode` | `off` | One of `off`, `shadow` or `enforce`. |
| `machine.auto_concurrency.admit_cpu_percent` | `90` | Forecast CPU **utilization** as a percentage of host cores; valid range `1..=100`. |
| `machine.auto_concurrency.admit_memory_percent` | `75` | Forecast **non-reclaimable memory** as a percentage of host memory; valid range `1..=100`, and strictly below the global `workflow.resource_throttle.memory_high_percent`. |
| `machine.auto_concurrency.profile_window` | `50` deliveries | **Advanced.** Most recent completed deliveries retained per profile key; must be at least `min_profile_deliveries`. |
| `machine.auto_concurrency.min_profile_deliveries` | `10` deliveries | Minimum samples before using a profile key rather than falling back along the profile chain. |
| `machine.auto_concurrency.horizon_cap_minutes` | `120` minutes | **Advanced.** Maximum forecast horizon for a candidate. |
| `machine.auto_concurrency.head_max_wait_minutes` | `30` minutes | **Advanced.** Starvation guard: once the first candidate gap fit judges has failed for longer than this, a drain admits nothing behind it until it fits, or until the host holds no running delivery or reservation, when it is admitted. |
| `machine.auto_concurrency.head_stuck_passes` | `20` passes | **Advanced.** Stuck head: once a protected head's miss has not improved (another resource, a later minute, or at least one percentage point less) over this many consecutive passes, an `enforce` drain files one friction naming the head, its profile source and key, the miss and the baseline, and `orbit run show` and `orbit run readiness` print a `Stuck head:` line while it lasts. Admission is unchanged. At least `1`. |

**Observing admission.** `orbit run show <drain>` renders an `Admission:`
line from `pipeline_state.drain_last_pass.admission_forecast`: skipped
candidate titles, the failing resource, percentage and minute, and a protected
queue head's starvation guard, followed by a `Stuck head:` line naming its
miss, profile and baseline while that head is stuck. Shadow decisions are
labelled as hypothetical; they do not hold admissions. With no block (off mode or an older drain), no
line appears. JSON preserves the complete recorded block, including the
profile source (`exact`, `without_complexity`, `base_class`,
`heaviest_of_class`, `heaviest_on_host`, or `none`).

`orbit run readiness` shows the same last-pass snapshot, CPU and memory peaks,
and headroom in percentage points below the **current** configured admission
limits. JSON adds `capacity.admission_forecast`, with the recorded fields,
`task_titles`, `headroom` (`cpu_percent`, `memory_percent`,
`admit_cpu_percent`, `admit_memory_percent`), and the drain that recorded it
(`drain_run_id`, with `drain_live` false once that drain has ended). An unknown
host capacity has unknown headroom; negative headroom means the peak exceeds
the limit. Text names each task as `ORB-12 "title"` and prints memory in GiB.
A pass that builds a forecast also retains it in
`pipeline_state.drain_last_forecast`; readiness exposes the same observation
as `capacity.last_admission_forecast`, with titles and current-limit headroom.
When later passes build none, both commands say so and show the retained
forecast as `Last forecast <time> (<age> ago, from drain <run id>)`, with its
admitted and skipped candidates, and its peaks as `Last forecast peak:`. A
finished drain's last pass is labelled the same way, adding `which has ended`,
and readiness adds `no drain is live` when neither a local nor a pull drain
is running. These historical decisions never change current task eligibility.
The current-pass keys keep their meaning; the retained forecast survives idle
passes and terminalization until another built forecast replaces it.

Each candidate the pass judged carries `tasks[].admission_forecast`, including
its profile source, mode, forecast time, and `decision` (`admitted` or
`skipped`); skipped candidates also carry the recorded resource, minute,
percentage and detail. An enforce skip from a live, open drain reports
`gap_fit` when no current hold takes precedence. A finished drain
shows its historical decisions without changing current eligibility. A shadow skip explains what would wait without changing
eligibility. These are recorded observations, not a fresh fit or a reservation:
`forecast_at` dates them, and the next pass or changed configuration can change
the answer. Without a recorded forecast, the readiness response is unchanged.
The recorded block also carries `series`: the forecast's CPU and memory over
its first hour as percentages of the host, one point per `bucket_minutes` (5)
from minute 0 to +60m, each the highest minute of its bucket, so the highest
point equals the peak whenever the peak is within the hour. It is absent when
the host could not be measured and in records written before it was kept.
The dashboard Drain card shows the same snapshot: forecast time, CPU and memory over the first hour as sparklines with the admit threshold dashed and the peak's minute marked (peak bars for a record without `series`), headroom, and each candidate enforce held. It does not list the tasks a pass admitted. A pass with no forecast says `no forecast this pass` and shows `capacity.last_admission_forecast` when one was retained. Shadow decisions are labelled hypothetical.

`orbit run profiles --accuracy` checks the forecast itself. Each local drain
pass under `shadow` or `enforce` that built a forecast records it at +5, +10,
+20 and +30 minutes, counting what the pass admitted, with its decisions;
`off` records nothing. The report pairs each point with the host's measured
minute and shows, per offset and for CPU and memory, the mean error
(forecast minus measured), how often the forecast was under, and the worst
miss. Per drain it counts the tasks shadow would have held or added against
what the drain admitted. Pull drains record each request's forecast, counting a
returned claim using the same profile as its reservation. `PULL_REQUESTS` counts
these forecasts, including requests enforce held; `PULL_SENT` counts requests
sent to the owner, including idle responses and failed attempts.
`PULL_WOULD_HOLD` counts shadow requests enforce would have held because no
kind of work fit (`forecast_no_headroom`), even if no claim returned.
`ENFORCE_ADMITTED` counts the distinct tasks local enforce passes admitted,
`GAP_HELD` the distinct tasks gap fit held (`fit_skipped`), and `PULL_HELD`
the enforce requests held because no work fit. Those three stay on enforce
passes; the shadow columns stay on shadow passes. JSON adds
`enforce_admitted`, `gap_held` (the task ids) and `pull_held` without
changing the existing shadow fields.
`--since <RFC3339>`, `--drain <RUN>` and `--mode shadow|enforce` intersect to
select a cohort; without filters the report uses all drains and modes from
the last 30 days. Both output formats identify that selection and report
time. Only fully ended target minutes with measurements form error pairs.
`pending` counts future/incomplete minutes, even with a partial measured row;
`missing` counts ended minutes with no row. `missing` splits into `unsampled`
(no delivery run was live in that minute, so an idle drain had nothing to
pair with), `partial` (a delivery overlapped the minute but none was running
for all of it; the sampler records only a minute a worker saw whole, so the
minute a delivery starts or finishes in is not a defect) and `gap` (a
delivery was running for the whole minute but wrote no host row). The
compatible `unmeasured` count is pending plus missing. `--json` adds the raw pairs and effective selection. A selected
drain/time range does not attest source, build-budget or threshold settings:
those are not stored in forecast rows. Per-task profiles remain a separate
population whose sparsity and spread still limit confidence. See the
[operator runbook](runbooks/forecast-accuracy.md). Records are kept for
30 days and pruned by `orbit gc runs --apply`. See
[auto-concurrency §8.1](design/auto-concurrency/2_design.md#81-the-accuracy-report).

Each drain also counts why its admission passes admitted nothing, in
`pipeline_state.drain_pass_counters`: every pass is one of `admitted`,
`failed`, `held`, `throttle`, `no_candidate`, `ceiling_full`, `gap_fit_held`
or `deferred`, with held passes split by reason, throttled passes by
resource, and the reasons tasks waited. A carried pull request or claim
waiting to reacquire forecast capacity counts as `gap_fit_held`, including
a request whose reservation expired; `admissions_stopped` requires an
observed operator stop or cancel during the pass. `orbit run show <drain>`
prints them as an `Admission passes:` line, and `orbit run profiles --accuracy` gives each
drain's limiter shares (`limiters` in `--json`). The counters survive resume
([auto-concurrency §7](design/auto-concurrency/2_design.md#7-surfaces-and-configuration)).

CPU utilization is bounded at 100% and is not compared with the resource
throttle's CPU percentage. The auto-concurrency memory forecast and the
resource throttle's memory percentage both measure non-reclaimable memory.

- `orbit config set` refuses `machine.id` and `machine.task_prefix`: changing either would orphan or renumber records minted under it.
- `orbit init` refuses to create an identity once the task store has minted ids under another prefix (tasks created before `orbit init` use the historical `ORB`). It fails before writing anything; run `orbit init` before creating tasks.
- Hand edits fail closed: a `[machine]` table missing an identity key, a `task_prefix` that contradicts the local task allocator, or an `id` that contradicts a workspace record naming this machine as owner is refused. Nothing falls back to the hostname.
- A legacy `~/.orbit/host.toml` is folded into `[machine]` on first load and removed. If both exist and disagree, Orbit refuses to start and names both paths; delete the stale one.
- `orbit init --force` deletes the global root, machine identity and executor sandbox settings included, before seeding defaults, so it creates a new machine ID and prompts for name and prefix. Non-interactively, pass both `--machine-name` and `--task-prefix`; missing or invalid flags are rejected before anything is deleted. Plain `orbit init` keeps an existing identity.

### Worker containment

- Without a systemd user manager (containers) under `warn`, or with `worker_containment = "off"`, workers run in the caller's cgroup and each Linux Orbit process logs one warning. macOS has no systemd and runs workers uncontained without a warning.
- `strict` starts no uncontained worker: the run reports why and how to enable a user manager or turn strict mode off, with error code `worker_containment_unavailable` (also in CLI JSON errors). The deprecated `worker_containment_strict = true` reads as `strict`, and fails config load beside `worker_containment = false`. `--strict-worker-containment` on `orbit run ship` or `orbit run auto` turns it on for one invocation; the auto coordinator passes it to leaf workers as `ORBIT_WORKER_CONTAINMENT_STRICT`.
- Every Linux worker, contained or not, starts with an `oom_score_adj` of at least 500, inherited by everything it runs, so a host-wide OOM kills a worker before the user systemd manager (100) or the drain that launched it. It is not configurable; scope units take no `OOMScoreAdjust=`, so the worker sets it on itself before `systemd-run` execs it.
- `machine.worker_cpu_quota` adds `CPUQuota=` to each scope, so one run cannot take every core. Only an unsigned integer loads, with no upper bound. `orbit doctor`'s `worker-containment` row reads `info` while it is unset and `ok` once it is set. systemd enforces it only where the user manager delegates the cpu controller; elsewhere the property is accepted and not enforced.
- A run that fails after hitting a worker limit carries error code `worker_resource_limit` in `orbit run show`. Inspecting scopes: [operational logs › Worker Resource Containment](../crates/orbit-core/assets/skills/orbit-setup/references/operational-logs.md#worker-resource-containment).
- Admitted names reach the contained worker through its environment, not the `systemd-run` command line. Orbit passes each name as `--setenv=NAME` with no value. systemd (211 and later, including 259) copies the value from the wrapper process, which already holds the allow-listed environment. That includes pass-listed provider credentials and clock-file defaults. A toolchain `PATH` override, an explicit removal, and the workspace allowlist are unchanged. Agent subprocesses still start from a cleared environment.

---

## `[workflow]` — branch and crew defaults

```toml
[workflow]
base_branch = "main"
default_crew = "opus"
low_complexity_crews = []
medium_complexity_crews = []
hard_complexity_crews = []
xhard_complexity_crews = []
provider_limit_max_used_pct = 90
provider_limit_overrides = []
provider_limit_budgets = []
provider_limit_explicit_crews = "wait"
```

| Key | Default | What it does |
|---|---|---|
| `workflow.base_branch` | `main` | **Advanced.** Fallback base branch for ship, auto and pilot. The workspace registry's value (`orbit workspace show`) wins; `--base <branch>` overrides both. For a two-branch repo, register with `--base-branch agent-main`. |
| `workflow.default_crew` | see [resolution](#resolution-precedence) | Crew for a task with no `crew`. Must name a defined crew. |
| `workflow.system_crew` | — | **Deprecated, ignored.** Loads with a deprecation warning for one release; `config get`/`set` refuse it. System work draws from the complexity pools instead: see [system work](#system-work-draws-from-the-complexity-pools). Delete the key. |
| `workflow.low_complexity_crews` | `[]` | Pool a crew-less `low` task draws from, and the task pilot and PR auto-fix while their [purpose pools](#purpose-crew-pools) are unset; empty uses `default_crew`. See [pools](#automatic-crew-pools-by-complexity). |
| `workflow.medium_complexity_crews` | `[]` | The same, for `medium`; step-failure and PR conflict recovery draw from it too while `workflow.purpose_crews.failure_recovery` is unset. |
| `workflow.hard_complexity_crews` | `[]` | The same, for `hard`. |
| `workflow.xhard_complexity_crews` | `[]` | The same, for `xhard`. |
| `workflow.purpose_crews.<purpose>` | `[]` | Weighted pool for one purpose of work (`task_pilot`, `failure_recovery`, `auto_fix`, `baseline_red_repair`, `sweep_tasks`). Unset or `[]` keeps the purpose's earlier route. `final_recovery` (default `["sol:100", "opus:20"]`, `[]` disables it; see [final recovery pool](#final-recovery-pool)), `review` and `pre_landing_review` (see [automatic review](#review-and-operation--automatic-review)) keep their own rules. See [purpose crew pools](#purpose-crew-pools). |
| `workflow.provider_limit_max_used_pct` | `90` | **Advanced.** Admission skips a crew while a live usage window of its provider or model is exhausted or used at or above this percent (`1`–`100`; `100` skips only on exhaustion). See [provider usage limits](#provider-usage-limits). |
| `workflow.provider_limit_overrides` | `[]` | Per-provider thresholds replacing the one above, each `provider:percent` (`["claude:80", "grok:100"]`) or `provider:off` (`["codex:off"]`, record readings without gating admission). One entry per provider, percent `1`–`100`, an alias such as `anthropic` stored as its provider. |
| `workflow.provider_limit_budgets` | `[]` | **Advanced.** Rolling budgets for providers that report no usage, each `provider:<amount><usd\|tokens>/<n><h\|d>` (`["grok:30usd/5h"]`). See [budgets](#budgets-for-providers-that-report-no-usage). |
| `workflow.provider_limit_explicit_crews` | `wait` | **Advanced.** A task whose explicit crew is limited: `wait` keeps it in the backlog; `pool` draws from the unlimited members of its complexity pool. |
| `workflow.auto_ship` | `false` | Opt this workspace in to `orbit run ship-sweep`, the cross-workspace unattended ship command (skipped with `auto_ship_disabled` while `false`). The seeded `ship-sweep` routine ignores this key; its `enabled:` flag is its only switch. Neither grants `--complete` or `--approve-proposed`, and a `no-auto-approve` task is never auto-approved by either flag's drain or the CI sweep. |
| `workflow.required_validation_commands` | `[]` | Commands every delivered candidate must pass before push or merge. Empty runs no required check. See [required validation](#required-validation). |
| `workflow.distributed_completion` | `review` | How far this owner takes an accepted distributed-drain handoff: `review` waits for an operator's **Approve handoff**; `done` authorizes it on acceptance and lands it through `task_landing_pipeline`, rechecking this key before the merge. |
| `workflow.unlocked_context_paths` | `["**/*.md"]` | Path globs whose `context_files` entries take no context lock. See [unlocked context paths](#unlocked-context-paths). |
| `workflow.unlocked_path_validation_commands` | `[]` | Commands a repaired candidate runs instead of the full required list when its tree differs from the run's last fully validated tree only in `workflow.unlocked_context_paths`. Empty always runs the full list. See [revalidating an unlocked-path repair](#revalidating-an-unlocked-path-repair). |
| `workflow.generated_paths` | `{}` | Generated files, as path glob = regenerate command. Conflict recovery regenerates a conflicting match instead of merging it. See [generated paths](#generated-paths-are-regenerated-not-merged). |
| `workflow.light_delivery_paths` | `["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]` | Path globs a candidate must change only within for a `skip-review` or `skip-validation` task to skip agent review or local validation. A leading `!` excludes. `[]` disables both tags. See [light delivery](#light-delivery-skip-review-and-skip-validation). |

### Unlocked context paths

`workflow.unlocked_context_paths` lists workspace-relative globs: `*` matches within one path component and `**` spans components, so `**/*.md` is every Markdown file and `docs/**` is everything under `docs/`. A `context_files` entry whose path matches stays in the task's context, and the agent still receives it, but takes no context lock. Local drain waves, readiness, backlog admission, `orbit task eligible`, lock contention and lock reservations skip it, so two tasks that share only a design doc beside their own code run together and landing reconciles the text by merge and conflict recovery.

A task whose every entry matches, such as a rewrite of one document, still locks them all, because two rewrites of one file are where a merge can silently produce contradictory text. A mixed task touching that file is not held back by the rewrite, since it does not request the file. Leave build and CI definitions (`Makefile`, `.github/workflows/*.yml`) and code out of the list: they define the gates, so a bad merge there breaks every delivery. `[]` locks every entry, as before the setting existed. Owner pull admission still checks a remote claim's full declared footprint. The value is read when an Orbit process opens the workspace.

### Revalidating an unlocked-path repair

A conflict recovery, step recovery or final recovery that only reconciled prose need not rerun the whole required list. Set `workflow.unlocked_path_validation_commands` to the commands such a candidate needs instead, for example the docs and formatting gates:

```toml
[workflow]
unlocked_path_validation_commands = ["make ci-fast"]
```

When required validation runs on a candidate, Orbit compares the candidate's tree with the last tree this run passed the full `workflow.required_validation_commands` list on. If every changed path matches `workflow.unlocked_context_paths`, only these commands run. Any other change runs the full list, for example a rebase onto a base that brought a code change, because that combination has never been validated. The decision rests on the tree diff alone, never on what an agent reports it changed.

- Only a full pass becomes the comparison point. A narrowed pass does not, so successive prose repairs are always compared with code that passed the full list.
- The comparison point lives in the memory of the Orbit process that ran the validation, where an agent cannot write it. A run resumed by another process, a claimed leaf, a changed required list and the first validation of a run all run the full list.
- Each validation log and the step's output record `validation_scope`. It holds `kind` (`full` or `narrowed`), `reason`, the `tested_tree`, and, when trees were compared, the `validated_tree` and `validated_head` with the changed paths. A narrowed record also lists the `required_commands` it stood in for, so review and landing can audit the substitution. Completion evidence that requires the full list does not accept a narrowed pass.
- Unset or `[]`, every candidate runs the full list, as before the setting existed.

### Light delivery: `skip-review` and `skip-validation`

Two task tags let a documentation or CI change land on the hosted pull-request checks alone. Only filing a task or an operator update sets them. An agent's update, including the implementer's own during its run, is refused when it adds either one. Naming an agent model on a tool or API update counts as that agent, including when the server process has no agent environment. Removing a tag is always allowed.

- `skip-review`: no before-PR review (`review.timing = before_pr`), no before-landing review (`review.timing = before_landing`, `review.before_landing_hosts`), and no after-landing delivery-code-review sampling.
- `skip-validation`: no local validation. The implementer runs no AGENTS.md gates and no required or final validation commands. Its task context carries no `final_validation_commands`, and its `light_delivery` field names the tags and the allow-list. Orbit's shipped tag instruction says so. Required validation (`candidate_validate`, and `claim_validate` on a claimed leaf) records `skipped_light_delivery`. A handoff without validation logs is accepted.

A tag applies only while every path the final candidate changes since its synchronized base matches `workflow.light_delivery_paths`. Deletions count, and a rename counts as both paths. It also needs every task the candidate delivers to carry the tag. The globs follow `workflow.unlocked_context_paths` (`*` within a component, `**` across components), and a pattern with a leading `!` excludes what it matches. A path is light when some plain pattern matches it and no `!` pattern does. The default covers `docs/**`, Markdown outside `crates/**` and `plugin/**`, `.github/**`, `scripts/ci-*` and `scripts/test-*-workflow*.py`:

```toml
[workflow]
light_delivery_paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]
```

A candidate that changes any other path, or nothing at all, ignores both tags and gets normal review and validation. Each decision is recorded once per candidate in the task's history: `light_delivery_applied` or `light_delivery_ignored`, with the tag, the candidate and the first path outside the list. The decision rests on the Git diff alone, never on what an agent reports it changed.

On a distributed claim, the owner captures its list in the claim's validation gates. The follower leaf judges its candidate against that copy, never its own config. An owner too old to capture one leaves the leaf with no list, so neither tag applies. The owner judges the handed-off candidate again in its own checkout when it accepts, approves or lands the handoff. It records the decision in the task's history, because a leaf cannot write history.

`review.baseline_commands` are the reviewer's own checks: they run only when a review runs. Tag a task with both tags to skip review and every local check.

### Generated paths are regenerated, not merged

`workflow.generated_paths` maps a generated file's path glob to the command that rebuilds it:

```toml
[workflow.generated_paths]
"docs/INDEX.md" = "make docs-index"
```

When a stopped rebase conflicts on a matching path, the conflict recovery agent is told not to hand-merge it (its input lists it under `regenerated_paths`). After the agent has resolved the other files and exits, the host continuation takes the upstream side of the file, or the replayed side when upstream deleted it. It then runs each distinct command once in the checkout with the validation environment, for at most ten minutes, and stages the output with the rest of the resolution. A command that exits non-zero, times out, leaves conflict markers in the file, or changes HEAD, the branch or the index fails the repair. The stopped rebase then goes to the failure handoff as any refused continuation does. Other files the command rewrites join the continued commit. The certified continuation lists the regenerated paths. Default empty: every conflict is resolved by the agent.

### What `orbit init` seeds

Only the global file, and only when it is absent (or under `--force`):

- every [built-in crew](#crewsname--which-provider-model-runs-the-task), enabled for each detected provider CLI and `enabled = false` otherwise;
- `default_crew`: the default crew of the first detected family in preference order;
- the four complexity pools, as bare lists:

  | Detected | `low` | `medium` | `hard` | `xhard` |
  |---|---|---|---|---|
  | codex + claude | `["haiku", "luna"]` | `["sol", "sonnet"]` | `["opus"]` | `["opus", "astra"]` |
  | codex only | `["luna"]` | `["sol"]` | `["sol"]` | `["astra"]` |
  | claude only | `["haiku"]` | `["sonnet"]` | `["opus"]` | `["opus"]` |

  On any row, and alone when neither codex nor claude is detected, a detected `grok` is appended to `medium`, and a Google CLI's crew to `low` (`antigravity` when `agy` is detected, otherwise `gemini`). Other families leave the pools `[]`, which route to `default_crew`. Init fails rather than write a pool entry it does not seed enabled.

Interactive init asks for the default crew by name, offering only enabled crews. It no longer writes or asks for `workflow.system_crew`: system work draws from the pools. With no supported CLI, every crew is written disabled and `default_crew` stays unset, so it resolves to the disabled `opus` and agent dispatch refuses until you run `orbit config set crews.<name>.enabled true`; deterministic jobs such as store retention still run. Init never writes `[crews.system]`, `[crews.custom]` or `[crews.qa]`.

### `[workflow.resource_throttle]` — host pressure

While CPU, memory, disk I/O (Linux) or a watched filesystem stays at or above its high mark, admission using those thresholds starts no new task work until it falls below the resume mark; running work is never cancelled. Drain, ship and readiness behavior: [host resource pressure runbook](runbooks/distributed-drain.md#host-resource-pressure).

A scheduled host shutdown is a separate hold. `machine.host_shutdown_hold_minutes` (default 180, global only, at least 1) stands new unattended work down only when the schedule is that close, so a reboot set for tomorrow does not idle the host all day. Readiness still names a later schedule. See [scheduled host shutdown](runbooks/distributed-drain.md#scheduled-host-shutdown-or-reboot).

| Key | Default | What it does |
|---|---|---|
| `workflow.resource_throttle.enabled` | `true` | **Advanced.** Enable the throttle verdict. `false` restores unthrottled admission; telemetry and severity are still reported. |
| `workflow.resource_throttle.cpu_source` | `"load"` | CPU signal: `load` or `utilization`. Linux utilization excludes idle and I/O wait; macOS uses Mach utilization for either choice. |
| `workflow.resource_throttle.cpu_high_percent` | `90` | CPU high-water mark. |
| `workflow.resource_throttle.cpu_resume_percent` | `85` | Resume below this CPU percentage. |
| `workflow.resource_throttle.cpu_light_leaves` | `2` | **Advanced.** Leaves a local drain still starts for `no-diff-expected` auto-tasks (reviews, curation) while CPU is the only held resource; memory, disk and I/O pressure hold them too. `0..=32`; `0` holds them with everything else. |
| `workflow.resource_throttle.memory_high_percent` | `90` | Memory high-water mark. |
| `workflow.resource_throttle.memory_resume_percent` | `85` | Resume below this memory percentage. |
| `workflow.resource_throttle.disk_high_percent` | `90` | High-water mark for each watched filesystem. |
| `workflow.resource_throttle.disk_resume_percent` | `85` | Resume below this filesystem percentage. |
| `workflow.resource_throttle.io_full_high_percent` | `20` | **Advanced.** Linux only. High-water mark for disk I/O stall: `full avg60` in `/proc/pressure/io`, the share of the last minute in which every runnable task was stalled on I/O. Hosts without pressure stall information (macOS, kernels without PSI) are not gated and report no I/O reading. |
| `workflow.resource_throttle.io_full_resume_percent` | `10` | Resume below this `full avg60` percentage. |

**Why disk I/O uses `full`, not `some`.** The CPU, memory and filesystem-space marks cannot see disk bandwidth: a host whose storage saturates before its CPU keeps admitting deliveries into I/O contention. `some` counts time when at least one task waits on I/O, which a healthy build host does constantly. `full` counts time when no runnable task could make progress, so it rises only when the disk, not one slow process, is the bottleneck. `avg60` smooths bursts such as a single link step; the ten-second sustain window below still applies. A 47% `full avg60` on a single NVMe running a dozen validating deliveries is the saturation this guards; the 20% / 10% defaults hold new admissions well before that and release once the backlog drains. Raise `io_full_high_percent` on storage that sustains more concurrent builds, or lower the drain ceiling instead. The mark guards admission only: running deliveries are never cancelled or paused, and a held reading reads `io full` in readiness and the drain's last pass.

Percentages are integers `1..=100`; each resume mark must be below its high mark. A workspace override applies only to that workspace's admission. Each resource's hysteresis history is shared by workspaces with identical high and resume marks; CPU history also requires the same source. Different marks or CPU sources keep independent sustain windows and holds against the same host readings. The host dashboard uses the serving machine's global values, read when its monitor first opens, so restart it after changing them.

`machine.auto_concurrency.admit_memory_percent` must stay below this table's
global `memory_high_percent`, since both memory thresholds count non-reclaimable
memory. Its CPU threshold measures forecast utilization. On Linux this
table's default CPU source is load-based (`/proc/loadavg` divided by online CPUs),
includes the run queue and I/O wait, and can exceed 100%. Opt into
`cpu_source = "utilization"` to use the same busy tick accounting as
auto-concurrency. It measures the interval between fresh samples (normally
five seconds during admission), with the same ten-second sustain window and
high/resume marks as load mode. The first Linux sample, a missing counter,
a counter reset, a CPU-count change, or a gap over fifteen seconds makes
utilization unknown and fails open until a usable pair arrives; it never falls
back to load. I/O pressure remains independently gated by `io full`.
Readiness reports `capacity.resource_cpu_source` in JSON and a CPU source line
in text, including while admission is healthy or the throttle is disabled;
`orbit doctor` reports the effective source as well.

**Verdict.** Below resume is `ok`, between resume and high `elevated`, at or above high `critical`. A resource is held once its high readings span ten seconds (repeated reads of one cached sample do not count; a gap over fifteen seconds restarts the window) and stays held until below resume. A sample that is unavailable, invalid, future-dated or older than fifteen seconds is `unknown` and releases that resource's hold (fail open). Readiness, `orbit run show`, `orbit run auto`, MCP and the dashboard Drain card name the held resource, value, threshold and since-when.

**Readings** are usage estimates, not OS memory-pressure classes, cached two seconds and read from procfs, Mach/sysctl and statvfs (via fs2), never a subprocess.

| Resource | Linux | macOS |
|---|---|---|
| CPU | `load` (default): one-minute load ÷ online CPUs × 100; can exceed 100% and includes I/O wait. `utilization`: busy `/proc/stat` tick deltas excluding idle and I/O wait. | Busy fraction of aggregate Mach CPU tick deltas. A fresh process reuses a CPU tick window from the host's shared pressure history when its baseline is at most fifteen seconds old. Without a usable baseline it waits about 1.1 seconds and reads again, crossing the kernel's shared statistics cache window. |
| Memory | `(MemTotal - MemAvailable) / MemTotal` | Physical memory minus free and reclaimable inactive pages, so compressed and wired memory count as used (speculative pages are already free; purgeable pages overlap other categories). See Apple's [VM](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/mach/vm_statistics.h) and [host CPU](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/mach/host_info.h) statistics. |
| Disk | `(total - available) / total` with space available to the current user, for each checkout's filesystem, `.orbit/state/worktrees` (or its nearest existing ancestor) and the global Orbit root (`~/.orbit` or the serving root override). | Same. |

On macOS, [XNU caches host statistics after a shared query quota is reached](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/host.c). Identical CPU tick counters do not mean zero utilization. The shared history retains both ends of the last measured window, so repeated kernel counters reuse a real delta without refreshing the baseline's timestamp. Missing, stale or future-dated baselines require a new sampling pair across the one-second cache window without polling or increasing the query rate; a failed native read remains `unknown` with reason `unavailable`.

Every watched path takes part in admission, but `GET /api/host/resources` reports one `disk` object (highest known percentage, severity and path; `null` when none is known), and a disk hold names only the highest held path. That endpoint covers every active registered local checkout whatever `?workspace=` says (or with none), never remote hosts, and returns severity, sample time and age, thresholds, verdict and reason. It also carries `provider_limits`, the same live readings as readiness's `provider_limits` (below), read through the default workspace's effective config for thresholds and crew coverage, for the dashboard's top bar; the field is `null` when no workspace can be opened or the store cannot be read.

**Dashboard.** The top bar's `load` (CPU as a multiple of online cores), `mem` and `disk` chips, and an `io` chip on hosts that report disk I/O pressure, show the selected host's readings, severity and held resources ([UI design §6](design/user-interface/2_design.md#6-top-level-navigation)); they poll every five seconds and mark aging data unknown even while a request is pending. **Settings › System** shows the enable switch and each resource's high and resume marks, with source, live reading, severity and verdict: six marks for CPU, memory and disk, and, on hosts that report disk I/O pressure (Linux only), the two `io_full_*` marks as well, for eight (`cpu_light_leaves` is edited in the Effective view). Its edits share the Effective view's `PUT /api/config/keys/{key}` validation but write the **global** file by default, because this host's chips and admission read global settings; a workspace override is marked and wins for that workspace's runtimes.

### `[workflow.task_pilot_freshness]` — when a task is piloted again

```toml
[workflow.task_pilot_freshness]
material_fields = ["title", "description", "criteria", "plan", "selectors"]
source_sensitivity = "ignore"
```

| Key | Default | What it does |
|---|---|---|
| `workflow.task_pilot_freshness.material_fields` | `["title", "description", "criteria", "plan", "selectors"]` | **Advanced.** Task inputs whose edit makes an accepted task-pilot assessment stale, so the task is piloted again. At least one of `title`, `description`, `criteria`, `plan`, `selectors` (`context_files`), `tags`, `crew` (the stored crew and its resolved model/provider), `tools`, `type`, `complexity`, `relations`, `dependencies` (each one's status and meaning) and `instructions` (`AGENTS.md`/`CLAUDE.md` at the pinned revision). |
| `workflow.task_pilot_freshness.source_sensitivity` | `ignore` | **Advanced.** Whether a move of the observed branch head makes an assessment stale. `ignore`: never (the pinned revision is still recorded). `context_files`: when the head changed a path a selector names (`file:` and `dir:` by prefix, `symbol:` through its file). `any`: every move. |

**Unprepared backlog work.** A backlog task with no `context_files` and no `no-diff-expected` tag holds no file lock. A local drain or discovery ship with more than one slot (`max_active_leaf_runs` > 1) therefore holds it: while an enabled `preparation_eligible` routine owned by this host (`trigger.state.owner_machine`) would still prepare it, it waits as `awaiting_footprint` for at most that trigger's `max_wait_minutes + deadline_minutes` from the task's last change. When no such pilot exists, the pilot assessed it and left it empty, or the wait elapsed, it starts only when no other leaf is in flight (`awaiting_exclusive_slot`) and holds the whole tree while it runs. A single-slot drain, an explicit `orbit run ship <id>` and owner pull admission admit it as before. `orbit run readiness` reports both reasons with the fix.

A routine's `eligibility` block still decides which tasks are piloted: a newly eligible task with no fresh assessment is piloted, while retagging an assessed, still-eligible task is not. A task-pilot routine's `trigger.state.freshness` overrides either key (routine, then this table, then defaults). A non-default value is itself material, so changing it re-pilots tasks assessed under the old value once. Assessments from before this setting existed stay fresh while their task is unchanged. See [automation triggers](design/automation-triggers/5_operations.md).

An `--approve-proposed` drain checks freshness under the approval lock against the exact material certified by the pilot's atomic write. The pilot's OS routing tag, selectors, complexity and automatic crew redraw therefore permit approval; later tag, crew, assignment provenance, relation or configured material edits withhold it. The `no-auto-approve` opt-out always wins. Replayed writes use their recorded audit rather than the task's current values; a pending approval from an older audit without that record needs a new assessment. Approval preserves routing and native-host admission holds.

### `[workflow.mid_run_steering]` — operator comments after dispatch

```toml
[workflow.mid_run_steering]
enabled = false
max_rounds = 2
```

| Key | Default | What it does |
|---|---|---|
| `workflow.mid_run_steering.enabled` | `false` | Record, in the run's durable state, the newest task comment each agent prompt that embeds task context included: the run's *steering watermark*. Deliver operator comments written after it at the run's later prompts, and hold the shipped delivery pipelines before `commit` while one is undelivered. `orbit init` does not enable it. |
| `workflow.mid_run_steering.max_rounds` | `2` | **Advanced.** Steering rounds a run may consume at the promotion gate before it holds as `steering_unconsumed` (`1`–`10`). |

The watermark is stored per task under `steering_watermarks` in the run's pipeline state, for the run as a whole and for each step. It names a comment by its position in the task's append-only comment log plus the comment's recorded timestamp, never by a clock reading, so a comment written in the same instant as the prompt still orders by position. It only advances. It belongs to the run: a resumed run starts without one, and two runs of a task never share it. Agent output cannot write it.

The run's first task-context prompt for a task records the watermark. A persisted run with no watermark first delivers outstanding guidance from its inherited or post-dispatch baseline, including after mid-flight enablement; an admitted steering round also supplies the candidate source the gate inherited. Comments omitted by the bounded task context therefore stay protected. Each later prompt the run builds for that task (a recovery attempt, a repair round, final recovery, a re-dispatch) is a *safe point*: when operator comments written after the watermark are waiting, the prompt's task context leads with a `steering` block naming the run and its snapshot and carrying those comments oldest first. Comments from Orbit's own automation and agents are not steering, and task history entries never are. The block holds at most 32 KiB; past that it keeps its head and tail around a marker that names the full size and `orbit task show <id>`. Building that prompt records a `steering_delivered` event in the task's history, naming the run, the step, the comments carried in full and those cut down, and advances the watermark only past what the block carried in full, so a comment is delivered once per run. A safe point with nothing waiting builds the prompt as before and records no event. A task that no longer accepts implementer writes gets no block. A claimed follower leaf reads the task and its comments from its owner, and the owner keeps the leaf's watermark against its claim and records its `steering_delivered` events. With `enabled = false`, prompts, task records and run state are exactly what they are without the feature; turning it off later leaves recorded watermarks in place, unused. `orbit doctor`'s `mid-run-steering` row shows the resolved values.

With it on, `task_pr_pipeline` and `task_local_pipeline` gate `commit` ([design](design/mid-run-steering/2_design.md#3-promotion-gate)). Before committing, the run re-reads the task, and while an operator comment its prompts never carried in full remains, it does not commit. Each refusal spends one steering round, recorded under `steering_rounds` in the run's state: the implement step runs again, even for a carried candidate, with `input.steering_round` set, and its prompt delivers the comments onto the candidate still uncommitted in the worktree. A run that carries an earlier run's candidate judges from the watermark that earlier run's prompts left, so a comment written after that prompt and before the new run is outstanding; a carried candidate whose source cannot be traced fails the run before `commit` (`[steering_gate_unreadable]`). When the run has spent `max_rounds`, or its last round delivered none of the comments still outstanding (one too long for the block, for example), the task is blocked under a `steering_unconsumed` history event naming each outstanding comment, and the run fails without committing. Read the comments with `orbit task show <id>`, then requeue the task, answer them differently, or accept the candidate as it is through the normal review record. A gate that cannot read the task or the run fails the run before `commit` (`[steering_gate_unreadable]`), and a task that is `done` or `archived` stops it there, naming the steering it never received. A resumed run starts with its full budget of rounds. Both claimed pipelines gate `commit` too, reading the comment log and the claim's watermark from the owner; an owner the leaf cannot read fails the run before `commit` (`[steering_gate_unreadable]`), and a replica never certifies delivery. A claim's watermark outlives its settlement, so the task's next claim or owner run never delivers the same comments again. While an execution claim protects a task, the owner allows human operator comment-only writes; other unscoped writes stay refused. The live leaf reads those comments and records delivery through its owner.

Steering settings refresh at prompt and gate checkpoints. Enabling with runs in flight leaves missing watermarks absent: their gates treat post-dispatch operator comments as undelivered (or use an inherited candidate/claim baseline when present). Disabling stops recording and gating at the next checkpoint; existing watermarks and steering-round state stay inert, not deleted. Invalid config retains the last successfully resolved settings. Prefer enabling between drains; enabling does not change admission authority or turn steering on in `orbit init`. See [migration](design/mid-run-steering/specs/steering-delivery.md#migration).

### `[workflow.regression_risk]` — regression-prone context files in prompts

```toml
[workflow.regression_risk]
enabled = true
window_days = 30
min_support = 5
threshold_percent = 40
max_files = 3
```

| Key | Default | What it does |
|---|---|---|
| `workflow.regression_risk.enabled` | `true` | Mark a task's regression-prone `file:` context files in its implement and review prompts. |
| `workflow.regression_risk.window_days` | `30` | **Advanced.** Trailing days of done tasks a mark is computed over (`1`–`365`). |
| `workflow.regression_risk.min_support` | `5` | **Advanced.** Done tasks in the window whose declared footprint named the file before it can be marked (`1`–`10000`). |
| `workflow.regression_risk.threshold_percent` | `40` | **Advanced.** Share of those tasks, in percent, that a `regression_from` link names before the file is marked (`1`–`100`). |
| `workflow.regression_risk.max_files` | `3` | **Advanced.** Most files one prompt marks, highest regression rate first (`1`–`20`). |

Analytics › Codebase knows which paths regress; these marks tell the agent that edits one, and its reviewer. An activity asks for marks by declaring a `regression_risk_instruction`; the shipped `agent_implement`, `agent_review_repair` and `review_reconciliation_review` do. For each canonical `file:` selector of the task, Orbit counts, among the tasks that became `done` in the trailing window and declared the file, those a `regression_from` link names, the same rate as the Regressions ranking at file grouping. The file is marked when it has at least `min_support` such tasks and at least `threshold_percent` of them regressed, so a file few tasks touched, or a hub every task touches, stays unmarked, and a fixed hotspot drops out once its regressed tasks leave the window. At most `max_files` files are marked, by rate, then regressed count, then path.

Marks reach the agent as `task.regression_risk` in the envelope: `window_days` and `marks`, each with `path`, `regressed` (`"N of M touching tasks"`), `nature` and `escape` (the one or two values most of the file's links record; a link without a typed classification reads `unclassified`, never a guess) and `lesson`, the newest regression task's title cut to one line, with internal task and friction IDs removed (omitted when nothing is left). The activity's `regression_risk_instruction` follows the main instruction and maps each nature and escape to one check: for example `no_test` asks the implementer for a focused regression check at the public boundary, `merge_reintroduced` to re-verify the touched area after a rebase, and `contract_drift` to update the affected docs or contract in the same diff, and the reviewer checks those categories on the marked files. With the switch off, a task without `file:` selectors, or no file qualifying, the envelope has no `regression_risk` key and the instruction is unchanged. `orbit codebase show <path>` and `orbit.codebase.show` give the tasks behind a mark.

Each prompt that carries marks records them on the task as a `regression_risk_marked` history event whose decision names the run, the activity, the marked paths and the window, once per run, activity and paths. A claimed follower leaf computes marks from its own records and records none, since it cannot write its owner's task. The Regressions ranking reads these events back and compares the marked paths' regression rate for tasks delivered with marks against the same paths' rate in the window before the first mark, with the counts shown ([dashboard](design/user-interface/2_design.md)). Building marks scans task metadata and the history of tasks updated in the window; a scan that fails, or a record that cannot be written, is logged and never refuses the dispatch. Settings are read when the runtime opens.

### `[workflow.tag_instructions.<tag>]` — extra instructions for tagged tasks

```toml
[workflow.tag_instructions.migration]
implement = "docs/agents/migration-implement.md"
review = "docs/agents/migration-review.md"

[workflow.tag_instructions.ui]
review = false            # switch off Orbit's shipped reviewer default
```

| Key | Default | What it does |
|---|---|---|
| `workflow.tag_instructions.<tag>.implement` | unset | File whose text `agent_implement` receives for a task tagged `<tag>`. QA sweeps run as implement tasks tagged `qa-sweep`, so `[workflow.tag_instructions.qa-sweep] implement = …` instructs QA. |
| `workflow.tag_instructions.<tag>.review` | unset | File for the reviewers: `agent_review_repair` and `review_reconciliation_review`. |
| `workflow.tag_instructions.<tag>.recovery` | unset | File for `step_failure_recovery`, `final_recovery` and `pr_conflict_recovery`. |

Each value is a path relative to the repository root (`/` separators, no `..`, no absolute path), or `false` to switch off Orbit's shipped default for that tag and role. Tags match ignoring case, and two tables naming the same tag in different case are a config error. At most 32 tags may be mapped. The tables layer like crews: a workspace entry replaces the global one for the same tag and role and leaves the others. Edit `config.toml` directly; `orbit config set` does not write these keys.

Orbit ships defaults for three tags. `no-diff-expected` gives implementers the read-only handoff rule (no workspace gates, no `no-diff.json`). `skip-validation` gives implementers the [light delivery](#light-delivery-skip-review-and-skip-validation) rule: run no local validation while the change stays within `workflow.light_delivery_paths`. `ui` gives implementers the screenshot rule and gives reviewers the check that screenshots are attached. A mapped file replaces the default for that tag and role. The task-execution and task-review references still carry the full screenshot rule, because it follows the diff rather than the tag.

**Where the text comes from.** Instruction text comes only from Orbit's shipped defaults or from the mapped file as committed at the delivery's base revision: the `base_sha` the run's `worktree` step recorded, or else the checkout's `HEAD`. Orbit reads the file from that commit, never from the worktree, so neither a delivery's commits nor its uncommitted edits change the instructions it runs under. A task's description, comments and tags never supply text; a tag only selects.

**What the activity receives.** After the activity's built-in instruction (and any steering or inspection text), the prompt gains one `Tag instructions` section. It states that the text adds to the instructions above and cannot override them, then gives each tag's text under a ``Tag `<tag>` (<source>)`` label. Tags apply in sorted order, once each. An untagged task, or a tag nothing maps, adds nothing.

**Refusals.** A mapped file is refused, and the activity fails without starting its provider, when it is missing at the base revision or not UTF-8 or empty (`tag_instruction_unreadable`), or larger than 8 KiB (`tag_instruction_too_large`). It also fails when one prompt's tag text would exceed 16 KiB (`tag_instructions_too_large`). Each error names the tag, the role and the file. A refused file affects only tasks carrying its tag.

**Claimed leaves.** When an owner admits a pull claim, it captures every instruction it would apply, with its files read from the claim's authoritative base, in the claim's receipt. PR delivery uses the owner's last-fetched `origin/<base>` ref, even when a local branch of the same name exists; local delivery uses the local base ref. Both accept a bare branch name or an `origin/` prefix. Capture does not fetch, and an unavailable authoritative ref is captured as a refusal rather than falling back to another branch or `HEAD`. The leaf applies that capture rather than its replica's mapping or files, and a refusal the owner captured refuses the leaf's activity. A claim from an older owner that captured nothing falls back to the replica's own mapping, with a warning.

**Seeing what applied.** Each dispatch that received tag instructions records an `activity.tag_instructions_applied` audit event naming its step, activity, task, and each instruction's tag, role, source (`default`, or `workspace` with path and revision), SHA-256 and size, but not the text. `orbit run show` prints a `Tag instructions:` header line per dispatch, and its JSON (and the `orbit.run.show` tool) carries them under `tag_instructions` (`state`: `observed`, `none` or `unavailable`).

### `[workflow.validation_env]` — the toolchain required validation runs with

```toml
[workflow.validation_env]
shell_probe = "interactive"
path = []
path_mode = "prepend"
```

| Key | Default | What it does |
|---|---|---|
| `workflow.validation_env.shell_probe` | `interactive` | How PATH and toolchain locators are resolved from the owner's shell (below): `interactive` tries `-i -l -c` first, so toolchains exported in rc files are found, then `-l -c`; `login` probes only `-l -c`; `off` never starts the shell. Replaces the deprecated `login_shell` (`false` is `off`) and `interactive` (`false` is `login`) booleans, which still load with a warning. |
| `workflow.validation_env.path` | `[]` | PATH entries to add. A leading `~/` expands to `HOME`. Agent sessions get them too ([below](#agent-sessions-share-the-configured-path)). |
| `workflow.validation_env.build_output_env` | `[]` | Output environment variable names to bind to managed host output for required validation and host evidence; see [build output](#workspace-build-output). |
| `workflow.validation_env.cargo_incremental` | `false` | **Advanced.** When a host validation target is bound, force `CARGO_INCREMENTAL=0` on the validation command. `true` leaves the variable untouched. See [incremental compilation](#incremental-compilation). |
| `workflow.validation_env.path_mode` | `prepend` | **Advanced.** `prepend` puts `path` first; `replace` makes it the whole PATH. |

Required validation commands and `local_shell` steps run in this environment over the [allowlisted agent environment](#executionenv--the-agent-subprocess-environment), so a drain started from a minimal PATH (a service manager, `env -i`, SSH) still finds the toolchain.

**Probe.** The shell is the account's user-database shell, else `$SHELL`, else `/bin/sh`. Orbit runs `<shell> -i -l -c …` (login profiles plus interactive rc files: `~/.zshrc`, or `~/.bashrc` when the bash login profile sources it), falling back to `<shell> -l -c …` on startup failure, nonzero exit, timeout or a missing environment marker. Stdin is null; output before the marker and stderr noise on success (bash's job-control warning) are ignored. Each attempt is limited to 10 seconds; the whole outcome, fallback included, is cached two minutes. Only `PATH` and the toolchain allowlist cross: `CARGO_HOME`, `RUSTUP_HOME`, `GOPATH`, `GOROOT`, `GOBIN`, `JAVA_HOME`, `PYENV_ROOT`, `NVM_DIR`, `VOLTA_HOME`, `PNPM_HOME`, `BUN_INSTALL`, `HOMEBREW_*`.

**Recorded.** Each validation log and step output records `validation_env`: the PATH; its `source` (`config` when `path` has entries, else `login_shell` when the probe returned a PATH, else `launcher_fallback`); the shell and any probe error; `probe_mode` (`interactive_login` or `login`, else null); and `fallback_reason` (why interactive startup failed when login-only succeeded, else null). When both probes fail, `login_shell_error` holds both.

**Diagnostics.** `orbit doctor`'s `validation-env` row shows the probe mode, fallback, PATH and the `python3`, `git` and `make` it finds, and warns (advisory) on a fallback or an earlier PATH entry shadowing a different executable (`/usr/bin/python3` before `/opt/homebrew/bin/python3`); duplicates and symlink aliases count once ([health checks](runbooks/health-checks.md#run-orbit-doctor)). While required commands are configured, `orbit run auto`, `orbit run ship`, and MCP `orbit.workflow.auto` (`status`, `start`) and `orbit.workflow.ship` warn when the shell cannot be probed, when resolution is off and `path` is empty, or when the resolved PATH drops login-shell entries (possible under `replace`).

Readiness snapshots (CLI, dashboard, fleet drains and MCP drain status) reuse the advisory `validation_env_warning` across processes for two minutes in the local checkout’s `.orbit/state/validation-env-warning.json`. The key includes the launcher environment, shell, validation policy and Orbit version; environment or policy changes invalidate it immediately. Profile edits become visible as the process probe cache and diagnostic cache expire. Missing, invalid or unwritable cache files fall back to normal resolution. This cache holds only the warning, never the environment used to run validation or submit a drain.

#### Workspace build output

`workflow.validation_env.build_output_env` maps each listed variable to one managed,
absolute host output directory. Commands must honor it, directly or through their
build-tool configuration. There is no toolchain-specific default. An empty list
allocates nothing and leaves inherited variables unchanged; command output is then
owned and cleaned according to the command's own configuration, not this contract.

For this Rust repository, merge [the repository configuration example](../config/orbit-workspace.toml)
into the owner's ignored `.orbit/config.toml` during the separate operator rollout:

```toml
[workflow.validation_env]
build_output_env = ["CARGO_TARGET_DIR"]
```

A non-Rust workspace can select its own locator and consume it in a required command:

```toml
[workflow]
required_validation_commands = ['sh scripts/validate.sh']
[workflow.validation_env]
build_output_env = ["DIST_DIR"]
```

Create the example validation script in that workspace to invoke `cmake -S . -B "$DIST_DIR"` and
`cmake --build "$DIST_DIR"`. Orbit supplies no Cargo variable to that workspace.
Multiple listed variables refer to the same directory; commands can use subdirectories
below it to separate outputs. Arbitrary placement outside managed host state is not
supported by this contract.

**Precedence.** Settings resolve built-in `[]`, then global, then workspace. An explicit
workspace list replaces the global list, including `[]`. Names are trimmed,
deduplicated and sorted, and must be shell environment identifiers. Execution identity
variables (`PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `TMPDIR`, `TMP`, `TEMP`, `PWD`,
`OLDPWD`) and `ORBIT_` variables are reserved, case-insensitively for portability. After allowlist and login/PATH resolution,
configured bindings replace inherited values, even non-empty ones. They are applied
only to host required-validation/replay/evidence commands, including baseline and
recovery/reconciliation, never to agents or ordinary `local_shell` steps. A command
that overrides the binding or ignores it gives up output isolation; configure such a
command to consume the locator before admitting it as required validation.

**Placement and provenance.** On every platform, configured output is
`<primary-repository>/.orbit/state/host-validation-targets/<sha256(canonical-source-checkout)>`.
The primary repository is derived from the Git common directory; redirects in managed
output path components are refused. This host execution context stays outside the
worker's writable build areas. Different canonical source checkouts cannot share
compiled paths; repeated host checks for the same source may reuse output. Records
carry `build_output.env`, `directory`, `source` and `context: "host"`; an unconfigured
run has empty `env` and null directory/source. Baseline cache schema 9 includes the
bindings in its key, retaining prior records rather than certifying this contract with
old results. Recovery fingerprints also include the bindings.

**Cleanup and transition.** The existing host-validation-targets layout, source keys,
run attribution, base-cache locks and scratch lease lineage stay unchanged. The
[descriptor-bound collector](runbooks/worktree-reclaim.md#host-validation-output) runs
on native Linux and macOS and continues to reclaim eligible historical and new output
after its checkout disappears; active, leased and ambiguous output stays. Host output on
other platforms remains isolated but this collector is not available there. Retain it
for operator maintenance; do not infer deletion authority from its hash. No old output, failed-run logs or audit evidence
is migrated or deleted by config resolution. Before deploying this runtime to a Rust
workspace, install the explicit binding in its owner configuration, preserving existing
settings; restart/upgrade active clients only through the supported operator procedure.
Existing clients keep the same valid source-keyed output namespace. An unconfigured
new client no longer receives the old implicit Cargo policy. Live rollout and storage
verification are separate operator work.

#### Incremental compilation

A host validation target is keyed by its checkout and is built once per candidate, so Cargo's incremental cache there is write-only. When a binding is configured, required validation, host replay, detached evidence, baseline reruns and recovery set `CARGO_INCREMENTAL=0` on the child, replacing any inherited value. The variable is applied where Orbit spawns the command, not by the repository Makefile. `workflow.validation_env.cargo_incremental = true` is the opt-in that leaves `CARGO_INCREMENTAL` untouched, so Cargo's profile default (incremental on for dev) or an allowlisted value stands. With no binding, Orbit does not set the variable. An incremental directory already in a target is left in place; later builds do not add to it. Baseline result identity is unchanged: the setting changes compiler writes, not command results.

#### Agent sessions share the configured PATH

A reviewer that resolves a different `python3` or `make` than host validation reports checks red that the host then refutes. So every agent session — the implementer's and reviewer's provider CLI, and the commands it runs through `proc.spawn` — starts with `workflow.validation_env.path` ahead of its [allowlisted](#executionenv--the-agent-subprocess-environment) PATH. The entries are prepended under either `path_mode`, since the rest of PATH still has to find the provider CLI. Every other entry of the resolved required-validation PATH, including what the login-shell probe found, is appended when the session's PATH lacks it, so a reviewer finds each tool required validation finds without building a PATH wrapper by trial. Appending keeps the session's own order deciding which copy runs; toolchain locators such as `CARGO_HOME` still come only from the allowlist. Implementer and reviewer sessions share this one environment. Settlement does not rerun a disputed check in the reviewer's environment: host validation stays the reference.

Codex runs each tool command through the user's shell as a login shell (`zsh -lc`) by default, and the profiles it rereads can reorder PATH: on macOS `/etc/zprofile` runs `path_helper`, which moves inherited entries behind `/etc/paths`, so `/usr/bin/python3` (3.9) would win over Homebrew's. While `path` has an entry, Orbit starts Codex with `--config allow_login_shell=false`. Its commands then run in a non-login shell (`zsh -c`, which still reads `~/.zshenv`) with the composed PATH, and Codex refuses a model request for a login shell. No `~/.zprofile` change is needed. In exchange, Codex commands no longer see variables only `~/.zprofile` or `~/.zlogin` sets; their PATH entries still arrive through the appended validation PATH. List a toolchain directory in `path` when its copy must win over the launcher's; after the session's PATH Orbit appends `~/.local/bin`, `~/.orbit/bin`, `~/.cargo/bin`, `~/bin`, `/opt/homebrew/bin` and `/usr/local/bin` when absent. With `path` empty, Codex keeps its login shell. Claude Code runs commands in a non-login shell and keeps the composed PATH. Another provider whose shell tool starts a login shell rereads profiles; check it with `zsh -l -c 'command -v python3'` against `orbit doctor`'s `validation-env` row, which warns when an agent session would resolve `python3`, `git`, `make` or `cargo` to a different executable than required validation.

#### Required validation

`task_pr_pipeline` and `task_local_pipeline` run `workflow.required_validation_commands` on the exact candidate before push or merge and attach each command's log to the task.

- Commands run sequentially in list order, and the first failing command fails the step, so put cheap gates before long test suites ([rollout](DEVELOPMENT.md#required-validation-commands)).
- These pipelines and the claimed leaves set `final_validation` on their implementer's input, and its envelope lists the commands this validation will run as `task.final_validation_commands`: this host's list, or the owner's list a claim captured. The implementer leaves those to the pipeline for a diff and still runs focused checks and every workspace check the list omits. A clean no-op or already-landed handoff skips this validation, so its implementer still runs every required command. The field is absent for an empty list, for a claim whose admission captured no owner gates (it validates against this replica's copy, which the owner may refuse), and outside these pipelines, such as a manual or direct run; the implementer then runs every check itself.
- The PR pipelines also pass that handoff to conflict recovery at `sync_base`.
  The executor proves a later resolved deterministic validator with the same
  task, run and worktree bindings and the synchronized base. Its guard must
  be unconditional or shared with the rebase, and its checkpoint must not
  skip execution. A copied flag or activity name without that continuation,
  an unknown compound continuation, or nested recovery keeps advisory checks.
  Claimed recovery also requires this run's trusted worker binding and the
  failed rebase's rendered task/run identity to agree with it.
  Recovery keeps focused resolution checks and commands outside the captured
  list. The host validates after the final rebase continuation and base chase;
  if the base absorbs the candidate, `git_rebase` runs and attaches those
  commands before no-diff settlement instead. Standalone and later completion
  recovery keep their advisory validation. A new head invalidates old evidence
  normally; this handoff adds no validation cache. A repair that changed
  only unlocked paths may run a narrower list instead
  ([revalidating an unlocked-path repair](#revalidating-an-unlocked-path-repair)).
- A failure goes to step recovery, except a [missing tool](#missing-tools-block-the-task) or a failure the base shares ([held](#a-red-base-is-held-not-blocked) until the command passes on a new base tip). [Network-inconclusive](#network-flakes-are-rerun) failures are rerun first.
- Distributed handoffs must carry exact-candidate validation matching the owner's list. A before-PR claim also freezes the list in the owner's review contract at admission; a later owner change refuses the handoff rather than replacing the snapshot.
- A `no-diff-expected` task with a clean tree runs none of these commands, as the owner's pipelines skip `validate` for the tag. A claimed one hands off a `NoDiff` skip with no validation logs; the owner accepts it only while its own copy of the task keeps the tag. A clean tree that proves itself with `no-diff.json` or `already-landed.json`, and a tagged task that produces a diff, still validate.
- Re-running a task whose failed run preserved a candidate from `commit` or later runs the commands on that candidate over the new base, to decide whether implementation runs at all. A candidate preserved from implementation or earlier always returns to the implementer ([re-running a task](../crates/orbit-core/assets/skills/orbit-orchestrate/references/workflows.md#re-running-a-task-with-a-preserved-candidate)).
- An empty list runs nothing, including on claimed handoffs; the other handoff guards still apply.
- Required validation, host replay, detached evidence, baseline reruns and recovery use the workspace's [build output bindings](#workspace-build-output). The runtime allocates no build output by default. Configured bindings isolate host output from worker builds, and logs record `validation_env.build_output`.
- An implementer whose affected-test gate passed with only deferral notices of sandboxes its lane refuses hands that gate off instead of failing, by naming the validation summary the gate wrote; Orbit derives the `deferred_sandbox_validation` record from it and runs each notice kind's probe in the implementer's lane itself [ORB-15516]: `DEFERRED: bubblewrap unavailable:` notices on Linux [ORB-15287], `SKIP: sandbox-exec cannot apply a profile:` notices on macOS, as a Mac claimed implementer's nested Seatbelt prints [ORB-15409], and `DEFERRED: ps denied:` notices from a claimed implementer on macOS, whose sandbox refuses to exec `ps` [ORB-15587]. One record may defer several kinds, such as a Mac claimed leaf's Seatbelt and ps-denied notices together, when every kind's probe refuses [ORB-15692]. The gate must be a required command or a `review.baseline_commands` entry. Validation runs it after the required commands, outside the agent sandbox (a claim's `claim_validate` on its claim host, before publication), on this run and on a resumed candidate: a pass that still prints any of these notices holds the candidate as a `validation_environment` failure, and a run that reports no executed tests is refused ([development guide](DEVELOPMENT.md#test-process-environment)).
- An implementer may hand off a criterion the task pilot typed `post_deploy` (evidence only the deployed build on a named host can produce) as `owed_criteria`, naming the host, measurement command and target, instead of failing. The implement step admits each against the pilot's finding and refuses any other owed criterion; a claimed handoff carries the list, and the owner admits it again on acceptance. The delivery runs these commands, review and landing as usual, then settles in `review` even under `--complete` or `workflow.distributed_completion = "done"`, until an operator records every outstanding criterion met with a `post-deploy: met <N>` comment (a `post-deploy: failed <N>` measurement keeps it outstanding). Claimed landing reads persisted obligations, so a repair or replacement handoff that omits `owed_criteria` preserves any earlier owed or failed criterion. Owed or failed criteria also refuse ordinary task updates to `done`, approval, desktop review completion, automation and final recovery; every owed criterion must be recorded `met` before completion. The human `--force`/dashboard force override remains available and records a `forced` status event without changing the measurement record ([task-execution reference](../crates/orbit-core/assets/skills/orbit/references/task-execution.md#criteria-owed-after-deploy)).

#### Missing tools block the task

A command that fails for lack of a tool says nothing about the candidate. Orbit detects exit status 127, a shell `command not found` / `not found` line, `make`'s `Error 127`, `No such file or directory` for the program, a missing cargo subcommand, or a guardrail's "`<tool>` is required … install" line for a tool absent from PATH.

The step fails with the `[validation_environment]` marker and error code `validation_environment`, naming the tool, PATH and source. The diagnostic includes the full searched PATH, even when it exceeds the bounds used for other diagnostic fields. The durable log preserves that same full PATH in its output and `validation_env.path`, and records `failure_kind: "environment"` and `missing_tool`. No step, final or blocked-task recovery runs, and no review, rework or recovery budget is spent. The handoff pushes nothing, opens no `[BLOCKED]` PR, and blocks the task under `validation_environment_blocked` with the candidate kept in its worktree. Fix the environment, check `orbit doctor`, then `orbit job resume <run>`. A claimed leaf skips repair the same way: its claim settles as a failure carrying the diagnostic and is not released for another follower to implement again.

#### Supervisor stops are inconclusive

Output-capture stops, incoming signal interruptions and children terminated
without an exit code are validation-environment failures. The shared runner
uses structured metadata from the process supervisor for candidate and claim
validation, base comparisons and hold refresh, review checks and resumed
candidates. A stopped base is inconclusive and is never cached. Printing a
supervisor diagnostic on stdout or stderr cannot establish a stop: a normal
nonzero exit still gets compared with the synchronized base. The failure
message still names the first failure the captured output shows (a test
panic and its message, or else the first compiler error), so a stop caused
by a flood of failures says what they were.

#### Network flakes are rerun

Network-inconclusive output (HTTP status `000`; a curl connect, resolve, timeout, TLS or receive error such as `curl: (6)`, `(7)`, `(28)`, `(35)`, `(56)`; a DNS resolution failure; a TLS handshake timeout) reruns the command up to twice, after 2 s and 6 s. Only the last attempt counts; the log records `network_retries`.

#### A red base is held, not blocked

The integration branch has no merge gates, so a command may already fail on the base. A command still failing after its reruns is rerun on the base commit in a clean detached worktree under `<git-common-dir>/orbit-baseline/`, cached per base commit, command and selection behind a file lock (concurrent candidates on one host share one base run). `validation/<run>/<n>.json` gets a `baseline` record; `validation/<run>/<n>.baseline.json` holds the base log. Candidate logs record `failures` and `baseline_reproduces`; baseline records and logs retain their parsed `failures`. The baseline record's `decision` describes the base run's outcome separately from whether it reproduces the candidate.

| Base run | Outcome |
|---|---|
| Comparable completed failure with the same exit status, and every identified candidate failure is a test that fails there too | Known red: the candidate proceeds (below). |
| Comparable completed failure with the same exit status and every candidate failure identity confirmed on the base, including compiler or lint errors | The task is held (below). |
| Fails with different or additional candidate identities, missing identities on either side, or a timeout | Matching failures are unconfirmed; candidate repair remains enabled, with both logs preserved. |
| Passes | Candidate failure; the error says the base passed, so step recovery can repair it. |
| Inconclusive (missing tool or network) | Candidate failure; nothing cached. |
| Not comparable (below) | Candidate failure; the `baseline` record's `decision` is `not_comparable`. |

**Known red.** The candidate introduced none of the base's failures when the candidate run did not time out, printed no host coverage refusal (sandbox-unavailable diagnostics, `DEFERRED:`, `SKIP:`, `skipping:` or `skipping ` notices), names at least one failure, and every failure it names is a failed test that also fails in the base run. Both runs' failures and the candidate's host coverage notices are read from the complete output, not the bounded log, using the identities a hold records (below). A declared deferred-sandbox replay must report executed tests before known-red continuation, including when it exits nonzero; a missing or zero executed-test count refuses the replay. The step does not fail: it records the command under `known_red` in its output (decision `passed_with_known_red`), marks the command's log `failure_kind: "known_red"` with the failures, appends a `known_red_validation` history event naming command, base commit, candidate commit and failing tests to each delivered task, and goes on to the next command. `pr_open` lists that run's known-red commands, for the published head and base, under `## Known-red validation` in a body it opens; one found by a later revalidation, after the PR exists, is only on the task and the run. The red base keeps its repair task through other holds. A compiler or lint error with an identity confirmed on the base can be held as below. Missing identities, timeouts, or any candidate identity absent from the base cannot establish an inherited failure and leave the candidate requiring repair. Host coverage refusals still follow the validation-environment rules. A resumed candidate is judged the same way.

**Hold.** The step fails with the `[baseline_red]` marker and error code `baseline_red`, and the run ends `held`, not `failed`; the gate and auto parents waiting on it pass it as held. No step or final recovery runs and no budget is spent; the handoff pushes nothing and opens no `[BLOCKED]` PR. The task moves to `backlog` under a `baseline_red_hold` history event naming base ref, base commit, command and what fails on the base (run finalization does this for a local pipeline), with the candidate kept in its worktree. The hold's `failures` record keeps up to 20 failure identities the base output names (each nextest `FAIL [..]` summary line as `test <binary-id> <test>`, a libtest `test <name> ... FAILED`, a compiler or lint error at its location) and counts the rest; when the output names none, it keeps the last 5 lines of that output instead. Admission withholds the task; `orbit run readiness --json` reports `reason: "baseline_red_hold"`, and its `detail`, like each hold verdict, names the failing tests. Operator steps: [stuck job runs](runbooks/stuck-job-runs.md#a-task-held-for-a-red-base).

**Repair task.** The owner's clock tick files one repair task per red base failure: the first standing hold for a command and failure set files a `bug` in the workspace's `backlog` (priority `high`, complexity `medium`, tag `baseline-red-repair`, external ref `baseline-red:<digest>`) naming the failures, the first red base commit and the held task. Every later hold with the same command and failures, on another task or a later base, is added to that task as a `related_to` relation with a comment. Once it is done, rejected or archived, a hold still naming the same failures files a new one. A hold that records no failures, such as one written before holds kept them, files nothing. A hold whose base already has a passing result for its command lifts on that tick without filing or linking a repair. New holds retain a SHA-256 digest of the complete trimmed, sorted, deduplicated failure identities before limiting their display to 20 entries and a 240-byte prefix plus an ellipsis for long entries. Repair identity uses that digest, so input order, duplicate names and display truncation do not change which complete set is tracked. Older holds without a digest retain their original display-based repair key and still find existing open repairs; a new digest-bearing hold can therefore file a separate repair from an older hold for the same failures. Output-only failures continue to use the bounded output tail.

**Release.** The held command must pass on a new base tip. When a hold records runnable failing test identities and a selection, refresh first reruns only those tests: a comparable failure keeps the hold without running the full command. If those tests pass, or their replay is inconclusive or not comparable (including a command that ignores the test filter or a renamed test that executes nothing), refresh runs the full recorded selection before lifting. A hold without recorded tests, with a non-test or shortened identity, or without a selection runs the full command directly. Narrowed and full runs retain separate cache entries per base tip, command and selection; cached full results take precedence, and cached narrowed failures also suppress another refresh on the same tip. A failed or inconclusive full run keeps the hold. `orbit clock tick` never runs it (it holds the sweep lock): it reads the cached base result for the new tip, or dispatches one `baseline_hold_refresh_pipeline` run per workspace at a time to run it and record a `baseline_red_hold_verdict` event. An inconclusive attempt suppresses refreshes for that tip and command for 15 minutes; a new tip is checked at once, and a conclusive cached result overrides the attempt. Readiness, the backlog snapshot and pull admission only read verdicts, so a hold with none stays held. The next run resumes the preserved candidate.

**Self-selecting commands** (`make ci-test-affected`) report their selection and executed-test count through `ORBIT_VALIDATION_SUMMARY`, and the base run reruns the candidate's selection from `ORBIT_VALIDATION_SELECTION`; a base run with another selection, or a pass with no counted test, is not comparable ([validation summary](DEVELOPMENT.md#validation-summary-and-base-reruns)). A hold records the selection and lifts only when that selection passes on a moved base with at least one test executed. A hold from before selections were recorded is judged by exit status, and a still-red base re-holds the next delivery with the selection. A command that writes no summary has no selection check, but reproduction still requires a completed failure with the same exit status and a nonempty candidate failure set covered by the base identities.

**Followers.** A claimed leaf validates before it pushes or opens a PR and judges a red base by the same rule against its own base run. A known-red command does not stop it: `claim_validate` reports it under `known_red` (decision `passed_with_known_red`), the leaf's `pr_open` lists it under `## Known-red validation`, and the command's claim validation log keeps the non-zero exit with a `known_red` report naming the base commit, the selection and the complete failing set. The owner does not take that report on the leaf's word. Before it accepts the handoff it judges each reported command it requires itself, on the handed-off base, which must be in the history of the base branch the owner captured when it admitted the claim (this membership rule applies to both same-OS and cross-OS settlements; a base off that branch is refused without running a validation command). For same-OS settlements, it uses a full result it has cached for that base, command and selection, else a replay of only the reported tests when the command reports a selection, else the whole command. When the follower was admitted on the same host OS as this owner, it accepts only when its own base run fails every reported test, the same way when the whole command ran, and then appends the `known_red_validation` history event to the task. A different host OS is a different validation environment: after checking that same base-branch membership, the owner does not run or consult its own base validation for that report, does not release the claim as `known_red_contradicted`, and accepts the follower's base evidence for the admitted OS, recording the same history event. The handoff is accepted, so a pull request the leaf opened stays this delivery rather than being left open while the task returns to the backlog. Owner settlement shares a ten-minute deadline across its base comparisons, below the fifteen-minute routed delivery budget, leaving room for observation, cleanup and the reply. Lock waits and base commands use the remaining time. An attempted base run that is cut or otherwise inconclusive is retained under the base, command and replay selection separately from conclusive base results: settlement retries reuse that refusal rather than starting another cold run. Partial failures from a cut never confirm known red, and ordinary candidate validation and reviewer base replays do not read these inconclusive claim records. A later conclusive base result may supersede the refusal. When the follower was admitted on this owner's OS, a report its own completed, comparable base run contradicts (the base passes the reported tests, fails without one of them, or fails the whole command with another exit status) can never be accepted, so the owner ends the claim instead of refusing it: it releases the task to the backlog as a `transient` failure whose reason, led by `known_red_contradicted`, names its verdict and counts against the task's release budget, and the follower closes its pending settlement obsolete. A report it cannot establish either way (the base run is inconclusive or not comparable, the report names another base, or the base is off the owner's base branch) refuses the settlement while the owner keeps the claim, and the leaf retries it; a non-zero log without a report is refused as before. A candidate failing a test its base lacks requires candidate repair; equal exit status alone never creates a `baseline_red_hold`. The candidate and base logs retain their complete parsed identity sets and the reproduction decision even when bounded output omits a failure. An owner running a release without this rule refuses a log carrying a report until it is upgraded.

**Candidate-only flakes.** A comparable passing base triggers one isolated candidate replay when the captured failure set contains at most three tests and the command reports a replayable selection. The affected-test gate runs only those tests serially, with retries disabled. A pass must report the exact selection and executed coverage without host refusals. Delivery then proceeds with `flaky_validation` history, captured original/base/replay evidence and a PR section naming the tests; each command/test signature is filed once as a workspace friction. Claimed handoffs carry the same pinned evidence, which the owner validates and records at acceptance. A repeated failure, an over-bound set, or missing comparable evidence keeps the normal failure handling. See [validation summaries](DEVELOPMENT.md#validation-summary-and-base-reruns).

**Reviewer claims.** A before-PR reviewer can attach a `baseline` claim to a further failing check. Settlement runs on the host, outside the reviewer's sandbox, so it reruns only `workflow.required_validation_commands` and `review.baseline_commands` entries (never the reviewer's command text) on the final candidate and the pinned base, using the lists captured at admission. On the base it reads a full result already cached for that exact base, or else, when every failure the candidate run printed is a test and the command reports a selection, replays only those tests (`failing_tests`, [validation summary](DEVELOPMENT.md#validation-summary-and-base-reruns)); other failures replay the whole selection. `review-baseline.json` records the path as `base_replay` (`cached_failure_set`, `failing_tests` or `full_selection`). Each claim's host runs get `review.minutes` of wall clock, the last quarter reserved for the base: the candidate rerun is stopped at three quarters (each run still at most 45 minutes), and a stopped run settles `incomplete` under `baseline_cut`, naming the cut and the failures it printed that the base shares. A confirmed claim with nothing else open holds the task as above, unless its candidate failures are known red by the rule above: then the review settles `accept` (`accept_with_fixes` over a reviewer repair), before the PR and before landing alike. The record stays `failed`; the certificate's `known_red` names the command, base, final candidate, run and tests, `review-baseline.json` records the check's `decision` as `known_red`, the step output carries `known_red`, and each task gets the same `known_red_validation` history event, which `pr_open` lists in the PR body. Outcomes for refuted or uncheckable claims: [review-gate design §4](design/review-gate/2_design.md#4-what-the-validation-records-establish-orb-11528-orb-11545).

#### Agent-declared blockers stop the run

An implementer that cannot proceed returns `blocker: {kind, evidence}` (a short token and the reason) from `implement_one` or a resolved `agent_implement` target. The step fails with the `[task_blocked_by_agent]` marker and error code `task_blocked_by_agent`; no step retry, step recovery or final recovery runs. On the PR pipeline the handoff commits, pushes and opens nothing, blocks the task under `task_blocked_by_agent` with the kind, and leaves the worktree as the implementer left it, uncommitted files included. `orbit job resume` does not move the task back; an operator does once the blocker is gone. An implementation activity cannot set status `blocked` itself (operators and other activities can). A claimed leaf stops before its next step and final recovery. Blocked-task recovery skips a marked failure note, or for a claimed leaf, a marker in the execution summary.

A blocker whose kind names an Orbit upgrade refusal (`upgrade_pending`, `upgrade_admission_refused`, `orbit.generation_switch_pending` and similar spellings) is the host's state, not the task's. It ends any agent step, reviewers included, with error code `upgrade_pending` instead. The step is not retried or recovered, the candidate is kept, and the task returns to the backlog rather than `blocked`. See [in-flight agent steps during a binary swap](runbooks/upgrades.md#in-flight-agent-steps-during-a-binary-swap).

`step_failure_recovery` declares the same blocker with an `external_blocker` decision when a human must act on a cause outside the run; the post-recovery attempt and final recovery are skipped and the task is blocked as above (or returned to the backlog, for an upgrade refusal). A recovery that writes no decision gets its one post-recovery attempt only if Orbit sees the worktree, the run's base-ref tip or the validation environment change while it ran. Mechanism: [activity-job design](design/activity-job/2_design.md#recovery-authority-is-host-only).

---

## `[crews.<name>]` — which provider-model runs the task

A crew is one provider-model assignment. An activity uses the crew named in its rendered input, otherwise the run's resolved crew.

| Field | Required | Values |
|---|---|---|
| `provider` | Yes | `claude`, `codex`, `antigravity`, `gemini`, `grok`, `copilot`, `cursor`, `pi`, `opencode`, `muse`. See [provider identity](#provider-identity-and-resolution). |
| `model` | Yes | Model ID passed to the provider CLI. |
| `enabled` | No | Default `true`. `false` keeps the crew defined and listed but refuses to run it; see [disabled crews](#disabled-crews). |
| `effort` | No | Reasoning effort (below). Omitted leaves the provider's default. |
| `description` | No | Summary. Trimmed; blank becomes absent. |
| `tags` | No | Discovery labels. Trimmed, blanks dropped, sorted and deduplicated. |

```toml
[crews.sol]
model = "gpt-6.1-sol"
provider = "codex"
effort = "high"
description = "Systems implementation"
tags = ["implementation", "review"]

[crews.gemini]
enabled = false
model = "gemini-3.8-flash"
provider = "gemini"
```

**Built-in crews**, in init's family preference order. A config with no `[crews]` table uses the full set. Existing explicit model pins are kept as written.

| Family | Binary | Crews (model) | Default crew |
|---|---|---|---|
| `claude` | `claude` | `opus` (`opus`), `sonnet` (`sonnet`), `haiku` (`haiku`), `fable` (`fable`) | `opus` |
| `codex` | `codex` | `astra` (`gpt-6-astra`), `sol` (`gpt-6.1-sol`), `luna` (`gpt-6-luna`) | `astra` |
| `antigravity` | `agy` | `antigravity` (`gemini-3.8-flash-high`) | `antigravity` |
| `gemini` | `gemini` | `gemini` (`gemini-3.8-flash`) | `gemini` |
| `grok` | `grok` | `grok` (`grok-4.7`) | `grok` |
| `copilot` | `copilot` | `copilot` (`claude-sonnet-5`) | `copilot` |
| `cursor` | `cursor-agent` | `cursor` (`gpt-5`) | `cursor` |
| `pi` | `pi` | `pi` (`sonnet`) | `pi` |
| `opencode` | `opencode` | `opencode` (`anthropic/claude-sonnet-4-5`) | `opencode` |
| `muse` | `muse` | `muse` (`muse-spark-1.3`) | `muse` |

**Reasoning effort.** Choose capability with the model, then tune `effort` within it.

| Provider | Accepted `effort` | Rendered as |
|---|---|---|
| `claude`, `codex`, `pi`, `muse` | `low`, `medium`, `high`, `xhigh`, `max` | `--effort` · `model_reasoning_effort` · `--thinking` · `--reasoning-effort` |
| `antigravity` | `low`, `medium`, `high` | `--effort` |
| `opencode` | `high`, `max` | `--variant` |
| `grok` (`grok-4.7`, `grok-4.6`) | `low`, `medium`, `high`, `xhigh` | `--reasoning-effort` |
| `grok` (`grok-4.5`) | `low`, `medium`, `high` | `--reasoning-effort` |
| others | none | — |

An unsupported `effort` (`max` on Grok, `effort = "hard"`) is ignored for that crew, never remapped, with a warning naming the file, crew, value and accepted values. `orbit doctor` lists ignored properties; `orbit config set` refuses a value load would drop. A selected crew's provider, model and effort each override an activity's inline baseline.

**Editing.** Crew fields are `crews.<name>.<field>`:

```bash
orbit config set crews.sol.effort high
orbit config get crews.sol.effort
orbit config set crews.gemini.enabled true
```

`config set` refuses invalid values, unsupported provider/model combinations and misspelled fields. It cannot create a crew: add a `[crews.<name>]` table with `model` and `provider` first. `orbit.workspace.list` with `include: ["crews"]` returns each checkout's effective normalized crews with `enabled` (schema version 3), or `crews_error` when its config cannot be read.

**Dashboard.** **Settings › Crews** lists provider, model, usage **Limit** ([provider usage limits](#provider-usage-limits)), effort, tags, layer and **Used by** (inline field labels at 900 px or less). **Used by** is resolved server-side from the default and review crews, the final-recovery and complexity pools, and enabled auto-tasks with an explicit crew whose plugin is active; if the auto-task listing fails, it omits them and logs a warning, and crew writes still work.

### System work draws from the complexity pools

Orbit's own work draws from the [complexity pools](#automatic-crew-pools-by-complexity) like a task does, unless its [purpose pool](#purpose-crew-pools) is configured. The table is the route each kind of work keeps while its purpose pool is unset.

| Work | Pool | Why |
|---|---|---|
| Shipped auto-task templates (code review, QA sweeps, doc duties, …) | the template's `complexity` | The minted task carries no crew, so it draws like any crew-less task. A `template.crew` setting, an explicit template `crew`, and `workflow.purpose_crews.review` for `delivery-code-review` still win. |
| Task pilot (`task_pilot_pipeline`) | `low` | Piloting reads tasks and writes a bounded proposal. |
| Step-failure recovery, PR conflict recovery | `medium` | Recovery diagnoses failures in work of any tier, so it is not low work; medium is also the tier an unassessed task is verified on. |
| [`orbit run auto --auto-fix`](#pr-auto-fix-repairs) repairs (failed required checks and merge conflicts on the drain's PRs) | `low` | The repair is bounded to one named blocker on a candidate that already passed its gates. |

A pilot, recovery or auto-fix activity names its purpose as `crew_config_key: workflow.purpose_crews.<purpose>` (an older asset may still name `workflow.<tier>_complexity_crews`, which draws that pool directly). The run draws once per pool on its first such dispatch: the pool's weights apply, disabled crews and those outside the run's `allowed_crews` are skipped, and crews at a [provider usage limit](#provider-usage-limits) are skipped while another member is left. Every later dispatch of that pool in the run, and a resume, reuses the draw, which the run's state records with the eligible members. An empty or wholly disabled pool falls back to `workflow.default_crew`, as an unassigned task does, and the recorded draw says so. The task pilot and finding verification inspect pinned source with the provider's native file-read and command tools, which only Codex and Claude have, so their draws skip crews of any other provider (Gemini, Grok, Antigravity, ...). A pool with no such member passes to the next tier up, then to `workflow.default_crew`; when that cannot inspect either, the activity fails with `inspection_crew_unavailable` before a provider launches. The pilot's recorded draw names the filter and the crews it skipped.

**Migration.** `workflow.system_crew` still loads for one release but is ignored, with a deprecation warning naming the pools. Orbit no longer defines a built-in `system` crew or aliases it to `[crews.qa]`. A template, job or activity still naming `crew: system` (or `crew_config_key: workflow.system_crew`) resolves through the pools with a warning: a template by its complexity, a job or activity step through `medium`. A user-authored `[crews.system]` table is an ordinary crew and keeps working wherever it is named.

### Purpose crew pools

Each purpose of work can draw its crew from a weighted pool of its own, written like the [complexity pools](#weighting-a-pool):

```toml
[workflow.purpose_crews]
task_pilot = ["luna:3", "sonnet:1"]
failure_recovery = ["sol"]
auto_fix = ["luna"]
baseline_red_repair = ["opus"]
```

| Purpose | Key | Chooses the crew for | While unset or `[]` |
|---|---|---|---|
| Task pilot | `workflow.purpose_crews.task_pilot` | `task_pilot_pipeline`'s pilot activities | `workflow.low_complexity_crews` |
| Failure recovery | `workflow.purpose_crews.failure_recovery` | step-failure and PR conflict recovery | `workflow.medium_complexity_crews` |
| Final recovery | `workflow.purpose_crews.final_recovery` | the [final-recovery](#final-recovery-pool) activity | its own default; there is no other route |
| PR auto-fix | `workflow.purpose_crews.auto_fix` | [`--auto-fix`](#pr-auto-fix-repairs) repairs | `workflow.low_complexity_crews` |
| Automatic review | `workflow.purpose_crews.review` | [automatic review](#review-and-operation--automatic-review): after-landing review tasks, and the reviewer before landing while the next row is unset | the review's existing rules |
| Pre-landing review | `workflow.purpose_crews.pre_landing_review` | before-PR and before-landing review | `workflow.purpose_crews.review` |
| Red-base repair | `workflow.purpose_crews.baseline_red_repair` | tasks the red-base re-check files (`baseline-red-repair` tag) | the task's complexity pool, then `default_crew` |
| Sweep tasks | `workflow.purpose_crews.sweep_tasks` | tasks the CI failure sweep files (`ci-failure-sweep` tag), the Dependabot and code-scanning sweeps file, and merged-head repairs a review reconciliation files (its follow-up tag) | the same |

Final recovery keeps its own rules (no fallback; `[]` turns it off), and automatic review keeps its own (required for a review layer before landing; it avoids the crew that implemented the reviewed work).

**Deprecated pool keys.** `workflow.final_recovery_crews` and `operation.review_crew` load into `final_recovery` and `review`, and the `ci_failure_sweep`, `dependabot` and `merged_head_repair` pools load into `sweep_tasks`, each with a deprecation warning. A file that also sets the replacement keeps the replacement. A file whose old sweep pools name different pools fails to load until you merge them into `sweep_tasks`, because every sweep-filed task now draws from one pool. Tasks and runs recorded under the old purposes keep resolving: a `crew_source` of `purpose:ci_failure_sweep`, `purpose:dependabot` or `purpose:merged_head_repair`, and a `crew-purpose:` tag naming one, read as `sweep_tasks`. The final-recovery activity's `crew_config_key` binding keeps the old `workflow.final_recovery_crews` name, so the draws earlier runs froze under it resume.

**Resolution order.** The first rule that applies wins:

1. **Explicit crew.** A `crew` on the task, the auto-task template or the activity input is used as it is and skips the purpose. An activity's explicit `crew` also beats its `crew_config_key`.
2. **Purpose pool.** The purpose's pool, drawn by weight once per run for an activity and at creation for a task. Disabled crews are skipped, `--allow-crew` narrows the draw (a pool with no permitted member fails as a complexity pool does), and [provider usage limits](#provider-usage-limits) and [failure holds](#provider-failure-holds) skip crews while another member is left. The task pilot keeps only members that [inspect natively](#system-work-draws-from-the-complexity-pools).
3. **Earlier route.** A purpose whose pool is unset, `[]`, wholly disabled, or (for the pilot) has no inspection-capable member resolves exactly as it did before purpose pools: the right-hand column above.

**A task's purpose** comes from its tags: a `crew-purpose:<purpose>` tag naming one of the `workflow.purpose_crews` purposes, else the tag the system filed it with. An auto-task template names a purpose for the tasks it mints by adding a `crew-purpose:<purpose>` tag. A task drawn from its purpose pool records `crew_source: purpose:<purpose>`; admission keeps that crew while it is still an enabled positive-weight member of the pool, and otherwise redraws as for a stale pool assignment. A [provider usage limit](#provider-usage-limits) or a [failure hold](#provider-failure-holds) that covers the assigned crew redraws from the pool's other enabled positive-weight members, keeping their weights and the run's allowlist, before the complexity pool. The task record is not rewritten. A purpose task that cannot use its pool falls back to its complexity pool. A complexity change keeps a purpose-sourced crew, because the purpose does not depend on the tier.

**Seeing the choice.** `orbit config show --all` lists **Crew purposes**: each purpose's resolved pool, the key it came from and that key's layer. The default text view omits that block because each purpose key is already a row; `--json` always includes `crew_purposes`. `orbit run show` prints one `Activity Crew:` line per pool a run drew from, naming the crew, the key and the draw's source; `--json` adds `activity_crew_draws` with a `rule` of `default_crew`, `purpose_pool`, `purpose_unset_fallback`, `final_recovery_pool` or `complexity_pool`.

**Resume.** Purpose pools are read from the current configuration when a run first draws, then frozen in its state like a complexity draw. A run that froze a complexity-pool draw before purpose pools existed keeps it on resume.

### Disabled crews

`enabled = false` switches a crew off without deleting it; a table without the key is enabled.

- **Listed.** `orbit config show` has an `ENABLED` column and counts disabled crews; `orbit config get crews.<name>.enabled` answers `true`/`false`; `orbit.workspace.list` carries `enabled`; the dashboard Config tab marks the row and offers a toggle.
- **Skipped by pools.** A disabled member is never drawn. An all-disabled pool acts as empty, so the task falls through to `default_crew`. It may stay listed, so re-enabling restores its share.
- **Refused for agent dispatch, never substituted.** A task's `crew`, an explicit run or activity crew, or `workflow.default_crew` that resolves to a disabled crew fails with its name and `orbit config set crews.<name>.enabled true`.
- **Deterministic jobs still run.** A job whose steps and hooks are all deterministic accepts a disabled crew and records no run crew, so retention and reap jobs run on hosts without a provider CLI. An unknown crew name still fails at startup.
- **Load succeeds.** A lane key naming a disabled crew does not stop unrelated commands; `orbit doctor` warns under `config` with the enabling command.
- **Not pinned at creation.** A crew-less task created while `default_crew` is disabled keeps `crew` unset, so dispatch refuses the default by name. `orbit task show` and the dashboard still report a disabled configured crew.

**Validation.**

- `model` and `provider` must be non-empty.
- `default_crew` must name a defined crew. Left unset with crews defined, Orbit uses `opus` (or a legacy `claude` crew), else refuses to load.
- A crew name may not contain `:`, which the pool grammar reserves ([repair](#repairing-a-config-that-already-names-a-crew-with-a-colon)).
- An Antigravity crew needs an `agy models` slug; a bare Gemini CLI ID such as `gemini-3.8-flash` fails with migration guidance.

**Retired crew shapes** fail load with migration guidance: `planner` / `implementer` / `reviewer` sub-tables (use flat `model` and `provider`); `backend` (`cli` is ignored, `http` and `auto` refused, likewise in `ORBIT_BACKEND` and `[runtime] backend`; remove it); `[agent.<role>]` tables (use `[crews.<name>]` plus `workflow.default_crew`).

### Repairing a config that already names a crew with a colon

Every command, `orbit config set` included, refuses to load, so edit the file by hand. Layers are checked before merging, so the error names the defining file:

```
error: invalid input: [crews]: crew name 'gpt-5:codex' must not contain ':'; ...
Edit '~/.orbit/config.toml' and rename or remove the [crews."gpt-5:codex"] table, then rerun the command
```

Rename the table (or delete it), update every reference (`default_crew`, pool entries, a task's `crew`), and rerun.

---

## Provider identity and resolution

Every `provider` string (a crew's, an activity's inline `provider`, setup detection) goes through one canonical parser.

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
| `muse` | `muse` | [Muse Code CLI](#muse-code-cli) |
| `ollama` | — | Recognized but unsupported at the Orbit CLI entry point (`provider.unsupported`). |
| `openai_compat` (`openai-compat`) | — | HTTP-only, no CLI runtime; selecting it fails. |

- Parsing ignores case and surrounding whitespace.
- Deprecated aliases resolve with an `orbit.config.crew` warning: `anthropic` → `claude`, `openai`/`chatgpt` → `codex`, `google` → `gemini`, `xai` → `grok`.
- `copilot`, `cursor`, `pi`, `antigravity`, `opencode` and `muse` have no aliases, and the model vendor a lane runs (a Claude model through Copilot) never changes the provider.
- The cross-repo Worker executor runs only `claude`, `codex`, `gemini` and `grok`; a Worker-routed step naming another lane is refused, not re-pointed.

**Audit attribution.** Process audit maps `ORBIT_AGENT_NAME` and `ORBIT_AGENT_MODEL` to an agent family. An invalid pair (`copilot` with a `claude-*` model) or an agent envelope with no identity records `unknown` with a warning. An agent envelope (non-empty name and model, truthy `ORBIT_MANAGED_RUN_CONTEXT`, `ORBIT_TASK_ACTOR_KIND=agent`) takes precedence over `ORBIT_ACTOR`, the operator override and the OS username. This affects recorded identity only, not command authorization.

### Resolution precedence

**Which crew a task dispatches.** The first tier that is set wins:

1. **explicit**: `--crew` or run-input `crew`.
2. **task_config**: an explicit `task.crew`, or a pool or [purpose](#purpose-crew-pools) assignment still valid for its pool (normally set at creation; admission redraws stale pool assignments and default fallbacks, see [pools](#automatic-crew-pools-by-complexity)).
3. **workspace_default**: `workflow.default_crew`.
4. **environment_default**: `CONSTELLATION_DEFAULT_PROVIDER`, a provider ID or alias. `claude` selects `opus` and `codex` selects `sol` when defined; any other ID selects the same-named crew. Never overrides a configured `default_crew`.
5. **system_default**: the `opus` crew, or a legacy `claude` crew.

The environment-default tier applies even when neither config file exists.
An unset or blank variable leaves the built-in default in effect. An invalid
provider is refused during config admission unless a file configures
`workflow.default_crew`, which takes precedence over the variable.

**Which crew an activity uses.** A non-empty `crew` in its rendered input, otherwise the pool its `crew_config_key` names ([purpose](#purpose-crew-pools), complexity or final-recovery pool), otherwise the run's crew. Activity and job assets that declare `role` are rejected.

**Crew over inline baseline.** For each of `provider`, `model` and `effort`, the selected crew's value wins when present, else the activity's inline `agent_loop` value. An unrecognized crew `provider` is logged and falls back to the inline provider, so a typo never moves dispatch to another runtime. A provider already recorded on a run is reused verbatim on reconciliation.

### No silent fallback

An explicit selection that cannot run fails with a stable diagnostic and never falls back to another runtime:

- `provider openai_compat is unsupported by the Orbit CLI entry point (HTTP-only)`
- `provider ollama is unsupported by the Orbit CLI entry point`
- `unknown provider '<x>'; expected one of claude, codex, gemini, grok, copilot, ollama, openai_compat, cursor, pi, antigravity, opencode, muse`
- A selected lane whose binary is missing fails permanently, naming the binary.

---

## Provider CLI notes

For every lane: Orbit passes `--model` from the crew and sends the prompt on **stdin**, never argv (process listings and audit see argv). Muse Code, which cannot read stdin, gets the prompt in a private file instead ([below](#muse-code-cli)). The Orbit OS sandbox is the filesystem boundary, and provider-specific write grants apply only while that provider runs. Credentials reach the agent only through [`[execution.env].pass`](#executionenv--the-agent-subprocess-environment); Orbit never puts a key on argv. A lane without native or Orbit-managed MCP reaches Orbit tools with `orbit tool run <tool> --input '<json>'` through its shell tool, under the same grants.

`orbit tool run` selects a workspace from any directory with `workspace` in the JSON input or the global `--workspace` flag: a name, `ws_*` ID, absolute local checkout path, or local `hm_*/ws_*` selector from the federated listing. A selector for another host fails locally, naming it; run the command on that owner host. Unparseable JSON or an unreadable `--input-file` is reported before workspace selection, runtime open or migration, so an input typo never migrates the checkout.

### GitHub Copilot CLI

| | |
|---|---|
| Install | `npm install -g @github/copilot`, then `copilot --version`. The retired `gh copilot` extension is not supported. |
| Auth | `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, else the `copilot /login` session (macOS keychain item `github-copilot-app`). Orbit forwards none of the token variables unless one is in `pass`. On a Mac, run `/login` once. |
| Model | `--model` is always passed, so `COPILOT_MODEL` and the saved `/model` are ignored. List IDs with `/model` inside `copilot`; recheck pins after CLI upgrades. |
| Flags | `--allow-all-tools --no-ask-user --output-format json`. Never `--allow-all`/`--yolo`, which widen paths and URLs. |
| Sandbox | Write: `COPILOT_HOME` (default `~/.copilot`) and `$XDG_CACHE_HOME/copilot`. macOS: read `~/Library/Keychains`. No access to `~/.config/gh`. |

An account without Copilot entitlement fails with `Error: Authentication failed`; org-policy-disabled third-party MCP servers produce a `session.warning` (`warningType: "policy"`). The GitHub org admin fixes both. A run with no assistant message fails the step.

### Cursor Agent CLI

| | |
|---|---|
| Install | `curl https://cursor.com/install -fsS \| bash`, then `cursor-agent --version`; put `~/.local/bin` on `PATH`. Cloud agents are not used. |
| Auth | `cursor-agent login` (check with `cursor-agent status`), in the macOS login keychain by default. A file-store login (`AGENT_CLI_CREDENTIAL_STORE=file` at login) also needs `pass = ["AGENT_CLI_CREDENTIAL_STORE"]`. Or `pass = ["CURSOR_API_KEY"]`. |
| Model | `cursor-agent models` (or `--list-models`). |
| Flags | `--print --force --output-format json`. The terminal `{"type":"result","subtype":"success","is_error":false,"result":…}` object is validated before the envelope is read. |
| Sandbox | Write: `~/.cursor`. macOS: read `~/Library/Keychains`. |

### Pi CLI

| | |
|---|---|
| Install | `npm install -g @earendil-works/pi-coding-agent`, then `pi --version`. |
| Auth | `/login` in an interactive `pi` session (under `$PI_CODING_AGENT_DIR`, default `~/.pi/agent`), or a vendor key via `pass`. Orbit never renders `--api-key`. |
| Model | A `--model` pattern, optionally vendor-prefixed (`openai/gpt-4o`); list with `pi --list-models`. `effort` renders as `--thinking <level>`. |
| Flags | `--mode json --no-session --no-approve --offline`: ephemeral sessions, no inherited project trust (project-local `.pi` extensions don't run; `AGENTS.md`/`CLAUDE.md` still load), no startup network calls. |
| Sandbox | Write: `$PI_CODING_AGENT_DIR`, else `~/.pi`. |

Pi has no MCP client and `orbit mcp init` offers none; keep Pi's `bash` tool available, since Orbit tools go through it. Completion is read only from the final assistant `message_end` frame. No provider token usage is reported; raw output is kept in the audit blob store.

### Antigravity CLI

`antigravity` launches `agy`, Google's current terminal agent: a separate lane from `gemini` (the legacy Gemini CLI), with different flags, MCP config and model slugs. `gemini-*` models still attribute to the Gemini model family.

| | |
|---|---|
| Install | See the [Antigravity CLI docs](https://www.antigravity.google/docs/cli/headless/), then `agy --version` and `agy models`. Init prefers it over `gemini` when both are present. |
| Auth | One interactive `agy` login, cached under `~/.gemini/antigravity-cli/`. An unauthenticated headless run exits with `authentication required`. |
| Model | A slug from `agy models`; `effort` accepts `low`/`medium`/`high` only. To migrate a `provider = "gemini"` crew, change the provider and use an `agy models` slug. |
| Flags | `--input-format stream-json --output-format stream-json --dangerously-skip-permissions`, plus `--print-timeout` set to the activity's wall-clock runtime minus 30 s, so `agy` stops itself before Orbit's deadline kills it. The argv is fixed at launch, before any build-admission credit is earned, so credit extends Orbit's deadline but not `agy`'s budget. A shorter custom value is kept. The invocation-start audit event records `print_timeout_ms` and the launch `effective_deadline_ms`; the step output records the same print budget and the final `effective_deadline_ms` (runtime plus earned credit). Deadlines are durations from spawn. For runtimes of 30 s or less the full runtime is used, with a minimum of 1 s, because the shutdown margin does not fit. Don't add `agy --sandbox` or Gemini CLI flags. |
| Sandbox | Write: `~/.gemini` (shared with the Gemini CLI). macOS: read `~/Library/Keychains`. |
| MCP | `~/.gemini/config/mcp_config.json` or `.agents/mcp_config.json`, not `.gemini/settings.json`. |

Orbit checks the response from a terminal `result` with `status: "SUCCESS"` for its completion envelope; a non-zero exit with a terminal `ERROR` surfaces the bounded, redacted `error` string. Running for the whole `--print-timeout` spends the provider budget and is never completion: a run that then exits 0 with `SUCCESS` but no Orbit completion envelope fails as a timeout (`timed_out: true`, retryable, transient) with a diagnostic naming the print-timeout and elapsed time, keeping the bounded `final_message`. An early exit without an envelope gets the ordinary completion-envelope message.

Background commands cannot be turned off for a headless run. `agy` 1.3.3 has no flag or setting for it (`agy --help` lists none), so Orbit passes nothing and instead tells every provider, in the shared prompt contract, to run commands in the foreground. `agy` itself waits for a background task it started only until `--print-timeout`, capped at 30 minutes, then ends the turn and exits 0 with the task still running and no envelope. When the last `manage_task`-style status report in stdout shows a `task-<id>` still running, the step fails naming that task, the elapsed time and the bounded last `response`, instead of the generic message. The exit code, a `SUCCESS` wrapper and a persisted summary never count as completion; whatever the run left in the checkout is unverified.

### OpenCode CLI

| | |
|---|---|
| Install | `opencode --version`, then `opencode models`. Served modes (`serve`, `--attach`, `web`) are not used. |
| Auth | `opencode auth login` (`auth.json` under `$XDG_DATA_HOME/opencode`), or a vendor key via `pass`. |
| Model | A fully qualified `<vendor>/<model>` (`anthropic/claude-sonnet-4-5`). `effort` renders as `--variant`: `high`/`max` only. |
| Flags | `run --format json --auto`. `--auto` is required unattended (otherwise every permission request is auto-rejected) and is not a security boundary. `--continue`, `--session` and `--share` are never passed. |
| Sandbox | Write: `$XDG_DATA_HOME/opencode`, the config root (`$OPENCODE_CONFIG_DIR`, else `$XDG_CONFIG_HOME/opencode`), `$XDG_STATE_HOME/opencode` and `$XDG_CACHE_HOME/opencode`. |

Orbit does not write OpenCode's `opencode.json` MCP config and `orbit mcp init` has no OpenCode target; Orbit tools go through the shell tool. Only assistant `text` parts are read; a terminal `error` event or non-zero exit fails the step. No provider token usage is reported.

### Muse Code CLI

`muse` runs Muse Code, Meta's terminal coding agent. The contract below was observed on Muse Code 1.4.10 (`1.4.10-R6427.1`, macOS): `muse exec --help` and offline `--provider echo` runs only, with no authenticated run.

| | |
|---|---|
| Install | `~/.local/bin/muse`, a launcher that execs `~/.local/bin/muse-bin-<version>`; check with `muse --version`. Init detects `muse` on `PATH` and enables the `muse` crew. Orbit sets `MUSE_NO_AUTO_UPDATE=1` and `MUSE_LOGIN=0` for every run (an outer value cannot override them), so a run never swaps the binary under running work or starts a device login; upgrade with the launcher yourself. |
| Routing | The shipped asset sets `host_os: [macos]`. A task assigned a muse crew waits for a macOS host and keeps that crew; a Linux drain does not redraw it onto another crew. Muse Code also ships a Windows build, but native Windows is not an Orbit runtime (ORB-13857) and spawning that binary from Orbit-in-WSL2 is unverified, so Windows stays off the list until that launch is verified. A Mac pull drain that claims these tasks must run the owner's exact Orbit build: `orbit host list` shows the owner reachable with the same `binary_version` and `protocol_fingerprint`. |
| Auth | `muse login` (Meta account, browser device code) once, outside Orbit. The login writes the file `auth.json` under the config root (`$XDG_CONFIG_HOME/muse`, else `~/.config/muse`), and on macOS `muse exec` also reads the login keychain, so Muse gets the read-only `~/Library/Keychains` carve-out; without it a valid login fails with `missing meta credentials`. Or `pass = ["META_API_KEY"]`, which Muse prefers; `muse auth set --api-key-stdin` also works. Orbit never logs in or forwards `META_API_KEY` unless `pass` names it. |
| Model | A catalog id (`muse-spark-1.3`); `muse model-profile show <model> [--effort <tier>]` resolves one. `effort` renders as `--reasoning-effort`; Muse accepts `none` … `ultra`, which covers every Orbit effort, and any other value is refused at config load. |
| Prompt | `muse exec` does not read stdin (piped stdin alone exits 2, `missing prompt`) and refuses `--prompt-file /dev/stdin` (`not a regular file`). Orbit writes the prompt to a mode-0600 file with an unpredictable name in the run's scratch directory (`<worktree>/.orbit/tmp/`), passes `--prompt-file <path>`, and deletes the file when the invocation ends, whatever its outcome. The envelope never reaches argv; the audit keeps the prompt in the invocation's stdin blob. An activity profile that cannot read the worktree cannot read the file either, and the run fails. |
| Flags | `exec --json --approval-mode never --disable-request-user-input --no-foreign-personal-context --disable-reminders --no-session-log`. `--approval-mode never` and `--disable-request-user-input`: nobody can answer mid-turn, and neither is a security boundary. `--no-foreign-personal-context`: Muse otherwise loads your Claude Code and Codex personal rules and skills. `--disable-reminders`: otherwise every turn spawns skill, goal and verify reminder child agents. `--no-session-log`: no session log per run (this also disables Muse's local session messaging). `--trust-workspace` is not passed: a worktree is content Orbit does not vouch for, so its Muse skills and rules stay unloaded and agent delegation stays off; the envelope directs the agent to read `AGENTS.md`. `--max-model-steps` and `--disable-web-tools` are not passed; the activity deadline bounds the run. |
| Sandbox | Write: the config root and `$XDG_DATA_HOME/muse` (else `~/.local/share/muse`: tracing, plugins, skills); on macOS also `~/Library/Application Support/Muse` (session-name authority). macOS: read `~/Library/Keychains`. Muse's own sandbox is on by default with network `proxy-only`; under Orbit's sandbox (or `sandbox: off`) Orbit appends `--disable-sandbox`, because a second Seatbelt profile cannot be applied inside Orbit's and Orbit's profile is the filesystem boundary. `--yolo` is never passed. |
| MCP | Muse has a native MCP client (`mcpServers` in `settings.json`, or `-c KEY=VALUE` per run), but per-run injection of Orbit's server was not verified, so Orbit passes no `-c` and never writes `settings.json`; Orbit tools go through `orbit tool run` on `PATH`. `orbit mcp init` has no Muse target. |

Orbit reads only string `payload.text` of the last `run.terminal.completed` record, and only when its `payload.terminal` is `completed`. Missing or non-string answer text cannot be replaced by `payload.message`, `payload.error`, or `payload.error.message`; those fields are fallback failure reasons for non-completed terminal diagnostics only. `failed`, `cancelled` and unknown values (the field is wire-open) fail the step, as do a missing terminal record, a stdout line that is not a JSON record with a `payload_type`, and a non-zero exit even with a completed record. The prompt echoes (`run.lifecycle.started`, `turn.input.user`), partial `run.output.delta` text and child-task records never count as completion. The terminal record carries no usage, so no provider token usage is reported. `muse serve` (MSP over stdio) is not used.

### MCP client registration

`orbit workspace init --mcp` registers Grok through the project `.mcp.json` shared with Claude Code; `orbit mcp init --scope home --grok` uses `~/.claude.json` (both listed in Grok's [MCP compatibility documentation](https://docs.x.ai/build/features/mcp-servers)). Re-running init migrates an older Orbit-generated Grok entry out of `.grok/config.toml`, keeping other Grok settings and servers. Init keeps the native `.grok/config.toml` target when `[claude_compat] imported = true` in `~/.grok/config.toml` (Grok then ignores project `.mcp.json`), and the native home target when `[compat.claude] mcps` is disabled or `GROK_CLAUDE_MCPS_ENABLED=0`. `grok inspect` shows the loaded source. Removing the shared `orbit` registration through Claude or Grok removes it for both. Gemini CLI still uses `.gemini/settings.json` (`~/.gemini/settings.json` for home scope); its [configuration reference](https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/configuration.md) does not document the shared locations.

---

## Per-task crew override

`workflow.default_crew` is only the fallback. Each task has an optional `crew`, and `orbit run ship` resolves each task by the [resolution precedence](#resolution-precedence), so one ship can mix crews; each child run records its crew (`resolved_crew` in `orbit run show`). A single child pipeline whose tasks name different crews, or mix set and unset crews, fails rather than falling back to the default.

### Automatic crew pools by complexity

```toml
[workflow]
low_complexity_crews = ["luna"]
medium_complexity_crews = ["grok", "sol"]
hard_complexity_crews = ["astra"]
xhard_complexity_crews = ["fable", "astra"]
```

- **Selected at creation.** A task created without `crew` (`orbit task add`, `orbit.task.add`, an auto-task mint with no template crew, an import) draws from its complexity's pool, else `default_crew`, storing `task.crew` and its provenance `task.crew_source`; a `crew_assigned` history entry records the source (`explicit`, `pool:<complexity>`, `purpose:<purpose>` or `default`). A task with a [purpose](#purpose-crew-pools) whose pool is configured draws from that pool instead of its complexity's. That entry is a second write after the insert; if it fails, the task is still created and returned, and the failure is logged. With no crews configured the field stays unset.
- **Complexity changes redraw automatic crews.** When the pilot or `task update --complexity` changes the tier, a crew sourced from another pool or the `default` fallback is redrawn in the same mutation, logged as `crew_redrawn` with previous and new source and crew. Explicit crews stay pinned, even when an update names the crew a pool already chose; status transitions keep the selection. `task update --crew <name>` pins; [`--crew ""`](#setting-taskcrew) redraws. Every stored change records the actor, prior and new crew, and whether it was named or drawn.
- **Tiers:** `low`, `medium`, `hard`, `xhard`. Unset or `unassessed` complexity uses the default chain. The task pilot never demotes a task out of `xhard`.
- **Empty pool** (`[]`, the init scaffold) means no pool: the task gets `default_crew`, as it does when all members are [disabled](#disabled-crews). Blank entries and unknown crew names fail before dispatch.
- **Preferences, not allowlists.** An explicit `task.crew`, an explicit run crew, and review and preparation jobs keep the crew they name, except that a standing [provider failure hold](#provider-failure-holds) redirects a task-crew draw away from the crews it excludes, and a [provider usage limit](#provider-usage-limits) removes limited crews from the draw.
- **Serving hosts at creation.** The creation and re-rate draws above keep a crew whose executor `host_os` (below) excludes the creating host when another host serving this workspace can run that executor. A host serves this workspace when the owner's own records show it pulled here recently: it is in the owner's host registry (`orbit host list` shows each host's recorded `host_os`, read from the host at `orbit host add`) and it holds an execution claim in this workspace from the last seven days. Registration alone never counts — a registered host that serves only other workspaces changes nothing — and a follower that stops pulling ages out after the window, so its OS-only crews stop being drawn; tasks already drawn keep their crew and wait. The task keeps the drawn crew and waits for that host; admission on each host, and activity and purpose-pool draws, still filter by the executing host alone. With no serving follower of the needed OS the creation draw skips the crew as before. Before drawing, creation, crew clearing and complexity re-rates also discard complexity-pool crews whose executor cannot intersect the task's `os:` tags, falling back to `default_crew` when the pool empties. A simultaneous tag edit supplies the tags for that draw. An untagged task can still draw a follower-only crew; an explicitly requested crew with an empty task OS intersection is refused before persistence.
- **Admission** (drain or ship) checks a pool-sourced crew against the current tier and its enabled, positive-weight members. A stale pool assignment or `default` fallback, or no crew, draws from the current pool, else the default chain; a default fallback is never an explicit pin. Without `crew_source`, legacy assignment history recovers provenance, and a crew with no assignment evidence counts as explicit. Admission never writes back to the task.
- **Run overrides.** `orbit run auto --low-complexity-crews …` (and `--medium-…`, `--hard-…`, `--xhard-…`) replaces that pool for one drain; with no names it disables it. `orbit run ship` has no override flags.

#### Weighting a pool

```toml
[workflow]
low_complexity_crews    = ["luna:50", "sonnet:50"]
medium_complexity_crews = ["grok:70", "opus:10", "sol:20"]
hard_complexity_crews   = ["opus", "sol"]          # bare = uniform
```

- Weights are relative non-negative integers: `["grok:7", "sol:3"]` equals `["grok:70", "sol:30"]`.
- A pool is all bare or all weighted. Mixing them, or a suffix like `grok:-1` or `grok:2.5`, is an error at load, in `orbit config set` and on the CLI flags.
- Bare duplicates collapse to one ticket; a weighted pool names each crew once.
- Weight `0` parks a crew (never drawn); at least one entry must weigh more than 0.
- The draw walks the pool in canonical name order over `[0, total_weight)`.

**Allowlists.** With `--allow-crew`, the draw renormalizes over permitted members, keeping their ratios. A pool with no permitted positive-weight member is disjoint, and `orbit run readiness --allow-crew <crew>` reports `crew_not_allowed`. Without `--allow-crew`, readiness (and the dashboard Drain card) applies the live drain's own set and reports it as `capacity.allowed_crews`. The allowlist is checked against `task.crew` and still applies to system and review activities at dispatch.

**Frozen per run.** The admitting run captures the effective pools in run input `auto_crew_pools`, inherited by descendants. Each admitted leaf records `crew` and `crew_selection` (task ID, complexity, source, and the eligible `[{name, weight}]` after renormalization), shown as `Crew Selection:` in `orbit run show`. Retries and resumes keep it.

### Provider failure holds

A local run can fail because of its provider rather than the work. The step error then carries a typed marker followed by `provider=<name>`. Only the failing provider CLI's own stderr, terminal error or failure frames set a marker, never the agent's transcript, tool output or Orbit envelopes (an agent reporting a GitHub rate limit is not a provider limit).

| Marker | Meaning | Excluded crews | Base backoff |
|---|---|---|---|
| `[provider_capacity]` | The selected model was at capacity. | The run's crew, or every crew of the failing step's provider when that step ran on another provider (a reviewer, say). | 15 min |
| `[provider_unavailable]` | Unusable on this host, such as failed authentication. | As for capacity. | 30 min |
| `[provider_refusal]` | Content policy refused the task: a Codex content-filter `error` or `turn.failed` frame (`This content was flagged for possible …`), or a Claude `result` with `stop_reason: "refusal"`. | Every crew of the provider, which would refuse the same content again. | 24 h |
| `[provider_limit]` | The account hit a usage limit (texts below). | Every crew of the provider; only crews whose model contains the named model (case-insensitive) when the limit names one. | Until the reported reset (below) |

Capacity, unavailability and refusal backoff doubles for each other hold the task got in the last 24 hours, up to 24 hours. A usage limit holds until the provider's reset, clamped to 5 minutes – 7 days after the failure; with no readable reset it starts at 30 minutes and doubles per earlier usage-limit hold on that provider in the last 24 hours, up to 6 hours. Provider labels are parsed first, so an alias is the same provider. Crews an earlier, still-standing hold excluded stay excluded, each on the machines it was excluded on.

Recognized usage-limit texts: Antigravity's `Individual quota reached … Resets in 1h37m37s`; Gemini's `TerminalQuotaError`; Codex's `You've hit your usage limit … try again at <time>` (an `error` and `turn.failed` pair); a Claude `result` with `is_error: true` starting `You've hit your` or `You've reached your`, or a `rate_limit_event` with status `rejected`; grok's `Too Many Requests (429)`. The marker then carries `model=<model>`, `window=<window>` and `resets_at=<RFC 3339 time>`. A relative reset is read from the failure time; a Codex `try again at` time is local to the host.

**Handoff.** Step and final recovery skip these failures. On the PR pipeline the handoff returns `held_provider_failure`: it commits the candidate on the run's branch, publishes no branch and opens no `[BLOCKED]` PR, but pushes the candidate to `refs/orbit/candidates/<task>/<run>` on `origin` so a claim on another host can continue it (a task comment says when that push failed). Run finalization moves the task to `backlog` under a `provider_failure_hold` history event, whose note carries the class, provider, excluded crews, `not_before` and run. A `[provider_unavailable]` hold also names the host's `machine`: a failed login is that host's, so its crews are excluded only there, while crews a still-standing capacity, limit or refusal hold excluded stay excluded on every machine (`carried_crews`). A hold stored without a `machine` excludes its crews everywhere, as before. A claimed leaf on a follower that ends with `[provider_refusal]` releases its claim as a `provider` failure, and the owner records the same hold from its own crews, naming the leaf run.

**Admission during the hold.** Until `not_before`, the crew is drawn from crews the hold does not exclude: the task's own crew or pool, then the other enabled members of its purpose pool when the assignment came from one, then its complexity pool, then `default_crew`; `crew_selection.source` names the hold. With none left, the local drain defers the task and `orbit run readiness --json` reports `reason: "provider_backoff"` with the release time. A follower's pull is not given a held task it would run as an excluded crew (its own crew, or the follower's default for a task naming none); the pull receipt's `deferred_conflicts` names the hold. An unavailable hold placed on another host excludes nothing from that follower. A hold that excludes nothing (the failed run resolved no crew) still defers until `not_before`. An explicit run-input `crew` ignores the hold. A usage-limit hold on an explicit crew (`crew_source` `explicit`, or a legacy pin) follows [`workflow.provider_limit_explicit_crews`](#provider-usage-limits): under `wait` the task waits with `provider_backoff` instead of being redirected; under `pool` it draws as above. The hold ends at `not_before` or any later status change, and the next run resumes the committed candidate.

**Host outages.** A local run that fails `[provider_unavailable]` also records each crew it excluded in the global store (`~/.orbit/orbit.db`, table `provider_crew_outages`), one row per provider and crew, until the hold's own backoff ends; a longer outage of the crew is kept. Until then this host's local admission draws no task onto that crew, not only the held one, and whatever became of the held task: a task whose own crew, complexity pool or default leaves another crew draws it, and the rest wait with `provider_backoff`. A crew of the same name on another provider is not covered. The record is advisory: a failure to record it logs a warning, and the task is still held. Followers and their pull drains are not affected; their own crew window handles their own logins. Operator steps: [stuck job runs](runbooks/stuck-job-runs.md#a-task-held-after-a-provider-failure).

**Routines.** The task-pilot routine honours a host provider failure hold. It reads recent runs on this host — the failures `orbit run readiness --json` projects as `provider_auth_failures`, plus `provider_capacity` — over a one-hour window of finish time (falling back to start time, then creation time), and does not write a new hold record. A recently finished failure remains in that window even when its pipeline was created more than an hour ago. Clearing successes use the same finish-time window. While a `provider_unavailable` failure from the last 30 minutes, or a `provider_capacity` failure from the last 15 minutes, covers every crew the pilot would draw (the inspection-capable members of `workflow.purpose_crews.task_pilot` when it has one, else of `workflow.low_complexity_crews`, or of the next tier up when none can inspect, else `workflow.default_crew`, after the usage-limit narrowing), the routine withholds the dispatch. The withhold reason starts with `provider_hold:` and names the provider or crew, when the backoff ends, the failing run, and the crews skipped. It does not start `task_pilot_pipeline`. Unavailability covers every crew of that provider on this host, because a failed login is the provider's. Capacity covers the crew the run ran as, or every crew of the failing step's provider when that step did not run as that crew. The routine and the readiness auth-failure card share crew attribution: `task_pilot_pipeline` uses its recorded task-pilot draw (or, for a run admitted before purpose pools, its `workflow.low_complexity_crews` draw), `blocked_task_recovery_pipeline` uses its `workflow.purpose_crews.final_recovery` draw, and `finding_verification_pipeline` uses its prepared verifier partitions, including escalations. These jobs' `resolved_crew` names the tasks' crews and does not identify the agents they dispatched. A missing draw counts as no known crew. A verifier failure counts for the partition whose recorded error the failure message carries; a verifier success counts only partitions with a recorded result. Other jobs use `resolved_crew`. A pilot's provider failure is recorded on the run itself: a failed pilot fan-out prefers a provider-classified worker message over an ordinary failure message, preserving the provider marker even when an earlier worker failed on the work. The backoff is the base in the table above, measured from the latest uncleared failure's finish time; it does not double. A successful run of that provider on this host that started after the failure, or a pull-drain auth probe that records `recovered_at` for that provider on this host, clears unavailability before the backoff ends; a success that started before the failure does not, even when it finishes after it, the same rule `provider_auth_failures` uses. Capacity clears when such a success is of the held crew, or of any of the provider's crews when the hold covers the provider. A probe does not clear capacity. `provider_limit` and `provider_refusal` do not withhold the routine. When any drawable crew remains, the routine still dispatches and the draw skips the held crews, naming the hold in the draw's source. The next tick after the hold lifts dispatches again with no operator action, once the routine's own debounce has passed. Another host's failure does not hold this one. Step-failure and PR conflict recovery share that draw, so they skip a held crew the same way; the routine withhold itself is only the task-pilot routine.

**The host's limit record.** A usage-limit failure also records an observation in the global store (`~/.orbit/orbit.db`, table `provider_limit_observations`): one row per provider, model and window with the reset, run, crew and provider text. A newer observation replaces the row, an older one never does. Recording is advisory: a failure to record logs a warning, and the run is still typed and held. After every Codex or Claude run, successful or not, the host also records the provider's own reading of each window (source `event`): percent used (can exceed 100), window length and reset.

- Codex: the last `token_count` with `rate_limits` in the run's own thread rollout (`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*-<thread_id>.jsonl`, `CODEX_HOME` from `[execution.env]`, else `~/.codex`), reading at most the last 1 MiB. `primary` and `secondary` windows are account-wide.
- Claude: the last stdout `rate_limit_event` (Orbit runs `claude --output-format stream-json --verbose`): each `unifiedWindows` entry (`five_hour`, `seven_day`, …) plus the currently limiting window if absent there. `seven_day_opus` and `seven_day_sonnet` carry their model; an overage window is recorded with `gating` off.
- A missing rollout, another thread's rollout or an unrecognized reading records nothing. Other providers report no usage window.

**Pull drains and claimed leaves.** An authentication failure releases the claim and excludes every crew of that provider (aliases included); a declared [`auth_probe`](#executor-authentication-recovery-probes) can re-admit them in the same window. A capacity failure excludes only the leaf's crew, since another model may have room, and only for a backoff: 10 minutes from when the leaf ended, doubling with each further capacity failure of that crew in the window up to an hour. The exclusion carries `until` and the crew is runnable again in the same window once it passes; while it holds the owner's before-PR or before-landing review crew, the drain requests nothing and its `Requests held:` line names the review's timing and when the backoff lifts. A usage limit releases the claim with failure class `provider` and `provider_limit: true`, which does not count against the task's release budget and excludes no crew itself; the follower's own limit record excludes the provider's crews until the reading lapses.

### Provider usage limits

A hold learns of a limit only after a run fails on it. Delivery admission also reads this host's [limit record](#provider-failure-holds) before dispatch and stops routing to a crew whose provider account, as logged in here, is at or near a window's limit.

```toml
[workflow]
provider_limit_max_used_pct = 90            # default
provider_limit_overrides = ["claude:80", "grok:100", "codex:off"]
provider_limit_budgets = ["grok:30usd/5h"]  # none by default
provider_limit_explicit_crews = "wait"      # or "pool"
```

**Limited crews.** A crew is limited while a live, gating reading of its provider is exhausted or at or above the provider's threshold (its `provider_limit_overrides` entry, else `provider_limit_max_used_pct`). A reading that names a model (Claude's `seven_day_opus`) limits only crews whose model contains it, case-insensitively; any other reading limits every crew of the provider (aliases included). At `100` only an exhausted window gates (99% gates nothing). An overage window (`gating` off) never gates.

**Gate off.** `provider:off` (for example `codex:off`) turns that provider's pre-dispatch gate off. Every live reading is still recorded and shown, with `threshold` `off` and `gated` false. No reading of that provider — an event reading, a usage-limit failure observation, or a ledger budget reading — limits a crew at admission, in a drain or ship draw, or in the pull window. `orbit doctor`'s `provider-limits:<provider>` row says gating is off, and the dashboard marks the reading the same way. A run that then fails on a usage limit still records the task's provider failure hold: `off` disables only the pre-dispatch gate. Removing the entry gates the same readings again at `provider_limit_max_used_pct`. `off` is not a percent, so it does not change [`provider_limit_budgets`](#budgets-for-providers-that-report-no-usage) arithmetic.

**Lapse.** A reading counts until its `resets_at`. Without one it counts for its window's length (60 minutes when that is unknown), or, for an exhausted error reading, for the first usage-limit backoff, 30 minutes. The crew is eligible again at the next admission pass, with no provider probe.

**Gated draws.** Only delivery admission (the crew draw of drains and `orbit run ship`, and the pull window); system, review, final-recovery and task-pilot crews are not gated. The complexity-pool draw of the task pilot, step-failure and PR conflict recovery still skips crews at a provider limit while another member remains, but never waits or redirects the way the gate does. It also skips crews under a host provider failure hold for unavailability or capacity, and that hold alone can withhold the task-pilot routine ([Routines](#provider-failure-holds) above).

- **Purpose tasks** (`crew_source` `purpose:<purpose>`): a limited assigned crew is redrawn from the unlimited enabled positive-weight members of that purpose pool. Their weights and the run's allowlist are kept, and the task record is not rewritten. With none left, the draw continues as a pool task does.
- **Pool and default tasks** (`crew_source` `pool:<tier>` or `default`, or no crew): limited members are removed and the rest renormalized, as with `--allow-crew`. A limited assigned crew is redrawn from the unlimited members of the current complexity pool; the task record is not rewritten.
- **No default fallback.** With every member limited the task waits; filtering never falls through to `workflow.default_crew`, usually the most expensive crew and often on the same provider.
- **Explicit crews** (`crew_source` `explicit`, or a legacy pin): `wait` (default) keeps the task in the backlog; `pool` draws from the unlimited members of its complexity pool.
- **Explicit run crews** (`orbit run ship --crew`) are the operator's decision and not gated; `crew_selection.provider_limit` records the limit.

A redrawn task's `crew_selection.source` names the limit. A task left with no unlimited crew waits with `reason: "provider_limit"` in `orbit run readiness --json` and the detail `<provider> <window> at <used>% (limit <threshold>%) until <reset>; crews <list> skipped`, lifting by itself after the reset. An `orbit run ship` naming such a task still runs it on its usual draw. A standing [provider failure hold](#provider-failure-holds) that leaves no crew reports `provider_backoff` first. Operator steps: [stuck job runs](runbooks/stuck-job-runs.md#a-task-waiting-on-a-provider-usage-limit).

**Pull drains.** Each pass of a follower's pull drain re-reads its own store and excludes each limited crew from its crew window with source `provider_limit` and an `until` time; unlike other exclusions, it lifts at `until` within the same drain. The owner never hands the follower a task on that crew meanwhile. The owner's own provider readings apply only to owner-local work; neither the owner's admission answer nor a settlement supplies readings to the follower's window. Each provider-limit exclusion names the host whose local store supplied it. The owner's before-PR reviewer is not gated.

**Surfaces** all share admission's view of live readings (liveness, threshold, crew rules), so none shows a crew runnable while admission skips it.

- `orbit run readiness`: a `Provider limits:` line per gated reading (`claude five_hour 93% >= 90% until 15:00Z: opus, sonnet skipped`; a reset on another UTC day includes its date) and each waiting task's detail. `--json` has a top-level `provider_limits` array of every live reading: `provider`, `scope` (the model a reading names, else `null`), `window`, `used_percent`, `exhausted`, `resets_at`, `source`, `observed_at`, `age_seconds` (never negative), `stale` (true when `observed_at` is an earlier UTC date than the snapshot), `gating`, `partial`, `threshold` (a percent, or the string `off` when that provider's gate is off), `gated` (false for every reading of a provider set to `off`), `until` and `crews` (enabled crews covered). `provider_auth_failures` lists each host and provider with a `provider_unavailable` authentication failure in the last 7 days (at most 200 recent failures and 200 recent successes) that no successful run of that provider on that host, started after the failure, has cleared. Each item is `host`, `provider`, `runs` (`run_id`, `at`, oldest first) and `fix_command` (the executor's auth-probe hint, else the provider's re-login command). Crew attribution uses the same recorded draws and verifier partitions as the routine's [provider failure hold](#provider-failure-holds), so a successful pilot, recovery or verifier on another provider cannot clear the card. A failure or success outside that window is not part of the projection.
- `orbit run show <pull-drain>`: `excluded <crew> (provider_limit until <time>): <reading>; reading from host <machine-id>` (`unregistered` when the runtime has no machine identity).
- `orbit doctor`: one `provider-limits:<provider>` row per provider an enabled crew uses (a provider with no usage windows and no budget, which is every provider but Codex and Claude, shows `info`; a budgeted one shows its `ledger` reading, such as `grok 5h budget 67%`; a provider set to `off` says gating is off and lists its readings), plus a `provider-limits:workflow.purpose_crews.review` warning when that ungated lane uses a gated provider (and a `provider-limits:workflow.purpose_crews.pre_landing_review` warning for that pool while it is set). Row semantics: [health checks](runbooks/health-checks.md#run-orbit-doctor). `orbit doctor providers` shows the same per executor (`LIMITS` column, `provider_limits` in `--json`).
- Dashboard: the Settings › Crews Limit column (tightest window, used percent, reset, `gated` badge, or `gating off` when the provider's gate is off); the Drain card names gated readings, lists every live reading with its age (`today` on the observation's UTC date, otherwise how long ago) and a stale marker when `stale` is set (a reading whose gate is off is marked `gating off`), lists each standing `provider_auth_failures` item with its host, provider, run links and fix command, and lists tasks waiting with `provider_limit`.

#### Budgets for providers that report no usage

Grok, Antigravity and Gemini don't report how close an account is to its limit; Orbit learns it only from the failure. `provider_limit_budgets` declares the operator's allowance per rolling window so Orbit gates the provider before it runs out.

- **Format.** `provider:<amount><unit>/<window>`: a positive amount in `usd` or whole `tokens`, a window of `<n>h` or `<n>d` up to 31 days, each provider once (`xai` is stored as `grok`). No defaults. Load and `orbit config set` reject a malformed entry.
- **Reading** (source `ledger`, account-wide): this host's spend on the provider in the trailing window ÷ the amount, gated at the provider's threshold. `usd` sums each invocation's reported cost; `tokens` sums input plus output tokens from the invocation ledger. Its reset is the earliest moment enough old spend ages out to drop below the threshold (at `100`, below the budget). `provider:off` does not change this arithmetic: the reset stays on `provider_limit_max_used_pct`, because `off` is not a percent. The reading is still shown, with `threshold` `off` and `gated` false, and it does not limit crews.
- **Partial.** A `usd` reading where some invocation recorded no cost is `partial` (`"partial": true`; surfaces note `partial: some invocations report no cost`), and its spend is a lower bound.
- **Precedence.** A live provider-reported reading or limit failure for the provider wins; the ledger applies again when it lapses. The ledger reading is computed on each read, never stored.
- **Attribution.** An invocation counts for the provider it ran on. Older invocations are attributed by agent name, so an earlier Antigravity run, recorded under its model's family, does not count toward an `antigravity` budget.
- **This host only.** Interactive use and other hosts on the same login are invisible. Set the budget below the account's allowance and keep the provider's own failure as the backstop.

### Final recovery pool

`workflow.purpose_crews.final_recovery` is the final-recovery [purpose pool](#purpose-crew-pools): the weighted pool the `final_recovery` activity draws one crew from, once per run, when a task fails after step-failure recovery is exhausted; that crew evaluates the failure and attempts a final typed decision and fix.

```toml
[workflow.purpose_crews]
final_recovery = ["sol:100", "opus:20"]
```

- **Unset:** `["sol:100", "opus:20"]`, filtered to crews defined in `[crews.*]`. With neither defined the pool is empty and final recovery is off, without a validation error.
- **Explicit:** standard pool grammar (`name` or `name:weight`); every named crew must exist in `[crews.*]`.
- **`[]`** disables final recovery.

A final-recovery `resume` records its rationale and the failed step's refusal
and output before discarding stale checkpoints. The refusal and output are
bounded the way final recovery's own input is (64 KiB, keeping head and tail),
so the resumed agent's turn stays within provider limits. The resumed top-level
phase and its nested activities receive this typed `final_recovery_resume`
input, including in claimed pipelines and after a process restart.
`agent_implement` addresses that repair mandate first. The saved context is
passed unchanged, including any literal template tokens in its text. Later
phases do not inherit it. Run-show recovery attempts include the resume
rationale in their detail.

### PR auto-fix repairs

`orbit run auto --auto-fix` lets the drain repair the pull requests its own runs
opened when a required check fails or the branch conflicts with its base. It is a
per-invocation flag with no configuration key, and it authorizes repair only:
merging still needs `--complete`. Without the flag nothing changes. The dashboard's
**Auto-fix PR blockers** checkbox starts the same owner drain with `auto_fix: true`.
It is off by default and independent of completion and proposed-task approval;
it requires an operator session and is unavailable on pull replicas. The receipt
and persisted run input carry the chosen repair authority. This control uses the
same watcher, crew pool and attempt ledger described below.

A delivery leaf whose worker dies after it opened its pull request and
promoted its task to `review` does not leave that pull request unwatched. The
owner drain's next pass resumes the interrupted leaf from its checkpoints, once
(a resumed attempt that dies again is left to an operator), when it holds the same `--complete` and `--auto-fix` grants
the leaf was started with, so the watcher repairs the same pull request and
head under the run's retry lineage and attempt ledger. The resumed run's crew
comes from the leaf's own input, not the adopting drain's `--allow-crew`. See
[adopting a dead leaf's pull request](runbooks/stuck-job-runs.md#a-delivery-leaf-that-died-after-opening-its-pull-request).

The crew comes from the existing pool resolution, in this order:

1. `--allow-crew` restricts which crews the draw may pick, as for every
   activity in the run.
2. `workflow.purpose_crews.auto_fix` is the pool when it has an enabled member,
   else `workflow.low_complexity_crews` ([purpose crew pools](#purpose-crew-pools)).
   The pool is drawn once per run with its weights, disabled crews skipped and
   provider usage limits honoured, as
   [above](#system-work-draws-from-the-complexity-pools). Conflict repairs under
   `--auto-fix` use this pool too, rather than the failure-recovery or medium pool.
3. An empty or wholly disabled low pool falls back to `workflow.default_crew`.

A pool none of whose crews the run may use stops auto-fix with a diagnostic
rather than escalating to another pool. Auto-fix never changes a task's
complexity or crew.

The PR watcher already used by `--complete` does the detecting, and the
existing recovery activities, final validation and push do the repair:

- On an open blocked PR, the watcher reads authenticated required-check policy
  for the pinned current head. Queued or running required checks keep waiting
  within the original completion deadline, even beside an unrelated optional
  failure. So does a required check that current branch policy names and a
  freshly pushed head has not reported yet; it is never treated as passed. An
  unclassified or optional pending check grants no wait, and a
  required failure still takes precedence over pending checks. Review gates and
  unreadable or stale evidence still stop the watcher. A failed required check
  whose job log shows a code failure is compared with the current base tip
  before a repair is admitted. Orbit reads the latest push run of the same
  workflow on that exact tip. If every failed required job succeeds there and
  the PR is behind, it refreshes the branch through the existing pinned rebase,
  validation and lease-checked publication path, then watches checks on the new
  head. This spends no agent repair attempt. A reviewed head returns for
  independent re-review before publication. A matching failure on the current
  base is known red: the watcher records it and waits within the completion
  deadline, spending no repair attempt. The existing inherited-failure wait also
  applies while a moved base's same job has not concluded. Missing, pending or
  unreadable tip evidence cannot prove a landed fix or a known-red base. A
  distinct candidate failure keeps the ordinary repair path and three-attempt
  budget. A conflict is rebased onto the current base by PR conflict recovery.
  If a published check repair then conflicts, recovery follows the contiguous
  auto-fix pushes recorded by the host for that PR and this run or its
  authenticated retry ancestors. The stopped rebase's branch and original head
  must both match that repaired head; its target base remains pinned. Missing,
  unrelated or disconnected push records cannot authorize a different head.
- A failed required check run that no workflow produced (a provider app's
  aggregate result, such as a code-scanning check beside passing analysis jobs)
  is repaired only from its own failure annotations. The watcher reads them
  from the forge for the check run and app identity that the head-bound
  required-check read reported, and requires a complete, unchanged answer bound to the
  repository, pull request, head and base branch. Each failure annotation must
  name a relative source file that exists at that head, through an exact-head
  blob link, with a line and a message. A passing analysis job does not satisfy
  the aggregate, and a failed workflow job beside it does not hide it: every
  failed required check needs its own evidence. Duplicate, foreign, stale,
  truncated or changed provider answers, a missing identity, an aggregate
  without failure annotations, and annotations on workflow configuration, on a
  path outside the checkout or showing a runner or network failure stop the
  watcher without spending an attempt. The answer is retained under
  `auto-fix/<run>/check-annotations-<failed-head>.json` before the repair is
  admitted from the same attempt budget.
- A job log that exceeds the display or full-command retention limit can still
  supply bounded failure regions. Recovery requires a fully scanned source,
  complete command and region selection, and exactly one provider-reported
  failed step. Retained job/step columns must agree throughout; raw job logs
  require a unique completed failed command. Missing provenance, conflicting
  labels and partial selections stop recovery without spending an attempt.
- An admitted required-check repair may change the source files needed to fix
  its evidenced failure outside the original task description or selectors,
  including a defect already on the shared base. Ordinary step recovery keeps
  its candidate-scope boundary. The repair writes the run/head-bound scope
  record named in its recovery input, explaining the cause and each changed
  path. The host checks that record against its admission and actual edits
  before committing, retaining a pre-commit copy under
  `auto-fix/<run>/scope-<failed-head>.json` and the committed record under
  `auto-fix/<run>/<repair-head>.json`. It widens task selectors with recovery
  provenance. Missing or stale records, unexplained files, protected metadata,
  environment files, symlinks and CI workflow
  edits are refused. Explanations are agent evidence for review; they cannot
  grant a repair admission or bypass validation. Required commands, test
  assertions and branch rules must not be weakened.
- An absent protected check can be recovered without an agent when the forge
  supplies complete effective classic and ruleset policy (including integration
  identity), an exact PR/head/base and latest workflow attempt, and source that
  directly links the missing job to a failed API-read prerequisite. The failed
  step must show an explicit GitHub HTTP 502, 503 or 504; credential or permission
  diagnostics veto recovery. The complete latest provider job/step table is bound
  to the immutable workflow and its bounded dependency graph. `--failed` repeats
  failed jobs and every transitive dependent, even a previously successful
  `if: always()` job. Recovery requires one uniquely diagnosed failed read job;
  additional failed jobs or unknown conclusions hold. Every source step of a
  previously executed selected job, including earlier/later steps and action
  pre/post effects, must be certified safe to repeat. Unknown provider steps,
  incomplete source/matrix coverage and unsupported repeated actions hold.
  Previously skipped selected jobs with zero steps may perform their first
  execution of the unchanged protected CI job, including analysis, local setup,
  immutable actions and check-result publication. This requires an effective
  protected context bound to that source job (or its literal matrix family) and
  the captured GitHub Actions app. Its explicit token permissions may grant
  reads and check/status/security-result publication, never unrelated writes;
  deployment environments, foreign credentials/secrets and unsupported actions
  hold. Unrelated fresh release, issue or deployment jobs have no such authority.
  Fresh unprotected jobs qualify only when their commands are supported reads.
  Source equality at the event base/head, original actor, PR/ref/head, provider
  run/attempt/suite and app bind this authority; job names or action pins alone
  grant nothing. The complete effect scope is revalidated before mutation.
  A literal `gh api repos/…` REST read may use the default GET or one explicit
  `--method GET`, `--method=GET`, `-X GET` or `-XGET`. Live PR-state workflows use
  `gh api --method GET 'repos/…' --jq '…' >> "$GITHUB_OUTPUT"`, with a fixed
  merged/open/closed projection producing one bounded `state` output. Its endpoint
  may interpolate only the provider's numeric PR expression. The historical
  `set -euo pipefail` / `state="$(gh api … | jq -er …)"` / local output and log
  template also qualifies unchanged: repository and numeric PR bindings are
  trusted, jq selects one of the same three scalars, and the only writes are
  isolated runner output and stdout. These are finite command templates, not a
  general shell parser or a workflow-specific exception.
  Repeated reads require GitHub-hosted Ubuntu using the default shell, `bash` or
  `sh` (the historical template requires `bash`). Containers, services, actions,
  working-directory and shell-startup/executable/host/config/output overrides
  hold. Token bindings must use the repository GitHub token, `GH_REPO` must use
  `${{ github.repository }}`, and `PR_NUMBER` must use the provider's numeric PR.
  Other options (including fields and input bodies), GraphQL, API writes,
  comments/echoed commands, arbitrary jq, pipelines outside the historical
  template and ambiguous shell composition hold without reservation or rerun.
  The adapter accepts literal job names and a bounded single-axis literal matrix,
  at most 100 jobs and 64 KiB of workflow source. A global cycle check visits each
  node once and each edge twice, including unrelated dependencies, so converging
  paths cannot multiply work. Cyclic, unresolved, dynamic or ambiguous
  dependencies hold. The workflow blob must match both the event base and exact
  PR head, proving the merge-ref workflow uses that DAG. Changed definitions,
  incomplete pages, source, jobs or logs, and additional administrative rules hold.
- Before rerunning failed jobs, the owner reserves one infrastructure rerun per
  repository/PR across all heads, resumes and sibling executions under the primary
  task's write lock. Its original policy, workflow source, identities and log are
  retained in history and `auto-fix/<run>/infrastructure-<workflow-run>-original.json`.
  Replies and observations are retained beside it. This budget is separate from
  the three agent repairs and draws no crew. A lost reply spends the reservation:
  subsequent polls and resumed executions observe the current attempt and status
  without repeating the mutation. A second failure, superseded attempt, changed
  policy or identity, or absent protected result holds the PR in review. Reads,
  the mutation and waiting share one absolute completion deadline. Before each
  prerequisite policy, API, source, remote or log read (including log fallbacks),
  the adapter refuses an expired budget and clamps the subprocess timeout to the
  remaining milliseconds. Expiry during a read prevents the next request and
  any new rerun or merge; pre-mutation revalidation uses that same deadline.
- Approvals, permissions, admin rules, unknown status, unreadable evidence,
  infrastructure failures outside that narrow prerequisite case and optional
  checks failing alone launch no agent; the watcher stops with the reason.
- The repaired candidate passes required validation, is pushed only when the
  remote branch is still at the head the repair was admitted for, and the
  watcher then checks the new head. A head that the review gate already
  approved goes back through the pipeline's re-review round before it can merge.
- Each admitted attempt is written to the primary task's history, with the
  PR, head, blocker, attempt number, crew and the draw's source, before any
  agent starts. That record removes duplicates across polls, restarts and
  owner/follower overlap: a PR head gets at most one repair per blocker. It
  also caps each PR at three repairs across all heads and runs. When the budget
  runs out, the watcher stops with `auto_fix_budget_exhausted` and lists the
  attempts.
- A resumed job inherits repair admissions and contiguous publication edges
  only from its durable, same-job retry ancestors. It can keep watching a
  published repair or validate and lease-push an admitted local repair left
  committed before interruption. Unrelated runs confer no authority; PR,
  branch, exact-head and review guards still apply. A subsequent base conflict
  leases against that authenticated current head while retaining the original
  publication checkpoint as provenance. The refreshed candidate passes captured
  validation before publication, or before returning for independent re-review;
  concurrent remote movement is refused. Resume spends no new
  repair attempt merely to finish an inherited repair and grants no merge
  authority.
- A base refresh retains the failed and passing job identities and exact base
  tip in `auto-fix/<run>/landed-<failed-head>.json` and records
  `pr_auto_fix_base_refresh` in task history before mutation. Its owner-side
  reservation bounds base refreshes independently of the repair-attempt budget.
  Old fork-point failure logs are unnecessary: the current tip's latest push
  jobs supply the evidence. A green tip alone does not prove the base fixed
  that one failure: when the latest push run of the head's fork point passed
  the same job, the failure is the candidate's. That check does not by itself
  end the comparison. When every failed required check is candidate-owned
  that way, completion spends no base refresh and admits one ordinary repair,
  even when the pull request's one base update is already reserved. The same
  repair path holds when one check is candidate-owned and another is green
  on the tip without a successful fork-point job, as long as no required
  check is known-red on the tip. A known-red sibling wins over a
  candidate-owned check: the tip fails that sibling with the same diagnostic,
  completion records `pr_auto_fix_base_red`, and no repair attempt is
  admitted. Other matching current-base failures also record
  `pr_auto_fix_base_red`. Ordinary agent repairs retain their existing
  admission history. Missing evidence leaves the ordinary diagnosis path in
  place.
- Closed or merged PRs, and heads superseded by a newer push, get no repair.

### Setting `task.crew`

| Surface | How |
|---|---|
| Dashboard | The crew dropdown on each task card. `default: <crew>` means the task has no `crew` and inherits `default_crew`. |
| CLI | `orbit task add --crew <name>`, or `orbit task update <id> --crew <name>`. `--crew ""` on `update` redraws for the current complexity. |
| MCP | `crew` on `orbit.task.add` / `orbit.task.update`. An empty string on update redraws; `null` is rejected. Omitting `crew` keeps an explicit pin; a complexity change can redraw a pool assignment or default fallback. |

### What "ran" vs what "was selected"

`orbit.task.show` returns `crew` (the stored selection) and `crew_source` (provenance, absent on legacy records), plus `resolved_crew` and `crew_model` when this host can resolve it; an associated run's persisted resolution takes precedence over current config. A `fields: ["crew"]` projection returns the stored selection. `task.crew` is validated on write. Deleting an explicitly pinned crew makes `orbit run ship` fail at run start, before any agent dispatches. Pool assignments are revalidated at admission, so keep pools valid when deleting members.

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

`$CARGO_HOME` is the child's variable if passed through `[execution.env].pass`, else `~/.cargo`. The grant applies only to profiles that already grant some write (`implementer`, `unrestricted`, `docs_writer`, …); read-only profiles such as `reviewer` and `pure_compute` get none. On Linux, a cache path missing on the host is skipped, not created.

## Sandbox pseudo-tty allocation (macOS)

The macOS profile allows `pseudo-tty`, `/dev/ptmx` (read, write, ioctl), and read, write and ioctl on `/dev/ttys[0-9]+`, so `openpty`/`posix_openpt` work inside a worker (a test driving a CLI through a real terminal, say). Other PTYs remain subject to normal OS checks. Linux needs no equivalent rule.

---

## Executor `host_os`

An executor asset may declare the host operating systems it can run on, beside `sandbox` and `model_flag`:

```yaml
spec:
  host_os:
    - macos
```

- The value is a non-empty subset of `linux`, `macos` and `windows`. An empty list or any other name is refused when the asset loads, and the error names that asset. Omit `host_os`, or set it null, and the executor runs on every host. Assets that omit it load as they did before the field existed.
- A task's effective OS requirement is its `os:` tags intersected with its assigned crew's executor `host_os`. Local admission, `orbit run ship` discovery, the drain-start warning and owner pull admission all read that one intersection, so they cannot disagree. An empty intersection (an `os:linux` task assigned a muse crew, for example) is refused when the task is created or updated. The reason names the crew, the executor constraint and the task's tags, and nothing is written: the task never becomes one that no host can run.
- **Operator readout.** Task show (`Runs on:` and JSON `os_requirement`), task list and dashboard task details derive the same tags/stored-crew intersection without writing task fields or adding OS tags. JSON includes `any_of`, `describe`, `source` (OS `tags`, assigned `crew`, and constrained `executor` when present) and `state`. Unknown or disabled crews are `unresolved`; disjoint constraints are `conflict` and admit no host. `orbit task list --runs-on linux|macos|windows` (MCP `runs_on`) filters before pagination, includes unrestricted tasks, and excludes unresolved/conflicting requirements. With neither tags nor a constrained stored crew the JSON key is omitted. For an unset crew this is a tags-only readout: automatic pool/default routing remains undecided and can impose further OS constraints. The stored crew is used even when a past run records a different crew.
- An explicit crew this host cannot run stays assigned and waits, for example `waits for a macos host (crew muse-wonder)`. It is not redrawn onto another crew. Admission draws and `workflow.default_crew` resolution skip a crew whose executor excludes this host; the other members keep their weights, and an excluded default is left unset rather than pinned. Creation-time complexity draws — task creation, `task update --crew ""`, and complexity re-rates — instead count a crew as runnable when any host serving this workspace can run it: the owner itself, plus each registered host (`~/.orbit/hosts.toml`, what `orbit host list` reports, including each host's `host_os`) whose recorded operating system the executor allows and which the owner's own claim records for this workspace show pulling here within the last seven days. Being registered is not enough: a host in the registry that serves only other workspaces — or none — never counts, and a follower that stops pulling ages out after the window and stops counting, though tasks already drawn keep their crew and wait. So a Linux owner whose Mac pull follower recently claimed work in this workspace can draw a muse crew from a complexity pool; the task is stored with that crew and pool provenance, waits on the Linux owner under the same effective-OS intersection, and is claimed only by a macOS follower through owner pull admission. A crew no serving host can run is still skipped at creation, and a workspace with no serving followers draws exactly as a single host always has. Activity and purpose pools (recovery, review, pilot) run on the executing host and keep filtering by that host alone, as does every admission-time draw. A host's OS is read from it when `orbit host add` registers it (discovery reports `host_os`); an entry registered before that field existed records no OS and counts for no limited crew until it is removed and added again. If no supported purpose or complexity pool remains, local admission and the automatic draw withhold that task with a wait naming the default and the OS it needs; leaving the crew unset never permits the unsupported default to launch. Discovery for a named task uses that run's admitted crew selection, including an explicit choice retained by replay, instead of retrying the unsupported automatic default. This does not override the task's stored crew constraints or its `os:` tags. Activity default fallbacks apply the same host check, including native-inspection activities. Provider preflight stays a backstop for a missing binary or a failed login on a host the executor does allow.
- Seeding copies a shipped `host_os` onto an installed default only when that install still omits the field. A value an operator set is kept. Clearing the field restores the shipped declaration on the next seed. Custom executors outside the shipped set are not rewritten.
- `orbit doctor providers` reports `unsupported on this OS` when `host_os` excludes this host, including when the binary is absent, so an operator is not told to install it here. `not installed` is only for an executor this host may run whose command is missing. The full `orbit doctor` and the dashboard's Doctor panel agree: a routed crew whose executor excludes this host is `skipped` with the same `unsupported on this OS` message and no remediation.
- Muse is the shipped case: `macos` only, as [Muse Code](#muse-code-cli) records. A Mac that pulls those tasks from the owner must run the owner's exact Orbit build. `orbit host list` shows the owner reachable with the same `binary_version` and `protocol_fingerprint` ([distributed drain](runbooks/distributed-drain.md)).

## Executor authentication recovery probes

An executor may declare `spec.auth_probe`. Only a pull drain's **auth-excluded** providers run it: never at drain start, for healthy crews, after the window closes, or for capacity or refusal exclusions. The first attempt is due ten minutes after the failed leaf; failures wait twenty, then thirty minutes (the cap). A pass durably acknowledges those auth failures and re-admits the provider's crews within the window; it never clears a preflight, capacity, crew or host exclusion. Without a probe the exclusion lasts the whole window: re-login and start a new drain. Owner-local drains use timed, task-specific [provider holds](#provider-failure-holds) instead. Drain-side view: [distributed drain runbook](runbooks/distributed-drain.md).

Only the shipped **claude** executor declares one: a minimal `claude -p --model haiku` call with built-in and MCP tools disabled ([CLI contract](https://code.claude.com/docs/en/cli-reference)), a one-line prompt on stdin, and success on exit zero plus `ORBIT_AUTH_OK` on stdout. Each attempt costs one model request. `claude auth status` is not used: cached status can lag a real 401 and may refresh shared OAuth credentials. Codex, Gemini, Antigravity, Grok, Copilot, Cursor, Pi, OpenCode and Muse declare none (their minimal model-call and success contracts are not established); local-shell has no provider authentication.

Seeding preserves existing operator executor definitions; to opt in, add the declaration to the installed definition and start a drain:

```yaml
auth_probe:
  args: ["-p", "--model", "haiku", "--output-format", "text", "--tools", "", "--disallowedTools", "mcp__*", "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}', "--max-turns", "1", "--no-session-persistence"]
  stdin: "Reply with exactly ORBIT_AUTH_OK."
  timeout_seconds: 60
  success: { kind: stdout_contains, text: ORBIT_AUTH_OK }
  relogin_hint: "Run `claude auth login`, or renew `claude setup-token`."
```

- `args` replaces the activity argv and uses the same resolved executor command and launcher.
- `success.kind` is `exit_zero`, or `stdout_contains` with non-empty `text`; both require exit zero. `timeout_seconds` is 1–120.
- A probe gets the leaf runner's cleared environment plus `[execution.env].pass`, provider-required and fixed entries, sandbox resolution and provider carve-outs (the macOS login keychain included), bounded process-tree supervision, and no Orbit activity tool grants. Without explicit `allow_fallback`, a sandbox refusal keeps the exclusion and backoff.

`orbit doctor`, `orbit run show <drain>` and the dashboard drain panel show provider, host, exclusion time, auth class, re-login hint, credential origin and next probe time; doctor reads the record and calls no provider. Credential origin names a passed `CLAUDE_CODE_OAUTH_TOKEN` (or `ANTHROPIC_API_KEY`) by name only, else the macOS keychain or provider cache: the configured route, not a keychain read or proof of which credential the CLI used. To avoid sharing a rotating Desktop login, pass a dedicated `claude setup-token` credential as `CLAUDE_CODE_OAUTH_TOKEN`.

## `[execution.env]` — the agent subprocess environment

Every agent subprocess (bare, Bubblewrap or `sandbox-exec`) starts from a **cleared** environment and receives only:

| Group | Contents |
|---|---|
| Baseline | `HOME`, `LANG`, `LC_ALL`, `LOGNAME`, `PATH`, `SHELL`, `TERM`, `TMPDIR`, `TZ`, `USER`. `PATH` starts with any [`workflow.validation_env.path`](#agent-sessions-share-the-configured-path) entries. |
| `pass` | Names listed in `execution.env.pass`. Default: `HOME`, `PATH`, `CODEX_HOME`, `TMPDIR`, `USER`, plus `__CF_USER_TEXT_ENCODING` on macOS. |
| Provider extras | Variables the selected provider runtime declares it needs. |
| Orbit envelope | Named `ORBIT_*` variables: run, task and session identity (`ORBIT_RUN_ID`, `ORBIT_TASK_ID`, `ORBIT_SESSION_ID`, …), locators (`ORBIT_WORKSPACE`, `ORBIT_WORKTREE_ROOT`, `ORBIT_BIN`, `ORBIT_REGISTRY_ROOT`, …) and activity bindings (`ORBIT_ACTIVITY_*`, `ORBIT_STEP_INDEX`, …). Privilege-bearing names (`ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN`) are not admitted; an inherited `ORBIT_ROOT` is removed. |

This is an allowlist, not a secret filter: a benignly named credential such as `DATABASE_URL` never reaches an agent unless you name it. Agents keep network access, so this is the boundary against accidental exfiltration.

```toml
[execution.env]
pass = ["HOME", "PATH", "CODEX_HOME", "TMPDIR", "USER", "GITHUB_TOKEN"]
```

- `pass` replaces the default rather than extending it; restate the baseline names you want.
- A listed name that is unset is absent, not empty. Names must be valid identifiers.
- `pass` is a [security key](#where-config-lives): a workspace file that omits it gets the built-in default, not the global value.
- Inheriting the full environment is not configurable; a stale `execution.env.inherit` is ignored.

**Unset names.** A `pass` name the launching process lacks reaches no agent, and the provider falls back to another login (a missing `CLAUDE_CODE_OAUTH_TOKEN` falls back to the desktop login). Start drains from a login shell, or export the variable in the service's environment. A drain or ship start (`orbit run auto`, `--pull` included, and `orbit run ship`; MCP `orbit.workflow.auto` start and `orbit.workflow.ship`; the dashboard) reports them in its `warning` (a `Warning:` line in CLI text), and `orbit run job` warns once on stderr, for each unset or empty name you added; all of them record the names on the run (`Unset env:` in `orbit run show`, `run.env_pass_unset` in JSON, the dashboard run detail). Built-in defaults are not reported (`CODEX_HOME` and macOS's `__CF_USER_TEXT_ENCODING` can legitimately be absent). `orbit doctor`'s `env-pass` row checks its own environment. These are warnings only; the start proceeds and no value is printed.

**Worker scope.** The same admitted names are given to a detached pipeline worker. On Linux, containment forwards them with name-only `systemd-run --setenv=NAME`, so the values stay in the child environment and never appear in `systemd-run` or worker argv. An explicit worker edit, such as a toolchain `PATH` or removing `ORBIT_ROOT`, wins over the admitted snapshot. See [worker containment](#worker-containment).

**Clock-started runs.** launchd (macOS) or a systemd user timer (Linux) starts `orbit clock tick` with no login environment. The tick reads `~/.orbit/clock.env` (created comment-only, mode `0600`, by `orbit routine init --install-clock`) and gives each discovered workspace's runs the names that workspace's effective `pass` admits, as child-environment defaults; the tick's own process environment is never changed. A non-empty value already in the tick's environment wins, and an empty one can be filled from the file. A dry run loads nothing. Unit files never contain a secret. A `clock.env` that is a symlink, owned by another user, or group- or world-readable is refused as a load error (the tick still runs). `orbit clock tick --json` reports loaded names as `clock_env_loaded`, never values. Sandboxed agents can't read `clock.env` itself ([SECURITY.md](../SECURITY.md#sandbox-model-and-known-limits)).

**Claude on macOS.** A Claude activity with neither `CLAUDE_CODE_OAUTH_TOKEN` nor `ANTHROPIC_API_KEY` in its environment is refused before `claude` starts, rather than fall back to the Claude Desktop login, which the Desktop revokes mid-run. A drain submitted on such a host (local or `--pull`) excludes the Claude crews from its crew window instead, so it claims only work another provider can run, and its submission names the remediation once; an explicit `--allow-crew` that names only Claude crews is refused. `orbit doctor`'s `claude-worker-token` row warns ahead of time, naming the `pass` list or `clock.env` that lacks the token; a `clock.env` token under a name `pass` omits counts as lacking.

---

## Other sections

The remaining keys, grouped as `orbit config show` groups them.

### Execution

| Key | Default | What it does |
|---|---|---|
| `execution.codex.sandbox` | `workspace-write` | Codex sandbox mode: `read-only`, `workspace-write` or `danger-full-access`. Under `workspace-write`, Codex's extra writable roots are Orbit's runtime stores, never the whole workspace `.orbit` or `~/.orbit`. `orbit init` seeds `danger-full-access` globally. [Security key](#where-config-lives). |
| `execution.codex.approval_policy` | unset | **Advanced.** `untrusted`, `on-request` or `never`. Security key. |
| `execution.env.pass` | `HOME`, `PATH`, `CODEX_HOME`, `TMPDIR`, `USER` (+ `__CF_USER_TEXT_ENCODING` on macOS) | Environment names passed into agent subprocesses. Security key. See [`[execution.env]`](#executionenv--the-agent-subprocess-environment). |
| `execution.proc_spawn_max_timeout_minutes` | `45` | **Advanced.** Longest timeout one `proc.spawn` call may use inside a managed activity (1–1440), also capped at the activity's remaining wall-clock budget (passed to the agent as `ORBIT_ACTIVITY_DEADLINE_UNIX_MS`). Outside an activity the ceiling is 60 seconds. The result reports `timeout_ceiling_ms` and its source. |
| `execution.agent_stall_minutes` | `30` | **Advanced.** Minutes a provider process may produce no stdout or stderr and use no CPU before Orbit ends it and fails the step as stalled, so the usual retry and recovery path runs. `0` disables the check and leaves only the wall-clock timeout. At most 1440. |

### `[review]` and `[operation]` — automatic review

Both tables resolve built-in → global → workspace. Unknown keys warn and are ignored; known values still validate at load. Design: [review gate](design/review-gate/2_design.md); operations: [review-gate runbook](runbooks/review-gate.md).

| Key | Default | What it does |
|---|---|---|
| `review.timing` | `off` | The one review layer before landing. `before_pr` holds PR creation for a fresh reviewer that fixes what it finds. `before_landing` opens the PR first, reviews its published head while hosted CI runs, and merges only the head the review settled; a reviewer fix is validated and pushed onto the PR under a lease, and any outcome but approve leaves the PR open and unmerged, with the task in `review` and a typed reason (see [before-landing review](design/review-gate/2_design.md#32-before-landing-review-of-the-open-pr-orb-14849)). PR route only. Captured at submission, so a run in flight keeps its value. |
| `review.before_landing_hosts` | `[]` | Owner policy for the distributed drain: machine ids (`hm_…`) whose claimed leaves review their open PR before it lands, while this owner's own deliveries do not. See the per-host note below the table. |
| `review.minutes` | `30` | Wall-clock limit for one candidate's review, fix commit and final validation included (1..=1440). Retries and resumes share the minutes; the reviewer is stopped when they run out, and a spent review is not restarted. A base replay stopped at its review deadline is inconclusive and is not cached [ORB-15350], so raising `review.minutes` allows a retry to complete without being suppressed by an earlier cutoff. A changed candidate (after a completion rebase, say) is a new review. |
| `review.baseline_commands` | `[]` | Commands settlement may rerun on the host, besides `workflow.required_validation_commands`, to confirm a [reviewer's red-base claim](#a-red-base-is-held-not-blocked). A listed command is binding: it needs a passing record or a claim settlement reproduces, or the review settles incomplete. Captured when the delivery or claim is admitted. With the required commands, also the gates an implementer may hand off when its sandbox deferred only one kind's tests (Bubblewrap, Seatbelt, or `ps` on a macOS claimed leaf) ([required validation](#required-validation)). |
| `review.verify_findings` | `false` | **Advanced.** Review findings must be verified before approval. When `true`, a review-filed `bug` (tagged `code-review`, `delivery-code-review`, `security-review` or `qa-sweep`) is refused at approval and promotion and withheld from drain admission as `unverified_review_finding` until `finding_verification_pipeline` records a CONFIRMED verdict for its current material. When `false` or absent, verification state is ignored there, and readiness, drain summaries and `orbit doctor` report no such hold. Read each time the gate is checked, so `orbit config set` or the dashboard's Settings › Effective switch applies to running drains without a restart. Turning it on does not enable the seeded `finding_verification` routine, and the job and routine stay available with it off; a REJECTED verdict still rejects the task. |
| `[[review.host_evidence]]` | `[]` | Checks a claimed leaf owes for the paths it changed; see [owed host evidence](#owed-host-evidence). A workspace list replaces the global one. |
| `workflow.purpose_crews.review` | unset | Crew for automatic review: review tasks the after-landing `delivery-code-review` auto-task mints (unset: that definition's template crew), and the reviewer `review.timing` runs while `workflow.purpose_crews.pre_landing_review` is unset. One crew name, or a weighted pool. See the review crew pool note below the table. |
| `workflow.purpose_crews.pre_landing_review` | unset | Crew for before-PR and before-landing review only, including the reviewer a claim captures for a `review.before_landing_hosts` machine. Unset or `[]`: `workflow.purpose_crews.review`. Written like it. See the review crew pool note below the table. |

**Deprecated review keys.** `review.before_pr` and `review.before_landing` load into `review.timing` with a deprecation warning: `before_pr = true` is `before_pr`, `before_landing = true` is `before_landing`, both off is `off`. A workspace that sets one of them keeps the other from the global file, as it did before. Load fails, naming the keys, while they would both be on, since there is one review layer before landing. `operation.review_crew` loads into `workflow.purpose_crews.review`. A file that also sets the replacement keeps the replacement. Runs, claims and pipeline state recorded with the old switches keep their captured review: the captured admission and ship contract still carry `before_pr` and `before_landing` flags.

**Per-host before-landing review.** `review.before_landing_hosts` is read only by the owner, and only for claimed leaves. A leaf on a listed machine reviews its PR before it lands, as `review.timing = before_landing` makes every leaf do; the owner's own deliveries and unlisted machines are unaffected. Entries are trimmed and de-duplicated. Each must be an `hm_` machine id, with letters, digits, `_` or `-` after the prefix; any other entry fails load, naming its index. The list is redundant while `review.timing` is `before_landing`, and loading fails while it is `before_pr` and the list is non-empty. The owner resolves it for the caller's machine when a follower probes or pulls, and the claim captures the result, so a follower that echoes its probed contract matches. The machine label is caller-chosen, so this is policy, not a security boundary. Protocol revision is unchanged; the contract is in the [`orbit.task.pull` spec](design/distributed-drain/specs/task-pull.md).

**Review crew pool.** `workflow.purpose_crews.review` is one crew name (`"sol"`) or a pool (`["sol", "grok"]`, or `["sol:3", "grok:1"]`), written like the [complexity pools](#weighting-a-pool): all bare or all weighted, and at least one member above weight `0`. Each review draws one crew by weight after three filters, each applied only when some member survives it: a member that is disabled or undefined here drops out; a member whose provider is at a usage limit here drops out; a member that implemented the reviewed work drops out (a distributed claim has no implementer, so it skips this filter). If no member survives the first filter, the review refuses to start with that member's refusal. A distributed owner draws one crew per claim, when it answers the probe. `[]` unsets it. Load refuses an empty name, a mix of bare and weighted entries, a weight that is not a non-negative whole number, a crew the registry does not define, a weighted pool that names a crew twice, and a pool whose weights are all `0`.

**Pre-landing review pool.** `workflow.purpose_crews.pre_landing_review` is written, admitted and drawn exactly like `workflow.purpose_crews.review`, and splits the two layers: before-PR and before-landing review (local deliveries, and the review contract an owner captures for claims) draw from it, while the review tasks `delivery-code-review` mints keep drawing from `workflow.purpose_crews.review`. So a host can review before landing on cheap, fast crews (`pre_landing_review = ["luna", "haiku"]`) and keep a stronger after-landing pool. While it is unset, or a later layer sets it to `[]`, the reviewer before landing falls back to `workflow.purpose_crews.review`, so a config without it behaves as before. A review admission records the layer of whichever key supplied its crew as `crew_source`. Before-PR and before-landing review refuse to start while neither key names a crew, and the refusal names both keys. `orbit config show --all` prints the effective pool on the before-PR and before-landing lines, with the key and layer it came from (and `workflow.purpose_crews.pre_landing_review is unset` when it is the fallback); `--json` carries it as `review.before_pr.crews`, `crew_source` and `crew_key`, and `review.after_landing.crews` and `crew_source` are the after-landing pool. The dashboard's Config tab shows both pools on the review-layer steps.

**After-landing review** is the `delivery-code-review` auto-task's `enabled` flag (`orbit auto-task toggle delivery-code-review on|off`), not a config key. `orbit config show --all` (and the `review` object in `--json`), `orbit doctor`'s `review` check, the dashboard Config tab and `orbit.drain.probe` report all three switches with their source (minutes for the first two, the next batch due for after-landing). The default text view already prints the `review.*` rows and omits that summary. The `review` check names the commit observed against `origin/<branch>` and fails once the oldest unobserved first-parent commit has waited the batch's `max_wait_minutes`, even if the tracking tip is recent. Doctor and auto-task inspection read the existing tracking ref without fetching (the local branch when there is no origin; a missing tracking ref leaves verification unknown). Delivery evaluation fetches, and defers on fetch failure without spending a coverage retry. A batch counts as reviewed only on [per-delivery evidence](design/automation-triggers/5_operations.md#evidence-submission) whose examined and skipped paths match each delivery's frozen diff, from a review task whose agent persisted its own execution summary; otherwise the batch stays owed with a typed reason. A landing on a base that moved after review is covered too, labelled `rebased_clean`, when the landed PR's head is the reviewed candidate and a conflict-free merge of that change onto the new base reproduces the landed tree exactly ([review gate §7](design/review-gate/2_design.md#7-delivery-coverage)). Any other base change, a head pushed after review, or a conflicting base stays an ordinary obligation.

**Tasks tagged `skip-review`.** A candidate whose tasks all carry `skip-review` and that changes only `workflow.light_delivery_paths` skips every timing: the gate admits no reviewer before the PR or before landing (`skip_review_light_delivery`), and the after-landing sampler excludes the landing. Any other changed path ignores the tag, and the task's history says why. See [light delivery](#light-delivery-skip-review-and-skip-validation).

**Local-only delivery.** Before-PR and before-landing review need a PR, so both are refused for local-only delivery ([captured timing](design/review-gate/2_design.md#2-captured-timing)). In a local-delivery workspace with either on, `orbit doctor`'s `review` check names the deciding layer (`review.timing = before_pr (global)` or `(workspace)`) and the remedy (ship through the PR route, or set `review.timing = off` for local delivery); readiness reports otherwise eligible local tasks as `eligible: false` with `reason` `local_route_before_pr` or `local_route_before_landing` and the layer in `detail`; and the local drain withholds them before spawning delivery (earlier per-task exclusions keep their own reasons). A workspace `timing = "off"` clears the hold, but direct `task_local_pipeline` admission still fails closed whenever its captured policy enables before-PR review.

#### Owed host evidence

Each `[[review.host_evidence]]` rule has `kind` (`codeql` or `host_sandbox_test`), `name`, `paths` (workspace-relative globs), `os` (`linux` or `macos`), the exact `command`, and a result `artifact` (a relative `.json` path, unique across rules). A rule Orbit could never fulfil fails load.

- A `codeql` rule is owed on a host of another OS and fulfilled by the owner on `os`.
- A `linux` `codeql` rule may add `hosted_check`, the pull request check that runs exactly its `command` on every candidate, named as the forge reports it (`CodeQL / Analyze (rust)`, or the check's own name). On a before-landing review on a claim host of another known OS, such as macOS, the requirement is then owed to the owner (`owner_owed_evidence`) instead of holding the review: owner acceptance refuses a certificate that owes it any check its captured rules did not declare that way, and the owner's landing merges the pull request only after that check reports an explicit success on the reviewed head (a CheckRun `SUCCESS` or a StatusContext `SUCCESS`). It waits while the check is absent, still running, or completed without a success (`SKIPPED` or `NEUTRAL`), and when the check failed it recovers or stops as for any failed pull request check (one branch update for a failure inherited from the base, one rerun for a transient one, the one automatic repair for the candidate's own; see the [distributed drain runbook](runbooks/distributed-drain.md)), whatever branch protection requires. Branch protection's own reading of a skipped or neutral required check is unchanged. Without `hosted_check` — a custom query, or a workspace with no hosted CodeQL — the requirement holds the review like any other owed check. Declare it only for a check that really runs the rule's command: Orbit compares the name, not the queries the job runs. Any other kind or OS with `hosted_check`, or an empty one, fails load.
- A `host_sandbox_test` rule is owed on a host of `os`, which runs it outside the agent sandbox at settlement, so its command must be one settlement admits. A held `linux` rule the leaf could not run is fulfilled by a Linux owner whose host can create namespaces.
- The owner captures the rules on each claim. The reviewer records an owed check `not_run` and never attempts it, and a verdict whose only gaps are owed checks holds for them instead of blocking.

Example: the [CodeQL runbook](runbooks/codeql-local.md#non-linux-hosts) rule (`kind = "codeql"`, `paths = ["**/*.rs"]`, `os = "linux"`, `command = "scripts/codeql-rust-local.sh …"`, `artifact = "evidence/codeql-rust-linux.json"`, and `hosted_check` only where a hosted job runs that command). Design: [owed evidence](design/review-gate/2_design.md#4-what-the-validation-records-establish-orb-11528-orb-11545).

#### Distributed PR delivery

At claim admission the owner captures `review.timing` as the contract's `before_pr` and `before_landing` flags and, when either is on, a review contract: its version, the reviewer crew from `workflow.purpose_crews.pre_landing_review`, else `workflow.purpose_crews.review` (for a pool, one crew drawn when the owner answers the probe, which the pull that echoes it keeps), `review.minutes`, `[[review.host_evidence]]`, `workflow.required_validation_commands` and `review.baseline_commands`. The follower runs that contract, never its own settings: it needs a matching build and protocol, a before-PR gate and the captured reviewer crew (else `before_pr_reviewer_unavailable`; the implementer's crew is never substituted), and its own `before_pr` is declared for diagnostics only. The handoff's typed evidence (reviewed head and base, reviewer, certificate) must satisfy the contract, with a passing required record per captured command and the captured `review.baseline_commands` unchanged; if the owner's current lists no longer match the snapshot, acceptance fails closed with fresh-claim guidance. Protocol revision 8 carries the list: explicit `[]` means no required checks, and a contract without the field is refused. A claim on a machine listed in `review.before_landing_hosts` captures `before_landing` on while the owner's own `review.timing` is not `before_landing`; `before_pr` stays off for it. Under `before_landing` the leaf reviews its own PR after `pr_open`, merges nothing, and hands off evidence for the head it settled; evidence of the other timing or another head is refused. After-landing review stays owner-side and does not refuse a pull. Wire contract and refusal codes: [`orbit.task.pull` spec](design/distributed-drain/specs/task-pull.md).

### `[pr]` — pull requests

| Key | Default | What it does |
|---|---|---|
| `pr.task_url_template` | unset | **Advanced.** URL template linking a task ID in PR descriptions. |
| `pr.close_on_terminal` | `true` | **Advanced.** Close a task's open Orbit-authored delivery and `[BLOCKED]` preservation PRs when it lands (`done`), is rejected or archived, when an operator or agent moves it back to `backlog` or `someday`, and when a newer delivery attempt opens its own PR (rules below). `false` disables all of these. |
| `pr.delivery_authors` | `[]` | **Advanced.** Forge logins (case-insensitive) whose PRs count as Orbit-authored, for closing them and for settling a task from its delivery PR merged outside Orbit ([stuck job runs](runbooks/stuck-job-runs.md#a-delivery-pull-request-merged-outside-orbit)). Empty: the login `gh` is authenticated as on this machine. Add a follower's login if it opens PRs this owner has not accepted as a handoff. |

`pr.close_on_terminal` closes a PR only if its head branch is `orbit/<TASK-ID>-…`, its body names the task, and it was opened by a `pr.delivery_authors` login or is the delivery PR of an accepted follower handoff. Human PRs and other tasks' PRs are never touched, and branches are never deleted. On `done` the landing PR stays open: any `#N` the done note names, or, when the note names none and the task leaves `review`, the PR it was reviewed through. The closing comment names the landing (the done note, such as `delivered by pull request #N merged as <sha>` or `already landed as <sha>`) or the state and reason. Starting a run or blocking a task closes nothing.

A task keeps one open delivery PR. When a run's `pr_open` (or a failure handoff) creates a PR, it closes the task's other open PRs that pass the same three tests, commenting `Superseded by #N` with the new PR. Reusing an existing PR closes nothing more. Without `pr.delivery_authors`, the author test uses the author of the PR the run just created. If the run stops between creating its PR and closing the others, the others stay open until the task's next created PR, an attributed requeue or a terminal decision closes them; close them by hand meanwhile. One listing reads at most 500 open PRs, so in a repository with more, an earlier PR beyond that page stays open; the same cap applies to the terminal and requeue closures below. This is how a re-admitted task's earlier `[BLOCKED]` PR closes: the failure handoff keeps it open while the task is blocked and until the next attempt publishes. The step's `closed_stale_pull_requests` output lists each close; a failed close is listed there and the step still succeeds.

An ordinary status change (`orbit task update --status`, the dashboard,
`orbit.task.update`) is refused while a non-terminal local job run lists the
task in its submitted `task_ids`. The refusal names the run, even when the task
has no `job_run_id` or distributed execution claim. Stop the run before moving
the task, or deliberately override with the human CLI's `--force` (dashboard
`force: true`); the override records a `forced` status event. The tool has no
force override. Same-status writes, field edits and pipeline-internal
transitions remain available.

An operator's or agent's move from active work back to `backlog` or `someday` (`orbit task update --status`, the dashboard, `orbit.task.update`) also closes the task's open Orbit-authored PRs, with a comment naming the move and its note. It keeps every PR a later run may still continue:

- all of them while a run of the task is live or a claim on it is unsettled, or when that cannot be read, since a leaf may still be landing one. A resumed or adopted `task_pr_pipeline` retry counts while it is live, even when the task's recorded run is still the finished source that retry continues. A move made then closes nothing, and only a later move from `backlog` to `someday` (or a terminal decision, or the task's next created PR) closes them;
- all of them when the task was last `blocked` before it was shelved, since the next run resumes the failure handoff's candidate;
- all of them when a final recovery requeued the task, since the next run resumes that published candidate;
- the PR of the candidate the owner kept from the task's last claim, or all of them when that candidate cannot be read.

The blocked and final-recovery exceptions hold however often the task moves between `backlog` and `someday` afterwards. `--discard-candidate` with the move drops every exception except a live run or claim. Moving from `someday` to `backlog`, or out of `done`, `rejected` or `archived` (including `--force`), closes nothing. Automation's own requeues and holds close nothing. Merged PRs are never touched.

A task with no recorded PR makes no forge call. A forge error is logged as a warning and never fails the transition.

### Housekeeping

| Key | Default | What it does |
|---|---|---|
| `tasks.id_start` | unset | Forward-only floor for this machine's task-ID allocator, raised on every runtime build and never lowered, so machines can hold disjoint ranges. For the first seed prefer `orbit workspace init --task-id-start N`. See [task migration](design/task-migration/1_overview.md). |
| `ci_failure.operator_suppression_hours` | `6` | **Advanced.** Hours a CI sweep finding archived or rejected without `covered_by` suppresses its exact failure key, from that event (0–720; 0 disables). An explicit task or GitHub PR cover instead holds the key while open, or until its landed commit is observed in the failing checkout; if it fails to hold, later completed repairs still own duplicates or defer older failures until their landing is tested. |
| `automation.stall_window_minutes` | `60` | **Advanced.** How long a delivery-automation consumer may sit on a stuck deferral (`history_diverged`, `repository_changed`, `provider_identity_missing`, `state_missing`) before a warning and one deduped friction (1–1440). Transient backpressure never escalates. See [auto-tasks](../plugin/skills/orbit-setup/references/auto-tasks.md). |
| `runtime.log_retention_days` | `7` | Delete `orbit.jsonl` and `orbit-agent.jsonl` archives older than N days (≥ 1). |
| `runtime.log_max_total_mb` | `500` | **Advanced.** Operational `orbit.jsonl` archive budget in MiB, pruned oldest first (≥ 1). |
| `runtime.log_max_file_mb` | `100` | **Advanced.** Roll the active operational log past N MiB (≥ 1, ≤ `log_max_total_mb`). |
| `retention.audit_days` | `60` | Days `orbit gc audit` keeps audit rows (1..=36500). Older host-wide command-audit rows, this workspace's run-audit rows, and run-audit rows whose workspace id is no longer registered become reclaimable, with blobs no remaining row or pending write names. Another registered workspace keeps its own window. Nothing is deleted until an operator runs `orbit gc audit --apply` or enables the `store-gc` routine. |
| `retention.runs_days` | `60` | Days after a terminal run finishes before `orbit gc runs` may drop its pipeline state (1..=36500). The run row, steps and summary stay; held and non-terminal runs are never selected. The same pass also reaches unregistered workspace ids, and prunes per-minute run and host resource history older than a fixed 30 days. On a large store, 14 reclaims much more terminal run state than 60 (about 1.8 GiB versus about 0.4 GiB on a store of about 6 GiB). |
| `retention.journal_days` | `14` | **Advanced.** Days after a task commit is applied before `orbit gc journal` may delete its row from the host store's commit journal (1..=36500). Recovery replays only `prepared` and `committed` rows; an `applied` row is a decision already published to its task, kept only to diagnose recent transitions. Prepared, committed and aborted rows, and each task's newest applied row, are never deleted. The same pass reaches unregistered workspace ids. The default is 14 rather than the 60 of the other windows because the journal grows fastest: at about 600 rows of about 80 KB a day, 60 days would let it pass 2.5 GiB. Nothing is deleted until an operator runs `orbit gc journal --apply`; the `store-gc` routine does not run it. |
| `retention.store_warn_bytes` | `1073741824` | **Advanced.** `orbit doctor` warns when the host store file is larger than this many bytes (default 1 GiB) and this workspace has no enabled `store-gc` routine. Zero warns for any non-empty file. The check stays ok under the threshold or while the routine is enabled. |
| `worktree.reclaim` | `["target", ".orbit/tmp/*-target", ".orbit/tmp/base-wt"]` | **Advanced.** Relative rebuildable path globs reclaimed in kept terminal worktrees; an explicit list replaces earlier layers, `[]` disables. `*` stays within a component; a whole-component `**` spans components. Absolute, parent or root-matching patterns fail load. Registration, worker, symlink, confinement and Git content gates protect candidates and tracked files. See [worktree reclamation](runbooks/worktree-reclaim.md). |
| `worktree.reclaim_below_free_mib` | unset | **Advanced.** Also reclaim during admission while free space beneath the state directory is below this, oldest terminal worktrees first; active runs stay protected. |
| `worktree.parked_reclaim_after_hours` | `336` | **Advanced.** Hours a task must stay parked in `someday`, counted from that status change, before worktree GC removes a terminal run's worktree serving it (0..=876000). A live worker, an unsettled claim or a Git worktree lock keeps it, and a clean worktree whose run branch holds commits no other ref reaches and `origin` lacks stays with its branch (`skipped:unpublished_commits`). See [parked tasks](runbooks/worktree-reclaim.md#worktrees-of-parked-tasks). |
| `security_alert_sweep.min_severity` | `moderate` | **Advanced.** Lowest Dependabot and Code scanning severity the security alert sweep files. See [below](#security_alert_sweep--security-finding-severity). |

Agent relay output goes to `orbit-agent.jsonl` with its own 200 MiB archive budget and 50 MiB active-file limit, so it cannot consume the operational budget; each feed is pruned by its own size and age limits. The dashboard and `orbit log tail` merge both active feeds. Full rotation runs in long-lived processes (`orbit mcp serve`, `orbit clock tick`/`orbit sweep`, `orbit web serve`); short-lived commands only roll an oversized active file.

### `[security_alert_sweep]` — security finding severity

`security_alert_sweep.min_severity` (`low`, `moderate`, `high` or `critical`; default `moderate`) is the lowest severity `dependabot_alert_sweep_pipeline` files for Dependabot and Code scanning findings. Secret scanning findings are always filed.

```toml
[security_alert_sweep]
min_severity = "high"
```

Precedence: run/job `min_severity` input, then workspace, then global, then `moderate`; `orbit run job dependabot_alert_sweep_pipeline --input min_severity=critical` overrides one run. Scheduled routines use the configured floor without shadowing the job. An invalid value fails validation naming the key. The file step records `min_severity`, `min_severity_source` (`input`, `workspace`, `global` or `built-in`) and `excluded_below_min_severity`; `orbit run show <RUN_ID>` summarizes the filed count, floor and source, and excluded alert count and numbers, even when the sweep succeeded.

## Plugins — `.orbit/plugins.yaml` and `[plugins.<ns>]`

Plugins install once per machine (`~/.orbit/plugins/<ns>/<version>/`); enable state and grants are host-local. A checkout keeps only its gitignored pin file, and `orbit plugin sync` installs what it names. Linked Git worktrees share the main checkout's `.orbit/plugins.yaml`: runtime loading, `orbit plugin sync` and `orbit plugin doctor` read that file and ignore a worktree-local one. Operator guide (installs, grants, source builds, workspace toggles): the orbit-setup [plugins reference](../crates/orbit-core/assets/skills/orbit-setup/references/plugins.md). Manifest spec: [plugin standard](design/plugins/1_scope.md).

```yaml
# .orbit/plugins.yaml — per-checkout, gitignored
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
- **Grants and builds are per invocation, never config.** A manifest's `spec.permissions` is only a request: `--grant` on `plugin add --enable`, `enable`, `upgrade` or `sync` grants it, an ungranted plugin's tools register inactive, and an upgrade that widens the request disables the plugin until re-granted. A `git+<url>#<full commit id>` source declaring `spec.build` compiles in an isolated build sandbox (Bubblewrap on Linux, `sandbox-exec` on macOS) and needs `--allow-build` on that `orbit plugin add` or `upgrade` (else `build_consent_required`, showing the build plan); consent never comes from pins, `config.toml`, the environment or MCP, and managed runs and routines cannot give it (`build_consent_unavailable`).
- **`[plugins.<ns>]`** holds the plugin's settings, validated against its `spec.config.schema` over its defaults, layered workspace over global. `orbit config get`/`set`/`unset plugins.<ns>.<key>` admits additional keys in open schemas (including when `additionalProperties` is omitted) and keys allowed by `patternProperties`. An unconditional `additionalProperties: false` (including through references and `allOf`) refuses unmatched keys with named properties/defaults as suggestions. Value-dependent and alternative-branch constraints are checked on writes against the entire effective section, including defaults. An invalid value disables only that plugin. A section for a plugin not installed here is warned about and ignored.
- **`[plugin_enablement]`** (workspace file only) switches a host-enabled plugin off here with `<ns> = false`; write it with `orbit plugin disable|enable <ns> --scope workspace`. An unset entry inherits the host state, and `true` never enables a plugin the host has disabled (`enable --scope workspace` refuses instead). `orbit plugin sync` applies a pin's `enabled: false` here, never to the host row. Values must be booleans keyed by namespace.

```toml
# .orbit/config.toml — this workspace only
[plugin_enablement]
graph = false
```

---

## Validation and errors

Config is parsed at startup, and invalid entries fail loud. Common errors:

| Message | Fix |
|---|---|
| `crew '<x>' is not defined in [crews.*]` | Name a crew that exists. |
| `[workflow].default_crew must be set when defining [crews.*]` | Set `default_crew`, or define an `opus` crew. |
| `[crews.<name>].<field> must not be empty` | Give the crew a `model` and `provider`. |
| ``crew `<x>`, which is disabled ([crews.<x>] enabled = false)`` | `orbit config set crews.<x>.enabled true`, or select an enabled crew. |
| `invalid type: string "…", expected a boolean` for `enabled` | Write `enabled = true` or `enabled = false`, unquoted. |
| `config schema no longer supports [agent.<role>] tables` | Migrate to `[crews.<name>]`. |
| `execution.codex.sandbox has invalid value '<x>'` | Use `read-only`, `workspace-write` or `danger-full-access`. |
| `operation.review_policy has invalid value '<x>'` | Use a supported legacy value or migrate to `[review]`. |
| `[task] artifact_store is no longer supported` | Remove the key. |

**Unknown keys and tables warn and are ignored.** Loading names the file and unknown path, suggests a nearby known key when available, and warns once per process for each file and path. An unknown table is reported once as a whole, including when empty; known section headers such as `[runtime]` stay quiet. This includes unknown `[operation]` and `[review]` fields; known values still undergo validation. Plugin-owned settings (`[plugins.<ns>]`), workspace plugin toggles and registered table-valued settings retain their own validation.

`orbit doctor` lists unknown paths alongside retired keys. `orbit doctor --fix-retired-config-keys` backs up each affected file, then removes unknown keys or whole unknown tables as well as compatibility keys. Check the list before repairing a file shared with a newer binary: a setting unknown to this version may belong to that newer version. Known settings, plugin sections and unrelated comments are preserved.

**Retired keys warn and are ignored.** Delete them; `orbit config get`/`set` reject them with migration notes. `orbit doctor` lists each one with the file that still sets it, and `orbit doctor --fix-retired-config-keys` backs up that file and removes those keys, preserving translated review settings as described below.

| Retired | Note |
|---|---|
| `operation.preset`, `completion`, `preparation`, `preparation_due_seconds`, `promotion`, `leaf_ceiling`, `recovery`, `recovery_episodes_per_task`, `recovery_minutes_per_task`, `delivery_cap` | Operation mode was removed; review settings live in `[review]` and `workflow.purpose_crews.review`. |
| `operation.review_repair_cycles`, `operation.review_reviewer_starts` | Each candidate gets one review whose reviewer fixes its findings in one commit, bounded by `review.minutes`. |
| `[docs]` | The docs corpus was removed. |
| `[semantic]`, `search.model` | Search is lexical (SQLite FTS5) and needs no model. The legacy `semantic.db` path remains the lexical search database. |
| `workflow.pilot_max_complexity` | Route a tier with `workflow.<tier>_complexity_crews`, or pin `crew` on the task. |
| `[duel]`, `[duel.models]` | Retired. |
| `[routines]` (`role = "source"`) | Every registered owner checkout is a routine source. |
| `knowledge.task_id_pattern` | Deprecated. |
| `execution.env.inherit` | Inheritance is fixed off. |

**Deprecated keys warn and are translated or ignored.** Still loaded, warned on every load, and refused by `orbit config get`/`set`; a later release makes them errors. `orbit doctor --fix-retired-config-keys` removes ignored keys directly. It rewrites each translated key into its replacement in the same file before removing the old keys, writing only replacements that file does not already set: `operation.review_policy` (`before-pr` or `none`) and the review booleans become `review.timing`, `operation.review_minutes` becomes `review.minutes`, the old pool keys become their `workflow.purpose_crews.*` replacements, and the containment and shell-probe pairs become their modes. A workspace file is rewritten against the global file beneath it, so a workspace that stated one review or shell-probe half gets the mode it already resolved to. Explicit modern values in that file win, preserving global/workspace precedence. Each edited file keeps an original backup; a second successful run writes nothing.

If **either file** contains `review_policy = "after-landing"`, even if another layer masks it, the repair refuses before changing or backing up either config. First choose the intended consumer state with `orbit auto-task toggle delivery-code-review on` or `off`. Set `review.timing` and `review.minutes` explicitly as needed, then remove the legacy policy manually and rerun the repair. Toggling the consumer alone does not make the legacy key safe for automatic removal.

| Deprecated | Translation |
|---|---|
| `workflow.system_crew` | Ignored: system work draws from `workflow.<tier>_complexity_crews` ([system work](#system-work-draws-from-the-complexity-pools)). Delete the key and set those pools. |
| `operation.review_policy` | `before-pr` sets `review.timing = "before_pr"`. `after-landing` enables the `delivery-code-review` auto-task while no operator has configured it; once its `enabled` flag is set by `orbit auto-task toggle` or any other edit, that flag decides. `none` turns neither on. A `[review]` table in the same file wins. |
| `operation.review_minutes` | Becomes `review.minutes`, now one candidate's review limit rather than a lineage total. |
| `operation.review_crew` | Becomes `workflow.purpose_crews.review`, the same crew or pool. |
| `workflow.final_recovery_crews` | Becomes `workflow.purpose_crews.final_recovery`, the same pool. |
| `workflow.purpose_crews.ci_failure_sweep`, `dependabot`, `merged_head_repair` | Become `workflow.purpose_crews.sweep_tasks`. Load fails while one file sets them to different pools. |
| `review.before_pr`, `review.before_landing` | Become `review.timing`: `before_pr`, `before_landing` or `off`. Both on fails load. |
| `machine.worker_containment = true`/`false`, `machine.worker_containment_strict` | Become `machine.worker_containment`: `true` is `warn`, `false` is `off`, and `worker_containment_strict = true` is `strict` (refused beside `false`). |
| `workflow.validation_env.login_shell`, `interactive` | Become `workflow.validation_env.shell_probe`: `login_shell = false` is `off`, `interactive = false` is `login`, otherwise `interactive`. |
| `scoring.enabled` | Ignored: the agent-family scoreboard was retired and nothing records scoreboard metrics. Delete the `[scoring]` table. Recognised until 2026-11-12; after that it is an unknown key. |
