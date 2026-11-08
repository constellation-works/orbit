---
type: context
summary: Orbit Configuration
last_validated: 2026-11-02
---

# Orbit Configuration

Reference for Orbit's `config.toml`: every fixed key, its default and what it does. `orbit config keys` prints the same keys with descriptions. `orbit init` writes a minimal template, [`crates/orbit-config/assets/default-config.toml`](../crates/orbit-config/assets/default-config.toml), with the agent environment allowlist, the Codex sandbox mode, review-skip mappings for shipped auto-task provenance tags, and the `[workflow]` table its crew keys go into; this page is the reference. The website has a shorter overview under [Reference › Configuration](https://orbit-cli.com/reference/config/). To add an execution lane rather than configure a shipped one, see the [executor onboarding runbook](runbooks/executor-onboarding.md).

## Resident evolution ledger

Resident checkpoints record capability gaps, with their occurrences and cost, in the
workspace's evolution ledger. `orbit evolution add|list|show` and the `orbit.evolution.*` tools
record and read ledger records; the operator-only `orbit evolution decline` suppresses one. Nothing files evolution proposal tasks automatically: the weekly
evolution pass routine was removed [ORB-22844]. `orbit resident digest --json` reports state,
escalations, vetoable decisions, the top five pains, open proposals and metrics; `--top` changes
the pain limit (1–100), and `--since` accepts a span or RFC 3339 window start.

The digest's default window, and the span of ledger cost a decline measures, is a fixed 14
days. A record stays declined for a fixed 30 days after its proposal task is rejected; a declined
record reopens early only when its cost within the window doubles. `resident.evolution.window_days`
and `decline_suppress_days`, which set these before, are [deprecated](#validation-and-errors), and
so are the removed pass's thresholds
(`min_occurrences`, `min_distinct_cycles`, `min_distinct_mandates`, `min_cost_minutes`,
`max_proposals`) are [deprecated](#validation-and-errors).

## Where config lives

| Path | Scope | Created by |
|---|---|---|
| `<workspace>/.orbit/config.toml` | Workspace-local (per user, gitignored) | `orbit workspace init`, hand-authored, or `orbit config set --fresh` / `--seed-from-global` |
| `~/.orbit/config.toml` | Global | `orbit init` (only when absent, or under `--force`) |

Settings inherit per key: workspace overrides global, global fills omissions, built-in defaults fill the rest. Keep a workspace file to genuine overrides.

Routine and auto-task schedules are not config keys: they live in the definitions under `<workspace>/.orbit/routines/` and `<workspace>/.orbit/auto_tasks/`. Change a schedule with `orbit auto-task update`, `orbit.routine.control` `set_trigger`, or the **edit** link on the dashboard's Automation rows, which edits only the schedule or trigger; see [`automation.md`](../crates/orbit-core/assets/skills/orbit-setup/references/automation.md#changing-a-routines-trigger).

`orbit workspace init` seeds missing review keys for the registered ship mode
(a new registration defaults to `pr`; `--force` retains its mode unless
`--ship-mode` changes it). A PR workspace gets `review.timing = "before_landing"`,
`review.minutes = 30`, and `workflow.purpose_crews.pre_landing_review` containing
the enabled members of `luna` and `haiku` in the effective host/workspace config.
With neither enabled, timing is explicitly `"off"` and the pool is empty; init
never substitutes a costlier reviewer. A local workspace gets timing `"off"`
because before-landing review needs the PR route, plus the same explicit
30-minute budget. Each seeded key has a short comment explaining the choice.
Existing workspace values (including legacy review switches) win, and repeated
init leaves them unchanged. Global `orbit init` seeds no review timing and
keeps the built-in `off` default for other workspaces. When init creates the
first workspace policy layer, it explicitly preserves the sandbox, approval
policy and environment allowlist previously in effect; it leaves an existing
workspace policy layer's security settings alone.

Both config files must be regular files. Runtime loads and workspace plugin-toggle polling refuse symlink leaves (including dangling links), directories, FIFOs and other non-regular leaves. Symlinked parent directories are supported and resolved before opening the file. A missing file or parent directory contributes no layer, so global settings and built-in defaults still apply.

- **Tables** layer down to individual settings; **scalars and arrays** replace the global value.
- **Named crews** layer by crew name and field: a workspace `[crews.sol]` holding only `model = "gpt-6-astra"` overrides that one field of the global `sol` crew.
- **Security keys** (`execution.codex.sandbox`, `execution.codex.approval_policy`, `execution.env.pass`) never inherit from global once a workspace file exists; an omitted one takes its built-in default. A workspace file holding only `[plugin_enablement]` does not count, so those keys keep inheriting.
- `[machine]` and each crew's [`env`](#per-crew-credentials-env) are global-only and `[plugin_enablement]` workspace-only; any of them in the wrong file is refused at load, naming the file. A `[plugin_enablement]` table in the global file is a typed `host_configuration` error naming the host and file (not the caller's `invalid_input`), so a follower whose remote call the owner serves is classified by final recovery as transient infrastructure and requeues once the table is removed. `orbit doctor` (and the dashboard's Doctor panel, which runs the same probes) reports it as the `plugin-enablement` check with the exact fix: remove the table from the global file; host enablement is `orbit plugin enable|disable <plugin>` and a per-workspace toggle adds `--scope workspace`. Because the table stops the runtime from opening, a plain `orbit doctor` reads it straight from the global config this invocation would load — `~/.orbit/config.toml` by default, or the config under the resolved `--root`/`ORBIT_ROOT` data directory (including an initialized `<checkout>/.orbit` child for `--root <checkout>`) — and reports that check (plus a `runtime` row saying no other check ran) instead of the bare refusal; every other command, and doctor's repair flags and subcommands, keep failing closed.

`.orbit/config.yaml` (it stores `workspace_id`) is the workspace identity file, not runtime config.

## Host-health auto-task

`host-health` reads `orbit doctor --json` and warning/error service logs daily
(at 07:10 in the scheduler's local timezone). Unlike every other shipped
definition, it starts enabled in a new workspace. In an existing workspace,
`orbit workspace sync` or `orbit workspace init` turns on a `host-health`
definition no operator has configured (its file still holds the bytes Orbit
wrote and `orbit auto-task toggle` never set its flag). `orbit init` leaves
existing definitions alone. Once an operator toggles it, or edits the file,
that choice decides. Only the workspace owner mints it. Turn it off or on,
and tune its schedule, with:

```sh
orbit auto-task toggle host-health off
orbit auto-task toggle host-health on
orbit auto-task update host-health --cron '10 7 * * *'
# Or use an interval:
orbit auto-task update host-health --every-minutes 1440
```

Preparation pins the owner pass with `required_machine`. Collection never
repairs doctor checks, restarts services, edits config or reads credentials.
Doctor warnings/errors qualify on first sight; log signatures require three
distinct events across two completed collection windows on the same host within
seven days. Signatures remove volatile IDs, SHAs, home paths, ports and times.
The owner searches open tasks, tasks closed within 30 days and open frictions,
comments new counts on a covering task, and retains rejected mappings without
refiling them. It files at most five tasks per pass.

**Service configuration.** There is no `host_health.units` config key. The
source of service names is the host's installed service-manager configuration:

| Host | Configuration read | Log inputs |
|---|---|---|
| Linux | Installed and loaded systemd **user** services: unit identity, fragment/drop-in paths, executable/subcommand, working directory and output destinations | `journalctl --user -u <discovered-unit> --since <window-start> --until <window-end> --output=json --no-pager`, selected configured log files |
| macOS | User LaunchAgents plists: `Label`, `Program`/`ProgramArguments`, `WorkingDirectory`, `StandardOutPath`, `StandardErrorPath` | Configured stdout/stderr files, or a bounded platform log query |

The pass discovers Orbit web, drain/run and clock/sweep launchers from those
properties, including renamed units and host-wide services. Configure or rename
services through the host's normal service manager; the next pass reads that
configuration. It also reads the global `~/.orbit/state/logs/orbit.jsonl` and rotated
archives, plus the existing workspace `.orbit/state/logs/` when present. It never dumps whole units/plists, environment properties or credential
files. Missing managers, inaccessible logs, rotation gaps and failed doctor
execution are reported as missing coverage. Other platforms need equivalent
configured service metadata and log access; unsupported collection is not
reported healthy.

**Follower collection.** Doctor is host-local; passing a remote workspace does
not run it on that host. To monitor a follower, the operator creates a collector
variant in the **owner's** `.orbit/auto_tasks/`, using the shipped collection and
report instructions. Give it a unique name such as `host-health-collector-build`,
tags `host-health-collector` and `no-diff-expected`, the explicit role
`collector-only`, the target's stable machine ID, and an earlier daily schedule.
Preparation must pin its `required_machine` to that target, and that follower
must already be pulling work from the owner. Do not enable an owner aggregator
on a replica. A collector files nothing; it returns its two artifacts through
`task_artifacts` in claimed mode. The owner reads completed collector-task
artifacts, verifies their machine identity, and imports each host/window once.
Reports over 36 hours old or absent are coverage gaps; the previous host cursor
is retained. Without configured collectors the shipped pass covers its owner
only. Collector variants are operator-managed definitions; copy future collection
contract changes deliberately.

**Routes and resolution.** A verified Orbit code/default defect becomes an agent
bug with file/function evidence quoted at the inspected commit. Host, credential,
service-unit, OS and operator-only work goes to a configured [human crew](#human-crews).
If no human crew can be selected, the pass files a proposed setup chore tagged
`needs-operator` and `no-auto-approve`, requiring human assignment before any
promotion. Tags alone do not prevent dispatch; keep these fallback chores
proposed until an operator assigns a human crew. Every filing includes a
`## Resolve` section with exact host/shell-specific commands or an attached
`resolve.sh` (with `--dry-run` for a state-changing script), or concrete numbered
operator steps when no command exists, plus a verification command and expected
result. The collection pass never executes those repairs.

**Evidence and continuation.** Every completed pass attaches
`host-health-report.json` (signature groups, routes, dispositions, per-host
counts and coverage gaps) and `host-health-cursor.json` (source positions,
seven-day counts, imported windows, rejected mappings and pending evidence).
Collection stops at 10,000 records or 8 MiB per host; a read or filing cap is a
successful partial pass with a fixed window and resume position. The next pass
drains pending groups before advancing, without recounting events. Excerpts are
limited to three lines/600 characters per signature and pass through Orbit's
text redaction before entering an artifact; if that route is unavailable,
excerpts are omitted. Never print `~/.zprofile`, `~/.orbit/clock.env` or credential
file contents. Raw logs are never attached. Empty passes still store both
artifacts.

## Inspecting and editing

| Command | What it does |
|---|---|
| `orbit config show` | Effective merged view. Identity (`machine.id`, `machine.task_prefix`) leads, then Machine, Delivery (`workflow.*`), Crews, Execution, Review and Housekeeping. Each row's description is one sentence, followed by the key's allowed values in brackets when it has any; defaults and interactions are in this reference. Unconfigured advanced keys are hidden with a count per section. `--all` (alias `--advanced`) lists every key, expands unset sections, and appends Paths, Crew purposes and Review, which restate rows above. `--json` adds `provenance` and always includes `crew_purposes` and `review`. |
| `orbit config get <key>` | One value. |
| `orbit config set <key> <value>` | Write the workspace file (`--global` for the global one). The value parses as a TOML literal, else a string. Without a workspace file, pass `--fresh` (start empty) or `--seed-from-global` (copy the global file without its global-only keys: `[machine]`, each `[crews.<name>] env`, `lease.intake.peer_user` and `[lease.listener]`). |
| `orbit config keys` | Every fixed settable key with type, section, description and allowed values (a fifth, last column). `--json` adds `detail` (longer guidance, or `null`) and `allowed`. |
| `orbit config path` | The resolved `config.toml` path. |

`config set` checks a registered setting's supplied value type before staging the edit. A type mismatch names the key and supplied value, explains the expected type, and confirms the edit was not saved. For example, `orbit config set --global review.minutes abc` is refused; supply an integer such as `30` instead. Full config validation still runs before saving. Errors loading an already invalid config identify its file.

`config get` and `config set` refuse unknown keys. For a misspelled fixed key such as `workflow.base_branc`, text errors suggest a nearby key and point to `orbit config keys` for the valid-key list. JSON errors keep the `invalid_input` code and include up to three nearby keys in `did_you_mean`, closest first; when none is close enough, that field is omitted. Refused writes leave the configuration unchanged. Removed keys retain their migration guidance.

**Allowed values.** Every registry key has a description of at most 100 characters, and keys that accept more than their type allows also declare `allowed`. In `--json` (`config keys`, the `show` provenance, and the dashboard's config rows) it is `null` for a bool, an unbounded number or a free-form string, and otherwise one of:

- `{"kind": "enum", "values": [...]}`: exactly one of the values.
- `{"kind": "enum_list", "values": [...]}`: a list whose every entry is one of the values.
- `{"kind": "range", "integer": true|false, "min", "max", "unit"}`: a number in `min..=max`; `max` is `null` when only the minimum is bounded.
- `{"kind": "format", "note"}`: a free-form value shaped as the note says.

Each also carries `summary`, the one-line form the tables print (`off | login | interactive`, `1–10 rounds`, `≥ 1`). Config load and `config set` check the same declaration, so a value outside it is refused with `<key> has invalid value <v>; expected …` naming the declared range or choices. The Settings topic pages show each row's description and allowed values; editing an enum offers only its values, and a range field refuses a value outside its bounds before saving.

**Advanced** keys are shown whenever any layer sets them, including a global security value that is not inherited. JSON always includes every key. Scoped text views apply the same tier filter to the selected file. The tier affects display only: loading, `config get`, and `config set` are unchanged.

Each `show` value has one state: `workspace`, `global` or `environment` (the layer that set it), `default` or `unset`. `machine.id` and `machine.task_prefix` are not config keys in the text view: they render under Identity with state `identity`. JSON `state` stays `set`, `default` or `unset` for every key, including those two. A row names a shadowed layer: `(overrides global: main)`, or `(global sets danger-full-access — not inherited)` for a security key the workspace omits. A registered checkout also gets a `Workspace` line with the registry's base branch and ship mode, which delivery uses. In `--json`, each key's provenance has `scope`, `path`, `section`, `tier` (`standard`/`advanced`), `description`, `allowed`, `state` (`set`/`default`/`unset`) and `shadowed_by` (`[{layer, value, reason}]`, reason `overridden` or `not-inherited`); top-level `workspace_binding` is the registered base branch and ship mode, or `null`.

`--scope global|workspace` shows one file's values plus built-in defaults, without inheriting the other layer. Crew references are still validated against both layers' crews (partial workspace overrides included), here and in the dashboard's Workspace file view; a crew in neither is reported by name and key. A dashboard file validation error names the file and asks you to fix it before reloading. Scoped JSON reports `exists` (`config get`: key in that file) and `source.exists` (`config show`: file exists).

The dashboard's Settings payloads (`GET /api/config/effective`, `GET /api/config/file?scope=…`) add per-key comparison facts to each row. `built_in` is `{state, value}`: the value Orbit ships with, `state` `unset` with a `null` value when there is none. `fallback` is `{layer, value, state}`: what the key resolves to if the layer that sets it stopped setting it, or `null` when nothing can be removed or no admitted fallback can be predicted. Removing a workspace security key falls to the built-in value, not the global one, while a workspace file exists. `equals` is `{global, built_in}`: whether the row's value restates the global file's value (`null` when the global file does not set the key, and on the Global file view itself) and the built-in default, by plain JSON equality, so `opus` and `opus:100` differ. On the Global file view, `reach` says what a set value does for the bound workspace: `{state: "in-effect"}`, `{state: "overridden", workspace_value, equals_global}` or `{state: "not-inherited"}`. On the file views, `fallback`, `equals` and `reach` are `null` for rows the file does not set, and also when the two layers do not load together. Each row carries the registry's `description`, `detail` and `allowed` for its key. Every row also says what `orbit config set` would refuse: `read_only_reason` is the refusal note for a key no write may change (`null` otherwise), and `global_only` marks a key only the global file may carry, such as `machine.*`. The file payload also carries `text`: the file's raw TOML (empty when the file does not exist), shown by Settings › All keys. File views do not build crew rows. They report `crews_enumerated: false` and list the file's `[crews.<name>]` tables in `crew_tables`, so an empty `crews` list does not mean no crew is defined.

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
| `machine.labels` | `[]` | Yes (`--global`) | Capability labels this machine carries for tasks that require them (`["gpu", "xcode"]`). Its OS (`linux`, `macos` or `windows`) is an implicit label. See [capability labels](#capability-labels-machinelabels-and-requires_labels). |
| `machine.worker_containment` | `warn` | Yes | `warn` runs each detached pipeline worker in a transient systemd user scope (`orbit-worker-<run_id>-<nonce>.scope`), so a runaway run is throttled or OOM-killed inside it. Where no scope is available it launches the worker uncontained with a warning; on macOS it runs an in-worker watchdog instead, which stops a run past the memory or process limit without kernel enforcement. `strict` refuses any launch without kernel containment, which includes every macOS launch; `off` uses neither a scope nor a watchdog. A boolean still loads with a deprecation warning (`true` is `warn`, `false` is `off`). See [worker containment](#worker-containment). |
| `machine.worker_memory_high` | `40%` | Yes | **Advanced.** Scope `MemoryHigh=` (throttle point): bytes with optional `K`/`M`/`G`/`T`, a percentage of physical RAM, or `infinity`. |
| `machine.worker_memory_max` | `50%` | Yes | **Advanced.** Scope `MemoryMax=` (OOM point). Same grammar. |
| `machine.worker_tasks_max` | `4096` | Yes | **Advanced.** Scope `TasksMax=` (processes plus threads), at least 1. The macOS watchdog counts processes only. |
| `machine.worker_cpu_quota` | `0` (unset) | Yes | **Advanced.** Scope `CPUQuota=` as a percentage of one core (`400` = four cores); `0` sets no CPU limit. Linux only. |
| `machine.mcp.toolsets` | `[]` | Yes (`--global`) | MCP toolsets `orbit mcp serve` and `orbit mcp listen` advertise beside the role, or beside the default surface without one (`notice`, `tasks-write`, `default`, `all`, …). A server's `--toolset` replaces it. See [MCP roles and toolsets](#mcp-roles-and-toolsets). |
| `machine.mcp.role` | unset | Yes (`--global`) | The MCP role `orbit mcp serve` and `orbit mcp listen` serve: `operator`, `manager`, `resident`, `agent` or `reader`. Unset serves the default surface. A server's `--role` replaces it. See [MCP roles and toolsets](#mcp-roles-and-toolsets). |

### MCP roles and toolsets

Every tool belongs to exactly one MCP toolset (`orbit tool list` shows it), and a role is a named set of toolsets defined in one place (`McpRole` in `orbit-types`). `orbit mcp serve --role <role>` serves the role's toolsets plus the always-on `core` (workspace discovery); `--toolset` or `machine.mcp.toolsets` add to it. With no `--role` and no `machine.mcp.role` a server advertises exactly the surface it did before roles existed, so running long-lived clients are unaffected. A role limits only what MCP lists and dispatches; it never grants authority, so operator-only tools still need operator capability under any role. A call outside the role is refused `toolset_not_enabled`, naming the toolset, the role and the roles that include it. `--mode remote` forwards `--role` to the destination; `--mode federated` applies the role to every destination at the mux. `orbit tool list --role <role>` reports a role's tool count and approximate token size; `crates/orbit-mcp/tests/boundary/mcp_role_budgets.json` holds each role's size budget, and a test fails when a role exceeds it. Design: [federated MCP §roles](design/federated-mcp/specs/mcp-roles.md).

### `[machine.auto_concurrency]` — host forecast admission

Global-only host thresholds for forecast-based delivery admission in local
and pull drains. `off`, the default, leaves admission unchanged. `shadow`
makes each local drain pass record in `drain_last_pass.admission_forecast`
what gap fit would admit, while it admits as `off` and reserves nothing.
`enforce` admits local candidates whose profile fits under the forecast
within `admit_cpu_percent` and the fixed 75% memory limit, after the drain's
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
next request is forecast. `min_profile_deliveries`, the fixed 50-delivery
profile window and the fixed 120-minute horizon cap also shape the resource
profiles `orbit run profiles` reports. See [auto-concurrency §6](design/auto-concurrency/2_design.md#6-pull-drains).

```toml
[machine.auto_concurrency]
mode = "off"
admit_cpu_percent = 90
min_profile_deliveries = 10
```

| Key | Default | What it is |
|---|---|---|
| `machine.auto_concurrency.mode` | `off` | One of `off`, `shadow` or `enforce`. |
| `machine.auto_concurrency.admit_cpu_percent` | `90` | Forecast CPU **utilization** as a percentage of host cores; valid range `1..=100`. |
| `machine.auto_concurrency.min_profile_deliveries` | `10` deliveries | Minimum samples before using a profile key rather than falling back along the profile chain; at most the 50-delivery profile window. |

The rest is fixed [ORB-22808]. Forecast **non-reclaimable memory** is admitted up to 75% of host
memory, strictly below the global `workflow.resource_throttle.memory_high_percent`. A profile key
keeps its 50 most recent completed deliveries, and a candidate's forecast horizon is at most 120
minutes. Starvation guard: once the first candidate gap fit judges has failed for longer than 30
minutes, a drain admits nothing behind it until it fits, or until the host holds no running
delivery or reservation, when it is admitted. Stuck head: once a protected head's miss has not
improved (another resource, a later minute, or at least one percentage point less) over 20
consecutive passes, an `enforce` drain files one friction naming the head, its profile source and
key, the miss and the baseline, and `orbit run show` and `orbit run readiness` print a
`Stuck head:` line while it remains in the current live pass. Retained forecasts do not show this
present-tense line. Admission is unchanged. `admit_memory_percent`, `profile_window`,
`horizon_cap_minutes`, `head_max_wait_minutes` and `head_stuck_passes` are
[deprecated](#validation-and-errors).

**Observing admission.** `orbit run show <drain>` renders an `Admission:`
line from `pipeline_state.drain_last_pass.admission_forecast`: skipped
candidate titles, the failing resource, percentage and minute, and a protected
queue head's starvation guard, followed by a `Stuck head:` line naming its
miss, profile and baseline while that head is stuck in the current live pass.
The present-tense line is omitted when a finished drain or a no-forecast pass
shows retained history. Shadow decisions are labelled as hypothetical; they do
not hold admissions. With no block (off mode or an older drain), no line
appears. JSON preserves the complete recorded block, including the profile
source (`crew`, `exact`, `without_complexity`, `base_class`,
`heaviest_of_class`, `heaviest_on_host`, or `none`).

`orbit run readiness` shows the same last-pass snapshot, CPU and memory peaks,
and headroom in percentage points below the **current** configured admission
limits. JSON adds `capacity.admission_forecast`, with the recorded fields,
`task_titles`, `task_statuses` (the current status of each named task,
including one the admission population no longer lists because it is `done`),
`headroom` (`cpu_percent`, `memory_percent`,
`admit_cpu_percent`, `admit_memory_percent`), and the drain that recorded it
(`drain_run_id`, with `drain_live` false once that drain has ended). An unknown
host capacity has unknown headroom; negative headroom means the peak exceeds
the limit. Text names each task as `ORB-12 "title"` and prints memory in GiB.
A pass that builds a forecast also retains it in
`pipeline_state.drain_last_forecast`; readiness exposes the same observation
as `capacity.last_admission_forecast`, with titles, current statuses and
current-limit headroom.
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
The dashboard Tasks-dock Drain card shows the same snapshot as compact peak meters against the admit thresholds, with forecast time and mode; timing, amount and headroom stay in each peak row's tooltip and accessible text, while the first-hour sparkline is hidden in the dock and the meter is shown whether or not `series` is recorded. A live built forecast is shown as **Forecast** with its current-pass decisions in present tense (`Admitted`, `Held`, `Starvation guard protects`; shadow `Would admit` and `Would hold`, labelled hypothetical). A built forecast whose drain has ended is shown as **Last forecast** history, even when it remains under `capacity.admission_forecast` and is also present in `capacity.last_admission_forecast`. That retained block shows only the forecast time, mode and peaks, plus the conditional window-ended note below; it lists no decisions, no per-task statuses and no shadow verbs. When that drain has ended with a zero shared-occupancy count (`capacity.active_leaf_runs`) the block shows no window-ended note. A positive count, such as an independent delivery holding a slot, says `window ended · other work running`; an unknown count says only `window ended`. A live or winding-down drain never shows the ended line. A pass with no current decisions and no retained history says `no forecast this pass`; when a retained forecast exists and the current pass decided nothing, the retained **Last forecast** block stands alone with no heading, while current-pass decisions, when present, are shown under a `no forecast this pass` heading above the retained block. The persisted `task_statuses` and `series` fields described above still exist on the recorded block even though the retained dock summary does not list them.

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
select a cohort; without filters the report uses the newest 5000 forecasts
of every drain and mode, whatever their age, and `--last <N>` the newest
`N` (alone or with the other selectors). Both output formats identify that selection and report
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
[operator runbook](runbooks/forecast-accuracy.md). Records older than 30
days are pruned by `orbit gc runs --apply`, except the newest 10,000 forecasts with
the host minutes they pair with, and each workspace, work class and complexity's
newest 200 deliveries. See
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

- Without a systemd user manager (containers) under `warn`, or with `worker_containment = "off"`, workers run in the caller's cgroup and each Linux Orbit process logs one warning. macOS uses a watchdog in each running worker under `warn`: once per second it sums `phys_footprint` across the tracked process tree and its owned sessions, and counts processes (not threads). Above `machine.worker_memory_max` or `machine.worker_tasks_max`, it records a bounded snapshot of the eight largest consumers (command, PID/start key, memory and CPU rate), sends descendants SIGTERM, then SIGKILL after one second, records the run as failed, and terminates the worker last. The watchdog survives the submitting CLI exiting. `off` disables it. Memory percentages resolve against physical RAM; `infinity` disables only the memory ceiling. The five-second profile sampling cadence stays unchanged.
- `strict` retains its kernel-enforced meaning and refuses macOS watchdog launches. It starts no uncontained worker: the run reports why and how to enable a user manager or turn strict mode off, with error code `worker_containment_unavailable` (also in CLI JSON errors). The deprecated `worker_containment_strict = true` reads as `strict`, and fails config load beside `worker_containment = false`. `--strict-worker-containment` on `orbit run ship` or `orbit run auto` turns it on for one invocation; the auto coordinator passes it to leaf workers as `ORBIT_WORKER_CONTAINMENT_STRICT`.
- macOS `orbit doctor` and submission notes report **watchdog**, never kernel containment. It does not implement `worker_memory_high` throttling or `worker_cpu_quota`. One-second polling reduces overshoot compared with the five-second telemetry interval but cannot prevent fast allocations between checks. A daemon that calls `setsid` after leaving the tracked tree, an unseen short-lived session, and descendants of an escaped process are not found. Unreadable snapshots skip a check; the watchdog cannot survive its own worker being killed or a host failure. It is a best-effort watchdog, not a kernel security boundary.
- Every Linux worker, contained or not, starts with an `oom_score_adj` of at least 500, inherited by everything it runs, so a host-wide OOM kills a worker before the user systemd manager (100) or the drain that launched it. It is not configurable; scope units take no `OOMScoreAdjust=`, so the worker sets it on itself before `systemd-run` execs it.
- `machine.worker_cpu_quota` adds `CPUQuota=` to each scope, so one run cannot take every core. Only an unsigned integer loads, with no upper bound. `orbit doctor`'s `worker-containment` row reads `info` while it is unset and `ok` once it is set. systemd enforces it only where the user manager delegates the cpu controller; elsewhere the property is accepted and not enforced.
- A macOS watchdog breach names top-consuming child commands and memory bytes in `orbit run show`. The same snapshot is kept with the run as resource evidence, one bounded record per run until that run is deleted, and survives worker termination; `orbit run show --json` returns it under `worker_resource_limit`.
- A run that fails after hitting a worker limit carries error code `worker_resource_limit` in `orbit run show`. A run interrupted because its worker vanished names the worker's exit and scope evidence instead ([stuck job runs › How the worker ended](runbooks/stuck-job-runs.md#how-the-worker-ended)). Inspecting scopes: [operational logs › Worker Resource Containment](../crates/orbit-core/assets/skills/orbit-setup/references/operational-logs.md#worker-resource-containment).
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
provider_limit_overrides = []
provider_limit_budgets = []
provider_limit_explicit_crews = "wait"
crew_selection = "static"
task_types = []
# [workflow.type_complexity_crews.<type>] <tier> = [...]  (unset)
```

| Key | Default | What it does |
|---|---|---|
| `workflow.base_branch` | `main` | **Advanced.** Fallback base branch for ship, auto and pilot. The workspace registry's value (`orbit workspace show`) wins; `--base <branch>` overrides both for the ship target only and does not change the provider boundary guard. For a two-branch repo, register with `--base-branch agent-main`. The provider boundary guard's baseline for the primary checkout is the workspace base branch (the registered value, else `workflow.base_branch`): a primary left on another branch is tolerated with a warning while it stays put, and moving it anywhere but back onto the base branch during a run fails that run with `primary_checkout_drift`. |
| `workflow.default_crew` | see [resolution](#resolution-precedence) | Crew for a task with no `crew`. Must name a defined crew. |
| `workflow.system_crew` | — | **Deprecated, ignored.** Loads with a deprecation warning for one release; `config get`/`set` refuse it. System work draws from the complexity pools instead: see [system work](#system-work-draws-from-the-complexity-pools). Delete the key. |
| `workflow.low_complexity_crews` | `[]` | Pool a crew-less `low` task draws from, and the task pilot and PR auto-fix while their [purpose pools](#purpose-crew-pools) are unset; empty uses `default_crew`. See [pools](#automatic-crew-pools-by-complexity). |
| `workflow.medium_complexity_crews` | `[]` | The same, for `medium`; step-failure and PR conflict recovery draw from it too while `workflow.purpose_crews.failure_recovery` is unset. |
| `workflow.hard_complexity_crews` | `[]` | The same, for `hard`. |
| `workflow.xhard_complexity_crews` | `[]` | The same, for `xhard`. |
| `workflow.type_complexity_crews.<type>.<tier>` | unset | Pool a task of [type](#task-types) `<type>` at complexity `<tier>` draws from instead of `workflow.<tier>_complexity_crews`. See [type pools](#type-pools-by-task-type-and-complexity). |
| `workflow.purpose_crews.<purpose>` | `[]` | Weighted pool for one purpose of work (`task_pilot`, `failure_recovery`, `auto_fix`, `baseline_red_repair`, `sweep_tasks`, `pilot_warning_triage`). Unset or `[]` keeps the purpose's earlier route. `pilot_warning_triage` keeps only crews whose executor declares `inspection_args`, and falls back to `workflow.hard_complexity_crews` while unset or when no member qualifies. `final_recovery` (default `["sol:100", "opus:20"]`, `[]` disables it; see [final recovery pool](#final-recovery-pool)), `review` and `pre_landing_review` (see [automatic review](#review-and-operation--automatic-review)) keep their own rules. See [purpose crew pools](#purpose-crew-pools). |
| `workflow.provider_limit_overrides` | `[]` | Per-provider thresholds replacing the fixed 90% at which admission skips a crew whose provider or model usage window is exhausted or used at or above it (see [provider usage limits](#provider-usage-limits)), each `provider:percent` (`["claude:80", "grok:100"]`) or `provider:off` (`["codex:off"]`, record readings without gating admission). One entry per provider, percent `1`–`100`, an alias such as `anthropic` stored as its provider. An entry may instead name a crew that sets [`env`](#per-crew-credentials-env) (`opus-credits:off`), its own account. |
| `workflow.provider_limit_budgets` | `[]` | **Advanced.** Rolling budgets for providers that report no usage, each `provider:<amount><usd\|tokens>/<n><h\|d>` (`["grok:30usd/5h"]`), or for one crew that sets [`env`](#per-crew-credentials-env) (`opus-credits:200usd/30d`). See [budgets](#budgets-for-providers-that-report-no-usage). |
| `workflow.provider_limit_explicit_crews` | `wait` | **Advanced.** A task whose explicit crew is limited: `wait` keeps it in the backlog; `pool` draws from the unlimited members of its complexity pool. |
| `workflow.crew_selection` | `static` | **Advanced.** How task creation, complexity re-rate and admission draws weigh pool members: `static` uses the configured weights; `usage_pressure` scales each by its crew's weekly usage pressure and re-draws a system-chosen crew at admission. This is the policy key; the `crew_selection` object in `orbit run show` and run input is the recorded draw. See [usage-pressure draws](#usage-pressure-draws). |
| `workflow.task_types` | `[]` | **Advanced.** Ordered task types that [type pools](#type-pools-by-task-type-and-complexity) route by and the implementer scorecard's by-type matrix splits by, each a `[[workflow.task_types]]` table with `name`, `tags` and `paths`. See [task types](#task-types). |
| `workflow.crew_performance_types` | — | **Deprecated.** The former name of `workflow.task_types`, read into it with a deprecation warning until 2026-12-07. A file that also sets `workflow.task_types` keeps that. `orbit doctor --fix-retired-config-keys` rewrites it. |
| `workflow.crew_performance`, `workflow.crew_performance_strength`, `workflow.crew_performance_cap`, `workflow.crew_performance_window_days` | — | **Deprecated, ignored.** Crew-performance weighting, its scorecard snapshots and the `crew_performance_refresh` routine were removed [ORB-22804]. Load with a deprecation warning until 2026-12-07; `config get`/`set` refuse them. Route by type with `workflow.type_complexity_crews`, and delete the keys. |
| `workflow.auto_ship` | — | **Deprecated, ignored.** The ship sweep it opted into was removed. Loads with a deprecation warning; `config get`/`set` refuse it. Dispatch with `orbit run auto` drains or `orbit run ship`, and delete the key. |
| `workflow.required_validation_commands` | `[]` | Commands every delivered candidate must pass before push or merge. Empty runs no required check. See [required validation](#required-validation). |
| `workflow.distributed_completion` | `review` | How far this owner takes an accepted distributed-drain handoff: `review` waits for an operator's **Approve handoff**; `done` authorizes it on acceptance and lands it through `task_landing_pipeline`, rechecking this key before the merge. |
| `workflow.unlocked_context_paths` | `["**/*.md"]` | Path globs whose `context_files` entries take no context lock. See [unlocked context paths](#unlocked-context-paths). |
| `workflow.lock_modes` | `[]` | **Advanced.** Ordered `[[workflow.lock_modes]]` tables, each a `pattern` regex and a `mode` of `exclusive` (default) or `shared`. An overlap between two footprints at a shared path does not block admission. See [footprint lock modes](#footprint-lock-modes). |
| `workflow.unlocked_path_validation_commands` | `[]` | Commands a repaired candidate runs instead of the full required list when its tree differs from the run's last fully validated tree only in `workflow.unlocked_context_paths`. Empty always runs the full list. See [revalidating an unlocked-path repair](#revalidating-an-unlocked-path-repair). |
| `workflow.generated_paths` | `{}` | Generated files, as path glob = regenerate command. Conflict recovery regenerates a conflicting match instead of merging it. See [generated paths](#generated-paths-are-regenerated-not-merged). |
| `workflow.light_delivery_paths` | `["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]` | Path globs a candidate must change only within for a `skip-review` or `skip-validation` task to skip agent review or local validation. A leading `!` excludes. `[]` disables both tags. See [light delivery](#light-delivery-skip-review-and-skip-validation). |
| `workflow.test_paths` | `["**/test/**", "**/tests/**", "**/__tests__/**", "**/spec/**", "**/testdata/**", "**/*_test.*", "**/*.test.*", "**/test_*.*", "**/*_spec.*", "**/*.spec.*"]` | **Advanced.** Path globs whose changed lines count as tests in landed-delivery diff stats. See [landed diff classification](#landed-diff-classification). |
| `workflow.non_code_paths` | `["docs/**", "**/*.md", "**/*.mdx", "**/*.rst", "**/*.txt", "**/*.json", "**/*.yaml", "**/*.yml", "**/*.toml", "**/*.lock", "**/*.csv", "**/*.golden", "**/*.png", "**/*.jpg", "**/*.jpeg", "**/*.gif", "**/*.webp", "**/*.svg", "**/*.ico", "**/LICENSE*"]` | **Advanced.** Path globs whose changed lines count as other (docs, config, data, assets) in landed-delivery diff stats; everything else is code. See [landed diff classification](#landed-diff-classification). |
| `workflow.non_reviewable_paths` | `[]` | **Advanced.** Path globs left out of the reviewable-lines denominator. Docs and config stay reviewable; list lockfiles, goldens, snapshots and generated files here. Empty counts every line that is not a pure rename. Distinct from `workflow.non_code_paths`. See [landed diff classification](#landed-diff-classification). |
| `workflow.review_skip_tags.<tag>.paths` | unset | Path globs (same syntax as `workflow.light_delivery_paths`; `["**"]` allowed) a candidate carrying `<tag>` must change only within to skip agent review. See [review-skip tags](#review-skip-tags-workflowreview_skip_tags). |
| `workflow.review_skip_tags.<tag>.guard` | unset | Optional repository-relative executable, read from the delivery's base revision, that must exit 0 (with the base and head as its first two arguments) for `<tag>` to skip agent review. |

### Task types

`workflow.task_types` is an ordered list of task types, one `[[workflow.task_types]]` table per type with a `name` and at least one of `tags` or `paths`:

```toml
[[workflow.task_types]]
name = "review"
tags = ["no-diff-expected"]

[[workflow.task_types]]
name = "ui"
tags = ["ui"]
paths = ["web/**"]
```

A task takes the first type whose `tags` it carries (any of) or whose `paths` globs match any of its `context_files` (any of); a task matching none is `other`. Names must be unique and non-empty, `other` is reserved, and an entry with neither `tags` nor `paths` is refused with a config error naming the entry. Orbit ships no types beyond `other`, so an empty list puts every task in `other`. Two readers use the list: [type pools](#type-pools-by-task-type-and-complexity) route a task of a type through `workflow.type_complexity_crews`, and the implementer scorecard's by-type matrix (`orbit agents scorecard --role implementers --by-type`) splits its cells by type. The scorecard classifies historical tasks with the current list at scan time, so editing it re-sorts history.

The list was named `workflow.crew_performance_types` before [ORB-22804]. That spelling still loads into `workflow.task_types` with a deprecation warning until 2026-12-07, with the same entries; a file that sets both keeps `workflow.task_types`. `orbit doctor --fix-retired-config-keys` rewrites it.

### Unlocked context paths

`workflow.unlocked_context_paths` lists workspace-relative globs: `*` matches within one path component and `**` spans components, so `**/*.md` is every Markdown file and `docs/**` is everything under `docs/`. A `context_files` entry whose path matches stays in the task's context, and the agent still receives it, but takes no context lock. Local drain waves, readiness, backlog admission, `orbit task eligible`, lock contention and lock reservations skip it, so two tasks that share only a design doc beside their own code run together and landing reconciles the text by merge and conflict recovery.

A task whose every entry matches, such as a rewrite of one document, still locks them all, because two rewrites of one file are where a merge can silently produce contradictory text. A mixed task touching that file is not held back by the rewrite, since it does not request the file. Leave build and CI definitions (`Makefile`, `.github/workflows/*.yml`) and code out of the list: they define the gates, so a bad merge there breaks every delivery. `[]` locks every entry, as before the setting existed. Owner pull admission still checks a remote claim's full declared footprint. The value is read when an Orbit process opens the workspace.

### Footprint lock modes

Two tasks whose `context_files` overlap normally cannot run together. The second waits for the first, whether it is a local drain wave, a pull admission, a reservation or a claim. `workflow.lock_modes` lets you name paths where an overlap is allowed. These are files that merge cleanly or are regenerated, such as append-only `mod.rs` registration lists or a generated catalog:

```toml
[[workflow.lock_modes]]
pattern = '(^|/)mod\.rs$'
mode = "shared"

[[workflow.lock_modes]]
pattern = '^crates/orbit-core/assets/auto_tasks/body-history\.json$'
mode = "shared"
```

- Each `pattern` is a Rust [`regex`](https://docs.rs/regex/latest/regex/#syntax) searched for in the selector's normalized, workspace-relative anchor path (`symbol:` selectors use their backing file). Anchor patterns with `^` and `$` to match the whole path.
- `mode` is `exclusive`, the default and the behaviour without the setting, or `shared`. The first matching entry wins, in config order, and a path no entry matches is `exclusive`. So an earlier `exclusive` entry can carve an exception out of a later `shared` one.
- The mode applies at the **overlap point**. That is the narrower selector's anchor, or the common anchor when the two anchors are equal. `file:a/mod.rs` against `file:a/mod.rs` is waived when `mod.rs` is shared. `dir:a` against `file:a/mod.rs` is waived too, because they meet at the shared file. `dir:a` against `file:a/lib.rs` still blocks, because `lib.rs` is exclusive. A task stays blocked while any of its overlaps is not waived, and admission diagnostics name only the overlaps that still block.
- `module:` and `command:` selectors have no path, so they always lock exclusively.
- Every footprint check that admits or defers work applies the setting. That covers local drain waves, readiness, backlog admission, `orbit task eligible`, lock reservations, owner pull admission, owner-wave admission, execution-claim checks and the in-progress footprint guard.
- A shared path trades prevention for repair. Two tasks may both change it, and the second to land resolves the conflict through `base_conflict`, a rebase or a landing repair. Keep build and CI definitions and most code exclusive.

An invalid regex, an empty pattern or an unknown mode fails config validation, and the error names the entry by position and pattern. The setting belongs to the owner. A follower never sends it and it is not part of the request fingerprint, so an owner or follower that predates it treats every overlap as exclusive. Orbit ships no shared patterns. The value is read when an Orbit process opens the workspace.

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

### Landed diff classification

Each landed delivery records the lines it added and deleted, and the files it changed, in three classes for Analytics › Metrics › Lines changed. A changed path matching `workflow.test_paths` counts as tests; otherwise one matching `workflow.non_code_paths` counts as other; anything else counts as code. Both lists use the `workflow.light_delivery_paths` glob syntax (`*` within a component, `**` across components, a leading `!` excludes), and a list with no positive glob matches nothing. The defaults are language-neutral naming conventions (test directories and `*_test.*`, `*.test.*`, `test_*.*`, `*_spec.*`, `*.spec.*` files; docs, Markdown, data, lock files, goldens and images). Set them to your repository's layout:

```toml
[workflow]
test_paths = ["**/tests/**", "e2e/**"]
non_code_paths = ["docs/**", "**/*.md", "website/**"]
```

A rename counts its changed lines only, and a binary file counts as a changed file with no lines. A change to either list applies to deliveries recorded after it; rows already recorded keep their classes.

`workflow.non_reviewable_paths` is a separate exclusion for the reviewable-lines denominator used by the per-line experiment metrics. It uses the same glob syntax. Docs and config stay reviewable, so this list is not `non_code_paths` and it is not a built-in toolchain list: unset and `[]` exclude nothing. A pure rename (a path move with no edited lines) is also left out. Each task's count is taken from that task's own commits. A row recorded before the denominator existed, or whose commits cannot be isolated to that task, keeps an unavailable denominator and is never filled in by re-diffing a shared landing range. A change to the list applies only to deliveries recorded after it.

```toml
[workflow]
non_reviewable_paths = ["**/*.golden", "**/*.lock", "**/generated/**"]
```

### Review-skip tags: `workflow.review_skip_tags`

A workspace (or the global config, layered per tag like `[workflow.tag_instructions]`) can map its own tag to a review skip for low-risk work that touches paths outside `workflow.light_delivery_paths`:

```toml
[workflow.review_skip_tags.comment-hygiene]
paths = ["crates/**"]
guard = "scripts/check-comment-only-diff.sh"
```

A candidate skips every agent review layer (before-PR, before-landing and after-landing delivery-code-review sampling) for a mapped tag only when every task it delivers carries the tag, every path it changes since its synchronized base matches that tag's `paths` (the `workflow.light_delivery_paths` glob syntax, `!` excludes; `["**"]` allows every path), and the guard, when set, exits 0. Any other candidate gets normal review. Each decision is recorded once per candidate in the task's history as `light_delivery_applied` or `light_delivery_ignored`, naming the tag, its profile key and the failed condition (a task without the tag, the first path outside `paths`, the guard's exit status, or a guard missing at the base). Check them with `orbit task show`.

The guard is a repository-provided executable: Orbit only runs it, so a language-specific check (for example, that the non-comment content is unchanged) lives in the workspace. It is read from the delivery's base revision, never the worktree, so a delivery cannot weaken its own guard, and it runs the way required validation commands run (same environment and sandbox rules), in a throwaway detached checkout of the candidate's head commit (so it sees the candidate's `HEAD` and files, wherever the owner's own checkout stands, and the owner's branch and edits are untouched), with the candidate's base and head commits as its first two arguments and as `ORBIT_REVIEW_SKIP_BASE` / `ORBIT_REVIEW_SKIP_HEAD` (the tag as `ORBIT_REVIEW_SKIP_TAG`).

Delivery automation dry runs defer mapped-tag decisions, including profiles without a guard: they run no guard and record no task history. Unless a before-PR certificate or the built-in `skip-review` rule already covers a landing, the preview counts it as work requiring review. Real evaluation applies the mapped profile and records its decision, so it may find less review work than the preview reports.

Mapped tags have the same protection as `skip-review`: only filing a task (including an auto-task definition) or an operator update may add one; an agent's update that adds one is refused, and removal is always allowed. A mapped tag skips review only, never validation: required validation, workspace gates and hosted CI still run. The built-in `skip-review` and `skip-validation` tags keep their behaviour above and cannot be remapped. Config validation refuses a mapping that reuses a built-in tag name, has an empty or malformed `paths`, or a `guard` that is absolute or contains `..`, naming the key. `orbit config show` lists the effective mappings under `Review skip tags:` (and as `review_skip_tags` in `--json`).

The seeded template includes mappings for selected shipped auto-tasks' `auto-task:<name>` provenance tags, all limited to the documentation/CI paths shown above. Report-only `host-health` passes need no review-skip mapping. Defaults a release demoted to workspace-local lose their seeded mapping, but an existing config keeps any it already has. The template is not migrated into existing config files: `orbit init` leaves an existing config alone unless run with `--force`. To opt in on an existing install, add the same tables to its `config.toml`:

```toml
[workflow.review_skip_tags."auto-task:agent-guidance-audit"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:auto-task-yield-review"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:architecture-audit"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:delivery-code-review"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:dependency-freshness"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:code-review"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:code-health-sweep"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:doc-duties"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:escape-to-guard"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:friction-curation"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:full-code-review"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:path-risk-review"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:qa-full-sweep"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:qa-sweep"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:run-failure-patterns"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]

[workflow.review_skip_tags."auto-task:security-review"]
paths = ["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]
```

These mappings apply only to the auto-task that carries its provenance tag. Do not map finding tags such as `code-review`, `delivery-code-review`, `security-review` or `qa-sweep`: fixes filed by those reviews must keep normal agent review. A wider mapping for a future auto-task should remain disabled unless its config includes an appropriate repository-specific guard.

On a distributed claim, the owner captures its mappings in the claim's validation gates alongside `workflow.light_delivery_paths`. The follower leaf judges its candidate against that copy, guard included, and the owner judges the handed-off candidate again (guard included) when it accepts, recording the decision in the task's history. An owner too old to capture mappings leaves the leaf with none, so no mapped tag applies.

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
  | codex + claude | `["haiku", "luna"]` | `["terra", "sonnet"]` | `["sol", "opus"]` | `["opus", "astra"]` |
  | codex only | `["luna"]` | `["terra"]` | `["sol"]` | `["astra"]` |
  | claude only | `["haiku"]` | `["sonnet"]` | `["opus"]` | `["opus"]` |

  On any row, and alone when neither codex nor claude is detected, a detected `grok` is appended to `medium`; a detected `agy` appends `gemini-flash` to `low` and `argon` to `hard`; a detected `muse` appends `muse-spark`, `muse-wonder` and `muse-wonder-full` to `low`, `medium` and `hard`. With codex, claude, grok, `agy` and `muse` all detected that gives `low = ["haiku", "luna", "gemini-flash", "muse-spark"]`, `medium = ["terra", "sonnet", "grok", "muse-wonder"]`, `hard = ["sol", "opus", "argon", "muse-wonder-full"]` and `xhard = ["opus", "astra"]`. Copilot leaves the pools as they are, and an empty pool routes to `default_crew`. Init fails rather than write a pool entry it does not seed enabled.
- these [purpose pools](#purpose-crew-pools), each keeping only the members whose crew is enabled: `review = ["sol", "sonnet", "muse-wonder", "grok", "opus@medium"]`, `task_pilot = ["luna@high", "haiku@high", "muse-spark@high"]`, `failure_recovery = ["luna", "haiku", "muse-spark", "grok"]`, `auto_fix = ["luna", "haiku", "muse-spark"]`, `final_recovery = ["sol", "muse-wonder", "opus@medium"]`, `baseline_red_repair = ["terra", "sonnet", "muse-wonder", "grok"]` and `pre_landing_review = ["luna", "haiku", "muse-spark"]`. A pool left empty is omitted and keeps its earlier route, except `final_recovery`, written as `[]` (final recovery off) so it never falls back to a crew this host does not run. `sweep_tasks` and `pilot_warning_triage` are not written;
- `experiments.enabled = true`, `workflow.crew_selection = "static"`, `workflow.unlocked_context_paths = ["**/*.json", "**/*.txt", "**/*.md", "**/mod.rs", "**/*.toml"]`, `workflow.mid_run_steering.enabled = true` and `workflow.task_pilot_freshness.material_fields = ["description", "criteria"]`;
- when init creates this machine's identity, `machine.auto_concurrency.mode = "enforce"` beside it, unless already set. An existing machine keeps its mode.

These choices apply to newly seeded global configurations. Existing crew tables and complexity or purpose pools keep their explicit values; workspace overrides keep their normal precedence. To use the new defaults in an existing configuration, edit those choices explicitly.

Interactive init asks for the default crew by name, offering only enabled crews. It no longer writes or asks for `workflow.system_crew`: system work draws from the pools. With no supported CLI, every crew is written disabled and `default_crew` stays unset, so it resolves to the disabled `opus` and agent dispatch refuses until you run `orbit config set crews.<name>.enabled true`; deterministic jobs such as store retention still run. Init never writes `[crews.system]`, `[crews.custom]` or `[crews.qa]`.

### `[workflow.resource_throttle]` — host pressure

While CPU, memory, disk I/O (Linux) or a watched filesystem stays at or above its high mark, admission using those thresholds is held until it falls below the resume mark; a local drain may still admit eligible CPU-light auto-tasks while CPU alone is held and reserved slots remain. Memory, disk and I/O pressure also hold CPU-light work, and pull drains do not use the budget. Running work is never cancelled. Drain, ship and readiness behavior: [host resource pressure runbook](runbooks/distributed-drain.md#host-resource-pressure).

A scheduled host shutdown is a separate hold. It stands new unattended work down only when the schedule is 180 minutes away or sooner, so a reboot set for tomorrow does not idle the host all day. Readiness still names a later schedule. See [scheduled host shutdown](runbooks/distributed-drain.md#scheduled-host-shutdown-or-reboot).

| Key | Default | What it does |
|---|---|---|
| `workflow.resource_throttle.enabled` | `true` | **Advanced.** Enable the throttle verdict. `false` restores unthrottled admission; telemetry and severity are still reported. |
| `workflow.resource_throttle.cpu_source` | `"load"` | CPU signal: `load` or `utilization`. Linux utilization excludes idle and I/O wait; macOS uses Mach utilization for either choice. |
| `workflow.resource_throttle.cpu_high_percent` | `90` | CPU high-water mark. |
| `workflow.resource_throttle.cpu_resume_percent` | `85` | Resume below this CPU percentage. |
| `workflow.resource_throttle.memory_high_percent` | `90` | Memory high-water mark. |
| `workflow.resource_throttle.memory_resume_percent` | `85` | Resume below this memory percentage. |
| `workflow.resource_throttle.disk_high_percent` | `90` | High-water mark for each watched filesystem. |
| `workflow.resource_throttle.disk_resume_percent` | `85` | Resume below this filesystem percentage. |

**Fixed internals.** A local drain still starts up to 2 `no-diff-expected` auto-tasks (reviews, curation) while CPU is the only held resource; memory, disk and I/O pressure hold them too. On Linux, disk I/O stall (`full avg60` in `/proc/pressure/io`, the share of the last minute in which every runnable task was stalled on I/O) throttles at 20% and resumes below 10%; hosts without pressure stall information (macOS, kernels without PSI) are not gated and report no I/O reading. These were `cpu_light_leaves`, `io_full_high_percent` and `io_full_resume_percent` until [ORB-22808]; the keys now load with a deprecation warning and are ignored until 2026-12-08.

**Why disk I/O uses `full`, not `some`.** The CPU, memory and filesystem-space marks cannot see disk bandwidth: a host whose storage saturates before its CPU keeps admitting deliveries into I/O contention. `some` counts time when at least one task waits on I/O, which a healthy build host does constantly. `full` counts time when no runnable task could make progress, so it rises only when the disk, not one slow process, is the bottleneck. `avg60` smooths bursts such as a single link step; the ten-second sustain window below still applies. A 47% `full avg60` on a single NVMe running a dozen validating deliveries is the saturation this guards; the fixed 20% / 10% marks hold new admissions well before that and release once the backlog drains. On storage that sustains more concurrent builds, lower the drain ceiling instead. The mark guards admission only: running deliveries are never cancelled or paused, and a held reading reads `io full` in readiness and the drain's last pass.

Percentages are integers `1..=100`; each resume mark must be below its high mark. A workspace override applies only to that workspace's admission. Each resource's hysteresis history is shared by workspaces with identical high and resume marks; CPU history also requires the same source. Different marks or CPU sources keep independent sustain windows and holds against the same host readings. The host dashboard uses the serving machine's global values, read when its monitor first opens, so restart it after changing them.

The global `memory_high_percent` must stay above auto-concurrency's fixed 75%
memory limit, since both memory thresholds count non-reclaimable memory. Its CPU threshold measures forecast utilization. On Linux this
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

Every watched path takes part in admission, but `GET /api/host/resources` reports one `disk` object (highest known percentage, severity and path; `null` when none is known), and a disk hold names only the highest held path. That endpoint covers every active registered local checkout whatever `?workspace=` says (or with none), never remote hosts, and returns severity, sample time and age, thresholds, verdict and reason. It also carries `provider_limits`, the same live readings as readiness's `provider_limits` (below), read through the default workspace's effective config for thresholds and crew coverage, for the dashboard's top bar, plus additive `provider_pressures` and `crew_pressures` arrays described below; these fields are `null` when no workspace can be opened or the store cannot be read.

**Dashboard.** The rail foot's host block shows the selected host's `load` (CPU as a multiple of online cores, or `cpu` as a busy percentage when `cpu_source = "utilization"`), `mem` and `disk` readings, and an `io` row on hosts that report disk I/O pressure, with severity and held resources ([UI design §6](design/user-interface/2_design.md#6-top-level-navigation)); they poll every five seconds and mark aging data unknown even while a request is pending. The top bar shows one warning chip only while a reading is elevated or worse. **Settings › Machine** shows the enable switch and each resource's high and resume marks, with source, live reading, severity and verdict: six marks for CPU, memory and disk, and, on hosts that report disk I/O pressure (Linux only), an `io` row with its fixed 20% / 10% marks, which cannot be edited. Its edits share the topic pages' `PUT /api/config/keys/{key}` validation but write the **global** file by default, because this host's readings and admission read global settings; a workspace override is marked and wins for that workspace's runtimes.

### `[workflow.task_pilot_freshness]` — when a task is piloted again

```toml
[workflow.task_pilot_freshness]
material_fields = ["title", "description", "criteria", "plan", "selectors"]
source_sensitivity = "ignore"
```

| Key | Default | What it does |
|---|---|---|
| `workflow.task_pilot_freshness.material_fields` | `["title", "description", "criteria", "plan", "selectors"]` | **Advanced.** Task inputs checked for drift while a pilot is prepared and applied. For an applied assessment, a description or criteria edit queues a fresh pilot through the task-pilot routine when no live execution claim or unresolved failed pilot holds the task, while edits to the other fields never do; an `--approve-proposed` drain still pilots a stale explicit/routine assessment again automatically when its certified description/criteria or other material (selectors, complexity, crew/provenance, relations, configured material fields) drifted and no live execution claim holds it, while scheduled discovery still pilots one again automatically only when its certified description/criteria drifted and no live execution claim holds it. At least one of `title`, `description`, `criteria`, `plan`, `selectors` (`context_files`), `tags`, `crew` (the stored crew and its resolved model/provider), `tools`, `type`, `complexity`, `relations`, `dependencies` (each one's status and meaning) and `instructions` (`AGENTS.md`/`CLAUDE.md` at the pinned revision). |
| `workflow.task_pilot_freshness.source_sensitivity` | `ignore` | **Advanced.** Whether a move of the observed branch head makes an assessment stale. `ignore`: never (the pinned revision is still recorded). `context_files`: when the head changed a path a selector names (`file:` and `dir:` by prefix, `symbol:` through its file). `any`: every move. |

**Unprepared backlog work.** A backlog task with no `context_files` and no `no-diff-expected` tag holds no file lock. A local drain or discovery ship with more than one slot (`max_active_leaf_runs` > 1) therefore holds it: while an enabled `preparation_eligible` routine owned by this host (`trigger.state.owner_machine`) would still prepare it, it waits as `awaiting_footprint` for at most that trigger's `max_wait_minutes + deadline_minutes` from the task's last change. When no such pilot exists, the pilot assessed it and left it empty, or the wait elapsed, it starts only when no other leaf is in flight (`awaiting_exclusive_slot`) and holds the whole tree while it runs. A single-slot drain, an explicit `orbit run ship <id>` and owner pull admission admit it as before. `orbit run readiness` reports both reasons with the fix.

Pilot preparation reserves its selection atomically inside the task commit boundary. If acquiring that boundary times out before the reservation operation starts, preparation excludes the waiting tasks as `pilot_reservation_lock_busy`, records the readable lock diagnostic in `reservation_deferred`, and returns no pilot partitions. The run launches no pilot agent; apply supersedes that preparation attempt so state-triggered members can be observed afresh, including CI-sweep children. Existing live pilot reservations remain in force. Reservation I/O errors and errors after the operation starts still fail; preparation never retries an operation that may have written state.

A routine's `eligibility` block decides which never-assessed tasks receive their first automatic pilot. Description/criteria edits renew automatic preparation eligibility for proposed and backlog tasks. Once a task has an applied assessment, a description or criteria edit queues a fresh pilot through the routine when no live execution claim or unresolved failed pilot holds the task; other material or freshness-policy edits do not. An `--approve-proposed` drain does queue a fresh pilot automatically when a task's certified description/criteria or other material (selectors, complexity, crew/provenance, relations, or a configured material field) changed after its applied assessment and no live execution claim holds it, and scheduled discovery does so only when its certified description/criteria changed and no live execution claim holds it, the same way they pilot a new proposal; a failed repilot holds the task until its meaning changes again or an operator acts. An operator can still request one explicitly with `orbit task repilot <id>`, `orbit.task.repilot`, task update `repilot: true`, or a decision option action `re_pilot`. Description/criteria edits hold dispatch as `pilot_assessment_stale` until a new assessment completes, unless an operator skips pilot admission with `no-pilot`; adding the tag after a receipt or while a pilot is pending requires operator authority. Other readiness and delivery holds still apply. A task-pilot routine's `trigger.state.freshness` overrides either key (routine, then this table, then defaults); the resolved value still guards prepare/apply drift. See [automation triggers](design/automation-triggers/5_operations.md).

The dashboard's Auto-drain card shows approval and hold counts with a short
breakdown by reason. The pilot-warning count links to proposed tasks filtered
by their holding findings. Task rows (including the grouped status cards) carry
an amber **⚠ pilot warnings · N** chip, and the detail's **Pilot warnings**
callout lists the full findings by kind. These come from the task's latest
durable pilot verification, so they remain visible without a live drain.
Advisory findings do not count. Approval, leaving proposed, or a clean pilot
verification clears them; an ordinary comment or edit alone does not.
Task list/detail JSON and MCP expose the same `pilot_warnings` array of
`{kind, text}` entries, and CLI readiness uses that source for warning holds.
The dashboard drain payload omits per-finding text.

Task rows and details also show a blue **triaging** pill while a triage or
recovery run is bound to the task. It links to that run in the task's workspace;
its tooltip names the recorded crew, or says the crew assignment is pending.
The dashboard computes this from active escalation preparation and recovery
checkpoints, including inline final recovery and the blocked-task backstop.
The dashboard task payload exposes `active_triage`, an array of `{run_id, crew}`
entries, only while such a binding is active. Finishing the run clears it;
an inline recovery decision also clears it while delivery continues. The owner
view uses the same projection in local and federated lists. If an active run's
pipeline state cannot be read, the service logs a warning and shows no triage
for any task until it can be read; the task list, detail and writes still succeed.

The pilot checks concrete contracts required by a task's description and
criteria against pinned source: tools and their output fields, schemas, port
methods, config sections and authority requirements. It reads the workspace's
repository guides for ownership rules and the recorded dependencies' scopes.
If a required contract is absent, no dependency supplies it, and the task is
not permitted and scoped to add it, a holding `utility_warnings` finding names
the contract, source or tool-schema evidence, dependency coverage and a
suggested prerequisite or criterion rewrite. It follows the existing warning
hold and triage path. A dependency explicitly adding the contract gates
execution through its existing edge; it needs no additional warning hold.

Full task records also expose `resolvable_pilot_admission_hold`, derived from
the current admission finding and trusted resolutions independently of
`pilot_warnings`. A held backlog task's detail offers **Clear hold** and
**Approve anyway**, each requiring evidence through the operator-only typed
route. Resolved holds remove these actions. When an open pilot-admission
decision exists, its **Decision needed** answers serve as the single action
set instead. Promotion warnings keep their proposed-only semantics.

An `--approve-proposed` drain checks freshness under the approval lock against the exact material certified by the pilot's atomic write. The pilot's OS routing tag, selectors, complexity and automatic crew redraw therefore permit approval; later tag, crew, assignment provenance, relation or configured material edits withhold it. The `no-auto-approve` opt-out always wins. Replayed writes use their recorded audit rather than the task's current values; a pending approval from an older audit without that record needs a new assessment. Approval preserves routing and native-host admission holds. A fresh assessment from an explicit or routine pilot is approved without another pilot. A stale one whose certified description/criteria or other material (selectors, complexity, crew/provenance, relations, or a configured material field) drifted is selected again for a fresh drain pilot like a new proposal when no live execution claim holds it; a failed repilot, a holding warning, a live claim, or a running pilot keeps it proposed until the task changes or an operator acts.

### `[workflow.mid_run_steering]` — operator comments after dispatch

```toml
[workflow.mid_run_steering]
enabled = false
max_rounds = 2
```

| Key | Default | What it does |
|---|---|---|
| `workflow.mid_run_steering.enabled` | `false` | Record, in the run's durable state, the newest task comment each agent prompt that embeds task context included: the run's *steering watermark*. Deliver operator comments written after it at the run's later prompts, and hold the shipped delivery pipelines before `commit` while one is undelivered. A new global config seeded by `orbit init` sets it to `true`; when the key is absent it resolves to `false`. |
| `workflow.mid_run_steering.max_rounds` | `2` | **Advanced.** Steering rounds a run may consume at the promotion gate before it holds as `steering_unconsumed` (`1`–`10`). |

The watermark is stored per task under `steering_watermarks` in the run's pipeline state, for the run as a whole and for each step. It names a comment by its position in the task's append-only comment log plus the comment's recorded timestamp, never by a clock reading, so a comment written in the same instant as the prompt still orders by position. It only advances. It belongs to the run: a resumed run starts without one, and two runs of a task never share it. Agent output cannot write it.

The run's first task-context prompt for a task records the watermark. A persisted run with no watermark first delivers outstanding guidance from its inherited or post-dispatch baseline, including after mid-flight enablement; an admitted steering round also supplies the candidate source the gate inherited. Comments omitted by the bounded task context therefore stay protected. Each later prompt the run builds for that task (a recovery attempt, a repair round, final recovery, a re-dispatch) is a *safe point*: when operator comments written after the watermark are waiting, the prompt's task context leads with a `steering` block naming the run and its snapshot and carrying those comments oldest first. Comments from Orbit's own automation and agents are not steering, and task history entries never are. The block holds at most 32 KiB; past that it keeps its head and tail around a marker that names the full size and `orbit task show <id>`. Building that prompt records a `steering_delivered` event in the task's history, naming the run, the step, the comments carried in full and those cut down, and advances the watermark only past what the block carried in full, so a comment is delivered once per run. A safe point with nothing waiting builds the prompt as before and records no event. A task that no longer accepts implementer writes gets no block. A claimed follower leaf reads the task and its comments from its owner, and the owner keeps the leaf's watermark against its claim and records its `steering_delivered` events. With `enabled = false`, prompts, task records and run state are exactly what they are without the feature; turning it off later leaves recorded watermarks in place, unused. `orbit doctor`'s `mid-run-steering` row shows the resolved values.

With it on, `task_pr_pipeline` and `task_local_pipeline` gate `commit` ([design](design/mid-run-steering/2_design.md#3-promotion-gate)). Before committing, the run re-reads the task, and while an operator comment its prompts never carried in full remains, it does not commit. Each refusal spends one steering round, recorded under `steering_rounds` in the run's state: the implement step runs again, even for a carried candidate, with `input.steering_round` set, and its prompt delivers the comments onto the candidate still uncommitted in the worktree. A run that carries an earlier run's candidate judges from the watermark that earlier run's prompts left, so a comment written after that prompt and before the new run is outstanding; a carried candidate whose source cannot be traced fails the run before `commit` (`[steering_gate_unreadable]`). When the run has spent `max_rounds`, or its last round delivered none of the comments still outstanding (one too long for the block, for example), the task is blocked under a `steering_unconsumed` history event naming each outstanding comment, and the run fails without committing. Read the comments with `orbit task show <id>`, then requeue the task, answer them differently, or accept the candidate as it is through the normal review record. A gate that cannot read the task or the run fails the run before `commit` (`[steering_gate_unreadable]`), and a task that is `done` or `archived` stops it there, naming the steering it never received. A resumed run starts with its full budget of rounds. Both claimed pipelines gate `commit` too, reading the comment log and the claim's watermark from the owner; an owner the leaf cannot read fails the run before `commit` (`[steering_gate_unreadable]`), and a replica never certifies delivery. A claim's watermark outlives its settlement, so the task's next claim or owner run never delivers the same comments again. While an execution claim protects a task, the owner allows human operator comment-only writes; other unscoped writes stay refused. The live leaf reads those comments and records delivery through its owner.

Steering settings refresh at prompt and gate checkpoints. Enabling with runs in flight leaves missing watermarks absent: their gates treat post-dispatch operator comments as undelivered (or use an inherited candidate/claim baseline when present). Disabling stops recording and gating at the next checkpoint; existing watermarks and steering-round state stay inert, not deleted. Invalid config retains the last successfully resolved settings. Prefer enabling between drains; enabling does not change admission authority. A new global config seeded by `orbit init` sets `workflow.mid_run_steering.enabled = true`; when the key is absent the built-in default `false` applies. See [migration](design/mid-run-steering/specs/steering-delivery.md#migration).

<a id="task-riskiness-scores"></a>

### Task riskiness scores

The riskiness score is fitted on task size alone over the done tasks of the trailing 30 days: the declared path count before a diff exists, and the lines changed once a review candidate or landed diff resolves. A fit needs at least 5 done tasks on its basis. Both numbers are fixed. The Tasks row shows the score as a percentage, prefixed with `~` when some selectors name no path; when no estimate is available, the row shows no risk part. Task detail shows the score with its basis, size and how many recent done tasks of that size or larger regressed, including the reason for no estimate: **no files** (the task has no context files and no diff), **no paths** (its selectors identify no file or directory), **thin history** (too few done tasks in the window to fit), or **unavailable** (the estimate could not be computed). `@auto` effort and analytics read the same score.

Regression-risk prompt marks were removed [ORB-22806]: implement and review prompts no longer carry a `task.regression_risk` section, no task records a new `regression_risk_marked` history event (earlier ones still load), and Analytics › Codebase has no with-marks comparison. `workflow.regression_risk.enabled`, `window_days`, `min_support`, `threshold_percent` and `max_files` are deprecated and ignored ([validation and errors](#validation-and-errors) lists them).

### Retired explorer configuration

`workflow.explore.enabled`, `workflow.explore.max_calls_per_run`,
`workflow.explore.max_concurrent_per_run`, `workflow.explore.timeout_seconds`,
`workflow.explore.max_answer_bytes`, and `workflow.purpose_crews.explorer`
are deprecated and ignored. Existing files still load with a warning; remove
these keys with `orbit doctor --fix-retired-config-keys`, which backs up each
affected file first, or delete the lines by hand from the `config.toml` that
sets them. Explorer delegation and its MCP tool have been removed. Stored runs
and experiment readouts remain readable.

### `[workflow.agent_corpus]` — activity-scoped workspace knowledge

```toml
[workflow.agent_corpus]
agent_implement = "docs/agents/corpus/INDEX.md"
final_recovery = "docs/agents/corpus/INDEX.md"
task_pilot = "docs/agents/corpus/INDEX.md"
triage_escalation = "docs/agents/corpus/INDEX.md"
```

Unset by default. Map exact activity names to the same repository-relative
corpus index (at most 32 activities; absolute and traversal paths are refused).
The host reads generated rows at the run's base revision, selects only matching
`applies_to` rows, and omits resolved issues. It injects at most 8 KiB, with a
note when rows are omitted, and records `activity.agent_corpus_applied` with
paths, revision, digest and truncation in the run audit trail. A claimed run
uses the owner's captured rows. Unmapped activities receive nothing.

Human decision answers and typed engine recovery matches create deterministic
append proposals in task history. Batch materialization creates ordinary
reviewed corpus changes; it never commits or merges automatically. Generate
and check the index with `scripts/agent-corpus.py` and `--check`, respectively.
See [workspace agent corpus](design/workspace-agent-corpus/1_scope.md) for document
schema, append materialization and the orbit-office Library boundary.

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

Orbit ships defaults for three tags. `no-diff-expected` gives implementers the read-only handoff rule (no workspace gates, no `no-diff.json`). `skip-validation` gives implementers the [light delivery](#light-delivery-skip-review-and-skip-validation) rule: run no local validation while the change stays within `workflow.light_delivery_paths`. `ui` routes implementers to a host with a verified browser kit and selects the optional PR screenshots section; an empty section never blocks opening. Visual task artifact evidence follows the workspace guide and the actual diff, not the tag. A mapped file replaces the default for that tag and role.

**Where the text comes from.** Instruction text comes only from Orbit's shipped defaults or from the mapped file as committed at the delivery's base revision: the `base_sha` the run's `worktree` step recorded, or else the checkout's `HEAD`. Orbit reads the file from that commit, never from the worktree, so neither a delivery's commits nor its uncommitted edits change the instructions it runs under. A task's description, comments and tags never supply text; a tag only selects.

**What the activity receives.** After the activity's built-in instruction (and any steering or inspection text), the prompt gains one `Tag instructions` section. It states that the text adds to the instructions above and cannot override them, then gives each tag's text under a ``Tag `<tag>` (<source>)`` label. Tags apply in sorted order, once each. An untagged task, or a tag nothing maps, adds nothing.

**Refusals.** A mapped file is refused, and the activity fails without starting its provider, when it is missing at the base revision or not UTF-8 or empty (`tag_instruction_unreadable`), or larger than 8 KiB (`tag_instruction_too_large`). It also fails when one prompt's tag text would exceed 16 KiB (`tag_instructions_too_large`). Each error names the tag, the role and the file. A refused file affects only tasks carrying its tag.

**Claimed leaves.** When an owner admits a pull claim, it captures every instruction it would apply, with its files read from the claim's authoritative base, in the claim's receipt. PR delivery uses the owner's last-fetched `origin/<base>` ref, even when a local branch of the same name exists; local delivery uses the local base ref. Both accept a bare branch name or an `origin/` prefix. Capture does not fetch, and an unavailable authoritative ref is captured as a refusal rather than falling back to another branch or `HEAD`. The leaf applies that capture rather than its replica's mapping or files, and a refusal the owner captured refuses the leaf's activity. A claim from an older owner that captured nothing falls back to the replica's own mapping, with a warning.

**Seeing what applied.** Each dispatch that received tag instructions records an `activity.tag_instructions_applied` audit event naming its step, activity, task, and each instruction's tag, role, source (`default`, or `workspace` with path and revision), SHA-256 and size, but not the text. `orbit run show` prints a `Tag instructions:` header line per dispatch, and its JSON (and the `orbit.workflow.run.show` tool) carries them under `tag_instructions` (`state`: `observed`, `none` or `unavailable`).

### `[workflow.validation_env]` — the toolchain required validation runs with

```toml
[workflow.validation_env]
path = []
```

| Key | Default | What it does |
|---|---|---|
| `workflow.validation_env.path` | `[]` | PATH entries to put ahead of the resolved PATH. A leading `~/` expands to `HOME`. Agent sessions get them too ([below](#agent-sessions-share-the-configured-path)). |
| `workflow.validation_env.build_output_env` | `[]` | Output environment variable names to bind to managed host output for required validation and host evidence; see [build output](#workspace-build-output). |

Required validation commands and `local_shell` steps run in this environment over the [allowlisted agent environment](#executionenv--the-agent-subprocess-environment), so a drain started from a minimal PATH (a service manager, `env -i`, SSH) still finds the toolchain.

**Probe.** The shell is the account's user-database shell, else `$SHELL`, else `/bin/sh`. Orbit always runs `<shell> -i -l -c …` (login profiles plus interactive rc files: `~/.zshrc`, or `~/.bashrc` when the bash login profile sources it), falling back to `<shell> -l -c …` on startup failure, nonzero exit, timeout or a missing environment marker. Stdin is null; output before the marker and stderr noise on success (bash's job-control warning) are ignored. Each attempt is limited to 10 seconds; the whole outcome, fallback included, is cached two minutes. Only `PATH` and the toolchain allowlist cross: `CARGO_HOME`, `RUSTUP_HOME`, `GOPATH`, `GOROOT`, `GOBIN`, `JAVA_HOME`, `PYENV_ROOT`, `NVM_DIR`, `VOLTA_HOME`, `PNPM_HOME`, `BUN_INSTALL`, `HOMEBREW_*`.

**Recorded.** Each validation log and step output records `validation_env`: the PATH; its `source` (`config` when `path` has entries, else `login_shell` when the probe returned a PATH, else `launcher_fallback`); the shell and any probe error; `probe_mode` (`interactive_login` or `login`, else null); and `fallback_reason` (why interactive startup failed when login-only succeeded, else null). When both probes fail, `login_shell_error` holds both.

**Diagnostics.** `orbit doctor`'s `validation-env` row shows the probe mode, fallback, PATH and the `python3`, `git` and `make` it finds, and warns (advisory) on a fallback or an earlier PATH entry shadowing a different executable (`/usr/bin/python3` before `/opt/homebrew/bin/python3`); duplicates and symlink aliases count once ([health checks](runbooks/health-checks.md#run-orbit-doctor)). While required commands are configured, `orbit run auto`, `orbit run ship`, and MCP `orbit.workflow.auto` (`status`, `start`) and `orbit.workflow.ship` warn when the shell cannot be probed.

Readiness snapshots (CLI, dashboard and MCP drain status) reuse the advisory `validation_env_warning` across processes for two minutes in the local checkout’s `.orbit/state/validation-env-warning.json`. The key includes the launcher environment, shell, validation policy and Orbit version; environment or policy changes invalidate it immediately. Profile edits become visible as the process probe cache and diagnostic cache expire. Missing, invalid or unwritable cache files fall back to normal resolution. This cache holds only the warning, never the environment used to run validation or submit a drain.

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

A host validation target is keyed by its checkout and is built once per candidate, so Cargo's incremental cache there is write-only. When a binding is configured, required validation, host replay, detached evidence, baseline reruns and recovery set `CARGO_INCREMENTAL=0` on the child, replacing any inherited value. The variable is applied where Orbit spawns the command, not by the repository Makefile. There is no opt-out: `workflow.validation_env.cargo_incremental` was removed [ORB-22808] and loads with a deprecation warning, ignored, until 2026-12-08. With no binding, Orbit does not set the variable. An incremental directory already in a target is left in place; later builds do not add to it. Baseline result identity is unchanged: the setting changes compiler writes, not command results.

#### Agent sessions share the configured PATH

A reviewer that resolves a different `python3` or `make` than host validation reports checks red that the host then refutes. So every agent session — the implementer's and reviewer's provider CLI, and the commands it runs through `proc.spawn` — starts with `workflow.validation_env.path` ahead of its [allowlisted](#executionenv--the-agent-subprocess-environment) PATH. The entries are prepended, since the rest of PATH still has to find the provider CLI. Every other entry of the resolved required-validation PATH, including what the login-shell probe found, is appended when the session's PATH lacks it, so a reviewer finds each tool required validation finds without building a PATH wrapper by trial. Appending keeps the session's own order deciding which copy runs; toolchain locators such as `CARGO_HOME` still come only from the allowlist. Implementer and reviewer sessions share this one environment. Settlement does not rerun a disputed check in the reviewer's environment: host validation stays the reference.

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
- An implementer whose affected-test gate passed with only deferral notices of sandboxes its lane refuses hands that gate off instead of failing, by naming the validation summary the gate wrote; Orbit derives the `deferred_sandbox_validation` record from it and runs each notice kind's probe in the implementer's lane itself [ORB-15516]: `DEFERRED: bubblewrap unavailable:` notices on Linux [ORB-15287], `SKIP: sandbox-exec cannot apply a profile:` notices on macOS, as a Mac claimed implementer's nested Seatbelt prints [ORB-15409], and `DEFERRED: ps denied:` notices from a claimed implementer on macOS, whose sandbox refuses to exec `ps` [ORB-15587]. One record may defer several kinds, such as a Mac claimed leaf's Seatbelt and ps-denied notices together, when every kind's probe refuses [ORB-15692]. The gate must be a required command or a `review.baseline_commands` entry. A required `orbit plugin test` is such a gate inside a macOS worker: given `ORBIT_VALIDATION_SUMMARY`, `ORBIT_VALIDATION_GATE` and `ORBIT_VALIDATION_BASE`, it defers each case with the Seatbelt notice instead of starting the backend, and without them it fails `[validation_route_unavailable]`, which recovery treats as a validation-environment failure ([policy-sandbox design](design/policy-sandbox/2_design.md#the-plugin-backend-boundary)) [ORB-19224]. Validation runs it after the required commands, outside the agent sandbox (a claim's `claim_validate` on its claim host, before publication), on this run and on a resumed candidate: a pass that still prints any of these notices holds the candidate as a `validation_environment` failure, and a run that reports no executed tests is refused ([development guide](DEVELOPMENT.md#test-process-environment)).
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

#### Delivery refusals are held

A fail-closed delivery decision ends the run `held` with a typed diagnostic code:

| Code | Decision |
| --- | --- |
| `delivery_head_changed` | The PR reports a head different from the one this run published. |
| `delivery_evidence_stale` | Pinned delivery identity is missing or cannot be confirmed, or merge evidence is contradictory. |
| `delivery_repair_dispatched` | A repair was already dispatched for the blocked head. |
| `delivery_pr_draft` | The PR is still a draft. |
| `delivery_pr_closed` | The PR closed without merging. |
| `delivery_task_not_promotable` | The task's current status refuses promotion. |
| `delivery_landing_conflict` | Owner landing requires a conflict repair with fresh validation and a new handoff. |
| `delivery_requirements_unsatisfied` | Required reviews or checks are unsatisfied, without a failed check. |
| `delivery_base_checks_pending` | Failed checks are inherited from, or suspected to affect, the base; wait for the base's own result before candidate recovery. |
| `delivery_completion_budget_expired` | The completion budget (deadline) expired before completion could be confirmed; the last observed pull request state was unmerged and the task stays in review. The pull request may have merged since, through auto-merge (a hold does not cancel it) or another actor, so check the forge. |
| `delivery_check_evidence_inconclusive` | An auto-fix stop whose check evidence could not be read conclusively; no repair agent runs. When completion mutation provenance exists in this invocation or task history, a transient required-checks read failure is held as `delivery_check_evidence_inconclusive`; the next auto-fix pass rereads PR status and reconciles any earlier mutation. The required-checks read itself retries an empty, truncated or `unexpected end of JSON input` response a bounded number of times (never past the completion deadline) before holding. A failed job's log read that the forge throttled for the authenticated account first waits for the forge's reset (or a bounded backoff when none is reported), at most three times per job and never past the completion deadline; each wait is recorded in the task history as `pr_auto_fix_read_deferred`, and a resumed completion honors it. |
| `delivery_forge_observation_unavailable` | Completion's read-only forge reads (PR status, required checks, merge policy, prerequisite evidence) kept failing with a transport error before any completion mutation: after the one authenticated completion retry, when the run has no final recovery, past the completion budget, or after repair provenance that rules the retry out. Nothing changed on the forge and the task stays in review; `orbit job resume` reads the forge again and continues completion from its checkpoints. The owner's landing also holds this way when GitHub rate-limits its failed-check job or log reads past its bounded waits or its deadline; the reason names the reset ([landing a handoff](runbooks/distributed-drain.md#let-the-owner-land-follower-deliveries)). |
| `delivery_origin_unavailable` | `worktree_setup` could not reach origin during pre-admission base fetch while every task being admitted was still in backlog, including a fetch-lock wait whose bounded origin probe confirms a transport outage. The run is held without changing those tasks; a later admission retries after origin recovers. If any task is already in progress, the outage follows ordinary run failure handling (or claim settlement for a claimed leaf) instead of a hold with no generic resume path. Authentication, permission and missing-ref refusals remain failures. |
| `delivery_check_repair_activity_absent` | An in-budget `--auto-fix` check repair was admitted, and the job has no `step_failure_recovery` to run it. The run holds, the repair stays uncommitted, and the pull request stays open. Give the job that recovery activity and start a new delivery attempt. Spending the repair budget still fails the run. |

These codes come from typed refusals, never from matching diagnostic wording.
The refusing action keeps its existing task transition and landing evidence;
run finalization does not change the task's status. Retry, step recovery, final
recovery and the failure handoff do not run for these decisions. Gate and auto
parents pass the held child, run history lists it as `held`, and reliability
counts it under held, outside the failure rate. For `delivery_origin_unavailable`,
backlog tasks remain there and a later normal admission retries worktree setup.
An already-in-progress task follows ordinary run failure handling, or claim
settlement for a claimed leaf; the held run itself has no generic resume path. Routine fires retain their
existing `succeeded` status for a held underlying run.

Missing pinned identity fields, including the PR head, and contradictory merge
evidence produce typed `delivery_evidence_stale` holds. A permanent failure to
read required-check evidence in auto-fix, including a credential or permission
refusal, holds as `delivery_check_evidence_inconclusive` before or after a
completion mutation; no repair agent runs and no merge is admitted on that
evidence. Permanent forge-read failures at other boundaries, failed candidate
checks, and unlisted refusals remain failures. Transport failures after a merge,
auto-merge or rebase request (in that run or recorded in the task's history)
also remain failures, except that a narrowly classified synchronous HTTP
500/502/503/504 merge server error permits one read-only reconciliation within
the original deadline. Only fresh `MERGED` evidence matching the published and
reviewed candidate, PR, repository, head branch and base branch permits
completion; a failed response grants no managed-merge attribution. An
inconclusive read retains the original failure and mutation provenance and
never sends another merge request. A transient auto-fix required-check read
holds as `delivery_check_evidence_inconclusive` when completion mutation
provenance is present in the invocation or task history. In particular,
`delivery_evidence_stale` text alone does not establish a hold. Completion's
ordinary conflict-recovery path also retains its existing failure and repair
semantics. Resolve the named refusal before starting a new delivery attempt;
apart from `delivery_forge_observation_unavailable`, these holds do not enable
generic `orbit job resume`.

#### A red base is held, not blocked

The integration branch has no merge gates, so a command may already fail on the base. A command still failing after its reruns is rerun on the base commit in a clean detached worktree under `<git-common-dir>/orbit-baseline/`, cached per base commit, command and selection behind a file lock (concurrent candidates on one host share one base run). `validation/<run>/<n>.json` gets a `baseline` record; `validation/<run>/<n>.baseline.json` holds the base log. Candidate logs record `failures` and `baseline_reproduces`; baseline records and logs retain their parsed `failures`. The baseline record's `decision` describes the base run's outcome separately from whether it reproduces the candidate.

| Base run | Outcome |
|---|---|
| Comparable completed failure with the same exit status, and every identified candidate failure is a test that fails there too | Known red: the candidate proceeds (below). |
| Comparable completed failure with the same exit status and every candidate failure identity confirmed on the base, including compiler or lint errors | The task is held (below). |
| Neither run names identities, but complete normalized diagnostics and exit status match, with no timeout or coverage refusal | The task is held, with a diagnostic fingerprint as its failure identity. |
| Fails with different or additional candidate identities, unmatched diagnostics, identities missing on only one side, or a timeout | Matching failures are unconfirmed; candidate repair remains enabled, with both logs preserved. |
| Passes | Candidate failure; the error says the base passed, so step recovery can repair it. |
| Inconclusive (missing tool or network) | Candidate failure; nothing cached. |
| Not comparable (below) | Candidate failure; the `baseline` record's `decision` is `not_comparable`. |

**Known red.** The candidate introduced none of the base's failures when the candidate run did not time out, printed no host coverage refusal (sandbox-unavailable diagnostics, `DEFERRED:`, `SKIP:`, `skipping:` or `skipping ` notices), names at least one failure, and every failure it names is a failed test that also fails in the base run. Both runs' failures and the candidate's host coverage notices are read from the complete output, not the bounded log, using the identities a hold records (below). A declared deferred-sandbox replay must report executed tests before known-red continuation, including when it exits nonzero; a missing or zero executed-test count refuses the replay. The step does not fail: it records the command under `known_red` in its output (decision `passed_with_known_red`), marks the command's log `failure_kind: "known_red"` with the failures, appends a `known_red_validation` history event naming command, base commit, candidate commit and failing tests to each delivered task, and goes on to the next command. `pr_open` lists that run's known-red commands, for the published head and base, under `## Known-red validation` in a body it opens; one found by a later revalidation, after the PR exists, is only on the task and the run. The red base keeps its repair task through other holds. A compiler or lint error with an identity confirmed on the base can be held as below. Diagnostic fingerprints never qualify as known-red tests. Timeouts, unmatched diagnostics, or any candidate identity absent from the base leave the candidate requiring repair. Host coverage refusals still follow the validation-environment rules. A resumed candidate is judged the same way.

**Hold.** The step fails with the `[baseline_red]` marker and error code `baseline_red`, and the run ends `held`, not `failed`; the gate and auto parents waiting on it pass it as held. No step or final recovery runs and no budget is spent; the handoff pushes nothing and opens no `[BLOCKED]` PR. The task moves to `backlog` under a `baseline_red_hold` history event naming base ref, base commit, command and what fails on the base (run finalization does this for a local pipeline), with the candidate kept in its worktree. The hold's `failures` record keeps up to 20 failure identities the base output names (each nextest `FAIL [..]` summary line as `test <binary-id> <test>`, a libtest `test <name> ... FAILED`, a compiler or lint error at its location) and counts the rest; when neither run names identities, equal diagnostic fingerprints establish reproduction instead. The fingerprint hashes the complete output after stripping ANSI codes, collapsing whitespace, replacing workspace/worktree roots and temporary paths, removing PATH lines and elapsed-time tokens, and sorting lines. Empty normalized output has no fingerprint. The hold records a readable SHA-256 fingerprint identity; legacy holds without identities retain up to 5 output lines instead. Older cached results without fingerprints remain readable but cannot establish identity-less reproduction from bounded logs. Admission withholds the task; `orbit run readiness --json` reports `reason: "baseline_red_hold"`, and its `detail`, like each hold verdict, names the failing tests. Operator steps: [stuck job runs](runbooks/stuck-job-runs.md#a-task-held-for-a-red-base).

**Repair task.** The owner's clock tick files one repair task per red base failure: the first standing hold for a command and failure set files a `bug` in the workspace's `backlog` (priority `high`, complexity `medium`, tag `baseline-red-repair`, external ref `baseline-red:<digest>`) naming the failures, the first red base commit and the held task. Every later hold with the same command and failures, on another task or a later base, is added to that task as a `related_to` relation with a comment. Once it is done, rejected or archived, a hold still naming the same failures files a new one. A hold that records no failures, such as one written before holds kept them, files nothing. A hold whose base already has a passing result for its command lifts on that tick without filing or linking a repair. New holds retain a SHA-256 digest of the complete trimmed, sorted, deduplicated failure identities before limiting their display to 20 entries and a 240-byte prefix plus an ellipsis for long entries. Repair identity uses that digest, so input order, duplicate names and display truncation do not change which complete set is tracked. Older holds without a digest retain their original display-based repair key and still find existing open repairs; a new digest-bearing hold can therefore file a separate repair from an older hold for the same failures. Fingerprint holds key repairs on command plus fingerprint, so changing host paths or output tails cannot file another open repair for the same diagnostics. Legacy output-only failures continue to use the bounded output tail.

**Release.** The held command must pass on a new base tip. When a hold records runnable failing test identities and a selection, refresh first reruns only those tests: a comparable failure keeps the hold without running the full command. If those tests pass, or their replay is inconclusive or not comparable (including a command that ignores the test filter or a renamed test that executes nothing), refresh runs the full recorded selection before lifting. A hold without recorded tests, with a non-test or shortened identity, or without a selection runs the full command directly. Narrowed and full runs retain separate cache entries per base tip, command and selection; cached full results take precedence, and cached narrowed failures also suppress another refresh on the same tip. A failed or inconclusive full run keeps the hold. `orbit clock tick` never runs it (it holds the sweep lock): it reads the cached base result for the new tip, or dispatches one `baseline_hold_refresh_pipeline` run per workspace at a time to run it and record a `baseline_red_hold_verdict` event. An inconclusive attempt suppresses refreshes for that tip and command for 15 minutes; a new tip is checked at once, and a conclusive cached result overrides the attempt. Readiness, the backlog snapshot and pull admission only read verdicts, so a hold with none stays held. The next run resumes the preserved candidate.

**Self-selecting commands** (`make ci-test-affected`) report their selection and executed-test count through `ORBIT_VALIDATION_SUMMARY`, and the base run reruns the candidate's selection from `ORBIT_VALIDATION_SELECTION`; a base run with another selection, or a pass with no counted test, is not comparable ([validation summary](DEVELOPMENT.md#validation-summary-and-base-reruns)). A hold records the selection and lifts only when that selection passes on a moved base with at least one test executed. A hold from before selections were recorded is judged by exit status, and a still-red base re-holds the next delivery with the selection. A command that writes no summary has no selection check, but reproduction still requires completed failures with the same exit status and either a nonempty candidate failure set covered by the base identities or equal diagnostic fingerprints when neither run names identities.

**Followers.** A claimed leaf validates before it pushes or opens a PR and judges a red base by the same rule against its own base run. A known-red command does not stop it: `claim_validate` reports it under `known_red` (decision `passed_with_known_red`), the leaf's `pr_open` lists it under `## Known-red validation`, and the command's claim validation log keeps the non-zero exit with a `known_red` report naming the base commit, the selection and the complete failing set. The owner does not take that report on the leaf's word. Before it accepts the handoff it judges each reported command it requires itself, on the handed-off base, which must be in the history of the base branch the owner captured when it admitted the claim (this membership rule applies to both same-OS and cross-OS settlements; a base off that branch is refused without running a validation command). For same-OS settlements, it uses a full result it has cached for that base, command and selection, else a replay of only the reported tests when the command reports a selection, else the whole command. When the follower was admitted on the same host OS as this owner, it accepts only when its own base run fails every reported test, the same way when the whole command ran, and then appends the `known_red_validation` history event to the task. A different host OS is a different validation environment: after checking that same base-branch membership, the owner does not run or consult its own base validation for that report, does not release the claim as `known_red_contradicted`, and accepts the follower's base evidence for the admitted OS, recording the same history event. The handoff is accepted, so a pull request the leaf opened stays this delivery rather than being left open while the task returns to the backlog. Owner settlement shares a ten-minute deadline across its base comparisons, below the fifteen-minute routed delivery budget, leaving room for observation, cleanup and the reply. Lock waits and base commands use the remaining time. An attempted base run that is cut or otherwise inconclusive is retained under the base, command and replay selection separately from conclusive base results: settlement retries reuse that refusal rather than starting another cold run. Partial failures from a cut never confirm known red, and ordinary candidate validation and reviewer base replays do not read these inconclusive claim records. A later conclusive base result may supersede the refusal. When the follower was admitted on this owner's OS, a report its own completed, comparable base run contradicts (the base passes the reported tests, fails without one of them, or fails the whole command with another exit status) can never be accepted, so the owner ends the claim instead of refusing it: it releases the task to the backlog as a `transient` failure whose reason, led by `known_red_contradicted`, names its verdict and counts against the task's release budget, and the follower closes its pending settlement obsolete. A report it cannot establish either way (the base run is inconclusive or not comparable, the report names another base, or the base is off the owner's base branch) refuses the settlement while the owner keeps the claim, and the leaf retries it; a non-zero log without a report is refused as before. A candidate failing a test its base lacks requires candidate repair; equal exit status alone never creates a `baseline_red_hold`. The candidate and base logs retain their complete parsed identity sets and the reproduction decision even when bounded output omits a failure. An owner running a release without this rule refuses a log carrying a report until it is upgraded.

**Candidate-only flakes.** A comparable passing base triggers one isolated candidate replay when the captured failure set contains at most three tests and the command reports a replayable selection. The affected-test gate runs only those tests serially, with retries disabled. A pass must report the exact selection and executed coverage without host refusals. Delivery then proceeds with `flaky_validation` history, captured original/base/replay evidence and a PR section naming the tests; each command/test signature is filed once as a workspace friction. Claimed handoffs carry the same pinned evidence, which the owner validates and records at acceptance. A repeated failure, an over-bound set, or missing comparable evidence keeps the normal failure handling. See [validation summaries](DEVELOPMENT.md#validation-summary-and-base-reruns).

**Reviewer claims.** A before-PR reviewer can attach a `baseline` claim to a further failing check. Settlement runs on the host, outside the reviewer's sandbox, so it reruns only `workflow.required_validation_commands` and `review.baseline_commands` entries (never the reviewer's command text) on the final candidate and the pinned base, using the lists captured at admission. On the base it reads a full result already cached for that exact base, or else, when every failure the candidate run printed is a test and the command reports a selection, replays only those tests (`failing_tests`, [validation summary](DEVELOPMENT.md#validation-summary-and-base-reruns)); other failures replay the whole selection. `review-baseline.json` records the path as `base_replay` (`cached_failure_set`, `failing_tests` or `full_selection`). Each claim's host runs get `review.minutes` of wall clock, the last quarter reserved for the base: the candidate rerun is stopped at three quarters (each run still at most 45 minutes), and a stopped run settles `incomplete` under `baseline_cut`, naming the cut and the failures it printed that the base shares. A confirmed claim with nothing else open holds the task as above, unless its candidate failures are known red by the rule above: then the review settles `accept` (`accept_with_fixes` over a reviewer repair), before the PR and before landing alike. The record stays `failed`; the certificate's `known_red` names the command, base, final candidate, run and tests, `review-baseline.json` records the check's `decision` as `known_red`, the step output carries `known_red`, and each task gets the same `known_red_validation` history event, which `pr_open` lists in the PR body. Outcomes for refuted or uncheckable claims: [review-gate design §4](design/review-gate/2_design.md#4-what-the-validation-records-establish-orb-11528-orb-11545).

#### Agent-declared blockers stop the run

An implementer that cannot proceed returns `blocker: {kind, evidence}` (a short token and the reason) from `implement_one` or a resolved `agent_implement` target. The step fails with the `[task_blocked_by_agent]` marker and error code `task_blocked_by_agent`; the run ends `held`, and gate and auto parents pass their success guards. The held diagnostic in `orbit run show` retains the blocker kind. No step retry, step recovery or final recovery runs. On the PR pipeline the handoff commits, pushes and opens nothing, blocks the task under `task_blocked_by_agent` with the kind, and leaves the worktree as the implementer left it, uncommitted files included. `orbit job resume` does not move the task back; an operator does once the blocker is gone. An implementation activity cannot set status `blocked` itself (operators and other activities can). A claimed leaf stops before its next step and final recovery. Blocked-task recovery skips a marked failure note, or for a claimed leaf, a marker in the execution summary.

A blocker whose kind names an Orbit upgrade refusal (`upgrade_pending`, `upgrade_admission_refused`, `orbit.generation_switch_pending` and similar spellings) is the host's state, not the task's. It ends any agent step, reviewers included, with error code `upgrade_pending` instead. The step is not retried or recovered, the candidate is kept, and the task returns to the backlog rather than `blocked`. See [in-flight agent steps during a binary swap](runbooks/upgrades.md#in-flight-agent-steps-during-a-binary-swap).

`step_failure_recovery` declares the same blocker with an `external_blocker` decision when a human must act on a cause outside the run; the post-recovery attempt and final recovery are skipped and the task is blocked as above (or returned to the backlog, for an upgrade refusal). A recovery that writes no decision gets its one post-recovery attempt only if Orbit sees the worktree, the run's base-ref tip or the validation environment change while it ran. Mechanism: [activity-job design](design/activity-job/2_design.md#recovery-authority-is-host-only).

---

## `[crews.<name>]` — which provider-model runs the task

A crew is one provider-model assignment. An activity uses the crew named in its rendered input, otherwise the run's resolved crew.

| Field | Required | Values |
|---|---|---|
| `provider` | Yes | `claude`, `codex`, `antigravity`, `gemini`, `grok`, `copilot`, `muse`. See [provider identity](#provider-identity-and-resolution). |
| `model` | Yes | Model ID passed to the provider CLI. |
| `enabled` | No | Default `true`. `false` keeps the crew defined and listed but refuses to run it; see [disabled crews](#disabled-crews). |
| `effort` | No | Reasoning effort (below). Omitted leaves the provider's default. |
| `description` | No | Summary. Trimmed; blank becomes absent. |
| `tags` | No | Discovery labels. Trimmed, blanks dropped, sorted and deduplicated. |
| `env` | No | Variables this crew's agents receive on top of `execution.env.pass`, as `NAME` or `NAME=SOURCE`. Global config only; see [per-crew credentials](#per-crew-credentials-env). |
| `vision` | No | Default `true`. `false` opts a text-only model out of image input on an image-capable lane; see [executor image input](#executor-image-input-image_input). |

```toml
[crews.sol]
model = "gpt-6.1-sol"
provider = "codex"
effort = "high"
description = "Systems implementation"
tags = ["implementation", "review"]

[crews.argon]
enabled = false
model = "gemini-4-argon-high"
provider = "antigravity"
```

**Built-in crews**, in init's family preference order. A config with no `[crews]` table uses the full set. Existing explicit model pins are kept as written. The legacy Gemini CLI lane has no built-in crew: define one under `[crews.<name>]` to use it.

| Family | Binary | Crews (model) | Default crew |
|---|---|---|---|
| `claude` | `claude` | `opus` (`opus`), `sonnet` (`sonnet`), `haiku` (`haiku`), `fable` (`fable`) | `opus` |
| `codex` | `codex` | `astra` (`gpt-6-astra`), `sol` (`gpt-6.1-sol`), `terra` (`gpt-6.1-terra`), `luna` (`gpt-6-luna`) | `astra` |
| `antigravity` | `agy` | `gemini-flash` (`gemini-3.8-flash-high`), `argon` (`gemini-4-argon-high`) | `gemini-flash` |
| `grok` | `grok` | `grok` (`grok-4.7`) | `grok` |
| `copilot` | `copilot` | `copilot` (`claude-sonnet-5`) | `copilot` |
| `muse` | `muse` | `muse-spark` (`muse-spark-1.3`), `muse-wonder` (`muse-wonder-1.0`), `muse-wonder-full` (`muse-wonder-full-1.0`) | `muse-spark` |

**Reasoning effort.** Choose capability with the model, then tune `effort` within it.

| Provider | Accepted `effort` | Rendered as |
|---|---|---|
| `claude`, `codex`, `muse` | `low`, `medium`, `high`, `xhigh`, `max` | `--effort` · `model_reasoning_effort` · `--reasoning-effort` |
| `antigravity` | `low`, `medium`, `high` | `--effort` |
| `grok` (`grok-4.7`, `grok-4.6`) | `low`, `medium`, `high`, `xhigh` | `--reasoning-effort` |
| `grok` (`grok-4.5`) | `low`, `medium`, `high` | `--reasoning-effort` |
| others | none | — |

An unsupported `effort` (`max` on Grok, `effort = "hard"`) is ignored for that crew, never remapped, with a warning naming the file, crew, value and accepted values. `orbit doctor` lists ignored properties; `orbit config set` refuses a value load would drop. A selected crew's provider, model and effort each override an activity's inline baseline.

**Crew references.** Wherever a crew is named — the complexity, type, purpose, review and final-recovery pools, `workflow.default_crew`, a task or auto-task `crew`, `--crew` and MCP `crew` inputs — it can carry an effort: `<crew>@<effort>`, followed in pools by the usual `:<weight>` (`hard_complexity_crews = ["opus@medium:100", "opus:100", "sol:100"]`). The reference runs that crew at that effort, ahead of the crew's own `effort` and the activity's inline baseline; without `@effort` the crew's own `effort` applies, as before. `opus@medium` and `opus` are distinct pool members with their own weights, and runs, scorecards and experiments record them under those names. They share the crew's provider account, holds and limits: a [provider failure hold](#provider-failure-holds) or pull-drain exclusion on either one covers the whole family. Unlike a crew table's `effort`, a reference effort the provider or model does not accept (`grok@max`, `luna@hard`) is refused at load and by `orbit config set`, naming the reference and the accepted values. Crew names may therefore contain neither `:` nor `@`.

**Effort chosen per draw (`<crew>@auto`).** A pool entry may leave the effort to the draw: `opus@auto`, or `opus@auto:100` in a weighted pool. Only the implementer pools (`workflow.<tier>_complexity_crews` and `workflow.type_complexity_crews`), `workflow.purpose_crews.task_pilot` and the two review pools (`workflow.purpose_crews.review`, `workflow.purpose_crews.pre_landing_review`) accept it, and only for a crew with bands in `workflow.auto_effort.bands`; any other pool, a crew without bands, `workflow.default_crew`, a task `crew` and `--crew` refuse it at load, by `orbit config set` and on run overrides. Each band sets an `effort` and at least one condition, and the first band whose every condition the draw's signal meets chooses the effort:

```toml
[workflow.auto_effort]
bands.opus = [
  { complexity = ["hard", "xhard"], effort = "xhigh" },
  { below_files = 4, effort = "medium" },
  { below_lines = 400, effort = "medium" },
]
```

| Condition | Meets it when |
|---|---|
| `complexity = [..]` | the signal's tier is listed (`low`, `medium`, `hard`, `xhard`) |
| `below_files = N` | the signal counts fewer than `N` files |
| `below_lines = N` | the signal counts fewer than `N` reviewable lines |

Each role resolves at admission from the best signal it has. An implementer uses the task's assessed complexity and its declared footprint: the number of distinct files its `context_files` name, the size the task-risk forecast uses before dispatch. It never reads the final diff, so `below_lines` never matches an implementer. A task pilot uses the filed complexity only, because the pilot is what produces a size. A reviewer uses the candidate's reviewable lines (as `workflow.non_reviewable_paths` counts them), its changed files and the reviewed tasks' highest settled complexity. With no signal, or no band that matches, the crew's own `effort` applies (`opus`, or `opus@high` when its table sets `effort = "high"`). Bands are tuned from the Analytics › Agents › Implementers size curves; Orbit ships none.

The run, its crew selection, the reviewer record, the Implementers, Pilots and Reviewers analytics tables and experiment readouts record the resolved `<crew>@<effort>`, never `@auto`. An implementer's `crew_selection` carries `auto_effort` (`requested`, `resolved`, `policy`, `basis` of `band`, `fallback` or `escalation`, the matching `band` index, `explored` and the `signal`); a frozen activity draw and an admitted reviewer carry the same record, and their `source` ends with `auto effort opus@auto -> opus@medium (band 2)`. A task keeps its `@auto` member while the effort it resolves to moves: the next admission re-resolves it against the task's current tier and footprint.

A fixed share of `0.1` (10%) dispatches of seeded draws at another effort the crew's bands or table name, so each side of a cutover keeps getting data. An explored draw records `explored: true` with the `policy` reference it replaced, and its source says `explored`. The roll is seeded by the task (implementers), the run and partition (pilots) or the run and candidate head (reviewers), so a retried selection resolves alike. A task's creation draw never explores.

A task-pilot run draws one crew from its pool as before. When that pool has `@auto` members, preparation also splits partitions so each resolves to a single effort for whichever `@auto` crew the run draws, and each partition freezes its own `<crew>@<effort>` under `workflow.purpose_crews.task_pilot#pilots:<partition>` in the run's crew draws. **Pilot escalation:** when a pilot that ran at an `@auto` effort raises the task above its filed tier to one whose band maps to a higher effort, or reports confidence `medium` or `low` (target: the highest effort the crew's bands name), apply holds that valid assessment back as `escalated` and the run's repair round re-pilots the task once at the higher effort (`workflow.purpose_crews.task_pilot#repair_pilots:<partition>`, basis `escalation`). The higher-effort assessment is the one applied; the repair round never escalates again. An effort at or below the one the pilot ran at never escalates.

| Key | Default | What it does |
|---|---|---|
| `workflow.auto_effort.bands` | `{}` | **Advanced.** Per crew (its `[crews.<name>]` name, never `<crew>@<effort>`), the ordered bands a `<crew>@auto` pool entry resolves through. Each band needs an `effort` the crew's provider and model accept and at least one of `complexity`, `below_files`, `below_lines`; bounds are above `0`. |

**Recorded effort.** Each provider launch records its resolved crew, model and
`reasoning_effort` before the process starts, after crew/inline precedence and
configuration admission. `null` means no effort option was rendered (provider
default); `"unknown"` identifies history without a dispatch record. Editing a
crew never changes these records, and audit history is not backfilled.
`orbit run show --json` and `orbit.workflow.run.show` expose `agent_executions`
on the run and its displayed steps, including retries and interrupted launches.
The dashboard shows the same provenance on run detail and step rows.

Scorecard rows keep their compatible crew/model ranking and include an
`effort_breakdown` of recorded invocation counts, linked tasks, duration, tokens
and provider-reported cost. The score itself still describes the whole row. The top-level scorecard
`effort_breakdown` covers completed workspace invocations in the selected
window (`effort_population` names that population).
Orchestrator authorship has no recorded execution effort and stays `unknown`;
it never borrows an implementer's effort. Experiment arms include the completed invocations
linked to their enrolled tasks; managed usage buckets break down by actual
crew, model and effort. Null/default and historical unknown remain separate.

**Editing.** Crew fields are `crews.<name>.<field>`:

```bash
orbit config set crews.sol.effort high
orbit config get crews.sol.effort
orbit config set crews.argon.enabled true
```

`config set` refuses invalid values, unsupported provider/model combinations and misspelled fields. It cannot create a crew: add a `[crews.<name>]` table with `model` and `provider` first. `orbit.workspace.list` with `include: ["crews"]` returns each checkout's effective normalized crews with `enabled` (schema version 3), or `crews_error` when its config cannot be read.

**Dashboard.** **Settings › Crews** lists provider, model, usage **Limit** ([provider usage limits](#provider-usage-limits)), effort, tags, layer and **Used by** (inline field labels at 900 px or less), and a crew's `env` entries under its name. **Used by** is resolved server-side from the default and review crews, the final-recovery and complexity pools, and enabled auto-tasks with an explicit crew whose plugin is active; if the auto-task listing fails, it omits them and logs a warning, and crew writes still work.

### System work draws from the complexity pools

Orbit's own work draws from the [complexity pools](#automatic-crew-pools-by-complexity) like a task does, unless its [purpose pool](#purpose-crew-pools) is configured. The table is the route each kind of work keeps while its purpose pool is unset.

| Work | Pool | Why |
|---|---|---|
| Shipped auto-task templates (code review, QA sweeps, doc duties, …) | the template's `complexity` | The minted task carries no crew, so it draws like any crew-less task. A `template.crew` setting or an explicit template `crew` still wins. For `delivery-code-review`, a set `workflow.purpose_crews.review` wins over both: the definition's own crew applies only while that key is unset, and `orbit auto-task show` and `update` say so when it names one. |
| Task pilot (`task_pilot_pipeline`) | `low` | Piloting reads tasks and writes a bounded proposal. |
| Step-failure recovery, PR conflict recovery | `medium` | Recovery diagnoses failures in work of any tier, so it is not low work; medium is also the tier an unassessed task is verified on. |
| [`orbit run auto --auto-fix`](#pr-auto-fix-repairs) repairs (failed required checks and merge conflicts on the drain's PRs) | `low` | The repair is bounded to one named blocker on a candidate that already passed its gates. |

A pilot, recovery or auto-fix activity names its purpose as `crew_config_key: workflow.purpose_crews.<purpose>` (an older asset may still name `workflow.<tier>_complexity_crews`, which draws that pool directly). The run draws once per pool on its first such dispatch: the pool's weights apply, disabled crews and those outside the run's `allowed_crews` are skipped, and crews at a [provider usage limit](#provider-usage-limits) are skipped while another member is left. Every later dispatch of that pool in the run, and a resume, reuses the draw, which the run's state records with the eligible members. An empty or wholly disabled pool falls back to `workflow.default_crew`, as an unassigned task does, and the recorded draw says so. The task pilot and escalation triage inspect pinned source with a CLI's own native file-read and command tools, so their draws keep only crews whose provider has a CLI runtime and whose executor declares [`inspection_args`](#executor-inspection-tools-inspection_args). A pool with no such member passes to the next tier up, then to `workflow.default_crew`; when that cannot inspect either, the activity fails with `inspection_crew_unavailable` before a provider launches. The recorded draw names the filter and the crews it skipped. Trust in an inspection still comes from its result: each result reports the `HEAD` and tree it observed in its checkout, and the apply step (or, for triage, `orbit.task.block_triage`) refuses a result whose proof is missing or differs from the pinned source.

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
| Red-base repair | `workflow.purpose_crews.baseline_red_repair` | tasks the red-base re-check files (`baseline-red-repair` tag) | the task's matching [type pool](#type-pools-by-task-type-and-complexity), when it has one, then its complexity pool, then `default_crew` |
| Sweep tasks | `workflow.purpose_crews.sweep_tasks` | tasks the CI failure sweep files (`ci-failure-sweep` tag), the Dependabot and code-scanning sweeps file, and merged-head repairs a review reconciliation files (its follow-up tag) | the same |
| Pilot-warning triage | `workflow.purpose_crews.pilot_warning_triage` | the triager of each held task-pilot assessment (`escalation_triage_pipeline`, subject `pilot_warning`) | `workflow.hard_complexity_crews` |

A triager inspects pinned source natively, so only members whose executor declares [`inspection_args`](#executor-inspection-tools-inspection_args) are drawn, by weight and seeded by task, and for a pilot warning the crew that wrote the held assessment is passed over while the pool offers another. A pool with no such member keeps the hard-tier route (the next tier up, then `workflow.default_crew`, as above). The recorded draw names its source as `purpose:pilot_warning_triage` (`pool:<tier>` or `default` on the hard-tier route) in each prepared task row (`crew_source`) and partition (`draws`).

Final recovery keeps its own rules (no fallback; `[]` turns it off), and automatic review keeps its own (required for a review layer before landing; it avoids the crew that implemented the reviewed work).

**Deprecated pool keys.** `workflow.final_recovery_crews` and `operation.review_crew` load into `final_recovery` and `review`, and the `ci_failure_sweep`, `dependabot` and `merged_head_repair` pools load into `sweep_tasks`, each with a deprecation warning. A file that also sets the replacement keeps the replacement. A file whose old sweep pools name different pools fails to load until you merge them into `sweep_tasks`, because every sweep-filed task now draws from one pool. Tasks and runs recorded under the old purposes keep resolving: a `crew_source` of `purpose:ci_failure_sweep`, `purpose:dependabot` or `purpose:merged_head_repair`, and a `crew-purpose:` tag naming one, read as `sweep_tasks`. The final-recovery activity's `crew_config_key` binding keeps the old `workflow.final_recovery_crews` name, so the draws earlier runs froze under it resume.

**Resolution order.** The first rule that applies wins:

1. **Explicit crew.** A `crew` on the task, the auto-task template or the activity input is used as it is and skips the purpose. An activity's explicit `crew` also beats its `crew_config_key`.
2. **Purpose pool.** The purpose's pool, drawn by weight once per run for an activity and at creation for a task. Disabled crews are skipped, and `--allow-crew` narrows the draw (a pool with no permitted member fails as a complexity pool does). For activity draws, [provider usage limits](#provider-usage-limits) and [failure holds](#provider-failure-holds) skip crews while another member is left. At task creation, limited members are skipped; if none remain, the task stays unassigned for admission to retry. The task pilot keeps only members that [inspect natively](#system-work-draws-from-the-complexity-pools).
3. **Earlier route.** A purpose whose pool is unset, `[]`, wholly disabled, or (for the pilot) has no inspection-capable member resolves to the right-hand column above. Only the tasks the red-base re-check and the sweeps file (`baseline_red_repair`, `sweep_tasks`) consult a matching [type pool](#type-pools-by-task-type-and-complexity) on that route, before their complexity pool. The activity purposes `task_pilot`, `failure_recovery` and `auto_fix` go straight to their complexity pool.

**A task's purpose** comes from its tags: a `crew-purpose:<purpose>` tag naming one of the `workflow.purpose_crews` purposes, else the tag the system filed it with. An auto-task template names a purpose for the tasks it mints by adding a `crew-purpose:<purpose>` tag. A task drawn from its purpose pool records `crew_source: purpose:<purpose>`; admission keeps that crew while it is still an enabled positive-weight member of the pool, and otherwise redraws as for a stale pool assignment. A [provider usage limit](#provider-usage-limits) or a [failure hold](#provider-failure-holds) that covers the assigned crew redraws from the pool's other enabled positive-weight members, keeping their weights and the run's allowlist, before the complexity pool. Admission writes the crew it runs back to the task (see [Admission write-back](#automatic-crew-pools-by-complexity)). A purpose task whose pool is unset, `[]` or wholly disabled falls back to its matching type pool, when one exists, then to its complexity pool. A pool whose members are all held or limited instead retries its other members, then the complexity pool, and never the type pool. A complexity change keeps a purpose-sourced crew, because the purpose does not depend on the tier.

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
- A crew name may not contain `:` or `@`, which the crew-reference grammar reserves ([repair](#repairing-a-config-that-already-names-a-crew-with-a-colon)).
- An Antigravity crew needs an `agy models` slug; a bare Gemini CLI ID such as `gemini-3.8-flash` fails with migration guidance.

**Retired crew shapes** fail load with migration guidance: `planner` / `implementer` / `reviewer` sub-tables (use flat `model` and `provider`); `backend` (`cli` is ignored, `http` and `auto` refused, likewise in `ORBIT_BACKEND` and `[runtime] backend`; remove it); `[agent.<role>]` tables (use `[crews.<name>]` plus `workflow.default_crew`).

### Human crews

`kind = "human"` declares a crew for operator work: tasks a person does themselves, such as approving a contract or renewing a domain, tracked beside agent work but never dispatched. `kind` defaults to `"agent"`; any other value fails load.

```toml
[crews.dk]
kind = "human"
description = "Daniel's own work"
```

- **Keys.** A human crew takes only `description` and `tags`. Setting `provider`, `model`, `effort`, `env` or any other agent key fails load. Its name must not match an agent crew, built-in ones included. Human crews are kept apart from the agent crews, so a `[crews]` table holding only human crews still gets the built-in agent crews.
- **Never drawn.** `workflow.default_crew`, the complexity, type and purpose pools, `purpose_crews.review`, and experiment arm overlays may not name one. Config load refuses the first four, and `orbit experiment start` refuses an arm. A redraw never selects or moves a task onto a human crew.
- **Never admitted.** A drain or pull follower skips a human-crew task even when `--allow-crew` names the crew. `--allow-crew` accepts a human crew's name, but it admits nothing. `orbit run readiness` reports the task with reason `human_crew` and the crew. An explicit ship of the task admits nothing, and the dashboard shows no Ship control for it.
- **Never piloted.** The task pilot excludes the task (`human_crew`), refuses an explicit request (`task_pilot_human_crew`), and records no assessment, warning or hold on it.
- **Closed by the operator.** An agent-attributed `orbit.task.update` (one that passes `model`) cannot move a human-crew task to `done`. The operator's own update or the dashboard can, and without the lifecycle gates: `orbit task update <id> --status <status>` or the dashboard's status control (which offers every status for such a task) moves a human-crew task from any status to any other, for example `proposed` to `done`, with no plan, review, completion evidence or `--force`. A note stays optional. Each move is recorded as a `forced` status entry with the actor, the from and to statuses and a note naming the human crew (`human-crew bypass`). Only the operator's CLI and dashboard get this; `orbit.task.update` and MCP keep the normal lifecycle on the same task, so assigning a human crew never lets an agent skip review. A non-terminal local run that owns the task still refuses the move, and leaving `done` or `archived` still needs `--force`.
- **Out of agent analytics.** Crew analytics, performance rankings and provider usage leave human crews out.
- **Listed.** `orbit config show` lists a human crew with provider `human`, and `orbit config get crews.<name>.kind` answers `human`. For an agent crew, `kind` is absent. The dashboard tags each human-crew task `for <name>`, classifying it by the task's own workspace: task reads and write responses carry `human_crew` (`{name, description}`) for a task assigned to a human crew, so the tag, the operator status control and the missing Ship control hold in the All workspaces view too, even when another workspace configures the same name as an agent crew. The **Waiting on you** chip, under **More** on the Tasks page, counts open human-crew tasks and tasks with open decisions, never a `done`, `archived`, `rejected` or cancelled one. The **My work** chip beside it (`#tasks?crew=mine`) keeps the open tasks of the viewer's human crew: the only human crew, or else the one matching the OS user that runs `orbit web serve`.
- **Seeded by `orbit init`.** When the global config has no human crew, `orbit init` adds one named after the OS user, normalized to lowercase letters, digits, `_` and `-`. At a terminal it prompts with the OS user as the default; without a terminal, or with `--non-interactive`, it takes the OS user as-is. `--human-name <NAME>` sets the name. A name an agent crew already holds is asked again at a terminal and refused otherwise, before init writes anything. A re-run leaves an existing human crew unchanged.

### The `no-drain` task tag

Tag a task `no-drain` when an interactive session owns it and no drain should pick it up. It is a reserved task tag, not a config key; there is no `workflow.drain_exclude_tags` setting.

- **Never admitted automatically.** `orbit run auto` drains, pull-drain followers and any automatic admission pass skip a backlog task carrying `no-drain`. `orbit run readiness` and the backlog-exclusion listing report it with reason `no_drain_tag`.
- **Implies `no-auto-approve`.** Drain-window auto-approval (`select_proposed_approvals`) never approves a proposed `no-drain` task, even with a clean pilot assessment (held as `no-drain`). Task-pilot may still assess it.
- **Explicit dispatch still works.** `orbit run ship <id>` runs it, and an agent or human can claim it to `in-progress` and deliver it by hand.
- **Removable.** Removing the tag with `orbit.task.update` (send the full tag list without `no-drain`) makes it eligible on the next admission pass.

### Per-crew credentials (`env`)

Every crew of a provider reads its credential from the same agent environment, so by default they share one account. A crew's `env` list points that crew alone at another one, such as an API key with prepaid credits next to a Claude subscription:

```toml
# ~/.orbit/config.toml (global only)
[crews.opus-credits]
provider = "claude"
model = "claude-opus-5-5"
effort = "high"
env = ["ANTHROPIC_API_KEY=ANTHROPIC_CREDITS_API_KEY_1"]

# or read the key from a file instead of a variable
[crews.opus-credits-file]
provider = "claude"
model = "claude-opus-5-5"
effort = "high"
env = ["ANTHROPIC_API_KEY=file:~/.config/anthropic/credits-key"]   # chmod 600
```

- **`NAME`** passes `NAME` from the launching process to this crew's agents only, whether or not [`pass`](#executionenv--the-agent-subprocess-environment) lists it.
- **`NAME=SOURCE`** gives this crew's agents `NAME` with the value of `SOURCE` from the launching process, or from `~/.orbit/clock.env` for [clock-started runs](#executionenv--the-agent-subprocess-environment). Keep the source under a name no CLI reads (`ANTHROPIC_CREDITS_API_KEY_1`), so interactive shells and other crews are unaffected and different crews can use different keys. The agent never receives `SOURCE` itself.
- **`NAME=file:PATH`** gives this crew's agents `NAME` with the contents of the file at `PATH`, with the trailing newline trimmed. The host opens the file itself each time it starts an agent, outside the agent sandbox, and sets only `NAME` in the agent's environment, so no login shell, service unit or `~/.orbit/clock.env` has to export anything: one config line works for CLI, dashboard and clock-started drains alike. `PATH` must be absolute or start with `~/` (the launchers have different working directories). The `file:` prefix cannot be mistaken for a variable name, which never contains `:`.
- **Names and paths only, never values.** `NAME` and a variable `SOURCE` must be variable names (letters, digits and `_`, not starting with a digit), and neither may be an `ORBIT_*` name. A malformed entry (a `file:` path that is empty, relative or ends in `/` included), or two entries for one `NAME`, fails load naming the crew and entry.
- **A file source is held to the `clock.env` rules.** The file must be a regular file owned by the user running Orbit, mode `600` or stricter, and not a symlink (a symlinked parent directory is fine). A missing, empty, symlinked, foreign-owned or group- or world-readable file is not read: the crew is unavailable exactly like a crew whose source variable is unset (the drain's window leaves it out and its activities are refused), and the warning names the crew, the variable, the path and the file's state (`does not exist`, `refusing mode 644 … run chmod 600`), never the value.
- **Global config only.** A workspace `config.toml` that sets `crews.<name>.env` fails load naming the crew, and `orbit config set` refuses it without `--global`, so a checkout cannot choose which host secrets its agents read. Set it with `orbit config set --global crews.opus-credits.env '["ANTHROPIC_API_KEY=ANTHROPIC_CREDITS_API_KEY_1"]'`.
- **The crew's entry wins.** It replaces the same name from `pass`. When it sets a Claude credential (`ANTHROPIC_API_KEY` or `CLAUDE_CODE_OAUTH_TOKEN`), the other one is removed from that crew's agents, so exactly the credential the crew chose is present. Other crews keep the shared `pass` environment.
- **Services have their own environment.** Dashboard-started drains inherit `orbit-web.service` (or the web LaunchAgent), and clock-started drains inherit `orbit-sweep.service` / `com.orbit.sweep` plus `~/.orbit/clock.env`. A variable exported by a login shell does not reach an already running service. On Linux, put sources in an owner-only environment file and reference it with `EnvironmentFile=/absolute/path/to/crew.env` in a `[Service]` drop-in created with `systemctl --user edit orbit-web.service`; run `systemctl --user daemon-reload`, restart the service, and start a new drain. Configure the sweep unit similarly if needed; clock runs can instead read the sources from owner-only `~/.orbit/clock.env`. On macOS, provide the source in the LaunchAgent's `EnvironmentVariables` and reload it, or use `~/.orbit/clock.env` for clock runs. A `file:` source needs none of this: the host reads it directly in every launcher. Keep values out of config and command arguments. `orbit config set crews.<crew>.env` and `orbit doctor` warn separately about missing shell sources and installed managed services (a `file:` source is reported by its file's state instead); a stopped or unreadable service is reported as unverified, never assumed to inherit the shell. Linux checks the running unit's process environment; macOS checks launchd's service environment. Clock sources in `~/.orbit/clock.env` are checked separately and never assumed to supply the web service.
- **An unset source excludes the crew.** A drain or ship start (`orbit run auto`, `--pull` included, `orbit run ship`, MCP and the dashboard) reports each crew whose source is unset or empty in its `warning`, and `orbit run job` warns on stderr; each names the crew, source and variable, and records them on the run (`Unset crew env:` in `orbit run show`, `run.crew_env_unset` in JSON, the dashboard run detail). The drain's crew window leaves the crew out (a pull drain's preflight lists it as excluded), and an activity dispatched as that crew anyway, such as an explicit `--input crew=`, is refused before its provider starts rather than run on the provider's default login. `orbit doctor`'s `env-pass` row reports it.
- **Readiness names the cause.** Tasks excluded for missing sources show `crew_env_unset` with the crew and source names. With a live drain, readiness uses that drain's recorded missing sources; otherwise it checks the current launching environment. A positive-weight complexity/type pool crew excluded on every observed admission pass for at least two minutes (and two passes) produces a run-level warning in `orbit run show` and readiness and a deduplicated automation friction on an owner drain (a structured `crew_pool_excluded` warning signal on a follower). The streak resets when the crew is available again or leaves the pool; reading readiness alone never advances it.
- **Everywhere the agent environment goes.** The overlay applies to implementer, reviewer and recovery agents and to a pull drain's [authentication probe](#executor-authentication-recovery-probes) for that crew. A detached worker receives each source variable by name (name-only `systemd-run --setenv` under [containment](#worker-containment)) and composes the crew's environment itself, reading a `file:` source from disk as the same user.
- **Its own account.** A crew that sets `env` is a provider account of its own; every other crew of the provider shares the provider's default login. [Usage-limit readings](#provider-usage-limits), [provider failure holds](#provider-failure-holds), host holds of the task-pilot routine, pull-drain [authentication exclusions and probes](#executor-authentication-recovery-probes), `--allow-crew` and `provider_limit_budgets` / `provider_limit_overrides` key on the provider and that crew for such a crew, and on the provider alone for the rest, which behave as before. The subscription reaching its 5-hour limit does not stop `opus-credits`, and an expired or invalid key on `opus-credits` neither holds nor excludes the subscription's crews. Surfaces name such an account `provider/crew` (`claude/opus-credits`). Records written before accounts were named carry none, and read as the provider's default login's.

`orbit config show` and `config get crews.<name>.env` print the entries as written, and the dashboard shows them under the crew's name; none of them, `orbit doctor`, run records or logs ever print a value.

### Repairing a config that already names a crew with a colon

Every command, `orbit config set` included, refuses to load, so edit the file by hand. Layers are checked before merging, so the error names the defining file:

```
error: invalid input: [crews]: crew name 'gpt-5:codex' must not contain ':' or '@'; ...
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
| `antigravity` | `agy` | [Antigravity CLI](#antigravity-cli) |
| `muse` | `muse` | [Muse Code CLI](#muse-code-cli) |
| `ollama` | — | Recognized but unsupported at the Orbit CLI entry point (`provider.unsupported`). |
| `openai_compat` (`openai-compat`) | — | HTTP-only, no CLI runtime; selecting it fails. |

- Parsing ignores case and surrounding whitespace.
- Deprecated aliases resolve with an `orbit.config.crew` warning: `anthropic` → `claude`, `openai`/`chatgpt` → `codex`, `google` → `gemini`, `xai` → `grok`.
- `copilot`, `antigravity` and `muse` have no aliases, and the model vendor a lane runs (a Claude model through Copilot) never changes the provider.
- The cross-repo Worker executor runs only `claude`, `codex`, `gemini` and `grok`; a Worker-routed step naming another lane is refused, not re-pointed.

**Audit attribution.** Process audit maps `ORBIT_AGENT_NAME` and `ORBIT_AGENT_MODEL` to an agent family. An invalid pair (`copilot` with a `claude-*` model) or an agent envelope with no identity records `unknown` with a warning. An agent envelope (non-empty name and model, truthy `ORBIT_MANAGED_RUN_CONTEXT`, `ORBIT_TASK_ACTOR_KIND=agent`) takes precedence over `ORBIT_ACTOR`, the operator override and the OS username. This affects recorded identity only, not command authorization.

### Resolution precedence

**Which crew a task dispatches.** The first tier that is set wins:

1. **explicit**: `--crew` or run-input `crew`.
2. **task_config**: an explicit `task.crew`, or a pool or [purpose](#purpose-crew-pools) assignment still valid for its pool (normally set at creation; admission redraws stale pool assignments and default fallbacks, see [pools](#automatic-crew-pools-by-complexity)).
3. **workspace_default**: `workflow.default_crew`.
4. **environment_default**: `CONSTELLATION_DEFAULT_PROVIDER`, a provider ID or alias. `claude` selects `opus`, `codex` selects `sol`, `antigravity` selects `gemini-flash` and `muse` selects `muse-spark` when defined; any other ID selects the same-named crew. Never overrides a configured `default_crew`.
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
- `unknown provider '<x>'; expected one of claude, codex, gemini, grok, copilot, ollama, openai_compat, antigravity, muse`
- `provider 'cursor' is retired and no longer supported; pick a supported provider (…)` — likewise for `pi` and `opencode` (see [retired providers](#retired-providers))
- A selected lane whose binary is missing fails permanently, naming the binary.

---

## Provider CLI notes

For every lane: Orbit passes `--model` from the crew and sends the prompt on **stdin**, never argv (process listings and audit see argv). Muse Code, which cannot read stdin, gets the prompt in a private file instead ([below](#muse-code-cli)). The Orbit OS sandbox is the filesystem boundary, and provider-specific write grants apply only while that provider runs. Credentials reach the agent through [`[execution.env].pass`](#executionenv--the-agent-subprocess-environment), which every crew shares; through a crew's global [`env`](#per-crew-credentials-env) entry, which reaches only that crew's agents; or from the provider's own credential store. Orbit never puts a key on argv. A lane without native or Orbit-managed MCP reaches Orbit tools with `orbit tool run <tool> --input '<json>'` through its shell tool, under the same grants.

`orbit tool run` selects a workspace from any directory with `workspace` in the JSON input or the global `--workspace` flag: a name, `ws_*` ID, absolute local checkout path, or local `hm_*/ws_*` selector from the federated listing. A selector naming a registered remote host (`orbit host list`) is delivered to that host over the federated route, and fails with `unreachable_destination` when the route is down. A selector naming a host this machine does not list fails locally with `invalid_input`, naming the host. Unparseable JSON or an unreadable `--input-file` is reported before workspace selection, runtime open or migration, so an input typo never migrates the checkout.

### Retired providers

The `cursor`, `pi` and `opencode` lanes were retired (ORB-18096). A crew naming one fails to load, naming the crew, the retired provider and the remedy: pick a supported provider (`claude`, `codex`, `gemini`, `grok`, `copilot`, `antigravity`, `muse`). `orbit doctor` reports the same refusal. Persisted records naming them still read and display. The Cursor editor MCP client (`orbit mcp init --cursor`) is a separate surface and is unaffected.

### GitHub Copilot CLI

| | |
|---|---|
| Install | `npm install -g @github/copilot`, then `copilot --version`. The retired `gh copilot` extension is not supported. |
| Auth | `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, else the `copilot /login` session (macOS keychain item `github-copilot-app`). Orbit forwards none of the token variables unless one is in `pass`. On a Mac, run `/login` once. |
| Model | `--model` is always passed, so `COPILOT_MODEL` and the saved `/model` are ignored. List IDs with `/model` inside `copilot`; recheck pins after CLI upgrades. |
| Flags | `--allow-all-tools --no-ask-user --output-format json`. Never `--allow-all`/`--yolo`, which widen paths and URLs. |
| Sandbox | Write: `COPILOT_HOME` (default `~/.copilot`) and `$XDG_CACHE_HOME/copilot`. macOS: read `~/Library/Keychains`. No access to `~/.config/gh`. |

An account without Copilot entitlement fails with `Error: Authentication failed`; org-policy-disabled third-party MCP servers produce a `session.warning` (`warningType: "policy"`). The GitHub org admin fixes both. A run with no assistant message fails the step.

### Antigravity CLI

`antigravity` launches `agy`, Google's current terminal agent: a separate lane from `gemini` (the legacy Gemini CLI), with different flags, MCP config and model slugs. `gemini-*` models still attribute to the Gemini model family.

| | |
|---|---|
| Install | See the [Antigravity CLI docs](https://www.antigravity.google/docs/cli/headless/), then `agy --version` and `agy models`. |
| Auth | One interactive `agy` login, cached under `~/.gemini/antigravity-cli/`. An unauthenticated headless run exits with `authentication required`. |
| Model | A slug from `agy models`; `effort` accepts `low`/`medium`/`high` only. To migrate a `provider = "gemini"` crew, change the provider and use an `agy models` slug. |
| Flags | `--input-format stream-json --output-format stream-json --dangerously-skip-permissions`, plus `--print-timeout` set to the activity's wall-clock runtime minus 30 s, so `agy` stops itself before Orbit's deadline kills it. The argv is fixed at launch, before any build-admission credit is earned, so credit extends Orbit's deadline but not `agy`'s budget. A shorter custom value is kept. The invocation-start audit event records `print_timeout_ms` and the launch `effective_deadline_ms`; the step output records the same print budget and the final `effective_deadline_ms` (runtime plus earned credit). Deadlines are durations from spawn. For runtimes of 30 s or less the full runtime is used, with a minimum of 1 s, because the shutdown margin does not fit. Don't add `agy --sandbox` or Gemini CLI flags. |
| Sandbox | Write: `~/.gemini` (shared with the Gemini CLI). macOS: read `~/Library/Keychains`. |
| MCP | `~/.gemini/config/mcp_config.json` or `.agents/mcp_config.json`, not `.gemini/settings.json`. |

Orbit checks the response from a terminal `result` with `status: "SUCCESS"` for its completion envelope; a non-zero exit with a terminal `ERROR` surfaces the bounded, redacted `error` string. Running for the whole `--print-timeout` spends the provider budget and is never completion: a run that then exits 0 with `SUCCESS` but no Orbit completion envelope fails as a timeout (`timed_out: true`, retryable, transient) with a diagnostic naming the print-timeout and elapsed time, keeping the bounded `final_message`. An early exit without an envelope gets the ordinary completion-envelope message.

Background commands cannot be turned off for a headless run. `agy` 1.3.3 has no flag or setting for it (`agy --help` lists none), so Orbit passes nothing and instead tells every provider, in the shared prompt contract, to run commands in the foreground. `agy` itself waits for a background task it started only until `--print-timeout`, capped at 30 minutes, then ends the turn and exits 0 with the task still running and no envelope. When the last `manage_task`-style status report in stdout shows a `task-<id>` still running, the step fails naming that task, the elapsed time and the bounded last `response`, instead of the generic message. The exit code, a `SUCCESS` wrapper and a persisted summary never count as completion; whatever the run left in the checkout is unverified.

### Muse Code CLI

`muse` runs Muse Code, Meta's terminal coding agent. The contract below was observed on Muse Code 1.4.10 (`1.4.10-R6427.1`, macOS): `muse exec --help` and offline `--provider echo` runs only, with no authenticated run. The account-window allowance was additionally observed on Muse Code 1.4.12 in an authenticated macOS run (2026-10-23).

| | |
|---|---|
| Install | `~/.local/bin/muse`, a launcher that execs `~/.local/bin/muse-bin-<version>`; check with `muse --version`. Init detects `muse` on `PATH` and enables the `muse-spark`, `muse-wonder` and `muse-wonder-full` crews. Orbit sets `MUSE_NO_AUTO_UPDATE=1` and `MUSE_LOGIN=0` for every run (an outer value cannot override them), so a run never swaps the binary under running work or starts a device login; upgrade with the launcher yourself. |
| Routing | The shipped asset sets no `host_os`: Muse runs on macOS and Linux, so a muse crew is admitted on either host and the seeded sandbox is translated per host (`macos-sandbox-exec` on macOS, `linux-bwrap` on Linux). Native Windows is not an Orbit runtime (ORB-13857), so a Windows host still has no runnable Muse lane through the normal executor/sandbox availability path. |
| Auth | `muse login` (Meta account, browser device code) once, outside Orbit. The login writes the file `auth.json` under the config root (`$XDG_CONFIG_HOME/muse`, else `~/.config/muse`), and on macOS `muse exec` also reads the login keychain, so Muse gets the read-only `~/Library/Keychains` carve-out; without it a valid login fails with `missing meta credentials`. Or `pass = ["META_API_KEY"]`, which Muse prefers; `muse auth set --api-key-stdin` also works. Orbit never logs in or forwards `META_API_KEY` unless `pass` names it. |
| Model | A catalog id (`muse-spark-1.3`); `muse model-profile show <model> [--effort <tier>]` resolves one. `effort` renders as `--reasoning-effort`; Muse accepts `none` … `ultra`, which covers every Orbit effort, and any other value is refused at config load. |
| Prompt | `muse exec` does not read stdin (piped stdin alone exits 2, `missing prompt`) and refuses `--prompt-file /dev/stdin` (`not a regular file`). Orbit writes the prompt to a mode-0600 file with an unpredictable name in the run's scratch directory (`<worktree>/.orbit/tmp/`), passes `--prompt-file <path>`, and deletes the file when the invocation ends, whatever its outcome. The envelope never reaches argv; the audit keeps the prompt in the invocation's stdin blob. An activity profile that cannot read the worktree cannot read the file either, and the run fails. |
| Flags | `exec --json --approval-mode never --disable-request-user-input --no-foreign-personal-context --disable-reminders --no-session-log`. `--approval-mode never` and `--disable-request-user-input`: nobody can answer mid-turn, and neither is a security boundary. `--no-foreign-personal-context`: Muse otherwise loads your Claude Code and Codex personal rules and skills. `--disable-reminders`: otherwise every turn spawns skill, goal and verify reminder child agents. `--no-session-log`: no session log per run (this also disables Muse's local session messaging). `--trust-workspace` is not passed: a worktree is content Orbit does not vouch for, so its Muse skills and rules stay unloaded and agent delegation stays off; the envelope directs the agent to read `AGENTS.md`. `--max-model-steps` and `--disable-web-tools` are not passed; the activity deadline bounds the run. |
| Sandbox | Write: the config root and `$XDG_DATA_HOME/muse` (else `~/.local/share/muse`: tracing, plugins, skills); on macOS also `~/Library/Application Support/Muse` (session-name authority). macOS: read `~/Library/Keychains`. Muse's own sandbox is on by default with network `proxy-only`; under Orbit's sandbox (or `sandbox: off`) Orbit appends `--disable-sandbox`, because a second Seatbelt profile cannot be applied inside Orbit's and Orbit's profile is the filesystem boundary. `--yolo` is never passed. |
| MCP | Muse has a native MCP client (`mcpServers` in `settings.json`, or `-c KEY=VALUE` per run), but per-run injection of Orbit's server was not verified, so Orbit passes no `-c` and never writes `settings.json`; Orbit tools go through `orbit tool run` on `PATH`. `orbit mcp init` has no Muse target. |
| Images | `muse exec --image <PATH>` (repeatable) attaches a local image. On Muse Code 1.4.14 (Linux) an authenticated canary probe passed for all three shipped crews, so the shipped executor declares [`image_input`](#executor-image-input-image_input) `attach_args` for PNG. |

Orbit reads only string `payload.text` of the last `run.terminal.completed` record, and only when its `payload.terminal` is `completed`. Missing or non-string answer text cannot be replaced by `payload.message`, `payload.error`, or `payload.error.message`; those fields are fallback failure reasons for non-completed terminal diagnostics only. `failed`, `cancelled` and unknown values (the field is wire-open) fail the step, as do a missing terminal record, a stdout line that is not a JSON record with a `payload_type`, and a non-zero exit even with a completed record. The prompt echoes (`run.lifecycle.started`, `turn.input.user`), partial `run.output.delta` text and child-task records never count as completion. The terminal record carries no usage, so no provider token usage is reported. Account usage windows instead come from top-level usage records with `payload.kind = "run_usage"`: a `status` record (Muse Code 1.4.12) or, from Muse Code 1.4.13, a `run.usage` record (`payload_type` `run.usage`, `record_type` `status`, the same allowance shape). Orbit reads both and takes the last one. `payload.allowance.window` and `.weekly` report `used_percent` and `resets_at_ms` (epoch milliseconds). Orbit preserves the `window` / `weekly` labels, records them at run completion and gates them through the shared provider-limit policy; it does not infer a window duration or read per-model token usage from this record. `muse serve` (MSP over stdio) is not used.

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

- **Selected at creation.** A task created without `crew` (`orbit task add`, `orbit.task.add`, an auto-task mint with no template crew, an import) draws from its complexity's pool, else `default_crew`, storing `task.crew` and its provenance `task.crew_source`; a `crew_assigned` history entry records the source (`explicit`, `pool:<complexity>`, `type:<type>/<tier>`, `purpose:<purpose>` or `default`). A task with a [purpose](#purpose-crew-pools) whose pool is configured draws from that pool instead of its complexity's, and a task whose type has a [type pool](#type-pools-by-task-type-and-complexity) for its tier draws from that. That entry is a second write after the insert; if it fails, the task is still created and returned, and the failure is logged. With no crews configured the field stays unset.
- **Provider limits at creation.** Creation, crew clearing and complexity re-rates skip pool members covered by this host's live [provider usage limits](#provider-usage-limits), before image capability filtering and weighting. If every positive-weight member is limited, the task stays unassigned for admission to retry; it does not fall back to another pool or the default crew. Once the reading resets, that member can be drawn again. An explicitly named crew keeps its assignment; admission applies the configured explicit-crew limit policy.
- **Complexity changes before admission redraw automatic crews.** When the pilot or `task update --complexity` changes the tier of unreserved proposed, backlog or someday work, a crew sourced from another pool or the `default` fallback is redrawn in the same mutation, logged as `crew_redrawn` with previous and new source and crew. Explicit crews stay pinned, even when an update names the crew a pool already chose; status transitions keep the selection. `task update --crew <name>` pins; [`--crew ""`](#setting-taskcrew) redraws. Every stored change records the actor, prior and new crew, and whether it was named or drawn.
- **Tiers:** `low`, `medium`, `hard`, `xhard`. Unset or `unassessed` complexity uses the default chain. The task pilot never demotes a task out of `xhard`. An `unassessed` recommendation for a task that already has an assessed tier keeps that tier and its pool, and holds the task on its unresolved footprint until the pilot reassesses it.
- **Empty pool** (`[]`, the init scaffold) means no pool: the task gets `default_crew`, as it does when all members are [disabled](#disabled-crews). Blank entries and unknown crew names fail before dispatch.
- **Preferences, not allowlists.** An explicit `task.crew`, an explicit run crew, and review and preparation jobs keep the crew they name, except that a standing [provider failure hold](#provider-failure-holds) redirects a task-crew draw away from the crews it excludes, and a [provider usage limit](#provider-usage-limits) removes limited crews from the draw.
- **Serving hosts at creation.** The creation and re-rate draws above keep a crew whose executor `host_os` (below) excludes the creating host when another host serving this workspace can run that executor. A host serves this workspace when the owner's own records show it pulled here recently: it is in the owner's host registry (`orbit host list` shows each host's recorded `host_os`, read from the host at `orbit host add`) and it holds an execution claim in this workspace from the last seven days. Registration alone never counts — a registered host that serves only other workspaces changes nothing — and a follower that stops pulling ages out after the window, so its OS-only crews stop being drawn; tasks already drawn keep their crew and wait. The task keeps the drawn crew and waits for that host; admission on each host, and activity and purpose-pool draws, still filter by the executing host alone. With no serving follower of the needed OS the creation draw skips the crew as before. Before drawing, creation, crew clearing and complexity re-rates also discard complexity-pool crews whose executor cannot intersect the task's `os:` tags, falling back to `default_crew` when the pool empties. A simultaneous tag edit supplies the tags for that draw. An untagged task can still draw a follower-only crew; an explicitly requested crew with an empty task OS intersection is refused before persistence.
- **Admission** (drain or ship) checks a pool-sourced crew against the current tier and its enabled, positive-weight members. A stale pool assignment or `default` fallback, or no crew, draws from the current pool, else the default chain; a default fallback is never an explicit pin. Without `crew_source`, legacy assignment history recovers provenance, and a crew with no assignment evidence counts as explicit.
- **Admission write-back.** `task.crew` names the crew of the task's latest delivery. When admission runs a task on a crew other than its recorded one, it writes that crew back with a `crew_source` naming why and a `crew_assigned` history entry (``crew changed from `muse-wonder` to `luna` by experiment exp-4e3e0399 arm cheap-pool``): `experiment:<id>/<arm>` for an experiment arm's crew overlay, else `redraw:<reason>`, where the reason is `provider_hold`, `provider_limit`, `usage_pressure`, `run_crew` (an explicit run-input `crew`) or `pool`. A claimed leaf draws its crew on its executor, so the owner writes the leaf's reported crew back when the leaf settles, with `redraw:claimed_leaf` when no crew experiment chose it. Only the settlement the owner commits writes it: a replayed settlement, including one that overlapped the first, returns the original receipt and leaves the task's crew alone. A written-back crew is not a standing assignment: the next admission draws the task's pool again (or its experiment arm, or the explicit run crew), and a terminal task is never rewritten. The write lands only once the run is durably admitted: a refused or disabled submission, and a keyed resubmission that resolves an existing run, leave the task's crew, source and history as they were, and the write applies only while the task still names the crew its draw read. This also replaces an explicit `task.crew` that a run crew or a provider hold overrode; its history entry names the earlier crew, and `orbit.task.update` sets the pin again. `orbit migrate --backfill-task-crews` repairs tasks admitted before the write-back, as `crew_source: backfill:admission`.
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

#### Type pools by task type and complexity

```toml
[workflow.type_complexity_crews.ui]
hard = ["opus"]

[workflow.type_complexity_crews.security]
hard = ["opus"]
medium = ["sol:200", "opus:50"]
```

`workflow.type_complexity_crews` routes crews by task kind. A task's type is the one [`workflow.task_types`](#task-types) gives it from its current tags and context files, the same lens the implementer scorecard measures by. When `workflow.type_complexity_crews.<type>.<tier>` is set for that type and the task's complexity, the pool replaces `workflow.<tier>_complexity_crews` for that draw; any other task, `other` included, draws from its complexity pool exactly as before.

- **Entries** are written like a complexity pool's (`crew` or `crew:weight`), but every entry must name a defined, enabled crew with a positive weight, and a pool must not be empty: remove the key to fall back to the complexity pool. The type must be a `workflow.task_types` name and the tier one of `low`, `medium`, `hard` or `xhard`. Each refusal is a load error naming the full key.
- **Layering.** Each `<type>.<tier>` pool is one setting: a workspace pool replaces the same type and tier from global, and the global's other pools still apply.
- **Precedence.** An explicit crew, then a [purpose pool](#purpose-crew-pools), then the type pool, then the complexity pool, then `default_crew`. A type pool whose members are all excluded by executor `host_os` or the task's `os:` tags falls through to the complexity pool. OS filtering and [usage-pressure](#usage-pressure-draws) weights apply to type-pool members as to complexity-pool members.
- **Provenance.** A type-pool draw records `crew_source: type:<type>/<tier>` (such as `type:ui/hard`), and its `crew_assigned` or `crew_redrawn` history names that source. `orbit task show` prints the source. Admission keeps that `type:` source in `crew_selection.source` in `orbit run show` while the task keeps its assigned member; a draw from the whole type pool records the pool's key, `workflow.type_complexity_crews.<type>.<tier>`, with `; usage pressure` appended when usage pressure weighs it. A provider hold or usage limit adds its own note to the source it applies to (`; provider hold excludes <crews> until <time>` or `; provider limit: <detail>`), ahead of any pressure suffix.
- **Where it draws.** Creation, `task update --crew ""`, a complexity re-rate before admission and fresh admission. A tag or context-file edit that moves an unreserved proposed, backlog or someday task with a pool, type or default source to another type pool redraws it in the same update, as a re-rate does. Implicit edits preserve the crew and source of admitted deliveries, including stopped runs and preserved candidates, in-progress or review work, and work reserved by a non-terminal delivery run or protecting execution claim, even before setup binds the run to the task. Scope metadata still changes under its existing authority rules; it never rewrites recorded run crew/model facts or emits a redraw for a crew that did not run. Local admission serializes selection, run insertion and crew writeback with task edits. An authorized recovery or requeue permits a genuinely fresh admission to evaluate the updated classification and record its selected crew; a candidate resume keeps its original admission provenance. Explicit audited crew corrections retain their existing authority rules.
- **Admission** keeps a `type:` crew while the task still classifies to that type and tier and the crew is an enabled, positive-weight member of the pool. Otherwise the task draws from the pool it now routes to. A [provider usage limit](#provider-usage-limits) or [failure hold](#provider-failure-holds) covering the crew redraws from the pool's other members, then the complexity pool, then `default_crew`. A claimed leaf on a follower keeps the owner's type-pool crew, because the claimed snapshot carries no tags or context files to classify by.
- **Seeing it.** `orbit config show` and `orbit config keys` list the table, and Settings › Crews names each `workflow.type_complexity_crews.<type>.<tier>` pool a crew is in under **Used by**.

**Allowlists.** With `--allow-crew`, the draw renormalizes over permitted members, keeping their ratios. A pool with no permitted positive-weight member is disjoint, and `orbit run readiness --allow-crew <crew>` reports `crew_not_allowed`. Without `--allow-crew`, readiness (and the dashboard Drain card) applies the live drain's own set and reports it as `capacity.allowed_crews`. When a previously drawn purpose, type, or complexity pool crew is excluded, admission redraws from permitted members of that same pool and records the actual crew on the task and run. An explicit or legacy pinned crew still waits; a pool with no permitted member also waits. The allowlist still applies to system and review activities at dispatch.

**Frozen per run.** The admitting run captures the effective pools in run input `auto_crew_pools`, inherited by descendants. Each admitted leaf records `crew` and `crew_selection` (task ID, complexity, source, and the eligible `[{name, weight}]` after renormalization, with `pressure` and `effective_weight` under [usage-pressure draws](#usage-pressure-draws); a run admitted before [ORB-22804] may also carry a `performance` factor per member), shown as `Crew Selection:` in `orbit run show`. Retries and resumes keep it.

### Provider failure holds

A local run can fail because of its provider rather than the work. The step error then carries a typed marker followed by `provider=<name>`. Only the failing provider CLI's own stderr, terminal error or failure frames set a marker, never the agent's transcript, tool output or Orbit envelopes (an agent reporting a GitHub rate limit is not a provider limit).

| Marker | Meaning | Excluded crews | Base backoff |
|---|---|---|---|
| `[provider_capacity]` | The selected model was at capacity. | The run's crew when the failing step has the same provider and account. If either differs, crews of the failing provider that use the failing account: the named crew for `account=<crew>`, or provider crews without `env` for the default login. If none match, the run's crew. | 15 min |
| `[provider_unavailable]` | Unusable on this host, such as failed authentication. | As for capacity. | 30 min |
| `[provider_refusal]` | Content policy refused the task: a Codex content-filter `error` or `turn.failed` frame (`This content was flagged for possible …`), or a Claude `result` with `stop_reason: "refusal"`. | Every crew of the provider, which would refuse the same content again. | 24 h |
| `[provider_limit]` | The account hit a usage limit (texts below). | The failing account's crews on that provider. If the limit names a model and at least one of them runs a model containing it (case-insensitive), only those crews; otherwise all of them. | Until the reported reset (below) |

**Accounts.** When the failing crew sets [`env`](#per-crew-credentials-env), the marker is followed by `account=<crew>` after the provider (`[provider_limit] provider=claude account=opus-credits …`): that crew is its own account. For capacity and unavailability, the table uses the failing account when the failing step's provider or account differs from the run crew's: a named account selects that crew, while the default login selects only the provider's crews without `env`. If the provider and account both match the run crew, only the run crew's family is excluded (below), not the account's other crews; if no crews match the failing account, the run crew is the fallback. A usage limit independently excludes the failing account's crews on that provider, narrowed further by a reported model as described above. A capacity, unavailability, or limit failure without `account`, including older markers, is for the provider's default login. A refusal is the provider's content policy and still excludes every crew of the provider. A stored hold keeps the crews it named; its note carries `account` when it is a crew account's.

**Crew families.** A hold matches each crew it names by family: the crew and every [`<crew>@<effort>` reference](#crewsname--which-provider-model-runs-the-task) to it, which share its model and account. The rules above still select which crews a hold names, and its note lists them as selected, but naming `opus` also keeps `opus@medium` and `opus@high` out of the draw, and a capacity or unavailability hold after a run as `opus@medium` keeps `opus` and its other effort references out too. Crews carried from an earlier hold keep their machine scope and match by family the same way. A pull drain's crew window matches its exclusions, backoffs and runnable crews by family as well.

Capacity, unavailability and refusal backoff doubles for each other hold the task got in the last 24 hours, up to 24 hours. A usage limit holds until the provider's reset, clamped to 5 minutes – 7 days after the failure; with no readable reset it starts at 30 minutes and doubles per earlier usage-limit hold on that provider account in the last 24 hours, up to 6 hours. Provider labels are parsed first, so an alias is the same provider. Crews an earlier, still-standing hold excluded stay excluded, each on the machines it was excluded on.

Recognized usage-limit texts: Antigravity's `Individual quota reached … Resets in 1h37m37s`; Gemini's `TerminalQuotaError`; Codex's `You've hit your usage limit … try again at <time>` (an `error` and `turn.failed` pair); a Claude `result` with `is_error: true` starting `You've hit your` or `You've reached your`, or a `rate_limit_event` with status `rejected`; grok's `Too Many Requests (429)` and Grok Build's `API error (status 402 Payment Required): Grok Build usage balance exhausted` (which names no reset time and takes the default hold backoff). The marker then carries `model=<model>`, `window=<window>` and `resets_at=<RFC 3339 time>`. A relative reset is read from the failure time; a Codex `try again at` time is local to the host.

**Run outcome.** An owner-local run stopped by `[provider_capacity]` or `[provider_limit]` ends `held`, with diagnostic code `provider_capacity` or `provider_limit`. `orbit run history` shows the hold, reliability counts it under held rather than failed, and the gate and auto parents pass their success guards without echoing a failure. Incidents retains a provider deferral record. Authentication and content refusals keep their existing failed run outcomes. A routine fire backed by a held run retains the existing `succeeded` fire status; its underlying run still reads `held`.

**Handoff.** Step and final recovery skip these failures. On the PR pipeline the handoff returns `held_provider_failure`: it commits the candidate on the run's branch, publishes no branch and opens no `[BLOCKED]` PR, but pushes the candidate to `refs/orbit/candidates/<task>/<run>` on `origin` so a claim on another host can continue it (a task comment says when that push failed). Run finalization moves the task to `backlog` under a `provider_failure_hold` history event, whose note carries the class, provider, excluded crews, `not_before` and run. A `[provider_unavailable]` hold also names the host's `machine`: a failed login is that host's, so its crews are excluded only there, while crews a still-standing capacity, limit or refusal hold excluded stay excluded on every machine (`carried_crews`). A hold stored without a `machine` excludes its crews everywhere, as before. A claimed leaf on a follower that ends with `[provider_refusal]` releases its claim as a `provider` failure, and the owner records the same hold from its own crews, naming the leaf run.

**Admission during the hold.** Until `not_before`, the crew is drawn from crews the hold does not exclude: the task's own crew or pool, then the other enabled members of its purpose or type pool when the assignment came from one, then its complexity pool, then `default_crew`; `crew_selection.source` names the hold. With none left, the local drain defers the task and `orbit run readiness --json` reports `reason: "provider_backoff"` with the release time. A follower's pull is not given a held task it would run as an excluded crew (its own crew, or the follower's default for a task naming none); the pull receipt's `deferred_conflicts` names the hold. An unavailable hold placed on another host excludes from that follower only the crews it carries from an earlier standing capacity, limit or refusal hold (`carried_crews`); those stay excluded on every machine. A hold that excludes nothing (the failed run resolved no crew) still defers until `not_before`. An explicit run-input `crew` ignores the hold. A usage-limit hold on an explicit crew (`crew_source` `explicit`, or a legacy pin) follows [`workflow.provider_limit_explicit_crews`](#provider-usage-limits): under `wait` the task waits with `provider_backoff` instead of being redirected; under `pool` it draws as above. The hold ends at `not_before` or any later status change, and the next run resumes the committed candidate.

**Host outages.** A local run that fails `[provider_unavailable]` also records each crew it excluded in the global store (`~/.orbit/orbit.db`, table `provider_crew_outages`), one row per provider and crew, until the hold's own backoff ends; a longer outage of the crew is kept. Until then this host's local admission draws no task onto that crew, not only the held one, and whatever became of the held task: a task whose own crew, complexity pool or default leaves another crew draws it, and the rest wait with `provider_backoff`. A crew of the same name on another provider is not covered. The record is advisory: a failure to record it logs a warning, and the task is still held. Followers and their pull drains are not affected; their own crew window handles their own logins. Operator steps: [stuck job runs](runbooks/stuck-job-runs.md#a-task-held-after-a-provider-failure).

**Routines.** The task-pilot routine honours a host provider failure hold. It reads recent runs on this host — the failures `orbit run readiness --json` projects as `provider_auth_failures`, plus `provider_capacity` from held runs and older failed runs — over a one-hour window of finish time (falling back to start time, then creation time), and does not write a new hold record. A recently finished failure remains in that window even when its pipeline was created more than an hour ago. Clearing successes use the same finish-time window. While a `provider_unavailable` failure from the last 30 minutes, or a `provider_capacity` failure from the last 15 minutes, covers every crew the pilot would draw (the inspection-capable members of `workflow.purpose_crews.task_pilot` when it has one, else of `workflow.low_complexity_crews`, or of the next tier up when none can inspect, else `workflow.default_crew`, after the usage-limit narrowing), the routine withholds the dispatch. The withhold reason starts with `provider_hold:` and names the provider or crew, when the backoff ends, the failing run, and the crews skipped. It does not start `task_pilot_pipeline`. Unavailability covers every crew of that provider account on this host, because a failed login is the account's: the crew the failure names as its `account`, else the provider's crews without `env`. Capacity covers the crew the run ran as, or every crew of the failing step's provider when that step did not run as that crew. The routine and the readiness auth-failure card share crew attribution: `task_pilot_pipeline` uses its recorded task-pilot draw (or, for a run admitted before purpose pools, its `workflow.low_complexity_crews` draw), `blocked_task_recovery_pipeline` uses its `workflow.purpose_crews.final_recovery` draw, and a `finding_verification_pipeline` run recorded before that job was removed [ORB-22844] uses its prepared verifier partitions, including escalations. These jobs' `resolved_crew` names the tasks' crews and does not identify the agents they dispatched. A missing draw counts as no known crew. A verifier failure counts for the partition whose recorded error the failure message carries; a verifier success counts only partitions with a recorded result. Other jobs use `resolved_crew`. A pilot's provider failure is recorded on the run itself: a failed pilot fan-out prefers a provider-classified worker message over an ordinary failure message, preserving the provider marker even when an earlier worker failed on the work. The backoff is the base in the table above, measured from the latest uncleared failure's finish time; it does not double. A successful run of that provider account on this host that started after the failure, or a pull-drain auth probe that records `recovered_at` for that account on this host, clears unavailability before the backoff ends; a success that started before the failure does not, even when it finishes after it, the same rule `provider_auth_failures` uses. Capacity clears when such a success is of the held crew, or of any of the provider's crews when the hold covers the provider. A probe does not clear capacity. `provider_limit` and `provider_refusal` do not withhold the routine. When any drawable crew remains, the routine still dispatches and the draw skips the held crews, naming the hold in the draw's source. The next tick after the hold lifts dispatches again with no operator action, once the routine's own debounce has passed. Another host's failure does not hold this one. Step-failure and PR conflict recovery share that draw, so they skip a held crew the same way; the routine withhold itself is only the task-pilot routine.

Readiness and host purpose-draw holds also read trusted `provider.invocation.observed` audit events. The CLI runner records the actual provider, crew account and invocation interval independently of the ordinary step and final-recovery decision. An authentication failure during step or final recovery therefore holds that account even when the primary error is unrelated, the parent later succeeds or ends held, or the parent is still running. The primary diagnostic remains intact. Each projection reads at most 200 recent invocation outcomes in addition to its existing run pages; a busy host can push older evidence past those bounds. Unavailability and capacity keep the same backoffs and probe rules. Only a successful invocation or run of the same provider account on the same host that started strictly after the failing invocation ended clears it; a successful primary invocation does not stand in for its recovery provider. Historical invocation ledgers and free-text recovery decisions without trusted outcome events remain unknown; Orbit does not infer authentication from agent prose or an exit code alone.

**The host's limit record.** A usage-limit failure also records an observation in the global store (`~/.orbit/orbit.db`, table `provider_limit_observations`): one row per provider, model and window with the reset, run, crew and provider text. A run of a crew that sets [`env`](#per-crew-credentials-env) records its observations under that crew account instead (table `provider_account_limit_observations`, one row per provider, account, model and window), so they never replace or gate the default login's. A newer observation replaces the row, an older one never does. Recording is advisory: a failure to record logs a warning, and the run is still typed and held. After every Codex, Claude, Muse, Grok or Antigravity run, successful or not, the host also records the provider's own reading of each window (source `event`): percent used (can exceed 100), window length when known and reset.

- Codex: the last `token_count` with `rate_limits` in the run's own thread rollout (`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*-<thread_id>.jsonl`, `CODEX_HOME` from `[execution.env]`, else `~/.codex`), reading at most the last 1 MiB. `primary` and `secondary` windows are account-wide.
- Claude: the last stdout `rate_limit_event` (Orbit runs `claude --output-format stream-json --verbose`): each `unifiedWindows` entry (`five_hour`, `seven_day`, …) plus the currently limiting window if absent there. `seven_day_opus` and `seven_day_sonnet` carry their model; an overage window is recorded with `gating` off.
- Muse: the last stdout `status` (Muse Code 1.4.12) or `run.usage` (Muse Code 1.4.13) record with `payload.kind = "run_usage"` and an allowance object. Each recognized `allowance.window` / `allowance.weekly` with a nonnegative numeric `used_percent` and valid integer `resets_at_ms` yields an account-wide reading, timestamped at run completion. Labels are preserved and window lengths remain unknown.
- Grok: after the run, the host reads `https://cli-chat-proxy.grok.com/v1/billing?format=credits` with the existing first-party CLI login in `$GROK_HOME/auth.json` (else `$HOME/.grok/auth.json`), using the child's state root. Grok CLI 1.0.67 reports `config.creditUsagePercent` and `config.currentPeriod` (`type`, `start`, `end`): the recognized weekly period yields an account-wide `weekly` reading, its duration in minutes and its end as the reset. Headless `usage` and `grok usage` contain invocation/session token and cost totals, not quota windows. This read needs `curl` on the child's `PATH`, adds no secret to the agent environment, follows no redirects and has a three-second request timeout. Missing or ambiguous credentials, credential/endpoint environment overrides, unavailable transport and unrecognized responses record nothing and never fail the run. Such accounts can still use [`provider_limit_budgets`](#budgets-for-providers-that-report-no-usage).
- Antigravity: after every run, the host invokes the same resolved `agy` launcher with `--print /usage --output-format json --print-timeout 3s`, using the child's environment and working directory. Verified with agy 1.4.6: the native command's top-level `command.name = "usage"` / `command.data.groups[].buckets[]` reports `remaining_fraction`, `window` (`5h` or `weekly`) and RFC 3339 `reset_time`. Orbit records percent used, 300 or 10080 minutes, and the reported reset, preserving the bucket ID as the window label. Shared buckets are scoped to Gemini, or Flash/Pro when a separate Argon bucket exists; third-party buckets are split into Claude/GPT scopes, and Argon has its own scope. It never reads assistant `response` text or treats token accounting as quota. The query runs host-side so agy can use its existing login, including the macOS login keychain, without adding a secret to the worker environment. A five-second deadline and 64 KiB output bound keep it advisory: unavailable commands, failed reads and unknown shapes record nothing and never fail the run. Older versions without structured quota data can use [`provider_limit_budgets`](#budgets-for-providers-that-report-no-usage).
- A missing rollout, another thread's rollout or an unrecognized reading records nothing. Other providers report no usage window.

**Pull drains and claimed leaves.** An authentication failure releases the claim and excludes every crew of that provider account (aliases included): the leaf's crew alone when it sets [`env`](#per-crew-credentials-env), else every crew of the provider without `env`; a declared [`auth_probe`](#executor-authentication-recovery-probes) can re-admit them in the same window. A capacity failure excludes only the leaf's crew and its `@<effort>` references, since another model may have room, and only for a backoff: 10 minutes from when the leaf ended, doubling with each further capacity failure of that crew in the window up to an hour. The exclusion carries `until` and the crew is runnable again in the same window once it passes; while it holds the owner's before-PR or before-landing review crew, the drain requests nothing and its `Requests held:` line names the review's timing and when the backoff lifts. A usage limit releases the claim with failure class `provider` and `provider_limit: true`, which does not count against the task's release budget and excludes no crew itself; the follower's own limit record excludes the provider's crews until the reading lapses.

### Provider usage limits

Limits are read and applied per provider account on this host: the provider's default login, which every crew of it without [`env`](#per-crew-credentials-env) shares, and each crew that sets `env`, which is an account of its own.

A hold learns of a limit only after a run fails on it. Delivery admission also reads this host's [limit record](#provider-failure-holds) before dispatch and stops routing to a crew whose provider account, as logged in here, is at or near a window's limit.

```toml
[workflow]
provider_limit_overrides = ["claude:80", "grok:100", "codex:off"]
provider_limit_budgets = ["grok:30usd/5h"]  # none by default
provider_limit_explicit_crews = "wait"      # or "pool"
```

**Limited crews.** A crew is limited while a live, gating reading of its provider account is exhausted or at or above that account's threshold (its `provider_limit_overrides` entry, else the fixed 90%). A reading that names a model (Claude's `seven_day_opus`) limits only crews whose model contains it, case-insensitively; any other reading limits every crew of the account (aliases included). A reading belongs to the account it was read from: one taken under the default login, such as Claude's `five_hour` and `seven_day` subscription windows, never limits a crew that sets `env`, and one taken by such a crew limits only it. A crew that sets `env` and has no reading is gated only by its own budget, if it has one. Its threshold is its own override (`opus-credits:95` or `opus-credits:off`), else its provider's, else 90%. An override naming a crew that is not defined, or that sets no `env`, fails load naming it. At `100` only an exhausted window gates (99% gates nothing). An overage window (`gating` off) never gates.

**Gate off.** `provider:off` (for example `codex:off`) turns that provider's pre-dispatch gate off. Every live reading is still recorded and shown, with `threshold` `off` and `gated` false. No reading of that provider — an event reading, a usage-limit failure observation, or a ledger budget reading — limits a crew at admission, in a drain or ship draw, or in the pull window. `orbit doctor`'s `provider-limits:<provider>` row says gating is off, and the dashboard marks the reading the same way. A run that then fails on a usage limit still records the task's provider failure hold: `off` disables only the pre-dispatch gate. Removing the entry gates the same readings again at 90%. `off` is not a percent, so it does not change [`provider_limit_budgets`](#budgets-for-providers-that-report-no-usage) arithmetic.

**Lapse.** A reading counts until its `resets_at`. Without one it counts for its window's length (60 minutes when that is unknown), or, for an exhausted error reading, for the first usage-limit backoff, 30 minutes. The crew is eligible again at the next admission pass, with no provider probe.

**Gated draws.** Only delivery admission (the crew draw of drains and `orbit run ship`, and the pull window); system, review, final-recovery and task-pilot crews are not gated. The complexity-pool draw of the task pilot, step-failure and PR conflict recovery still skips crews at a provider limit while another member remains, but never waits or redirects the way the gate does. It also skips crews under a host provider failure hold for unavailability or capacity, and that hold alone can withhold the task-pilot routine ([Routines](#provider-failure-holds) above).

- **Purpose tasks** (`crew_source` `purpose:<purpose>`): a limited assigned crew is redrawn from the unlimited enabled positive-weight members of that purpose pool. Their weights and the run's allowlist are kept, and admission writes the crew it runs back to the task as `redraw:provider_limit`. With none left, the draw continues as a pool task does.
- **Pool and default tasks** (`crew_source` `pool:<tier>`, `type:<type>/<tier>` or `default`, or no crew): limited members are removed and the rest renormalized, as with `--allow-crew`. A limited assigned crew is redrawn from the unlimited members of the current complexity pool; a `type:` crew is redrawn first from its type pool's other unlimited members, as [type pools](#type-pools-by-task-type-and-complexity) describes. Admission writes a changed crew back to the task as `redraw:provider_limit`, except that a weighted [usage-pressure re-draw](#usage-pressure-draws) keeps the task's `crew_source`.
- **No default fallback.** With every member limited the task waits; filtering never falls through to `workflow.default_crew`, usually the most expensive crew and often on the same provider.
- **Explicit crews** (`crew_source` `explicit`, or a legacy pin): `wait` (default) keeps the task in the backlog; `pool` draws from the unlimited members of its complexity pool.
- **Explicit run crews** (`--input crew=<name>` on `orbit run job`) are the operator's decision and not gated; `crew_selection.provider_limit` records the limit.

A redrawn task's `crew_selection.source` names the limit. A task left with no unlimited crew waits with `reason: "provider_limit"` in `orbit run readiness --json` and the detail `<provider> <window> at <used>% (limit <threshold>%) until <reset>; crews <list> skipped`, lifting by itself after the reset. An `orbit run ship` naming such a task still runs it on its usual draw. A standing [provider failure hold](#provider-failure-holds) that leaves no crew reports `provider_backoff` first. Operator steps: [stuck job runs](runbooks/stuck-job-runs.md#a-task-waiting-on-a-provider-usage-limit).

**Pull drains.** Each pass of a follower's pull drain re-reads its own store and excludes each limited crew from its crew window with source `provider_limit` and an `until` time; unlike other exclusions, it lifts at `until` within the same drain. The owner never hands the follower a task on that crew meanwhile. The owner's own provider readings apply only to owner-local work; neither the owner's admission answer nor a settlement supplies readings to the follower's window. Each provider-limit exclusion names the host whose local store supplied it. The owner's before-PR reviewer is not gated.

**Surfaces** all share admission's view of live readings (liveness, threshold, crew rules), so none shows a crew runnable while admission skips it.

- `orbit run readiness`: a `Provider limits:` line per gated reading (`claude five_hour 93% >= 90% until 15:00Z: opus, sonnet skipped`, `claude/opus-credits 30d budget …` for a crew account; a reset on another UTC day includes its date) and each waiting task's detail. `--json` has a top-level `provider_limits` array of every live reading: `provider`, `account` (present only for a crew account's reading), `scope` (the model a reading names, else `null`), `window`, `used_percent`, `exhausted`, `resets_at`, `source`, `observed_at`, `age_seconds` (never negative), `stale` (true when `observed_at` is an earlier UTC date than the snapshot), `gating`, `partial`, `threshold` (a percent, or the string `off` when that provider's gate is off), `gated` (false for every reading of a provider set to `off`), `until` and `crews` (enabled crews covered). The additive `provider_pressures` array lists every enabled crew account (`provider`, optional `account`, `reports_usage`, `threshold`, `gated`, `crews`, `pressure`, `pressure_window`, and `shares_default_login` when that env account shares another enabled crew's credential and no crew of the provider uses the default login); `crew_pressures` lists every enabled `crew`, `pressure` and `pressure_window`. Text shows `Provider pressure:` and `Crew pressure:` lines. `pressure_window` names the binding `window`, model `scope`, `used_percent` and `resets_at`, or is `null` for neutral unknown usage. `provider_auth_failures` lists each host and provider with a `provider_unavailable` authentication failure in the last 7 days (at most 200 recent failures and 200 recent successes) that no successful run of that provider on that host, started after the failure, has cleared. Each item is `host`, `provider`, `account` (present only for a crew that sets `env`, which is grouped and cleared on its own), `runs` (`run_id`, `at`, oldest first) and `fix_command` (the executor's auth-probe hint, else the provider's re-login command; for a crew account, the variables its `env` passes, by name). Crew attribution uses the same recorded draws and verifier partitions as the routine's [provider failure hold](#provider-failure-holds), so a successful pilot or recovery on another provider cannot clear the card. A failure or success outside that window is not part of the projection.
- `orbit run show <pull-drain>`: `excluded <crew> (provider_limit until <time>): <reading>; reading from host <machine-id>` (`unregistered` when the runtime has no machine identity).
- `orbit doctor`: one `provider-limits:<provider>` row per provider an enabled crew without `env` uses, and one `provider-limits:<provider>/<crew>` row per enabled crew that sets `env` (a provider with no usage windows and no budget, which is every provider but Codex, Claude, Muse and Grok, shows `info`; a budgeted one shows its `ledger` reading, such as `grok 5h budget 67%`; a provider set to `off` says gating is off and lists its readings), plus a `provider-limits:workflow.purpose_crews.review` warning when that ungated lane uses a gated provider (and a `provider-limits:workflow.purpose_crews.pre_landing_review` warning for that pool while it is set). Each account row appends its raw weekly pressure and binding window/reset (or neutral unknown usage). Row semantics: [health checks](runbooks/health-checks.md#run-orbit-doctor). `orbit doctor providers` shows the same per executor (`LIMITS` column, `provider_limits` in `--json`).
- Dashboard: the provider strip shows one chip per provider login. A crew that sets its own credential is its own chip. Crews that set the same credential, when none of that provider's crews uses the default login, are marked `shares_default_login` and merged into the provider's chip; that chip's pressure is the default login's weekly pressure, including weekly readings stored on those crews, never another login's. A login with no reading stays its own chip, reading `no usage reported`. The strip shows that pressure beside the readings, with the binding window and reset in its accessible title. Narrow bars show a summary login's pressure with every login and window in the title. The Settings › Crews Limit column (tightest window, used percent, reset, `gated` badge, or `gating off` when the provider's gate is off); the Drain card names gated readings, lists every live reading with its age (`today` on the observation's UTC date, otherwise how long ago) and a stale marker when `stale` is set (a reading whose gate is off is marked `gating off`), lists each standing `provider_auth_failures` item with its host, provider, run links and fix command, and lists tasks waiting with `provider_limit`.

**Weekly usage pressure** is shown raw on these surfaces; it changes draws only under [`workflow.crew_selection = "usage_pressure"`](#usage-pressure-draws). For live provider-reported gating windows of exactly 10080 minutes, pressure is `max(0, 1 - used_percent / 100) / clamp(time_to_reset / 7 days, ε, 1)`, shown uncapped (for example `claude ×25.2 (seven_day 10%, resets 18:00Z)` with six hours left). Five-hour and overage windows do not count. Account pressure uses account-wide weekly readings; crew pressure takes the minimum across those and weekly model windows covering its model. Crews with `env` use only their own account. With no valid weekly reading, a missing/past reset, an error reading or a ledger budget, pressure is neutral `1`. A neutral unknown does not constrain other valid weekly windows. Muse's `weekly` window is normalized to 10080 minutes when parsed.

#### Usage-pressure draws

```toml
[workflow]
crew_selection = "usage_pressure"   # default "static"
```

With `workflow.crew_selection = "usage_pressure"`, crew pool draws spend weekly capacity that is about to reset before capacity that is not: a Claude week 10% used with six hours left (pressure about 25) wins over a fresh Codex week (pressure 1).

- **Effective weight.** Each pool member draws on `weight × clamp(pressure, 1/8, 8)`, where pressure is its crew's [weekly usage pressure](#provider-usage-limits) on this host. Tickets are whole numbers, ×1000 and rounded, so a weight-1 member at the `1/8` floor keeps 125 tickets; a positive weight never rounds to zero, and a weight of `0` stays undrawable. The multiplier applies after provider-hold and usage-limit narrowing. Task admission applies it before the run's `--allow-crew` list; other draws apply it after their existing membership filters. In each case, pool membership is unchanged, and a crew at its limit is still skipped only by the 90% threshold or its `provider_limit_overrides` entry. A draw with a single candidate has nothing to weigh.
- **Draws it covers.** Task creation, `task update --crew ""` and the re-draw after a complexity change (each a fresh draw from the purpose, type or complexity pool, recorded under that pool's source), delivery admission, and a pull-drain follower's claimed leaf: a leaf whose snapshot crew came from a `pool:`, `purpose:` or `default` source is re-drawn inside that source's pool, as the follower captures it, while a `type:` crew stays the owner's (see the admission re-draw below), with the follower's own readings and under the follower's hold, limit and `--allow-crew` narrowing. The owner's readings never weigh a follower's run, and a follower never rewrites the owner's task; the leaf simply runs on the crew its own host drew. Activity pools (including task pilots and final recovery), review pools, finding-verification pools (initial and escalation rounds) use the same multiplier after their existing membership filters.
- **Admission re-draw.** At admission, a task whose crew the system chose (`crew_source` `pool:<tier>`, `purpose:<purpose>`, `type:<type>/<tier>` or `default`) is drawn again from that source's whole pool instead of keeping its assigned member, under the same OS, hold, usage-limit and allowlist narrowing. A claimed leaf on a follower keeps the owner's type-pool crew, because its snapshot carries no tags or context files to classify the task by. When the weighted draw picks another crew, the task record takes the new crew and keeps its `crew_source` unchanged, and its history gains a `crew_redrawn` entry naming both crews and their pressures (``crew redrawn from pool:hard (`sol`) to pool:hard (`opus`) at admission under usage pressure: `sol` ×1 (…), `opus` ×25.2 (…)``). An explicit crew (`crew_source` `explicit`, a legacy pin, or an explicit run-input `crew`) is never re-drawn.
- **Provenance.** A weighted admission's `crew_selection.source` ends in `; usage pressure`, and each `eligible_pool` member gains `pressure` (raw) and `effective_weight` (the weight it drew on) beside its configured `weight`. Activity draws freeze these fields in `activity_crew_draws`; admitted reviewers record them in `reviewer.eligible_pool`. Retries reuse frozen activity records, including their original weights. Singleton draws keep configured weights without pressure fields.
- **Pull offer order.** A follower under `usage_pressure` also reports each crew's weekly pressure in its pull capability (recomputed each pass; absent under `static`, and an absent report reads as neutral `1`). The owner orders that follower's offers by it only below band, priority, expiry and census reach/affinity: of tasks tied on all of those, the one whose pool contains the follower's highest-pressure crew is offered first. Pressure never overrides a higher priority or an expiring task, never gates admission, and owner-local dispatch, which has no pressure input, orders exactly as before.

Under `static`, or with the keys unset, every seeded draw uses its original configured weights and consumes the same random tickets; recorded members omit pressure fields. The pressure is this host's reading; a drain admitting on this host weighs its own readings.

#### Budgets for providers that report no usage

Gemini doesn't report how close an account is to its limit; Orbit learns it only from the failure. Antigravity's structured `/usage` command and Grok's CLI billing API provide live windows when available; accounts or CLI versions without an available reading can use the same fallback. `provider_limit_budgets` declares the operator's allowance per rolling window so Orbit gates the provider before it runs out.

- **Format.** `provider:<amount><unit>/<window>`: a positive amount in `usd` or whole `tokens`, a window of `<n>h` or `<n>d` up to 31 days, each provider once (`xai` is stored as `grok`). No defaults. Load and `orbit config set` reject a malformed entry.
- **Crew accounts.** An entry may name a crew that sets [`env`](#per-crew-credentials-env) instead of a provider (`opus-credits:200usd/30d`): it reads only that crew's own spend and gates only that crew, and a provider's budget reads only its default login's spend, the invocations of its crews without `env`. A name that is a provider is the provider; an entry naming a crew that is not defined, or that sets no `env`, fails load naming it.
- **Reading** (source `ledger`, account-wide): this host's spend on the provider account in the trailing window ÷ the amount, gated at the account's threshold. `usd` sums each invocation's reported cost; `tokens` sums input plus output tokens from the invocation ledger. Its reset is the earliest moment enough old spend ages out to drop below the threshold (at `100`, below the budget). `provider:off` does not change this arithmetic: the reset stays on the fixed 90%, because `off` is not a percent. The reading is still shown, with `threshold` `off` and `gated` false, and it does not limit crews.
- **Partial.** A `usd` reading where some invocation recorded no cost is `partial` (`"partial": true`; surfaces note `partial: some invocations report no cost`), and its spend is a lower bound.
- **Precedence.** A live provider-reported reading or limit failure for the provider account wins; the ledger applies again when it lapses. The ledger reading is computed on each read, never stored.
- **Attribution.** An invocation counts for the provider it ran on, and for the crew account it ran under (the invocation ledger's `account`, shown with each invocation record) when its crew sets `env`. Older invocations are attributed by agent name, so an earlier Antigravity run, recorded under its model's family, does not count toward an `antigravity` budget, and every invocation recorded before accounts were named is the default login's.
- **This host only.** Interactive use and other hosts on the same login are invisible. Set the budget below the account's allowance and keep the provider's own failure as the backstop.

### Final recovery pool

`workflow.purpose_crews.final_recovery` is the final-recovery [purpose pool](#purpose-crew-pools): the weighted pool the `final_recovery` activity draws one crew from, once per run, when a task fails after step-failure recovery is exhausted; that crew evaluates the failure and attempts a final typed decision and fix.

```toml
[workflow.purpose_crews]
final_recovery = ["sol:100", "opus:20"]
```

- **Unset:** `["sol:100", "opus:20"]`, filtered to crews defined in `[crews.*]`. With neither defined the pool is empty and final recovery is off, without a validation error.
- **Explicit:** standard pool grammar (`crew[@effort]` or `crew[@effort]:weight`); every named crew must exist in `[crews.*]`.
- **`[]`** disables final recovery.

Final recovery keeps task-write tools closed. A repair that will `resume` can
return an optional `repair_evidence` object beside its typed decision, containing `execution_summary`
(up to 65536 characters) and `task_artifacts` (`[{path, source_path}]`, up to
32 files, each at most 1 MiB). Sources must be regular files under the assigned
worktree's `.orbit/tmp/`; names must be unique and cannot overwrite reserved
artifacts, including `review-*`, `validation/` and `no-diff-handoff/`. The host
validates the entire batch before writing its summary and artifacts to the
admitted task. On a claimed run it sends the bytes through the authenticated
owner route. The handoff accepts no task selector or lifecycle edits; invalid
evidence refuses the decision and leaves the candidate for inspection. The
blocked-task backstop excludes `resume` and does not accept this handoff.

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

When the resumed phase revisits a published candidate, Orbit authenticates
the exact host commit, branch, preparation and synchronization checkpoints.
An interruption after a replay rebase can continue before the new head is
pushed: the old push and PR outputs still describe the previous publication.
The setup pin remains immutable, and ancestry alone cannot authorize a changed
HEAD. Missing or inconsistent provenance refuses the candidate. Resuming the
phase reruns candidate validation, configured review and handoff evidence;
the existing pull request is reused. Republication is leased to the exact
previously published head, including when the repeated sync has no rebase
left to perform. A changed or deleted remote branch refuses that push.

A transient read-only PR or repository observation failure gets one
host-authenticated completion retry within the original completion deadline.
The host checks the unchanged publication, validation receipts and captured
review certificate first; the retried completion reads PR state again and
checks the published head and base before merging. The recovery rationale
records `transient-observation`. Review provenance changes and the retired
capture-only after-landing timing do not add review obligations. New
validation captures bind non-review inputs separately so review-only config
drift can be checked against the candidate's captured obligations; older
captures still require their original full policy digest to match.

A changed review obligation (timing, reviewer pool, budget, baseline commands or
host evidence) refuses completion reuse and records `policy-changed`. The
existing bounded requeue preserves the branch and PR for a fresh run under
current policy, with fresh validation and review. Changed candidate/base
bindings, mutation or rewrite provenance, and missing or stale evidence
retain their refusals, recorded as `candidate-changed`. Conflicts, red checks
and permission failures keep their existing recovery path; a lost mutation
reply never grants this read-only retry.

A historical supported completion-repair grant is a separate case: it may have
renewed a deadline without reaching a repair agent. Fresh operator source
repair can acknowledge that stopped lineage when the host's unique grant
record, complete parent chain, receipt origin, step audits and unchanged
publication prove no source or mutation effects. Plain descendants must retain
the granted deadline and marker. This admission creates a repair-only attempt
under current validation and review policy; it preserves the old records and
inherits no completion, merge or automatic-fix authority. See
[repairing a kept inline publication](runbooks/stuck-job-runs.md#repair-a-kept-inline-publication-after-completion-expired).

**Escalated blocks.** A final-recovery `requeue` is bounded to two per task
in 24 hours; past that the decision is refused and the task stays blocked.
The bound has no configuration key. Before it escalates, final recovery runs
bounded mechanical steps (resume the run, rerun CI, requeue), and a
deterministic recheck settles a block whose pull request merged outside Orbit,
whose base check went green or whose infrastructure cleared. What remains waits
in `blocked` behind a decision card (a summary, two or three options with
consequences, a recommendation) that an operator answers; the answer applies the
option and moves the task out of `blocked` in one operation. The seeded
`blocked_task_triage` routine and its `block_escalated` trigger are retired: a
`blocked-task-triage-*` routine file loads inert and `orbit workspace sync`
retires a seeded copy. See
[final recovery](../plugin/skills/orbit-setup/references/automation.md#built-in-final-recovery-of-blocked-tasks).

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

An `--auto-fix` drain also repairs Orbit pull requests that no run is watching
any more. When a task's newest delivery leaf ended `failed`, `timeout` or
`interrupted` and the task waits in `review`, or was blocked or requeued by that
leaf's own delivery or final recovery, each pass reads the task's recorded pull
request. If it conflicts with its base or a required check failed on its head,
the pass resumes the leaf at the last completion round it reached, which pins
the head the leaf last published, with `auto_fix` granted. The
watcher there admits the repair against the same attempt ledger and budget (three
per pull request, one per head and blocker), and the auto-fix crew takes the
task's footprint locks before it edits, pushes to the same branch and leaves
completion to merge. The resume keeps the leaf's own `completion`, so a repair
never merges a pull request whose leaf lacked `--complete`. Operator holds,
`no-auto-approve` tasks, a leaf started with `--complete` under a drain without
it, and pull requests not opened by a delivery identity on the task's
`orbit/<task>-` branch are never repaired. Each hand-back is recorded in task
history as `pr_auto_fix_out_of_run`; a head and blocker is handed back once, and
a pull request at most as many times as its budget. `orbit run readiness`
lists live repairs and the latest drain pass's skipped candidates under
`out_of_run_auto_fix` (`state: "skipped"`, with the PR, reason, detail and
observation time). Blockers are null when a refusal preceded the forge read.
`orbit run show <drain-run> --step adopt_orphans --json` exposes the persisted
pass in `step_output`, including `auto_fix.skipped`. See
[a conflicting or red pull request no run is watching](runbooks/stuck-job-runs.md#a-conflicting-or-red-pull-request-no-run-is-watching).

The dashboard's **Fix PR** action on a `review` task starts the same repair for
that one task without a drain (`POST /api/tasks/<task>/auto-fix`). It runs the
drain pass's checks and start: the same attempt ledger and budget, footprint
locks and `pr_auto_fix_out_of_run` record. It needs an operator session, governed
as `auto_drain.auto_fix`. It grants repair only: the resumed leaf keeps its own
`completion`, and a leaf started with `--complete` is resumed only when the
session also holds completion authority (`auto_drain.complete`). The button
shows only when the task records a pull request. It is enabled only when that
pull request conflicts with its base or a required check failed on its head;
otherwise it is disabled with the reason in its title. A refused start answers
`409` with code `task_auto_fix_<reason>`, using the drain's reasons plus the
single-task ones; nothing starts. Unlike a drain pass, Fix PR also hands
back a delivery that succeeded without `--complete` and left its pull request
in review, as a dashboard ship or `orbit run ship` does. Such a delivery's own
run ends at once, so a detached `task_pr_watch_pipeline` watches the pull
request's checks for up to an hour. It records one `landing_checks_observed`
history entry on the task (`state` `passed`, `failed`, `conflicting`,
`blocked` or `pending`; for `failed`, the failed checks and the owner landing's
`classification` of them). It never merges, reruns or repairs, and the task
detail shows the newest entry beside Fix PR. See the
[runbook](runbooks/stuck-job-runs.md#a-conflicting-or-red-pull-request-no-run-is-watching)
for the reasons.

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
  within the original completion deadline, even beside a failed required or
  optional check. Repair admission waits until all reported required checks
  conclude and includes every required failure from the latest policy read. A
  required check that current branch policy names and a freshly pushed head
  has not reported yet also waits; it is never treated as passed. An
  unclassified or optional pending check grants no wait. Review gates and
  unreadable or stale evidence still stop the watcher. Untouched-file or
  flaky attribution does not delay auto-fix: unless the known-red-base path
  applies, a source failure goes to repair on its first completion poll,
  without an unchanged-head rerun or eligible-set authorization. Suspicion
  alone never records `pr_auto_fix_base_red`. Every mergeable head is checked
  against its latest head-bound required-check results before merge. A red
  required result stays blocking regardless of attribution; only a later
  passing run on that same head or on the repaired head clears it. Skipped or
  neutral reruns do not clear a required check that failed on that head, and
  neither does a skip when the head's check history is unreadable. A required
  check that concluded skipped or neutral without ever failing on the head (a
  docs-only change) is satisfied, as branch protection reads it. A failed required check
  whose job log shows a code failure is compared with the current base
  before a repair is admitted. Orbit reads the latest push run of the same
  workflow on the exact base tip. When that run skipped the job (a docs-only
  or change-scoped commit) or lists no such job, it reads back along the base
  branch's first-parent history, up to eight commits including the tip, to the
  newest commit whose same job ran to success or failure, and judges the
  failure there. If every failed required job succeeds on the base and the PR
  is behind, it refreshes the branch onto the current tip, which contains
  those green commits, through the existing pinned rebase, validation and
  lease-checked publication path, then watches checks on the new head. This
  spends no agent repair attempt. A reviewed head returns for independent
  re-review before publication. A matching failure on the base is known red:
  the watcher records it and waits within the completion deadline, spending no
  repair attempt. A failure the base already failed the same way at the head's
  fork point also waits, without spending a repair attempt, while the moved
  tip cannot prove it fixed: its same job is still pending, skipped or not
  listed, no commit within the bound ran it, or its jobs cannot be read. Each
  poll evaluates it again, so the wait ends once the tip's job concludes. Once
  a head is recorded that way (`pr_auto_fix_base_red`), a later poll that
  cannot read the base evidence again keeps waiting instead of repairing. A
  distinct candidate failure keeps the ordinary repair path and three-attempt
  budget. A conflict is rebased onto the current base by PR conflict recovery.
  If a published check repair then conflicts, recovery follows the contiguous
  auto-fix pushes recorded by the host for that PR and this run or its
  authenticated retry ancestors. The stopped rebase's branch and original head
  must both match that repaired head; its target base remains pinned. Missing,
  unrelated or disconnected push records cannot authorize a different head.
- CI rerun-planner refusals are retained on the owning task in
  `ci-rerun/<execution>/diagnostics.json`, including the observed PR, head,
  run and job identities, planner stage and a redacted reason header capped
  at 1 KiB. These are observations, not authenticated mutation authority.
  The single replaceable artifact keeps the first refusal and the latest 15
  transitions, with an omitted-transition count; identical consecutive reads
  add no history. A later successful planner read clears the current refusal.
  A changed head clears it before any completion hold, while earlier heads
  remain historical observations. Completion retains its typed outcome beside
  this journal and includes a current refusal in the stop diagnostic, including
  when the original completion budget expires. A diagnostic write failure is
  reported in a warning and never grants recovery permission.
- A rerun-planner hold in owner-local `auto_fix=true` completion is bounded.
  The owning task records `pr_auto_fix_ci_rerun_hold` with the head and
  reason when a hold begins on a head. After 15 minutes on that head, or on
  the completion's last poll, the hold expires: `pr_auto_fix_ci_rerun_hold_expired`
  is recorded and the failed checks take the ordinary repair path, whose
  known-red-base and evidence gates still admit a repair or stop with a
  reason. A dependency-only record holds only when no other job failed: a
  failed sibling the logs cannot tie to a changed file (for example a
  guardrail check) is still a candidate failure and goes to repair.
- Final recovery after `complete_pr` re-reads required checks on the current
  head and receives each failed check's name and diagnostic evidence in
  `required_check_failures`, independently of the earlier step error. The
  exception is an untouched-path failure whose base evidence is still
  incomplete: no passing same-check base run exists yet, or the base run passes
  but no same-signature `pull_request` rerun of the unchanged head is recorded.
  That wait withholds `required_check_failures` for every check, and the
  attempt requeues the published candidate instead of diagnosing it. Pending,
  unreadable or changing checks refuse a partial diagnosis without renewing
  the completion watch deadline.
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
- When a failed required check is a dependency waiter, auto-fix uses the
  authenticated workflow and job evidence to include bounded excerpts from
  the failed dependencies in the repair input. If the complete proof or a
  dependency log is unavailable, the repair retains the waiter excerpt.
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
  provenance. Before a local drain or follower pull admits more work, the owner
  reconciles recovery-only selectors against stopped local PR deliveries and
  their observed candidates.
  A selector is released only after an explicit candidate discard or a verified
  candidate/publication no longer changes that path. Original selectors stay;
  live runs, unsettled claims, kept claim candidates, still-published repairs,
  uncommitted repairs that were not discarded, and unreadable or incomplete
  outcomes keep their protection.
  Read-only readiness reports the last durable declaration; it does not perform
  this cleanup. Each cleanup records its released selectors and candidate
  observations in task history. Missing or stale records, unexplained files,
  protected metadata, environment files, symlinks and CI workflow
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
- A base refresh retains the failed and passing job identities, the base
  commit each passing job ran on and the exact base tip in
  `auto-fix/<run>/landed-<failed-head>.json` and records
  `pr_auto_fix_base_refresh` in task history before mutation. Its owner-side
  reservation bounds base refreshes independently of the repair-attempt budget.
  Old fork-point failure logs are unnecessary: the base's latest push jobs,
  read back from the tip past commits that skipped the job, supply the
  evidence. A green base alone does not prove the base fixed that one failure:
  when the head's fork point, read back the same way, passed the same job, the
  failure is the candidate's. That check does not by itself
  end the comparison. When every failed required check is candidate-owned
  that way, completion spends no base refresh and admits one ordinary repair,
  even when the pull request's one base update is already reserved. The same
  repair path holds when one check is candidate-owned and another is green
  on the base without a successful fork-point job, as long as no required
  check is known-red on the base. A known-red sibling wins over a
  candidate-owned check: the base fails that sibling with the same diagnostic,
  completion records `pr_auto_fix_base_red`, and no repair attempt is
  admitted. Other matching current-base failures also record
  `pr_auto_fix_base_red`. Ordinary agent repairs retain their existing
  admission history. Missing refresh or known-red evidence leaves the
  inherited-failure wait and the ordinary diagnosis path in
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
- A task's effective OS requirement is its `os:` tags intersected with its assigned crew's executor `host_os`. Local admission, `orbit run ship` discovery, the drain-start warning and owner pull admission all read that one intersection, so they cannot disagree. An empty intersection (an `os:linux` task assigned a crew whose executor declares `host_os: [macos]`, for example) is refused when the task is created or updated. The reason names the crew, the executor constraint and the task's tags, and nothing is written: the task never becomes one that no host can run.
- **Operator readout.** Task show (`Runs on:` and JSON `os_requirement`), task list and dashboard task details derive the same tags/stored-crew intersection without writing task fields or adding OS tags. JSON includes `any_of`, `describe`, `source` (OS `tags`, assigned `crew`, and constrained `executor` when present) and `state`. Unknown or disabled crews are `unresolved`; disjoint constraints are `conflict` and admit no host. `orbit task list --runs-on linux|macos|windows` (MCP `runs_on`) filters before pagination, includes unrestricted tasks, and excludes unresolved/conflicting requirements. With neither tags nor a constrained stored crew the JSON key is omitted. For an unset crew this is a tags-only readout: automatic pool/default routing remains undecided and can impose further OS constraints. The stored crew is used even when a past run records a different crew.
- An explicit crew this host cannot run stays assigned and waits, for example `waits for a macos host (crew <macos-only-crew>)`. It is not redrawn onto another crew. Admission draws and `workflow.default_crew` resolution skip a crew whose executor excludes this host; the other members keep their weights, and an excluded default is left unset rather than pinned. Creation-time complexity draws — task creation, `task update --crew ""`, and complexity re-rates — instead count a crew as runnable when any host serving this workspace can run it: the owner itself, plus each registered host (`~/.orbit/hosts.toml`, what `orbit host list` reports, including each host's `host_os`) whose recorded operating system the executor allows and which the owner's own claim records for this workspace show pulling here within the last seven days. Being registered is not enough: a host in the registry that serves only other workspaces — or none — never counts, and a follower that stops pulling ages out after the window and stops counting, though tasks already drawn keep their crew and wait. So a Linux owner whose Mac pull follower recently claimed work in this workspace can draw a macOS-only crew from a complexity pool; the task is stored with that crew and pool provenance, waits on the Linux owner under the same effective-OS intersection, and is claimed only by a macOS follower through owner pull admission. A crew no serving host can run is still skipped at creation, and a workspace with no serving followers draws exactly as a single host always has. Activity and purpose pools (recovery, review, pilot) run on the executing host and keep filtering by that host alone, as does every admission-time draw. A host's OS is read from it when `orbit host add` registers it (discovery reports `host_os`); an entry registered before that field existed records no OS and counts for no limited crew until it is removed and added again. If no supported purpose or complexity pool remains, local admission and the automatic draw withhold that task with a wait naming the default and the OS it needs; leaving the crew unset never permits the unsupported default to launch. Discovery for a named task uses that run's admitted crew selection, including an explicit choice retained by replay, instead of retrying the unsupported automatic default. This does not override the task's stored crew constraints or its `os:` tags. Activity default fallbacks apply the same host check, including native-inspection activities. Provider preflight stays a backstop for a missing binary or a failed login on a host the executor does allow.
- Seeding copies a shipped `host_os` onto an installed default only when that install still omits the field. A value an operator set is kept. Clearing the field restores the shipped declaration on the next seed. Custom executors outside the shipped set are not rewritten.
- `orbit doctor providers` reports `unsupported on this OS` when `host_os` excludes this host, including when the binary is absent, so an operator is not told to install it here. `not installed` is only for an executor this host may run whose command is missing. The full `orbit doctor` and the dashboard's Doctor panel agree: a routed crew whose executor excludes this host is `skipped` with the same `unsupported on this OS` message and no remediation.

## Capability labels: `machine.labels` and `requires_labels`

A machine declares the capabilities it has as opaque labels, and a task names the ones it needs:

```toml
# ~/.orbit/config.toml on the host with the GPU
[machine]
labels = ["gpu", "cuda-12"]
```

```sh
orbit task add --title "Train the model" --requires-labels gpu,linux ...
orbit task update <id> --requires-labels ""   # clears the requirement
```

- A host carries its `machine.labels` plus its OS as an implicit label (`linux`, `macos` or `windows`). `orbit doctor` prints the effective set in its `host-labels` row. Orbit gives no other label a meaning, and does not detect labels.
- Labels are trimmed and lowercased, and may use ASCII letters, digits, `-`, `_`, `.`, `:` and `/`, up to 64 bytes. A setting or task field with a blank or invalid label is refused.
- A task's `requires_labels` (MCP `orbit.task.add` and `orbit.task.update`, CLI `--requires-labels`) is all-of: a host may start the task only when it carries every label. `os:` tags keep their any-of meaning, and both apply: a task tagged `os:macos` that requires `xcode` runs only on a macOS host labelled `xcode`.
- **Write-time check.** Add and update refuse a requirement that no registered serving host carries together with the task's `os:` tags, naming the labels no host carries and every known label set, and write nothing. The known hosts are this host and every executor whose census row in this workspace was declared within the last seven days, which a pull-drain follower sends with each pull. A follower is known only once it has pulled from this workspace.
- **Admission.** Owner-local drains, `orbit run ship` discovery and readiness withhold a task this host lacks a label for, with reason `host_labels_missing` and a detail naming the labels it lacks. An explicit ship refuses it. Owner pull admission checks the labels the follower declares in its pull request, and the follower's idle pass records the same reason. A task that is already running is not moved.
- **Grants pin ownership.** Any non-empty `granted_tools` restricts execution to the task's owner host, including grants for tools outside coordination. Pull admission withholds the task from every follower and names the granted tools. Required labels still apply to the owner: if it lacks a label, admission and placement waits name both the grant and the label gap, even while a labelled follower is pulling. Labels never move ownership or override the grant. An operator resolves the conflict by dropping the grant, dropping a required label, or moving the task's ownership. Grants do not extend the closed claimed-owner tool bridge.
- **Named wait.** While no live host carries the labels (this host, or a follower whose census row is live), task show (`Placement:`, JSON `placement_wait`), task list and `orbit run readiness` name the wait, for example ``waits for a host labelled `gpu` (requires_labels): no registered serving host carries them``. The task stays in `backlog`.
- **Crew draws.** A creation-time complexity draw for a labelled task narrows the task's OS requirement to the OSes of the known hosts that carry its labels, so it does not draw a crew whose executor `host_os` excludes every such host.
- **Mixed versions.** The pull request's `labels` field is part of the distributed-drain protocol fingerprint, so a follower and owner on releases with and without label support refuse each other's admission as protocol skew rather than ignoring the field. Upgrade the hosts in lockstep.

## Executor authentication recovery probes

An executor may declare `spec.auth_probe`. Only a pull drain's **auth-excluded** providers run it: never at drain start, for healthy crews, after the window closes, or for capacity or refusal exclusions. The first attempt is due ten minutes after the failed leaf; failures wait twenty, then thirty minutes (the cap). Exclusions and probes are per provider account: a crew that sets [`env`](#per-crew-credentials-env) is excluded, probed and re-admitted alone, and the provider's default login covers its crews without `env`. A pass durably acknowledges those auth failures and re-admits that account's crews within the window; it never clears a preflight, capacity, crew or host exclusion. Without a probe the exclusion lasts the whole window: re-login and start a new drain. Owner-local drains use timed, task-specific [provider holds](#provider-failure-holds) instead. Drain-side view: [distributed drain runbook](runbooks/distributed-drain.md).

Only the shipped **claude** executor declares one: a minimal `claude -p --model haiku` call with built-in and MCP tools disabled ([CLI contract](https://code.claude.com/docs/en/cli-reference)), a one-line prompt on stdin, and success on exit zero plus `ORBIT_AUTH_OK` on stdout. Each attempt costs one model request. `claude auth status` is not used: cached status can lag a real 401 and may refresh shared OAuth credentials. Codex, Gemini, Antigravity, Grok, Copilot, and Muse declare none (their minimal model-call and success contracts are not established); local-shell has no provider authentication.

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
- A probe gets the leaf runner's cleared environment plus `[execution.env].pass` (with a crew account's own [`env`](#per-crew-credentials-env), so it checks the credential that crew's leaves used), provider-required and fixed entries, sandbox resolution and provider carve-outs (the macOS login keychain included), bounded process-tree supervision, and no Orbit activity tool grants. Without explicit `allow_fallback`, a sandbox refusal keeps the exclusion and backoff.

`orbit doctor` (`provider-auth:<provider>`, or `provider-auth:<provider>/<crew>` for a crew account), `orbit run show <drain>` and the dashboard drain panel show provider account, host, exclusion time, auth class, re-login hint, credential origin and next probe time; doctor reads the record and calls no provider. Credential origin names a passed `CLAUDE_CODE_OAUTH_TOKEN` (or `ANTHROPIC_API_KEY`) by name only, else the macOS keychain or provider cache: the configured route, not a keychain read or proof of which credential the CLI used. To avoid sharing a rotating Desktop login, pass a dedicated `claude setup-token` credential as `CLAUDE_CODE_OAUTH_TOKEN`.

## Executor inspection tools (`inspection_args`)

An executor may declare `spec.inspection_args`: the arguments appended to its argv for a pinned-source inspection run (task pilot, escalation triage), so the CLI's own file-read and shell tools work unattended in the inspection checkout.

```yaml
spec:
  inspection_args: [--config, features.shell_tool=true, --config, features.unified_exec=true]
```

- An empty list means the executor's static `args` already enable those tools. The shipped **claude** executor declares `[]` (`--tools default` enables Read and Bash) and the shipped **codex** executor declares the shell and unified-exec features above. No other shipped executor declares the field.
- Omitting the field means the executor has no known inspection surface. Inspection draws skip its crews (see [System work draws from the complexity pools](#system-work-draws-from-the-complexity-pools)), and an explicit or run crew that names it fails before launch with the non-retryable `InspectionToolsUnavailable` error, which names the executor and this field. Declare the arguments once the CLI's native tools are known to work non-interactively under the `reviewer` profile.
- An installed definition that omits the field takes the shipped default executor's declaration of the same name, so existing `codex` and `claude` installs inspect without reseeding. A value set on the installed definition wins.
- Read-only is enforced by the `reviewer` filesystem profile, not by these arguments.

## Executor image input (`image_input`)

An executor may declare `spec.image_input`: how image files reach its model. Images never travel in prompt text; they are staged as files in the run's scratch directory.

```yaml
spec:
  image_input:
    mode: attach_args          # or native_read
    formats: [png, jpeg, webp] # png is required: it is the canary's format
    max_images: 40             # canary included; at least 2
    attach_args: ["--image", "{path}"]   # attach_args only: argv per image
```

- `native_read`: the CLI's own file-read tool presents image files to the model. The prompt lists the staged paths and tells the agent to read each one.
- `attach_args`: the images are attached at launch, `attach_args` appended once per image with `{path}` replaced by the staged file. Only scratch paths reach argv, never content.
- A definition with an invalid declaration fails to load with `invalid spec.image_input: …`: an unknown key, no `png` in `formats`, a repeated format, `max_images` below 2, `attach_args` with `native_read`, `attach_args` mode without `attach_args`, or a template without `{path}`.
- Omitting the field means the executor cannot take images. An activity whose input asks for images (`image_input`) on such a crew, or on a crew that sets [`vision = false`](#crewsname--which-provider-model-runs-the-task), fails before launch with the non-retryable `image_input_unavailable` error, which names the executor and `spec.image_input`.
- An installed definition that omits the field takes the shipped default executor's declaration of the same name, as `inspection_args` does. A value set on the installed definition wins.
- Shipped declarations: **claude** `native_read` (PNG, JPEG, WebP, GIF), **codex** `attach_args` with `--image {path}` (PNG, JPEG, WebP) and **muse** `attach_args` with `--image {path}` (PNG). Each is declared only because its lane passed the canary probe below. The gemini, antigravity, grok and copilot executors declare none.
- Only regular files are staged. A requested image that is a FIFO, socket, directory, device or another non-regular file — including a symlink that resolves to one — is refused before launch with a `stage image …` error that names the path. When the source opens, the refusal reports `not a regular file (found <kind>)`; when opening the source itself fails — as it does for a Unix-domain socket on Linux, refused with its OS error such as `No such device or address (os error 6)` — the staging error retains that open error instead. The source is opened without blocking, so a named pipe with no writer cannot stall preparation before provider supervision starts.

**The canary.** Every image-input invocation is first given a canary: a generated PNG of a random six-character code, created per invocation in scratch and never written to a task. The agent must return the code as `result.canary`. A delivery result without it, or with another code, is refused as `[image_not_seen]`, a retryable step failure. The image-input probe records the same miss for that crew and the probe run still completes, so one crew does not fail the probe batch. Each check emits an `activity.image_canary_checked` audit event with the crew, mode and `seen`. The canary proves delivery, not judgement.

**Visual hold.** A crew whose newest two or more canary checks all missed is reported as held until 30 minutes after its last miss, doubling with each further consecutive miss up to 24 hours, like a [provider failure hold](#provider-failure-holds). Its next passing check clears it. The hold is read from the audit events, so it covers probes and deliveries alike.

**Probing.** `orbit agents vision-probe` runs the canary for every enabled crew outside any delivery (one `image_input_probe_pipeline` run each) and prints one row per crew: executor, model, mode, result and any visual hold. Results are `seen`, `image_not_seen`, `no_image_input`, `vision_off`, `no_cli_runtime`, `failed` or `timeout`. A crew that finishes without the code is `image_not_seen` on its own row, and that probe run still completes. Each row uses only that run’s canary check. If a settled run’s check cannot be found, its row is `failed` with an explanatory detail, even if the run succeeded; another run’s check cannot supply the result. `--crew <name>` (repeatable) probes only those crews, and fails before launching anything when a named crew cannot take images. `--timeout-seconds` bounds the wait (default 900). Declare `image_input` on an executor only after its probe passes, and set `vision = false` on a crew whose model keeps returning `image_not_seen` on a lane that passes for others.

## `[execution.env]` — the agent subprocess environment

Every agent subprocess (bare, Bubblewrap or `sandbox-exec`) starts from a **cleared** environment and receives only:

| Group | Contents |
|---|---|
| Baseline | `HOME`, `LANG`, `LC_ALL`, `LOGNAME`, `PATH`, `SHELL`, `TERM`, `TMPDIR`, `TZ`, `USER`. `PATH` starts with any [`workflow.validation_env.path`](#agent-sessions-share-the-configured-path) entries. |
| `pass` | Names listed in `execution.env.pass`. Default: `HOME`, `PATH`, `CODEX_HOME`, `TMPDIR`, `USER`, plus `__CF_USER_TEXT_ENCODING` on macOS. |
| Provider extras | Variables the selected provider runtime declares it needs. |
| Orbit envelope | Named `ORBIT_*` variables: run, task and session identity (`ORBIT_RUN_ID`, `ORBIT_TASK_ID`, `ORBIT_SESSION_ID`, …), locators (`ORBIT_WORKSPACE`, `ORBIT_WORKTREE_ROOT`, `ORBIT_BIN`, `ORBIT_REGISTRY_ROOT`, …) and activity bindings (`ORBIT_ACTIVITY_*`, `ORBIT_STEP_INDEX`, …). Privilege-bearing names (`ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN`) are not admitted; an inherited `ORBIT_ROOT` is removed. |
| Crew `env` | The global `[crews.<name>] env` entries of the crew the agent runs as, applied last. Each entry's name takes its source's value from the launching process and replaces the same name from `pass`. Other crews never receive it. An unset source leaves the name out for that crew. See [per-crew credentials](#per-crew-credentials-env). |

This is an allowlist, not a secret filter: a benignly named credential such as `DATABASE_URL` never reaches an agent unless you name it in `pass` or in a crew's `env`. Agents keep network access, so this is the boundary against accidental exfiltration.

```toml
[execution.env]
pass = ["HOME", "PATH", "CODEX_HOME", "TMPDIR", "USER", "GITHUB_TOKEN"]
```

- `pass` replaces the default rather than extending it; restate the baseline names you want.
- `pass` is shared by every crew. A crew's [`env`](#per-crew-credentials-env) entry adds a name for that crew alone and replaces the same name from `pass` for it.
- A listed name that is unset is absent, not empty. Names must be valid identifiers.
- `pass` is a [security key](#where-config-lives): a workspace file that omits it gets the built-in default, not the global value.
- Inheriting the full environment is not configurable; a stale `execution.env.inherit` is ignored. The internal inherit seam still removes privilege-bearing `ORBIT_*` names (`ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN`, and any other `ORBIT_*` name outside the execution envelope) before a child starts.

**Unset names.** A `pass` name the launching process lacks reaches no agent, and the provider falls back to another login (a missing `CLAUDE_CODE_OAUTH_TOKEN` falls back to the desktop login). Start drains from a login shell, or export the variable in the service's environment. A drain or ship start (`orbit run auto`, `--pull` included, and `orbit run ship`; MCP `orbit.workflow.auto` start and `orbit.workflow.ship`; the dashboard) reports them in its `warning` (a `Warning:` line in CLI text), and `orbit run job` warns once on stderr, for each unset or empty name you added; all of them record the names on the run (`Unset env:` in `orbit run show`, `run.env_pass_unset` in JSON, the dashboard run detail). Built-in defaults are not reported (`CODEX_HOME` and macOS's `__CF_USER_TEXT_ENCODING` can legitimately be absent). `orbit doctor`'s `env-pass` row checks its own environment for `pass` names and also checks [crew sources](#per-crew-credentials-env) against installed managed services. These are warnings only; the start proceeds and no value is printed.

**Worker scope.** The same admitted names are given to a detached pipeline worker. On Linux, containment forwards them with name-only `systemd-run --setenv=NAME`, so the values stay in the child environment and never appear in `systemd-run` or worker argv. An explicit worker edit, such as a toolchain `PATH` or removing `ORBIT_ROOT`, wins over the admitted snapshot. See [worker containment](#worker-containment).

**Clock-started runs.** launchd (macOS) or a systemd user timer (Linux) starts `orbit clock tick` with no login environment. The tick reads `~/.orbit/clock.env` (created comment-only, mode `0600`, by `orbit routine init --install-clock`) and gives each discovered workspace's runs the names that workspace's effective `pass` admits, and the sources its crews' [`env`](#per-crew-credentials-env) entries read, as child-environment defaults; the tick's own process environment is never changed. A non-empty value already in the tick's environment wins, and an empty one can be filled from the file. A dry run loads nothing. Unit files never contain a secret. A `clock.env` that is a symlink, owned by another user, or group- or world-readable is refused as a load error (the tick still runs). `orbit clock tick --json` reports loaded names as `clock_env_loaded`, never values. Sandboxed agents can't read `clock.env` itself ([SECURITY.md](../SECURITY.md#sandbox-model-and-known-limits)).

**Claude on macOS.** A Claude activity with neither `CLAUDE_CODE_OAUTH_TOKEN` nor `ANTHROPIC_API_KEY` in its environment is refused before `claude` starts, rather than fall back to the Claude Desktop login, which the Desktop revokes mid-run. The check reads the crew's composed environment, so a crew whose [`env`](#per-crew-credentials-env) maps its own `ANTHROPIC_API_KEY` runs without the shared token. A drain submitted on such a host (local or `--pull`) excludes the Claude crews that would be refused from its crew window instead, so it claims only work it can run, and its submission names the remediation once; an explicit `--allow-crew` that names only such crews is refused. `orbit doctor`'s `claude-worker-token` row warns ahead of time, naming the `pass` list or `clock.env` that lacks the token, or a crew credential source `clock.env` lacks; a `clock.env` token under a name `pass` omits counts as lacking.

---

## Other sections

The remaining keys, grouped as `orbit config show` groups them.

### Execution

| Key | Default | What it does |
|---|---|---|
| `execution.codex.sandbox` | `workspace-write` | Codex sandbox mode: `read-only`, `workspace-write` or `danger-full-access`. Under `workspace-write`, Codex's extra writable roots are Orbit's runtime stores, never the whole workspace `.orbit` or `~/.orbit`. `orbit init` seeds `danger-full-access` globally. [Security key](#where-config-lives). |
| `execution.codex.approval_policy` | unset | **Advanced.** `untrusted`, `on-request` or `never`. Security key. |
| `execution.env.pass` | `HOME`, `PATH`, `CODEX_HOME`, `TMPDIR`, `USER` (+ `__CF_USER_TEXT_ENCODING` on macOS) | Environment names passed into every crew's agent subprocesses; a crew's [`env`](#per-crew-credentials-env) adds names for that crew alone. Security key. See [`[execution.env]`](#executionenv--the-agent-subprocess-environment). |

One `proc.spawn` call inside a managed activity may use a timeout of at most 45 minutes, also capped at the activity's remaining wall-clock budget (passed to the agent as `ORBIT_ACTIVITY_DEADLINE_UNIX_MS`); outside an activity the ceiling is 60 seconds, and the result reports `timeout_ceiling_ms` and its source. A provider process that produces no stdout or stderr and uses no CPU for 30 minutes is ended and its step fails as stalled, so the usual retry and recovery path runs; the wall-clock timeout still applies. Both are fixed [ORB-22808]: `execution.proc_spawn_max_timeout_minutes` and `execution.agent_stall_minutes` are [deprecated](#validation-and-errors).

### `[review]` and `[operation]` — automatic review

Both tables resolve built-in → global → workspace. Unknown keys warn and are ignored; known values still validate at load. Design: [review gate](design/review-gate/2_design.md); operations: [review-gate runbook](runbooks/review-gate.md).

| Key | Default | What it does |
|---|---|---|
| `review.timing` | `off` | The one review layer before landing. `off` (recommended default) and `before_landing` (recommended when a pre-landing review is wanted) are the supported values. `before_pr` is deprecated but still holds PR creation for a fresh reviewer that fixes what it finds; an explicit `before_pr` keeps before-PR semantics with a bounded `review.timing` deprecation warning naming the file. `before_landing` opens the PR first, reviews its published head while hosted CI runs, and merges only the head the review settled; a reviewer fix is validated and pushed onto the PR under a lease, and any outcome but approve leaves the PR open and unmerged, with the task in `review` and a typed reason (see [before-landing review](design/review-gate/2_design.md#32-before-landing-review-of-the-open-pr-orb-14849)). PR route only. Captured at submission, so a run in flight keeps its value. New setups use `off` or `before_landing`; new run requests use `--review before-landing`. |
| `review.before_landing_hosts` | `[]` | Owner policy for the distributed drain: machine ids (`hm_…`) whose claimed leaves review their open PR before it lands, while this owner's own deliveries do not. See the per-host note below the table. |
| `review.minutes` | `30` | Wall-clock limit for one candidate's review, fix commit and final validation included (1..=1440). Retries and resumes share the minutes; the reviewer is stopped when they run out, and a spent review is not restarted. A base replay stopped at its review deadline is inconclusive and is not cached [ORB-15350], so raising `review.minutes` allows a retry to complete without being suppressed by an earlier cutoff. A changed candidate (after a completion rebase, say) is a new review. |
| `review.baseline_commands` | `[]` | Commands settlement may rerun on the host, besides `workflow.required_validation_commands`, to confirm a [reviewer's red-base claim](#a-red-base-is-held-not-blocked). A listed command is binding: it needs a passing record or a claim settlement reproduces, or the review settles incomplete. Captured when the delivery or claim is admitted. With the required commands, also the gates an implementer may hand off when its sandbox deferred only one kind's tests (Bubblewrap, Seatbelt, or `ps` on a macOS claimed leaf) ([required validation](#required-validation)). |
| `[[review.host_evidence]]` | `[]` | Checks a claimed leaf owes for the paths it changed; see [owed host evidence](#owed-host-evidence). A workspace list replaces the global one. |
| `workflow.purpose_crews.review` | unset | Crew for automatic review: review tasks the after-landing `delivery-code-review` auto-task mints (unset: that definition's template crew; set, it replaces that crew), and the reviewer `review.timing` runs while `workflow.purpose_crews.pre_landing_review` is unset. One crew name, or a weighted pool. See the review crew pool note below the table. |
| `workflow.purpose_crews.pre_landing_review` | unset | Crew for before-PR and before-landing review only, including the reviewer a claim captures for a `review.before_landing_hosts` machine. Unset or `[]`: `workflow.purpose_crews.review`. Written like it. See the review crew pool note below the table. |


**Review timing compatibility.** Readers also accept `none`, `after_landing` and `after-landing` as `off`, `before-pr` as `before_pr`, and `before-landing` as `before_landing`. The retired after-landing timing captures no run review; after-landing review is controlled by the `delivery-code-review` auto-task. Config writes, including doctor repairs and unrelated edits, persist only `off`, `before_pr` or `before_landing`, which the previous release also reads. Existing canonical values keep their spelling. During a mixed-version deployment, new writers therefore keep the config readable by older CLI and MCP processes. Normalize any already-written aliases with a new build before older readers use that file; editing aliases by hand while older readers remain active is unsupported. Alias normalization preserves the resolved policy and its source, so published-run policy digests remain unchanged and admissions need not be drained for this spelling-only rewrite. `review.timing = before_pr` (and `before-pr`) is deprecated but accepted with a bounded warning naming `review.timing`, the written value, and the file; the effective value and provenance stay truthful and before-PR behavior is unchanged. Deprecation alone never fails `orbit doctor` and never conceals a genuine configuration or gate error.

**Deprecated review keys.** `review.before_pr` and `review.before_landing` load into `review.timing` with a deprecation warning: `before_pr = true` is `before_pr`, `before_landing = true` is `before_landing`, both off is `off`. A workspace that sets one of them keeps the other from the global file, as it did before. Load fails, naming the keys, while they would both be on, since there is one review layer before landing. `operation.review_crew` loads into `workflow.purpose_crews.review`. A file that also sets the replacement keeps the replacement. Runs, claims and pipeline state recorded with the old switches keep their captured review: the captured admission and ship contract still carry `before_pr` and `before_landing` flags.

**Deprecated `before_pr` timing and its intentional migration [ORB-20099].** `review.timing = before_pr` (and `before-pr`) is deprecated; new setups use `off` or `before_landing` and new run requests use `--review before-landing`. An explicit `before_pr` still resolves to before-PR with its deciding layer/source, a bounded warning on load (`review.timing`, the written value, the file) and at submission (`review.timing` with the `global`/`workspace` source, or `--review before-pr` with the `run-option` source), and unchanged admission, publication order, reviewer, and budget. Migrate intentionally with the existing scoped audited writer: `orbit config set review.timing before_landing` for the workspace (add `--global` only when the global layer actually decides the timing; use explicit owner/workspace targeting for other stores). Show before/after effective policy and source (`orbit config show`); the migration preserves minutes, crew pools, validation requirements, host evidence, and unrelated settings, and distinguishes a workspace override from editing the global setting it masks. Ordinary writes, spelling normalization (`before-pr` to `before_pr`), default refresh, and doctor retired-key repair never silently change the timing nor materialize a masked global timing into another layer. Deferred removal of accepted `before_pr` input and internal before-PR handlers is a later separately scoped decision (consumed by ORB-20087 only after this deprecation lands).

**Per-host before-landing review.** `review.before_landing_hosts` is read only by the owner, and only for claimed leaves. A leaf on a listed machine reviews its PR before it lands, as `review.timing = before_landing` makes every leaf do; the owner's own deliveries and unlisted machines are unaffected. Entries are trimmed and de-duplicated. Each must be an `hm_` machine id, with letters, digits, `_` or `-` after the prefix; any other entry fails load, naming its index. The list is redundant while `review.timing` is `before_landing`, and loading fails while it is `before_pr` and the list is non-empty. The owner resolves it for the caller's machine when a follower probes or pulls, and the claim captures the result, so a follower that echoes its probed contract matches. The machine label is caller-chosen, so this is policy, not a security boundary. Protocol revision is unchanged; the contract is in the [`orbit.task.pull` spec](design/distributed-drain/specs/task-pull.md).

**Review crew pool.** `workflow.purpose_crews.review` is one crew name (`"sol"`) or a pool (`["sol", "grok"]`, or `["sol:3", "grok:1"]`), written like the [complexity pools](#weighting-a-pool): all bare or all weighted, and at least one member above weight `0`. Each review draws one crew by weight after three filters, each applied only when some member survives it: a member that is disabled or undefined here drops out; a member whose provider is at a usage limit here drops out; a member that implemented the reviewed work drops out (a distributed claim has no implementer, so it skips this filter). If no member survives the first filter, the review refuses to start with that member's refusal. A distributed owner draws one crew per claim, when it answers the probe. The review tasks `delivery-code-review` mints discard members with weight `0`, then apply a hard filter before those three: a member whose executor declares a `host_os` that shares no OS with the definition's `os:` tags drops out, since no host could run that task, and a single configured crew is never replaced by another. The draw is seeded by the batch, so a retried mint carries the same crew while the pool and tags are unchanged. When the filter leaves no member, the consumer holds the batch instead of failing every tick: see the after-landing note below. `[]` unsets it. Load refuses an empty name, a mix of bare and weighted entries, a weight that is not a non-negative whole number, a crew the registry does not define, a weighted pool that names a crew twice, and a pool whose weights are all `0`.

**Pre-landing review pool.** `workflow.purpose_crews.pre_landing_review` is written, admitted and drawn exactly like `workflow.purpose_crews.review`, and splits the two layers: before-PR and before-landing review (local deliveries, and the review contract an owner captures for claims) draw from it, while the review tasks `delivery-code-review` mints keep drawing from `workflow.purpose_crews.review`. So a host can review before landing on cheap, fast crews (`pre_landing_review = ["luna", "haiku"]`) and keep a stronger after-landing pool. While it is unset, or a later layer sets it to `[]`, the reviewer before landing falls back to `workflow.purpose_crews.review`, so a config without it behaves as before. A review admission records the layer of whichever key supplied its crew as `crew_source`. Before-PR and before-landing review refuse to start while neither key names a crew, and the refusal names both keys. `orbit config show --all` prints the effective pool on the before-PR and before-landing lines, with the key and layer it came from (and `workflow.purpose_crews.pre_landing_review is unset` when it is the fallback); `--json` carries it as `review.before_pr.crews`, `crew_source` and `crew_key`, and `review.after_landing.crews` and `crew_source` are the after-landing pool. The dashboard's Config tab shows both pools on the review-layer steps.

**After-landing review** is the `delivery-code-review` auto-task's `enabled` flag (`orbit auto-task toggle delivery-code-review on|off`), not a config key. `orbit config show --all` (and the `review` object in `--json`), `orbit doctor`'s `review` check, the dashboard Config tab and `orbit.drain.probe` report all three switches with their source (minutes for the first two, the next batch due for after-landing). The default text view already prints the `review.*` rows and omits that summary. The `review` check names the commit observed against `origin/<branch>` and fails once the oldest unobserved first-parent commit has waited the batch's `max_wait_minutes`, even if the tracking tip is recent. Doctor and auto-task inspection read the existing tracking ref without fetching (the local branch when there is no origin; a missing tracking ref leaves verification unknown). Delivery evaluation fetches, and defers on fetch failure without spending a coverage retry. A batch counts as reviewed only on [per-delivery evidence](design/automation-triggers/5_operations.md#evidence-submission) whose examined and skipped paths match each delivery's frozen diff, from a review task whose agent persisted its own execution summary; otherwise the batch stays owed with a typed reason. A landing on a base that moved after review is covered too, labelled `rebased_clean`, when the landed PR's head is the reviewed candidate and a conflict-free merge of that change onto the new base reproduces the landed tree exactly ([review gate §7](design/review-gate/2_design.md#7-delivery-coverage)). Any other base change, a head pushed after review, or a conflicting base stays an ordinary obligation.

The review tasks it mints carry `workflow.purpose_crews.review` when that key is set, whatever crew the definition names; `orbit auto-task update delivery-code-review --crew <crew>` stores the crew but prints a warning that the pool replaces it, and `orbit auto-task show` prints the same note (`crew_overridden` in `--json`). The definition's crew takes effect while the key is unset. If no positive-weight member of the pool can run under the definition's `os:` tags (each excluded member's executor declares a `host_os` outside them), the claimed batch is held and the consumer records one `crew_os_ineligible` stall instead of an error per tick. The stall names the definition, the pool key, each excluded crew with its executor and `host_os`, and the tags: `orbit auto-task show delivery-code-review` prints it (`automation.state.stall.crew_hold` in `--json`), and `orbit doctor`, `orbit config show` and the dashboard Config tab report it as an after-landing problem. Every tick re-reads the configuration; once the pool or the tags admit a positive-weight crew, the stall clears and that tick mints the held batch, with no operator recovery. A batch an earlier release left claimed with no action after its mint failed is minted the same way on the next tick.

**Tasks tagged `skip-review`.** A candidate whose tasks all carry `skip-review` and that changes only `workflow.light_delivery_paths` skips every timing: the gate admits no reviewer before the PR or before landing (`skip_review_light_delivery`), and the after-landing sampler excludes the landing. Any other changed path ignores the tag, and the task's history says why. See [light delivery](#light-delivery-skip-review-and-skip-validation).

**Local-only delivery.** Before-PR and before-landing review need a PR, so both are refused for local-only delivery ([captured timing](design/review-gate/2_design.md#2-captured-timing)). In a local-delivery workspace with either on, `orbit doctor`'s `review` check names the deciding layer (`review.timing = before_pr (global)` or `(workspace)`) and the remedy (ship through the PR route, or set `review.timing = off` for local delivery); readiness reports otherwise eligible local tasks as `eligible: false` with `reason` `local_route_before_pr` or `local_route_before_landing` and the layer in `detail`; and the local drain withholds them before spawning delivery (earlier per-task exclusions keep their own reasons). A workspace `timing = "off"` clears the hold, but direct `task_local_pipeline` admission still fails closed whenever its captured policy enables before-PR review.

#### Owed host evidence

Each `[[review.host_evidence]]` rule has `kind` (`codeql` or `host_sandbox_test`), `name`, `paths` (workspace-relative globs), `os` (`linux` or `macos`), the exact `command`, and a result `artifact` (a relative `.json` path, unique across rules). A rule Orbit could never fulfil fails load.

- A `codeql` rule is owed on a host of another OS and fulfilled by the owner on `os`.
- A `linux` `codeql` rule may add `hosted_check`, the pull request check that runs exactly its `command` on every
  candidate, named as the forge reports it (`CodeQL / Analyze (rust)`, or the check's own name). On a
  before-landing review on a claim host of another known OS, such as macOS, the requirement is then
  owed to the owner (`owner_owed_evidence`) instead of holding the review: owner acceptance refuses a
  certificate that owes it any check its captured rules did not declare that way, and the owner's
  landing merges the pull request only after that check reports an explicit success on the reviewed
  head (a CheckRun `SUCCESS` or a StatusContext `SUCCESS`). It waits while the check is absent, still
  running, or completed without a success (`SKIPPED` or `NEUTRAL`). When the check failed, landing
  recovers or stops as for any failed pull request check: a failure inherited from the base gets a
  branch update, and the owner waits within the landing deadline for a newer passing base, up to three
  updates per handoff. A transient failure gets one shared rerun. A required
  check whose only unsuccessful jobs concluded `cancelled` or `timed_out` is
  transient from that conclusion even when the job log cannot be read. An
  aggregator whose log is only a `ci-await` of those conclusions, or only a
  `ci-await` timeout, counts the same way. A job that concluded `failure`
  keeps log classification, and a mixed failure is not reduced to transient.
  When that one rerun is already spent and the jobs are still cancelled or
  timed out, completion records `required CI jobs keep cancelling or timing out`
  with the job names and dispatches no repair agent. For an untouched-path failure, a
  successful same-check push run on the accepted base ancestry authorizes one rerun of the original
  `pull_request` run; this base run may predate the candidate failure, and a base pass alone does not
  attribute the failure to the candidate. Both owner completion with `auto_fix=true` and claimed
  landing use the shared bounded eligible-set planner. It authorizes the workflow-wide action only
  for a complete authenticated failed-job set of eligible roots and dependency-only waiters; an
  independent candidate failure vetoes that retry, and uncertain evidence holds. The generic
  versioned producer contract authenticates the workflow and the producer's complete event-base
  source checkout and the workspace-owned `isolated-source-v1` execution promise. The trusted
  invocation must exclude ambient imports, shell hooks, configuration and executable search, and
  re-establish the actual producer bytes after the last candidate operation; this workspace uses
  an absolute isolated interpreter, a cleared environment and a raw git byte check before launch
  anchored through hash-verified commit/tree objects from the immutable trusted base,
  excluding mutable refs, clean filters and replacement-object indirection; execution uses
  those verified bytes without reopening the helper path.
  Runner/OS and filesystem integrity remain prerequisites; this is no attestation against a
  compromised runner. Changed/legacy/spoofed sources, an overwritten helper or records grant no
  dependency-only authority. `ci_check_rerun_reserved` consumes the shared once-per-head budget
  before sending, and survives restart or an unknown reply without another request. `auto_fix=false`
  keeps its conservative manual-repeat hold. No new recovery setting weakens required checks.
  See the [eligible-set recovery contract](design/distributed-drain/2_design.md) and
  [operator recovery procedure](runbooks/distributed-drain.md).
  Orbit runs `gh run rerun <id> --failed`; GitHub retains the
  original event's `GITHUB_SHA` and `GITHUB_REF`, while Orbit CI's candidate-build jobs explicitly
  check out the PR head rather than a merge with the current base. A passing rerun clears the failure;
  the same signature failing again with the passing base result enters the one automatic repair for
  the candidate's own failure. If that retry's evidence cannot be read or authenticated for
  `max_base_ci_cycles` consecutive polls, a same-check base pass sends a flaky failure to repair
  (or requeue once spent). Candidate failures keep their candidate repair/requeue route, and
  inherited failures keep their base update/wait route. Other failure classes with unreadable
  retry evidence stop in review naming the read failure. If untouched-path evidence remains
  incomplete, landing holds the candidate in review as `delivery_base_checks_pending` after
  `max_base_ci_cycles` observation cycles (default 3) or `max_wait_seconds` (default 3600 seconds),
  whichever comes first (see the [distributed drain runbook](runbooks/distributed-drain.md)). Branch
  protection's own reading of a skipped or neutral required check is unchanged. Without
  `hosted_check` — a custom query, or a workspace with no hosted CodeQL — the requirement holds the
  review like any other owed check. Declare it only for a check that really runs the rule's command:
  Orbit compares the name, not the queries the job runs. Any other kind or OS with `hosted_check`, or
  an empty one, fails load.
- A `host_sandbox_test` rule is owed on a host of `os`, which runs it outside the agent sandbox at settlement, so its command must be one settlement admits. A held `linux` rule the leaf could not run is fulfilled by a Linux owner whose host can create namespaces.
- The owner captures the rules on each claim. The reviewer records an owed check `not_run` and never attempts it, and a verdict whose only gaps are owed checks holds for them instead of blocking.

Example: the [CodeQL runbook](runbooks/codeql-local.md#non-linux-hosts) rule (`kind = "codeql"`, `paths = ["**/*.rs"]`, `os = "linux"`, `command = "scripts/codeql-rust-local.sh …"`, `artifact = "evidence/codeql-rust-linux.json"`, and `hosted_check` only where a hosted job runs that command). Design: [owed evidence](design/review-gate/2_design.md#4-what-the-validation-records-establish-orb-11528-orb-11545).

#### Distributed PR delivery

At claim admission the owner captures `review.timing` as the contract's `before_pr` and `before_landing` flags and, when either is on, a review contract: its version, the reviewer crew from `workflow.purpose_crews.pre_landing_review`, else `workflow.purpose_crews.review` (for a pool, one crew drawn when the owner answers the probe, which the pull that echoes it keeps), `review.minutes`, `[[review.host_evidence]]`, `workflow.required_validation_commands` and `review.baseline_commands`. The follower runs that contract, never its own settings: it needs a matching build and protocol, a before-PR gate and the captured reviewer crew (else `before_pr_reviewer_unavailable`; the implementer's crew is never substituted), and its own `before_pr` is declared for diagnostics only. The handoff's typed evidence (reviewed head and base, reviewer, certificate) must satisfy the contract, with a passing required record per captured command and the captured `review.baseline_commands` unchanged; if the owner's current lists no longer match the snapshot, acceptance fails closed with fresh-claim guidance. Protocol revision 8 carries the list: explicit `[]` means no required checks, and a contract without the field is refused. A claim on a machine listed in `review.before_landing_hosts` captures `before_landing` on while the owner's own `review.timing` is not `before_landing`; `before_pr` stays off for it. Under `before_landing` the leaf reviews its own PR after `pr_open`, merges nothing, and hands off evidence for the head it settled; evidence of the other timing or another head is refused. After-landing review stays owner-side and does not refuse a pull. Wire contract and refusal codes: [`orbit.task.pull` spec](design/distributed-drain/specs/task-pull.md).

### `[pr]` — pull requests

| Key | Default | What it does |
|---|---|---|
| `pr.task_url_template` | unset | **Advanced.** Stable task URL template linking a task ID in PR descriptions (`{task_id}`). New pull requests use this URL or a plain task ID; historical PR and other external-reference URLs never select the link. An authenticated same-PR continuation may link its verified target when this is unset. |
| `pr.delivery_authors` | `[]` | **Advanced.** Forge logins (case-insensitive) whose PRs count as Orbit-authored, for closing them and for settling a task from its delivery PR merged outside Orbit ([stuck job runs](runbooks/stuck-job-runs.md#a-delivery-pull-request-merged-outside-orbit)). Empty: the login `gh` is authenticated as on this machine. Add a follower's login if it opens PRs this owner has not accepted as a handoff. |
| `pr.title_template` | unset | **Advanced.** Single-task PR title template ([delivery templates](#delivery-templates-pr-commit-and-branch)). Unset: the task title. |
| `pr.body_template` | unset | **Advanced.** `"repo"` or a repository-relative path, read at the delivery's base revision. Unset: Orbit's layout. |
| `pr.agent_sections` | `[]` | **Advanced.** Body headings the implementer may fill through the `pr-sections.json` task artifact. |
| `pr.required_sections` | `[]` | **Advanced.** Body headings the implementer must fill; a gap follows `pr.missing_required_section`. |
| `pr.missing_required_section` | `draft` | **Advanced.** `draft`: open the PR as a draft with a note under the heading. `refuse`: open no PR and fail `pr_open`. Both record a `pr_section_gap` history event. |
| `pr.labels` | `[]` | **Advanced.** Labels `gh pr create` applies to each PR Orbit opens. |
| `pr.task_tag_labels` | `[]` | **Advanced.** Allowlist of task tags mirrored as PR labels, matched exactly (case-sensitive). |
| `pr.task_type_labels` | `{}` | **Advanced.** Table mapping `feature`, `bug`, `refactor` or `chore` to a PR label name. Unknown types are refused. |
| `pr.reviewers` | `[]` | **Advanced.** Reviewers (logins or `org/team`) requested on each PR Orbit opens. |

Orbit closes a task's open Orbit-authored delivery and `[BLOCKED]` preservation PRs when it lands (`done`), is rejected or archived, when an operator or agent moves it back to `backlog` or `someday`, and when a newer delivery attempt opens its own PR. It closes a PR only if its head branch is one `branch.name_template` names for the task (`orbit/<TASK-ID>-…` by default), its body names the task, and it was opened by a `pr.delivery_authors` login or is the delivery PR of an accepted follower handoff. Human PRs and other tasks' PRs are never touched, and branches are never deleted. On `done` the landing PR stays open: any `#N` the done note names, or, when the note names none and the task leaves `review`, the PR it was reviewed through. The closing comment names the landing (the done note, such as `delivered by pull request #N merged as <sha>` or `already landed as <sha>`) or the state and reason. Starting a run or blocking a task closes nothing.

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

### `[commit]` and `[branch]` — commit messages and branch names

| Key | Default | What it does |
|---|---|---|
| `commit.subject_template` | unset | **Advanced.** Single-task commit subject template; must contain `[{{task_id}}]`. Unset: Orbit's subjects. |
| `commit.body_template` | unset | **Advanced.** Single-task commit body template, placed before the trailers. Unset: Orbit's bodies. |
| `branch.name_template` | `{{prefix}}/{{task_id}}-{{hash}}` | **Advanced.** Single-task delivery branch name; must contain `{{task_id}}` and `{{hash}}`. |

### Delivery templates: `[pr]`, `[commit]` and `[branch]`

With none of these keys set, titles, bodies, commit messages and branch names are byte-for-byte Orbit's own. Templates apply to single-task deliveries; a bundle of several tasks keeps Orbit's batch body, `[Bundle]` title, batch commit and `<prefix>/bundle-<hash>` branch.

Placeholders are `{{name}}`, with optional spaces inside the braces. An unknown or misplaced placeholder, or an unterminated `{{`, fails the configuration load naming the key and the placeholder. Text templates (`pr.title_template`, the PR body, `commit.*_template`) take `task_id`, `title`, `type` (`feat`/`fix`/`refactor`/`chore` from the task type), `description`, `acceptance_criteria` (a checklist), `execution_summary`, `validation`, `freshness` (base and head refs, behind and ahead counts; empty in commits), `task_url` (from `pr.task_url_template`, empty when unset) and `ext.<system>` (the task's external ref ID for that system, empty when it has none). Branch templates take `prefix` (the `worktree_setup` step's branch prefix, `orbit` by default), `task_id`, `hash` (eight hex characters of the run ID) and `ext.<system>`. PR titles and commit subjects collapse to one line.

**Body template lookup.** `pr.body_template = "repo"` reads the forge's PR template at the delivery's base revision: the first `pull_request_template.md` (any case) in `.github/`, the repository root, then `docs/`. With none there, Orbit's layout is used. Any other value is a repository-relative path (no absolute path, `\`, `:`, `.` or `..` component), which must exist at the base revision; a missing file fails `pr_open`. The template is never read from the candidate branch, so a task cannot rewrite the body it is published with. Placeholders in a file are checked when `pr_open` renders it: an unknown one fails that step (phase `automation.vcs.pr.body`) naming `pr.body_template` and the placeholder, and no PR opens.

**Task identity.** A single-task body rendered from a template always ends with a generated HTML comment, `<!-- orbit-delivery-task: <TASK-ID> -->`, after the template text and after the signature and orchestration trailer. The comment is Orbit's, not part of the template. Delivery readers still require a branch `branch.name_template` names for the task, that task id as a whole token in the body, and an Orbit author. The comment supplies the token when the template has no `{{task_id}}` placeholder, and it is kept when the body is cut to GitHub's size limit. Orbit's own layout already names the task and does not add the comment.

**Agent-filled sections.** When `pr.agent_sections` or `pr.required_sections` is set, `agent_implement` tells the implementer to write `{"sections": {"<heading>": "<markdown>"}}` to `.orbit/tmp/pr-sections.json` and attach it as the `pr-sections.json` task artifact (a claimed leaf lists it in `task_artifacts`). `pr_open` reads that artifact; a missing or malformed one counts as no filled sections, with a warning. Each filled section replaces the body under the template's Markdown heading of the same name (ATX headings, case-insensitive, fenced code skipped), up to the next heading of the same or higher level. With Orbit's layout, filled sections are appended as `## <heading>`. An unfilled agent section keeps the template's own text. An unfilled required section is never published as an empty heading: its body becomes a "Missing required section" note, and `pr.missing_required_section` drafts the PR or refuses it. Either way the task's history gets a `pr_section_gap` event naming the sections; its typed decision records the sections, the policy, the run and, when `pr_open` created the PR, its head branch and the SHA-256 of the exact body it was created with. A required section the template has no heading for is not checked. A retried `pr_open` records any remaining gap. If the reused PR is a draft whose live body is exactly the body a recorded section gap created it with and every required section is now filled, Orbit refreshes its body from the current artifacts and marks it ready for review. A body edited after that publication is no longer section-gap evidence, even when it still carries the missing-section note. A PR published before Orbit recorded the typed decision, with no typed publication for its head branch, is recognized by the missing-section note under a required heading outside fenced code. Configured drafts, drafts without section-gap evidence and drafts with remaining gaps keep their body and draft state. Labels and reviewers are preserved. The typed `pr_draft_recovery` history event records the ready transition or the reason for keeping a draft. A durable body-refresh reservation lets a failed ready request retry without mistaking the updated body for an unrelated draft; human body edits invalidate that reservation.

**Screenshots.** For a single-task delivery tagged `ui`, `pr_open` publishes the task's `screenshots/` artifacts and renders them as a `## Screenshots` table. This section is optional evidence for PR opening: no publishable screenshots produces the neutral note `No screenshots attached.` and never counts as a required-section gap. Name each `screenshots/<view>-<theme>-<width>[-before|-after].<ext>`: the width is the last number, the theme the token before it, and the view (which may hold hyphens) the rest, in ASCII letters, digits, `-`, `_` and `.`. A name without a suffix is an After shot; the Before shot is optional. Only PNG, JPEG, GIF and WebP files within the 1 MiB artifact limit are published; any other artifact under `screenshots/`, an unconventional name, or a second shot of the same view, theme, width and phase is skipped with a warning in the log.

Before the body is built, `pr_open` commits exactly the published files, at `screenshots/<file>`, to the task's permanent evidence ref `refs/orbit/evidence/<TASK-ID>` on `origin`, through the same private VCS boundary as the branch push. The ref sits outside `refs/heads`, so it is never listed or merged as a branch, and nothing deletes it: landing, cleanup and worktree GC leave it, so a merged PR keeps working images. A retried or resumed `pr_open` with unchanged artifacts reuses the ref's tip; changed artifacts chain one new commit onto it without force, so earlier commits stay reachable. The table has one row per view, width and theme (views in name order, wider viewports first; the theme is named only when the shots carry several), with Before and After columns. Each image is `<repository URL>/blob/<commit sha>/screenshots/<file>?raw=true`, pinned to the evidence commit's sha, never the ref name, and shown 420 pixels wide, or 200 for a viewport of 480 or less.

With Orbit's layout, the section follows the task contract, ahead of the execution summary, so truncation to GitHub's body limit cuts the summary first. A `pr.body_template` heading named `Screenshots` (matched as agent-filled headings are) is filled in place; a template without one gets the section appended after its text and before the task identity comment. When the section applies and no screenshot is publishable, it carries the neutral `No screenshots attached.` note; no gap is recorded, and `pr.missing_required_section` does not draft or refuse the PR for it. On an untagged task, `pr_open` reads no screenshots and pushes no evidence ref, so the body is unchanged. A `body` input supplied to `pr_open` is published as given, without the section, and a bundle of several tasks keeps its batch body.

**Commit subjects** keep the 72-character budget. An over-long subject shortens `{{title}}` with an ellipsis first, keeping the rest of the template, including the `[<TASK-ID>]` marker that delivery checks such as already-landed detection read; the full title then leads the body. A subject still over budget is cut around that marker, reserving space for the complete marker within the budget. The subject template must therefore contain `[{{task_id}}]`.

**Branch names** are stable across a run's retries: `{{hash}}` comes from the run ID, and with `ext.*` placeholders an existing local branch carrying the run's hash is reused even if the task's refs changed. A sample render is checked at load, and each real render when `worktree_setup` names the branch: a name that is empty, contains `..`, `@{`, whitespace, a control character or one of `~^:?*[\`, starts with `-`, has an empty component, a component starting with `.` or ending in `.lock`, or ends with `.`, is refused before any ref is created. Readers that find a task from a branch (PR closure, external-merge settlement, PR adoption and CI-failure filing) match the complete template, including the eight-character hex hash and any trailing literal, with `prefix` bound to `orbit`. A task token inside an external reference or a prefix of the owning task ID does not establish ownership. An external reference is matched verbatim, including an id that contains `/`. The fixed hash length also lets readers identify the task when `{{hash}}` immediately follows `{{task_id}}`.

`pr.labels` and `pr.reviewers` apply when `pr_open` creates a PR (`gh pr create --label/--reviewer`); an entry starting with `-` is refused. Orbit opens PRs ready for review, and as a draft only for a required-section gap. Completion never merges a draft. Section-gap drafts can recover on a resumed `pr_open` as described above; other drafts must be marked ready on the forge to continue.

When a follower hands off a section-gap draft, owner landing returns the claim for an automatic repair instead of leaving it stopped in review. The `landing_stopped` history entry names the missing sections from the section-gap evidence above and asks the repair to fill them or attach their evidence, then resume `pr_open`. That step refreshes the body and marks the PR ready under the same recovery rules above. If the automatic repair has already been used, the owner preserves the candidate and returns it to the backlog for another repair round, subject to the existing landing repair budget. Drafts without section-gap evidence (including an edited body) keep their refusal, with an operator action to mark the PR ready and retry landing. Screenshots continue to follow the `ui` tag; a non-UI diff does not automatically override a `ui` tag.

Task-derived labels are opt-in and separate from `pr.labels`. For example:

```toml
[pr]
task_tag_labels = ["ui", "security"]

[pr.task_type_labels]
bug = "bug"
feature = "enhancement"
```

After creating or reusing a PR, Orbit unions the allowlisted tags present on every delivered task with their mapped type labels. Empty settings add no forge commands. Label names must be non-empty and must not start with `-`, just like `pr.labels`. Orbit creates missing labels, accepts existing labels without changing their color or description, then adds the labels. Label creation and application use normal transient retries; any remaining failure is warned and recorded in the `pr_open` output's `task_labels.applied` and `task_labels.failed` (label and reason), while the PR step still succeeds with its original decision and PR identity. It never removes labels or syncs later task edits.

### Housekeeping

| Key | Default | What it does |
|---|---|---|
| `tasks.id_start` | unset | Forward-only floor for this machine's task-ID allocator, raised on every runtime build and never lowered, so machines can hold disjoint ranges. For the first seed prefer `orbit workspace init --task-id-start N`. See [task migration](design/task-migration/1_overview.md). |
| `workflow.ci_infrastructure_signatures` | `[]` | **Advanced.** Log-line prefixes a failed CI job prints when infrastructure, not the change, failed, such as `["ci-download: outcome=failed class=transient"]` for a download helper that spent its retries. A line the job emitted must start with the entry, so a run step's script text or a test quoting the line does not match. A match is treated like a network failure: an auto-fix or landing watcher reruns the failed jobs once, starts no repair agent and stops in review if they fail again, and the CI failure sweep lists the job under `infrastructure` instead of filing a task. The sweep itself reruns nothing, so a failed landing-branch run stays red until a rerun or a newer push. Declare only failures a rerun can clear: no repair agent runs for a match. Empty declares none, and failures take their usual route. |
| `retention.audit_days` | `60` | Days `orbit gc audit` keeps audit rows (1..=36500). Older host-wide command audit, this workspace's run audit, unregistered workspace run audit, and unreferenced audit blobs become reclaimable. Another registered workspace keeps its own window. Health > Events, Incidents, Denials, `orbit audit` and the diagnostics friction view lose raw history older than the cutoff, with no rollups. Lock contention keeps durable denial facts alongside reservation history; Cost, task-store Codebase rankings, `orbit task flow`, friction stats, auto-concurrency profiles and forecast accuracy keep working. Nothing is deleted until `orbit gc audit --apply` or the opt-in `store-gc` routine runs. |
| `retention.runs_days` | `60` | Days after a terminal run finishes before `orbit gc runs` may drop its pipeline state (30..=36500); `--older-than-days` has the same minimum. The 30-day floor preserves Reliability > Pipeline echo dedupe and forecast-accuracy per-drain limiter shares. Existing global or workspace files with a value below 30 load with an effective value of 30 and a warning, without rewriting the file; repair it with `orbit config set retention.runs_days 30` (add `--global` for the global file). New writes below 30 are refused. Run rows, steps, summaries and commit outcomes stay, so implement_one duration with outcome=succeeded, run history, Cost, task-store Codebase rankings, `orbit task flow` and friction stats keep working; raw pipeline checkpoints older than the cutoff are lost. Held and non-terminal runs stay. The pass includes unregistered workspace ids and separately prunes run/host resource minutes and admission forecasts older than 30 days, whatever this window, except rows kept at any age by count: each workspace, work class and complexity's newest 200 deliveries, the newest 10,000 forecasts, and host minutes from the oldest kept forecast onward. Auto-concurrency profiles and forecast accuracy keep working on those rows; older raw samples outside them are lost. A value of 30 can reclaim more terminal state than the default. |
| `worktree.reclaim` | `["target", ".orbit/tmp/*-target", ".orbit/tmp/base-wt"]` | **Advanced.** Relative rebuildable path globs reclaimed in kept terminal worktrees; an explicit list replaces earlier layers, `[]` disables. `*` stays within a component; a whole-component `**` spans components. Absolute, parent or root-matching patterns fail load. Registration, worker, symlink, confinement and Git content gates protect candidates and tracked files. See [worktree reclamation](runbooks/worktree-reclaim.md). |
| `worktree.reclaim_below_free_mib` | unset | **Advanced.** Also reclaim during admission while free space beneath the state directory is below this, oldest terminal worktrees first; active runs stay protected. |
| `seeding.routines` | `all` | Which shipped default routines this workspace seeds: `"all"`, `"none"` or a list of file stems. See [below](#seeding--which-shipped-defaults-a-workspace-carries). |
| `seeding.auto_tasks` | `all` | Which shipped default auto-tasks this workspace seeds: `"all"`, `"none"` or a list of names. See [below](#seeding--which-shipped-defaults-a-workspace-carries). |
| `decision_channel.enabled` | `false` | Raise decision requests for work held on a human decision. |

Several housekeeping bounds are fixed [ORB-22808]; their former keys are [deprecated](#validation-and-errors):

- **CI failure suppression.** A CI sweep finding archived or rejected without `covered_by` suppresses recurrences of its evidenced cause for 6 hours from that status event. A recurrence whose retained diagnostic differs files again. One whose cause cannot be established stays held: within the window for a plain archive, and with no expiry under a cover. An explicit task or GitHub PR cover holds that cause while open, or until its landed commit is observed in the failing checkout; if it fails to hold, later completed repairs still own duplicates or defer older failures until their landing is tested. (`ci_failure.operator_suppression_hours`)
- **Automation stalls.** A delivery-automation consumer that sits on a stuck deferral (`history_diverged`, `repository_changed`, `provider_identity_missing`, `state_missing`) for 60 minutes logs a warning and files one deduped friction. Transient backpressure never escalates. See [auto-tasks](../plugin/skills/orbit-setup/references/auto-tasks.md). (`automation.stall_window_minutes`)
- **Operational log.** The active `orbit.jsonl` rolls past 100 MiB, its archives are pruned oldest first past a 500 MiB total, and `orbit.jsonl` and `orbit-agent.jsonl` archives older than 7 days are deleted.
- **Commit journal.** `orbit gc journal` may delete a task commit's journal row 14 days after it is applied, for this workspace and unregistered ids. Recovery replays only prepared and committed rows; these, aborted rows and each task's newest applied row stay. Older raw transition diagnostics are lost; published task history, Codebase rankings, `orbit task flow`, friction stats, Cost, auto-concurrency profiles and forecast accuracy keep working. Nothing is deleted until `orbit gc journal --apply`; the `store-gc` routine does not run it. (`retention.journal_days`)
- **Store size.** `orbit doctor` warns when the host store file is larger than 1 GiB and this workspace has no enabled `store-gc` routine. The check stays ok under the threshold or while the routine is enabled.
- **Parked worktrees.** Worktree GC removes a terminal run's worktree serving a task parked in `someday` once the task has stayed parked for 336 hours (14 days), counted from that status change. A live worker, an unsettled claim or a Git worktree lock keeps it, and a clean worktree whose run branch holds commits no other ref reaches and `origin` lacks stays with its branch (`skipped:unpublished_commits`). See [parked tasks](runbooks/worktree-reclaim.md#worktrees-of-parked-tasks). (`worktree.parked_reclaim_after_hours`)

### `[jobs]` — failing-job warning

A job's failure streak is its newest finished runs in a row whose state is `failed` or `interrupted`, counted back to the first `success`. Operator cancels, designed holds (`held`), timeouts and skipped runs are neither failures nor successes: they neither extend nor break a streak. The daily run-failure-patterns pass also counts a `cancelled` run with a non-null `error_code` as a failure, and the streak never reads those runs, so the two counts can differ. The streak is read from the newest 100 finished runs of the job, and a streak that fills that window reads "at least 100".

When a job's streak reaches 5, the dashboard's Jobs › Catalogue row shows a "Failing: N in a row since <day>" badge with the latest error, whether every run in the streak failed with the same error, and a link to the latest failed run. The Jobs navigation item shows a count of failing jobs, counting only the manual-runnable jobs the Catalogue shows; delivery and other non-runnable jobs' streaks do not count toward that badge. Below the threshold nothing is shown. `GET /api/jobs` reports each job's streak whatever the threshold; `GET /api/jobs/failure-streaks` returns the manual-runnable jobs at or above it.

The warning is display only: it never pauses, disables or changes a job, and it sends no notification outside the dashboard.

The threshold is fixed [ORB-22808]; `jobs.failure_streak_warn` is [deprecated](#validation-and-errors).

Agent relay output goes to `orbit-agent.jsonl` with its own 200 MiB archive budget and 50 MiB active-file limit, so it cannot consume the operational budget; each feed is pruned by its own size and age limits. A full agent queue blocks the relay instead of dropping lines. The operational feed can still drop a line when its queue is full. The dashboard and `orbit log tail` merge both active feeds. Full rotation runs in long-lived processes (`orbit mcp serve`, `orbit clock tick`/`orbit sweep`, `orbit web serve`); short-lived commands only roll an oversized active file.

### Fleet notices — fixed limits

Fleet notices have no settings [ORB-22808]. Their limits are fixed:

- **Expiry.** A post that names none lasts 72 hours (incident), 12 (maintenance) or 168 (info). The longest expiry is 720 hours for an operator, 168/72/168 for an orchestrator, and 24 for a managed agent, which is also an agent notice's default.
- **Size.** Titles up to 120 characters, bodies up to 4096 UTF-8 bytes, at most 8 evidence references.
- **Open notices.** 20 per workspace, 10 global per owner host, 2 per managed task.
- **Post rates.** 5 per managed task in 24 hours, corroborations included; 30 per orchestrator crew per hour.
- **Retention.** Closed and expired notices stay listable for 30 days, then are purged.
- **Agents and briefs.** Managed agents always may post and close their own notices through their bound task. An opted-in activity's prompt quotes at most 5 open notices, each body cut to 600 characters; a notice read that takes over 2000 ms gives an `unavailable` brief instead of delaying dispatch.

The former `notice.*` keys are deprecated and ignored ([validation and errors](#validation-and-errors) lists them).

### `[security_alert_sweep]` — security finding severity

`dependabot_alert_sweep_pipeline` files Dependabot and Code scanning findings of `moderate` severity and above. Secret scanning findings are always filed. The floor is fixed [ORB-22808]; `security_alert_sweep.min_severity` is [deprecated](#validation-and-errors).

A run's `min_severity` input (`low`, `moderate`, `high` or `critical`) overrides the floor: `orbit run job dependabot_alert_sweep_pipeline --input min_severity=critical` overrides one run. The file step records `min_severity`, `min_severity_source` (`input` or `built-in`; runs recorded before the floor was fixed may name `workspace` or `global`) and `excluded_below_min_severity`; `orbit run show <RUN_ID>` summarizes the filed count, floor and source, and excluded alert count and numbers, even when the sweep succeeded.

### Path leases

Path leases are not configurable for now: the `lease.*` keys, `orbit lease` and the `orbit.lease.*` tools were withdrawn, and a `[lease]` table loads with a deprecation warning and no effect ([ORB-22808]; [design](design/path-leases/1_overview.md)). The lease machinery is retained but stays disabled.

### `[experiments]` — experiments over agent treatments

An experiment compares a control arm against one or more treatment arms, each an overlay of instructions, skills, tools, crew, allowlisted config or crew memory ([scope](design/experiments/1_scope.md)). An operator defines it with `orbit experiment create`, edits it only while it is a draft, and moves it through `start`, `pause`, `resume`, `reweight`, `conclude` or `abandon`; each change is an event with its actor, time and reason, listed by `orbit experiment show`. Agents may show and list experiments but not change them. An operator dashboard session can also create, edit and start experiments on Analytics › Experiments (`POST /api/diagnostics/experiments`, `PATCH /api/diagnostics/experiments/<id>` and `POST /api/diagnostics/experiments/<id>/start`). These routes call the same create, update and start as the CLI, so the rules on this page and their refusal messages apply unchanged. The form offers only `experiments.config_keys` for config overlays and the configured crews for crew pools, and every write records actor `dashboard` with the given reason. `experiments.enabled` false or three experiments already running is answered as HTTP 409 with core's message.

| Key | Default | What it does |
|---|---|---|
| `experiments.enabled` | `false` | Allow experiments to start in this workspace. While false, new assignments are disabled and `start` is refused with an error naming this key; drafts can still be defined, edited, shown and abandoned. |
| `experiments.config_keys` | `[]` | **Advanced.** Config keys an arm's `config` overlay may set. A key must also be on the typed code allowlist below. `start` refuses unknown keys, keys absent from either list, and values of the wrong type or outside their bounds. The default allows no config overlay. |

```toml
[experiments]
enabled = true
```

At most 3 experiments run at once in a workspace: `start` and `resume` are refused while that many are running, and a paused experiment does not count. A readout gives a verdict only when at least 0.9 of each treatment arm's settled tasks received its treatment (every role its instructions, skills or tools target ran with the overlay and finished successfully); below it the verdict is `withheld`, with the reason `arm <arm> treatment delivered on <n>/<m> runs`. Both are fixed [ORB-22808]; `experiments.max_running` and `experiments.min_delivered_rate` are [deprecated](#validation-and-errors).

At a task's first delivery admission, running experiments (shadow or live) match complexity, task type, all included tags, no excluded tags and origin. The draw uses the leading eight bytes (big-endian) of `sha256("<salt>:<experiment_id>:<task_id>")` modulo the total active arm weight, walked in arm declaration order. The historical arm and segment survive retries, reweights and experiment conclusion. Historical enrollment is separate from fresh execution authority: concluded or abandoned experiments apply no overlay to a fresh admission. Already admitted owner runs and claim receipts retain their captured treatment, including on resume; paused experiments and retired arms keep their existing-assignment behavior. Later admissions do not reconsider an initially ineligible task, even after an edit makes it match, except that the v70 upgrade reopens every admission-only record once. This includes records from enabled admissions that matched no population, because the stored rows do not distinguish those from rows written while experiments were disabled; rows with an assignment or exclusion stay final. An admission while `experiments.enabled` is false records nothing, so the task's first admission after enabling is its first. Overlapping surfaces are reserved across all arms, including control; later experiments record `surface_conflict` exclusions. A task with an explicit crew (`crew_source: explicit`) is recorded with `explicit_crew` and no arm in an experiment whose arms overlay the crew surface, because every arm would run its explicit crew. An operator can force this `explicit_crew` exclusion into an arm with `orbit.experiment.assign`; that replaces only this exclusion, while an existing arm assignment or another exclusion for the experiment remains sticky and conflicting experiment surfaces still refuse the force. On a fresh admission, a force supersedes an older task pin, while a later audited explicit task crew change or an explicit run crew wins. Assignment and crew-change timestamps determine ordering, with force winning an exact tie. A pin without a recoverable timestamp supersedes ordinary enrollment but does not displace a force. A crew override suppresses the whole crew-bearing arm for that admission, leaving historical rows unchanged; the capture records `live: false` and `crew_selection.experiment_suppressions` records the intended digest and actual crew. A `shadow` experiment records assignments and applies nothing.

A `live` experiment applies its arm's instructions, skills, tools, crew pool and allowed config values. `start` refuses, in either mode, an instruction file or skill bundle missing or inadmissible at the workspace's current base (its base branch as last fetched from `origin`, else the local branch), an unsupported skill provider, an added tool that is not a registered, agent-facing, enabled tool named exactly, and a removed tool that is unknown or that the agent handoff contract needs (`orbit.task.show`, `orbit.task.update`, `orbit.task.artifact.put`).

A crew overlay is `{"crew":{"pool":[{"crew":"sol","weight":1},{"crew":"astra","weight":2}]}}`. It replaces the task's normal admission pool, including a crew assigned at task creation. `start` checks that every member exists and is enabled; crews have no per-complexity restrictions, so each may run every tier in the population. Admission still filters members by executor host support and run allowlists. Weekly usage pressure still weighs the pool. Provider holds and limits still apply, including the existing complexity-pool fallback. `crew_selection` records the pool, weights and overlay digest; a changed draw records `deviation` with the intended crew, actual crew and reason. An explicit run crew remains authoritative, recorded with source `explicit`. When treatment selects the crew, write-back records it on `task.crew` as `experiment:<id>/<arm>`. An override retains its own admission provenance rather than being attributed to the experiment; the immutable assignment remains available to intention-to-treat analysis. Claimed leaves freeze the follower's draw and its evidence in pipeline state's `crew_selection` before dispatch. An arm without a crew overlay keeps ordinary routing.

The current typed config allowlist is:

| Key | JSON type and bounds | Per-run effect |
| --- | --- | --- |
| `workflow.explore.max_calls_per_run` | Integer, `1`–`100` | Compatibility only: retained for historical experiment records; ignored since explorer removal. |
| `visual_review.reviewer` | Boolean | Compatibility only: retained for historical experiment records; ignored since visual review was removed [ORB-22807]. |

Neither key has a per-run effect today. Values and their overlay digest travel in the task-bound `experiment_capture` on owner runs and in claim receipts. Workspace config is never rewritten.

To opt in another key, add a variant to `orbit-config`'s `ExperimentConfigKey` and its `ALL` list, declare its dotted key, validate its JSON type and bounds, and wire its consumer to the task-bound run capture on owner and claimed paths, applying role filtering when appropriate. Update this table with that change. Adding a key to `experiments.config_keys` alone cannot enable it; both code and workspace permission are required at start.

- **Instructions.** An entry per role (`implement`, `review` or `recovery`) names a repository file, read like a tag instruction (UTF-8, at most 8 KiB) at the run's base revision. Its text follows any tag instruction in the prompt's tag-instruction section. `false` leaves Orbit's shipped default tag instructions out of that role. A role the entry does not name receives nothing.
- **Skills.** `{"skills":{"replace":{"orbit":"experiments/skills/orbit"},"roles":["implement"]}}` replaces the named personal skill for those roles (`implement` by default). The directory is read from the run's base commit, including supporting files and executable bits. It must contain a UTF-8 `SKILL.md` with YAML frontmatter whose `name` matches the replacement name, a nonempty `description`, and instructions. Names use lowercase ASCII letters, digits and hyphens, at most 64 characters. A bundle holds at most 256 regular files and 64 KiB of raw content, nested at most 16 directories; symlinks, submodules and unsafe paths are refused. Serialized bundles must also fit the experiment capture's 256 KiB budget, so the raw limit alone does not guarantee admission.
- **Tools.** `add` and `remove` apply to the roles the entry's `roles` lists (`implement` by default), after the activity's tool grant. An addition never overrides the activity's disallow list or the claimed-mode denial; a removal never takes a task's `required_tools`.

Skill overlays currently support **Claude on Unix only**. Each treated invocation gets a private `CLAUDE_CONFIG_DIR` under `.orbit/tmp/`, with the arm's bundles in `skills/<name>`. Other personal skills and existing provider state (settings, credentials, plugins and session directories) remain linked to their original locations; the default `~/.claude.json` sidecar is linked too. `HOME` and authentication variables retain their values. The private directory lives until the provider exits, then is removed. Control invocations keep their original environment and discovery roots. Orbit never rewrites shared skill directories or their links. Enterprise skills retain Claude's higher discovery precedence.

Codex, Gemini, Grok, Muse, Copilot, Ollama, Antigravity and mock providers are refused for skill overlays; Windows is also refused. Codex discovers personal skills independently of `CODEX_HOME` and permits duplicate names; an additional project skill therefore does not provide replacement. Claude's `--add-dir` namespaces conflicting skills instead of replacing them. The other provider discovery paths have no implemented isolation adapter. See [Claude skill discovery](https://code.claude.com/docs/en/skills#resolve-skills-that-share-a-name) and [Codex skill discovery](https://developers.openai.com/codex/skills#where-codex-loads-local-skills).

For an implementation-only skill overlay with an arm crew pool, `start` checks every member of that pool. Otherwise it conservatively checks every enabled crew, including possible explicit and purpose selections. Use a Claude-only arm pool on a mixed-provider host. Dispatch checks the actual provider again, so a later routing change, explicit run override or follower with a different provider refuses rather than silently running without the treatment. This restriction applies to shadow definitions at start too, though shadow runs apply no files.

Each dispatch a live arm changes records an `activity.experiment_overlay_applied` audit row: the arm, the instruction's hash and source, the tools actually added and removed, a map of skill names to bundle digests, any crew memory it received (`memory`: model, revision numbers, byte count, renderer version and content digest), and a `sha256:` digest over the received surfaces. That digest covers the crew memory renderer version but not the memory content, so a new memory revision does not change it. A bundle digest covers sorted relative paths, file contents and executable bits. `orbit run show` prints one `Experiment applied:` line per row, including each skill's digest, after the run's `Tag instructions:` lines; its JSON includes the same map.

`orbit experiment assign <experiment> --task <task> --arm <arm>` forces a task without an arm assignment, or one with an `explicit_crew` exclusion for that experiment, into an active arm and records `forced=true`. It works in either shadow or live mode while the experiment is running; a draft, paused, concluded or abandoned experiment is refused. Existing arm assignments and other exclusions for that experiment remain sticky, and overlapping surfaces owned by another experiment still refuse the force. `orbit task show --fields experiments` exposes the assignments and exclusions. `orbit run show` prints each captured arm as `shadow` or `live` with its overlay digest. Owner claims carry a task-bound overlay capture, limited to 256 KiB, with each live arm's instruction files and complete skill bundles read at the claim base; followers apply those captured bytes, refuse missing, malformed or digest-mismatched captures, and never consult local experiment configuration or skill bundle files. A live instruction file the owner cannot read is captured as a refusal that stops the claimed run in that role. An unreadable or oversized skill bundle refuses claim capture itself.

`orbit experiment delete <experiment> --reason <text>` (operator CLI only; not advertised over MCP) removes a draft, concluded or abandoned experiment with its arms, event log and assignment rows. It refuses a running or paused experiment, one that ran live and assigned tasks to arms (their `task.crew` and run captures cite it), and one concluded with an arm (its promotion task names it), including a shadow experiment. Shadow experiments concluded without an arm applied nothing, so they can be deleted once finished. The event log goes with the experiment, so the deletion is logged by the CLI process with its actor and reason.

`orbit experiment readout <experiment> [--metric <name>]` (tool `orbit.experiment.readout`, web `GET /api/diagnostics/experiments/<id>/readout?metric=<name>`) computes each arm's outcomes on read from the assignments, one task per unit, intention-to-treat; forced assignments and exclusions (`surface_conflict_excluded`, `explicit_crew_excluded`, and `interactive_excluded` for a task worked in an interactive session, which reached review or done without a delivery run and so was never treated) are counted and left out. Each metric reports per-arm n, mean and median with seeded bootstrap 95% intervals (Wilson intervals for rates), and the difference from control overall (post-stratified by complexity) and per complexity. The readout also reports crew mix from each task's `task.crew`, a `crew_mismatch` warning naming each task whose crew is outside its arm's crew pool (for an arm without a crew overlay, such as control, the pool recorded at admission for the task's delivery — its frozen crew selection, including any run-level pool override and its admission-tier, purpose or type route — so later workspace pool changes or a complexity rerate neither invent nor erase a historical mismatch; when that recorded admission pool is unavailable, a `crew_mismatch_unavailable` warning reports it instead of a confirmed mismatch) when any arm overlays the crew surface, a sample-ratio chi-square over each segment's weights (p < 0.001 warns and withholds the verdict), a drift warning when an arm saw more than one overlay digest, open lagging outcomes (unfinished tasks and 14-day regression windows), and guardrail checks. Crew treatment delivery requires the latest delivery's frozen experiment selection and digest without a deviation or suppression, or an owner-issued active capture joined to the actual crew reported at claim settlement. An explicit choice of the same crew is not delivered treatment, and a later suppressed admission cannot borrow an older delivery's evidence. Missing admission evidence leaves crew treatment unconfirmed. Any treatment arm with a delivered rate below 0.9 gets verdict `withheld` before the horizon, even when its primary comparison would otherwise be `winner` or `no difference`. When no check withholds the verdict, it is `collecting` until `max_days` have passed or every active arm has `min_sample` settled primary outcomes, then `winner <arm>` when a treatment's primary interval excludes zero in the better direction and no guardrail is breached or undecided, `no difference`, or `inconclusive`. The JSON carries `schema_version`; an unknown experiment or metric is refused. The dashboard's Analytics › Experiments page shows the list with per-arm enrollment and each experiment's readout; its endpoint response adds a `cumulative` per-arm series of the primary metric. An operator session also offers conclude on a running or paused experiment, and New experiment, Edit and Start for drafts.

`orbit experiment conclude <experiment> [--arm <arm>] --reason <text>` (tool `orbit.experiment.conclude`, dashboard `POST /api/diagnostics/experiments/<id>/conclude`) concludes a running or paused experiment. The conclusion event stores the readout JSON at that moment, which `orbit experiment show` returns, so the decision can be read against the numbers it was made on. A chosen arm files one proposed task in this workspace naming the experiment, the arm, the readout verdict and the overlay; omitting the arm files nothing. Enrollment stops and historical assignments remain available to analysis. Fresh admissions apply no concluded treatment; already admitted runs and owner-issued claims keep the capture they froze, including on resume. Concluding changes no configuration or repository file. Only an operator may conclude.

After the horizon, if no treatment wins and any primary comparison lacks an interval, the verdict is `inconclusive`, with a reason naming the treatment and metric. This includes no settled primary outcomes, fewer than two outcomes in either arm of a used complexity cell, or no common measured complexity cell. `no difference` requires a measured comparison; a treatment with a complete winning comparison can still win.

### `[crew_memory]` — per-model memory (experimental)

Crew memory gives each model a small memory space of Markdown files, plus one `global` space every model shares ([design](design/crew-memory/2_design.md)). Spaces are append-only revisions in the host store. Memory reaches a prompt only through the `memory` surface of a running `live` experiment (below); no setting delivers it outside an arm.

| Key | Default | Meaning |
| --- | --- | --- |
| `crew_memory.enabled` | `false` | Allow crew memory in this workspace. While false, `orbit memory write`, `reset` and `copy` are refused with an error naming this key (`show`, `history` and `export` still work), `orbit experiment start` refuses an experiment with a memory arm, and running memory arms deliver nothing. |

A memory arm's overlay is `{"memory":{"roles":["implement"],"global":true}}` (`roles` is any of `implement`, `review` and `recovery`, default `implement`; `global` defaults to `false`). It is its own surface, so one experiment per task may own it. After the crew draw, the owner reads the drawn crew's model space at its newest revision (and the `global` space when `global` is true), renders the block and digests it with the renderer version. A model with no revision renders an empty block, which is still delivered and recorded; a run with no recorded model receives none. The rendered block, both revision numbers and the digest travel in the task-bound `experiment_capture` on owner runs and in claim receipts, for shadow arms too. A claimed leaf delivers only the bytes its receipt captured and refuses a memory arm without a memory capture. Before delivering those bytes it asks the owner whether `crew_memory.enabled` is still true, and delivers nothing when the owner says no or cannot be asked; it does not rewrite the receipt or read its own memory tables or config. A dispatch whose model differs from the captured one (a follower redraw, or a reviewer crew the owner did not draw) receives no memory. On the owner, a reviewer or recovery dispatch with no captured delivery for its model is rendered then.

A live delivery adds a `crew_memory` field to the agent envelope, `{"model":[{"path","text"}],"global":"…","revision":{"model":N,"global":N}}`, never inside `instruction`, and appends one fixed sentence to the instruction chain after the tag and experiment instructions, saying the notes are data and that the task, its criteria, the instructions and the repository's guidance take precedence. A delivered block holds at most 9 KiB of note text (a full model space plus a full global space) and counts toward the prompt's size; a larger one is refused. The readout's `memory_strata` groups a memory arm's tasks by the ISO week of the revision their first dispatch received (`empty` for an empty block): a revision change is a stratum, not drift, while a renderer-version change drifts the arm.

Every write passes the same rules and is refused whole, naming the rule, when it breaks one: `caps` (a model space at most 8 files and 6 KiB, `global` 1 file and 3 KiB, file names `[a-z0-9-]{1,40}.md`), `provenance` (evidence must be task ids; a run's write must cite one), `score_blind` (scorecard vocabulary or another crew's or model's name, naming the token), `not_instructions` (a URL, an Orbit tool name, a shell command block or an injection phrase, naming the token) and `rewrite_whole` (each file once; a write replaces the whole space). A refusal is recorded as a `write_refused` event in `orbit memory history`.

### `[seeding]` — which shipped defaults a workspace carries

Orbit ships default routines (`.orbit/routines/`) and auto-tasks (`.orbit/auto_tasks/`) and seeds every one into a workspace. Two keys narrow that, per workspace or globally ([design](design/distributions/1_scope.md)):

| Key | Default | What it does |
|---|---|---|
| `seeding.routines` | `"all"` | `"all"`, `"none"`, or a list of shipped routine file stems (`["worktree_gc", "store_gc"]`). |
| `seeding.auto_tasks` | `"all"` | `"all"`, `"none"`, or a list of shipped auto-task names (`["doc-duties", "qa-sweep"]`). |

```toml
[seeding]
routines = ["worktree_gc", "store_gc"]
auto_tasks = "none"
```

- **Every seeding path honours it.** `orbit workspace init`, `orbit workspace init --force`, `orbit workspace sync` and the create-only seeding `orbit init` runs over registered workspaces after an upgrade all reconcile through the policy. `"all"` also seeds a default a later release adds; `"none"` and a list never do.
- **Admission.** A list name Orbit has never shipped fails config load, `orbit config set` and every seeding command, naming the key, the name and the shipped names. A name a later release retired or demoted still loads and simply seeds nothing; a demoted auto-task's existing copy stays as a workspace-local definition ([promotion on proven yield](design/auto-tasks/1_overview.md#promotion-to-the-shipped-defaults)). `orbit config keys` lists both keys.
- **Narrowing retires by digest.** A default the policy leaves out is retired exactly as a default a release dropped: an unedited file Orbit seeded is removed, and a locally edited one moves under `.orbit/.retired-managed/` with a warning to migrate it under a new user-authored name. A file Orbit never recorded is left alone.
- **Widening reseeds.** Adding a name back, or returning to `"all"`, seeds the shipped file again on the next sync.
- **Opt-outs.** An opt-out recorded with `orbit auto-task delete` survives while the policy leaves that auto-task out. `orbit auto-task restore` refuses a left-out name and names the policy. For routines, a list policy is the durable per-name opt-out.
- **Diagnostics.** `orbit workspace show` prints both policies, the defaults they leave out, and the applied preset (JSON `seeding` and `preset`). `orbit doctor` does not report a left-out default as missing.

#### Workspace presets

`orbit workspace init --preset <file>` applies a preset: a TOML file a distribution ships to set a workspace up in one step. The same flag works with `--force` on a registered workspace.

```toml
name = "graph-distro"            # required: 1-64 of [A-Za-z0-9._-]

[seeding]                        # optional: written as seeding.routines / seeding.auto_tasks
routines = ["worktree_gc"]
auto_tasks = "none"

[enable]                         # optional: plugin-seeded definitions to switch on
routines = ["graph-refresh"]
auto_tasks = ["graph-reindex"]

[config]                         # optional: workspace keys, nested tables or dotted keys
"workflow.crew_selection" = "static"
[config.review]
minutes = 45
```

- **Validated whole, before any write.** An unknown field in any table, a `[config]` key or value `orbit config set` refuses for this workspace, a `seeding.*` key under `[config]`, a blank or unseeded `[enable]` name, a plugin-seeded `[enable]` name whose file is missing or does not load (a routine's `job:` target must be a shipped default, a job on disk, or a job an active plugin supplies), or a file over 256 KiB is refused, and nothing is written or registered.
- **No consent.** A preset never grants plugin permissions or enables a plugin: a `grant`, `grants`, `permissions`, `plugin`, `plugins`, `enable_plugins` or `plugin_enablement` field, at the top level or under `[enable]`, and any `plugin_enablement*` key under `[config]`, is refused. `[enable]` only switches on a definition an already enabled plugin seeded into this workspace (the name `orbit routine list` or `orbit auto-task list` shows), so run `orbit plugin enable <plugin>` first, then `orbit workspace init --force --preset <file>`.
- **Order.** The `[seeding]` and `[config]` keys are written to `.orbit/config.toml` in one validated save before defaults are seeded, so the first seeding already follows the policy. `[enable]` is applied after seeding.
- **Provenance.** The preset's `name` and `sha256:` digest of the file bytes are recorded with the apply time in `.orbit/state/workspace-preset.json`. `orbit workspace init` prints them with the definitions it enabled, and `orbit workspace show` reports them.

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
| `workflow.validation_env.login_shell`, `interactive` | Ignored: required validation always probes the interactive login shell, falling back to a login shell. Delete the keys. Recognised until 2026-12-08. |
| `workflow.auto_ship` | Ignored: the ship sweep it opted into was removed; dispatch with `orbit run auto` drains or `orbit run ship`. Delete the key. |
| `workflow.crew_performance_types` | Becomes `workflow.task_types`, the same ordered entries. Recognised until 2026-12-07. |
| `workflow.crew_performance`, `crew_performance_strength`, `crew_performance_cap`, `crew_performance_window_days` | Ignored: crew-performance weighting and its snapshots were removed. Delete the keys. Recognised until 2026-12-07. |
| `scoring.enabled` | Ignored: the agent-family scoreboard was retired and nothing records scoreboard metrics. Delete the `[scoring]` table. Recognised until 2026-11-12; after that it is an unknown key. |
| `workflow.crew_ladder.mode`, `rungs`, `tiers`, `explore_rate`, `margin`, `look_every`, `max_sample`, `exclude_types`, `watch_window`, `retry_after_days`, `delivery_quality`, `quality_grade_margin`, `min_grade_coverage` | Ignored: the crew ladder, its standing shadow trials and `orbit agents ladder` were removed. Route each tier with `workflow.<tier>_complexity_crews` and compare crews with [experiments](#experiments--experiments-over-agent-treatments). Delete the `[workflow.crew_ladder]` table. Recognised until 2026-12-07; after that they are unknown keys. |
| `workflow.regression_risk.enabled`, `window_days`, `min_support`, `threshold_percent`, `max_files` | Ignored: regression-risk prompt marks were removed. Task riskiness scores use a fixed 30-day window and a minimum support of 5, the former defaults. Delete the `[workflow.regression_risk]` table. Recognised until 2026-12-07; after that they are unknown keys. |
| `workflow.explore_brief.enabled`, `tags`, `deliver` | Ignored: the task pilot's explore brief was removed. The pilot no longer returns a brief, no implement dispatch receives one, and the brief-accuracy diagnostics endpoint is gone. Existing `explore-brief.json` task artifacts and `activity.explore_brief_applied` audit rows stay readable. Delete the `[workflow.explore_brief]` table. Recognised until 2026-12-07; after that they are unknown keys. |
| `visual_review.manifests`, `default_width`, `mode`, `max_mocks`, `reviewer`, `max_images`, `max_image_tokens`, `max_findings`, `require_established`, `block_at`, `file_at`; `workflow.purpose_crews.visual_review` | Ignored: visual review (mock comparison, the `verify-mock-ui` capture, checks, comparison review and post-landing follow-ups) was removed [ORB-22807]. No delivery captures or compares mocks, `mocks/` and `visual/` are ordinary artifact names, and `orbit visual` and the `orbit.visual.*` tools are gone. Delete the `[visual_review]` table and the purpose pool. Recognised until 2026-12-07. |
| `resident.evolution.min_occurrences`, `min_distinct_cycles`, `min_distinct_mandates`, `min_cost_minutes`, `max_proposals` | Ignored: the weekly evolution pass routine (`evolution_pass_pipeline` and its rank, draft and apply activities) was removed [ORB-22844], so nothing files evolution proposal tasks automatically. The evolution ledger, `orbit evolution`, the `orbit.evolution.*` tools and `orbit resident digest` stay; the ledger keeps its fixed 14-day window and 30-day decline. Delete the keys. Recognised until 2026-12-07. |
| `ux_rules.surfaces`, `runner`, `ui_base_url`, `max_new_tasks`, `file_at`, `disabled` | Ignored: the `ux-rules` routine, `ux_rules_pipeline` and its rule-findings filer were removed [ORB-22844]; nothing runs the rules on a schedule or files tasks from them, and no `ux_rules_runner` script grant is needed. Delete the `[ux_rules]` table. If the workspace set up the routine, also delete `.orbit/resources/scripts/ux_rules_runner.yaml` and run `orbit script revoke ux_rules_runner`: its entry, the routine's deleted `step.mjs` adapter, no longer ships, and jobs that reference the manifest refuse until it is repaired or removed. `orbit doctor` reports it under `retired-scripts`. Recognised until 2026-12-07. |
| `machine.web_terminal.enabled`, `machine.web_session.ttl` | Ignored: dashboard browser sessions and `orbit web open` were removed [ORB-22808]. The dashboard opens its plain URL, and operator capability still comes from `orbit web serve --operator`. Delete the `[machine.web_terminal]` and `[machine.web_session]` tables. Recognised until 2026-12-08; after that they are unknown keys. |
| `publication.routine_attachments`, `publication.routine_allow_unscanned_attachments` | Ignored: the scheduled task-publication routine always omits attachments [ORB-22808], the former `omit` default. Publish attachments by hand with `orbit task publication publish --attachments`. Delete the keys. Recognised until 2026-12-08; after that they are unknown keys. |
| `review.verify_findings` | Ignored: the `finding_verification` routine and `finding_verification_pipeline` were removed [ORB-22844], and nothing holds a review-filed bug as `unverified_review_finding`; it is approved and promoted like any other task. The task pilot's `finding_invalid` result and an implementer's `finding-invalid.json` still reject an invalid finding with a REJECTED verdict, and verdicts the removed job wrote still load. Delete the key. Recognised until 2026-12-07. |
| `review.selective_before_landing.mode`, `risk_threshold`, `paths`, `unknown_risk` | Ignored: selective before-landing review was removed [ORB-22808], so no candidate is selected for review by risk or changed paths (the former `off` mode). Review before landing with `review.timing = "before_landing"` or `review.before_landing_hosts`. Runs, claims and handoffs recorded with a selective policy or receipt still load; the legacy fields are ignored. A stored claim that captured only that policy (both timing flags false, with the legacy review contract still present) is not a review requirement, so its pull-request handoff is accepted without review evidence. A claim that captured before-PR or before-landing review still requires that evidence. Delete the `[review.selective_before_landing]` table. Recognised until 2026-12-08; after that they are unknown keys. |
| `assistance` | Ignored: decision assistance was removed [ORB-22808]: its model service, the task-quality, delivery-quality and precedent-decision consumers, precedent retrieval and `orbit decision assist`. Nothing calls the OpenAI Decisions endpoint. Delete the global `[assistance]` table. Recognised until 2026-12-08; after that it is an unknown key. |
| `notice.default_expiry_hours.*`, `max_expiry_hours.*`, `max_title_chars`, `max_body_bytes`, `max_evidence`, `max_open_per_workspace`, `max_open_global_per_owner`, `max_open_per_agent_task`, `agent_posts_per_task_per_day`, `orchestrator_posts_per_hour`, `retention_days`, `agent_posting`, `brief_delivery`, `brief_max_items`, `brief_body_chars`, `brief_read_timeout_ms` | Ignored: fleet notice limits are fixed at the former defaults ([fleet notices](#fleet-notices--fixed-limits)); agent posting and brief delivery stay on. Delete the `[notice]` table. Recognised until 2026-12-08; after that they are unknown keys. |
| `pr.close_on_terminal`, `draft`, `freshness_section`, `orchestration_trailer`, `screenshot_section`, `signature`; `commit.orchestrated_by_trailer`, `regression_trailers` | Ignored [ORB-22808]: delivery always closes a task's open Orbit PRs when it ends, opens PRs ready for review (a draft only for a required-section gap), adds the Screenshots table for `ui`-tagged tasks, the Branch Freshness section, the model signature and the `Orchestrated-By` line, and adds the `Orchestrated-By` and regression trailers to commits. Delete the keys. Recognised until 2026-12-08. |
| `workflow.admission_affinity`, `auto_effort.explore_share`, `provider_limit_max_used_pct`, `usage_pressure_cap`, `usage_pressure_strength` | Ignored [ORB-22808]: admission affinity is fixed at `band`, the `@auto` exploration share at `0.1`, the provider-limit threshold at 90% (`workflow.provider_limit_overrides` still sets it per provider or crew), and the usage-pressure cap and strength at 8 and 1 (`workflow.crew_selection` still chooses the draw). Delete the keys. Recognised until 2026-12-08. |
| `workflow.resource_throttle.cpu_light_leaves`, `io_full_high_percent`, `io_full_resume_percent` | Ignored [ORB-22808]: the throttle keeps 2 CPU-light leaves under CPU-only pressure and throttles on Linux disk I/O stalls at 20%, resuming below 10%. Delete the keys. Recognised until 2026-12-08. |
| `workflow.validation_env.shell_probe`, `path_mode`, `cargo_incremental` | Ignored [ORB-22808]: required validation always probes the interactive login shell (falling back to a login shell), prepends `workflow.validation_env.path` to the resolved PATH, and sets `CARGO_INCREMENTAL=0` for bound host validation. Delete the keys. Recognised until 2026-12-08. |
| `attention.cursor_ttl_hours`, `run_window_hours`, `run_stuck_after_minutes`, `ci_cache_seconds`, `source_timeout_seconds`, `fanout_budget_seconds`, `per_workspace_item_cap`, `age_bump_hours` | Ignored [ORB-22808]: the attention feed's bounds are fixed at their former defaults: 24-hour cursors, a 48-hour failed-run window, runs stuck after 120 minutes, a 60-second CI cache, 10-second source reads, a 20-second cross-host budget, 200 candidates per workspace and severity steps at 24 and 72 hours. Delete the `[attention]` table. Recognised until 2026-12-08. |
| `automation.stall_window_minutes`, `ci_failure.operator_suppression_hours`, `jobs.failure_streak_warn`, `security_alert_sweep.min_severity`, `worktree.parked_reclaim_after_hours` | Ignored [ORB-22808]: fixed at 60 minutes, 6 hours, 5 failed runs, `moderate` and 336 hours. A security sweep run's `min_severity` input still overrides its floor. Delete the keys. Recognised until 2026-12-08. |
| `execution.agent_stall_minutes`, `execution.proc_spawn_max_timeout_minutes` | Ignored [ORB-22808]: fixed at 30 and 45 minutes. Delete the keys. Recognised until 2026-12-08. |
| `experiments.max_running`, `experiments.min_delivered_rate` | Ignored [ORB-22808]: fixed at 3 running experiments and a 0.9 delivered rate. Delete the keys. Recognised until 2026-12-08. |
| `machine.application_consent_max_days`, `machine.host_shutdown_hold_minutes` | Ignored [ORB-22808]: fixed at 30 days and 180 minutes. Global only. Delete the keys. Recognised until 2026-12-08. |
| `machine.auto_concurrency.admit_memory_percent`, `profile_window`, `horizon_cap_minutes`, `head_max_wait_minutes`, `head_stuck_passes` | Ignored [ORB-22808]: fixed at 75%, 50 deliveries, 120 minutes, 30 minutes and 20 passes. `mode`, `admit_cpu_percent` and `min_profile_deliveries` stay settable. Delete the keys. Recognised until 2026-12-08. |
| `resident.evolution.window_days`, `decline_suppress_days` | Ignored [ORB-22808]: the evolution ledger keeps a fixed 14-day window and 30-day decline. Delete the keys. Recognised until 2026-12-08. |
| `retention.journal_days`, `retention.store_warn_bytes` | Ignored [ORB-22808]: fixed at 14 days and 1 GiB. `retention.audit_days` and `runs_days` stay settable. Delete the keys. Recognised until 2026-12-08. |
| `runtime.log_retention_days`, `log_max_total_mb`, `log_max_file_mb` | Ignored [ORB-22808]: the operational log keeps archives 7 days within 500 MiB and rolls past 100 MiB. Delete the `[runtime]` table. Recognised until 2026-12-08. |
