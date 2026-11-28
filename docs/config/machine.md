# Machine configuration

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
| `machine.worker_containment` | `warn` | Yes | `warn` runs each detached pipeline worker in a transient systemd user scope (`orbit-worker-<run_id>-<nonce>.scope`), so a runaway run is throttled or OOM-killed inside it. Where no scope is available it launches the worker uncontained with a warning; on macOS it runs an in-worker watchdog instead, which stops a run past the memory or process limit without kernel enforcement. `strict` refuses any launch without kernel containment, which includes every macOS launch; `off` uses neither a scope nor a watchdog. A boolean still loads with a deprecation warning (`true` is `warn`, or `strict` beside `worker_containment_strict = true`; `false` is `off`). See [worker containment](#worker-containment). |
| `machine.worker_memory_high` | `40%` | Yes | **Advanced.** Scope `MemoryHigh=` (throttle point): bytes with optional `K`/`M`/`G`/`T`, a percentage of physical RAM, or `infinity`. |
| `machine.worker_memory_max` | `50%` | Yes | **Advanced.** Scope `MemoryMax=` (OOM point). Same grammar. |
| `machine.worker_tasks_max` | `4096` | Yes | **Advanced.** Scope `TasksMax=` (processes plus threads), at least 1. The macOS watchdog counts processes only. |
| `machine.worker_cpu_quota` | `0` (unset) | Yes | **Advanced.** Scope `CPUQuota=` as a percentage of one core (`400` = four cores); `0` sets no CPU limit. Linux only. |
| `machine.mcp.toolsets` | `[]` | Yes (`--global`) | MCP toolsets `orbit mcp serve` and `orbit mcp listen` advertise beside the role, or beside the default surface without one (`notice`, `tasks-write`, `default`, `all`, …). A server's `--toolset` replaces it. See [MCP roles and toolsets](#mcp-roles-and-toolsets). |
| `machine.github_quota.*` | `20` / `10` / `60` | Yes (`--global`) | The host's GitHub API quota reserves and refresh interval. See [`[machine.github_quota]`](#machinegithub_quota--the-hosts-github-api-quota). |
| `machine.mcp.role` | unset | Yes (`--global`) | The MCP role `orbit mcp serve` and `orbit mcp listen` serve: `operator`, `manager`, `resident`, `agent` or `reader`. Unset serves the default surface. A server's `--role` replaces it. See [MCP roles and toolsets](#mcp-roles-and-toolsets). |
| `machine.web.trusted_hosts` | `[]` | Yes (`--global`) | **Advanced.** DNS names, besides loopback, that `orbit web serve` admits as `Host`: lowercase, no scheme or port, not an IP address or `localhost`. A request through one needs a listed `Tailscale-User-Login`. Read at startup. See [Dashboard over Tailscale Serve](#dashboard-over-tailscale-serve). |
| `machine.web.trusted_logins` | `[]` | Yes (`--global`) | **Advanced.** Tailscale login names admitted through a trusted host, matched exactly. Required when `machine.web.trusted_hosts` is set: `orbit web serve` refuses to start without it. |
| `machine.web.public_url` | unset | Yes (`--global`) | HTTP or HTTPS origin readers can use to open this machine's dashboard links. Set an origin reachable from the reader, with no path, query or fragment. The Claude Code mod reads it at session start and falls back to `http://localhost:7878` when unset. See [Set the public dashboard address](https://orbit-cli.com/how-to/dashboard/#set-the-public-dashboard-address). |

### Dashboard over Tailscale Serve

The dashboard has no login and binds loopback only. Tailscale Serve can front it on the tailnet, with the tailnet identity as the login:

```sh
tailscale serve --bg 7878     # https://<node>.<tailnet>.ts.net → 127.0.0.1:7878, tailnet only
orbit config set --global machine.web.trusted_hosts '["<node>.<tailnet>.ts.net"]'
orbit config set --global machine.web.trusted_logins '["you@example.com"]'
orbit config set --global machine.web.public_url 'https://<node>.<tailnet>.ts.net'
# restart orbit web serve
```

- A request whose `Host` is a trusted host (no port, or `:443`) is admitted only with exactly one `Tailscale-User-Login` header listed in `trusted_logins`. When it carries `Origin`, or its method is `POST`, `PUT`, `PATCH` or `DELETE`, `Origin` must be exactly `https://<that host>`. A browser `Sec-Fetch-Site` other than `same-origin` or `none` is refused across API, `/healthz`, shell and embedded asset requests; loopback enforces `Sec-Fetch-Site` only on `/api` and `/healthz`, keeping loopback shell and asset navigation exempt. Anything else answers 403 `cross-origin requests not allowed`.
- Serve strips client-supplied `Tailscale-User-*` headers and sets them for tailnet users. Funnel and tagged-device requests carry none, so they are refused. The check holds only because the dashboard stays loopback-bound: any local process can send the header, the same trust loopback requests already get.
- Loopback requests are unchanged and ignore the header. Operator capability still comes only from `orbit web serve --operator`; a trusted login gets whatever that server grants.
- **Observe › Fleet › Remote access** (`GET /api/remote-access`) shows the policy the server started with, whether the saved keys differ from it (restart required once the saved pair is valid), the saved keys and the saved-policy error when the saved pair is invalid, what `tailscale serve status` routes to this listener (the port, and for an explicit `127.0.0.1` or `[::1]` target the same loopback family), and warnings when Serve sends an untrusted name here, a trusted name is not served, or Funnel is on. A saved host list without a login cannot start the dashboard, so the endpoint keeps `restart_required` false and reports `configured_policy_error`: add a login in Settings › Machine before restarting instead of restarting immediately. It lists devices seen through a trusted host (Serve's `X-Forwarded-For`, named by `tailscale whois`) with open streams and requests in the last 15 minutes, and refusals in the last 24 hours by reason. Requests are counted per second and refusals per minute, and a count stays until its second or minute has wholly left the window, so none drops early and none outlasts its window by more than a minute. A device is active while it has a request in the window or an open stream. These counts are kept in memory since the server started; loopback requests are not counted. Settings › Machine groups the two keys under **Remote access**.

### MCP roles and toolsets

Every tool belongs to exactly one MCP toolset (`orbit tool list` shows it), and a role is a named set of toolsets defined in one place (`McpRole` in `orbit-types`). `orbit mcp serve --role <role>` serves the role's toolsets plus the always-on `core` (workspace discovery); `--toolset` or `machine.mcp.toolsets` add to it. Without a role or extra toolsets, the core profile advertises task and artifact operations, friction, search, attention and workspace discovery. `orbit mcp serve --operator` selects the operator profile and operator authority (`tasks-read`, `tasks-write`, `task-repair`, `decisions`, `runs`, `dispatch`, `routines`, `search`, `exec`, `plugins`; the rarely used `automation`, `codebase`, `pipelines`, `task-triage` and `ui` toolsets are opt-in via `--toolset` or `machine.mcp.toolsets`); an explicit `--role` can narrow the profile. Managed workers use core plus explicit activity allowlist tools and admitted required or granted tools, independent of host configuration. `orbit mcp init --operator` registers the matching operator launch. A role limits only what MCP lists and dispatches; it never grants authority, so operator-only tools still need operator capability under any role. A call outside the role is refused `toolset_not_enabled`, naming the toolset, the role and the roles that include it. `--mode remote` forwards `--role` to the destination; `--mode federated` applies the role to every destination at the mux. `orbit tool list --role <role>` reports a role's tool count and approximate token size; `crates/orbit-mcp/tests/boundary/mcp_role_budgets.json` holds each role's size budget, and a test fails when a role exceeds it. Design: [federated MCP §roles](../design/federated-mcp/specs/mcp-roles.md).

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
`orbit.workflow` action `auto` accepts `auto_concurrency` on `mode: start`; HTTP
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
profiles `orbit run profiles` reports. See [auto-concurrency §6](../design/auto-concurrency/2_design.md#6-pull-drains).

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
[deprecated](validation.md#validation-and-errors).

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
[operator runbook](../runbooks/forecast-accuracy.md). Records older than 30
days are pruned by `orbit gc runs --confirm`, except the newest 10,000 forecasts with
the host minutes they pair with, and each workspace, work class and complexity's
newest 200 deliveries. See
[auto-concurrency §8.1](../design/auto-concurrency/2_design.md#81-the-accuracy-report).

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
([auto-concurrency §7](../design/auto-concurrency/2_design.md#7-surfaces-and-configuration)).

CPU utilization is bounded at 100% and is not compared with the resource
throttle's CPU percentage. The auto-concurrency memory forecast and the
resource throttle's memory percentage both measure non-reclaimable memory.

- `orbit config set` refuses `machine.id` and `machine.task_prefix`: changing either would orphan or renumber records minted under it.
- `orbit init` refuses to create an identity once the task store has minted ids under another prefix (tasks created before `orbit init` use the historical `ORB`). It fails before writing anything; run `orbit init` before creating tasks.
- Hand edits fail closed: a `[machine]` table missing an identity key, a `task_prefix` that contradicts the local task allocator, or an `id` that contradicts a workspace record naming this machine as owner is refused. Nothing falls back to the hostname.
- A legacy `~/.orbit/host.toml` left as the only identity source is refused by name: copy `machine_id` as `[machine] id`, `host_id` as `machine.name` and `task_prefix` as `machine.task_prefix` into `~/.orbit/config.toml` by hand, then delete `host.toml`. When `[machine]` already exists, a leftover `host.toml` is ignored.
- `orbit init --force` deletes the global root, machine identity and executor sandbox settings included, before seeding defaults, so it creates a new machine ID and prompts for name and prefix. Non-interactively, pass both `--machine-name` and `--task-prefix`; missing or invalid flags are rejected before anything is deleted. Plain `orbit init` keeps an existing identity.

### Worker containment

- Without a systemd user manager (containers) under `warn`, or with `worker_containment = "off"`, workers run in the caller's cgroup and each Linux Orbit process logs one warning. macOS uses a watchdog in each running worker under `warn`: once per second it sums `phys_footprint` across the tracked process tree and its owned sessions, and counts processes (not threads). Above `machine.worker_memory_max` or `machine.worker_tasks_max`, it records a bounded snapshot of the eight largest consumers (command, PID/start key, memory and CPU rate), sends descendants SIGTERM, then SIGKILL after one second, records the run as failed, and terminates the worker last. The watchdog survives the submitting CLI exiting. `off` disables it. Memory percentages resolve against physical RAM; `infinity` disables only the memory ceiling. The five-second profile sampling cadence stays unchanged.
- `strict` retains its kernel-enforced meaning and refuses macOS watchdog launches. It starts no uncontained worker: the run reports why and how to enable a user manager or turn strict mode off, with error code `worker_containment_unavailable` (also in CLI JSON errors). The deprecated `worker_containment_strict = true` reads as `strict`, and fails config load beside `worker_containment = false`. `--strict-worker-containment` on `orbit run ship` or `orbit run auto` turns it on for one invocation; the auto coordinator passes it to leaf workers as `ORBIT_WORKER_CONTAINMENT_STRICT`.
- macOS `orbit doctor` and submission notes report **watchdog**, never kernel containment. It does not implement `worker_memory_high` throttling or `worker_cpu_quota`. One-second polling reduces overshoot compared with the five-second telemetry interval but cannot prevent fast allocations between checks. A daemon that calls `setsid` after leaving the tracked tree, an unseen short-lived session, and descendants of an escaped process are not found. Unreadable snapshots skip a check; the watchdog cannot survive its own worker being killed or a host failure. It is a best-effort watchdog, not a kernel security boundary.
- Every Linux worker, contained or not, starts with an `oom_score_adj` of at least 500, inherited by everything it runs, so a host-wide OOM kills a worker before the user systemd manager (100) or the drain that launched it. It is not configurable; scope units take no `OOMScoreAdjust=`, so the worker sets it on itself before `systemd-run` execs it.
- `machine.worker_cpu_quota` adds `CPUQuota=` to each scope, so one run cannot take every core. Only an unsigned integer loads, with no upper bound. `orbit doctor`'s `worker-containment` row reads `info` while it is unset and `ok` once it is set. systemd enforces it only where the user manager delegates the cpu controller; elsewhere the property is accepted and not enforced.
- A Linux script step's own `orbit-script-*` scope takes the same `machine.worker_*` limits. `orbit script grant --memory-max`, `--tasks-max` and `--cpu-quota` narrow them for that script: each grant value applies only where it is lower than the machine value, and a grant CPU quota applies even while `machine.worker_cpu_quota` is unset.
- A macOS watchdog breach names top-consuming child commands and memory bytes in `orbit run show`. The same snapshot is kept with the run as resource evidence, one bounded record per run until that run is deleted, and survives worker termination; `orbit run show --json` returns it under `worker_resource_limit`.
- A run that fails after hitting a worker limit carries error code `worker_resource_limit` in `orbit run show`. A run interrupted because its worker vanished names the worker's exit and scope evidence instead ([stuck job runs › How the worker ended](../runbooks/stuck-job-runs.md#how-the-worker-ended)). A run the host manager stopped (a terminating signal whose recorded sender is `systemd`, with no memory or task-limit breach) is `held` with `operator_stopped` and is not a failure. Inspecting scopes: [operational logs › Worker Resource Containment](../../crates/orbit-core/assets/skills/orbit-setup/references/operational-logs-containment.md#worker-resource-containment).
- Admitted names reach the contained worker through its environment, not the `systemd-run` command line. Orbit passes each name as `--setenv=NAME` with no value. systemd (211 and later, including 259) copies the value from the wrapper process, which already holds the allow-listed environment. That includes pass-listed provider credentials and clock-file defaults. A toolchain `PATH` override, an explicit removal, and the workspace allowlist are unchanged. Agent subprocesses still start from a cleared environment.

---

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

A workspace can require capabilities for every task carrying a tag:

```toml
[workflow.tag_requires_labels]
ui = ["visual-fixture"]
```

The effective requirement is the sorted, deduplicated union of the task's
`requires_labels` and the labels mapped by **all** its tags. Tags match after
trimming and lowercasing; blank tags, duplicate normalized keys and invalid
labels are refused at config load. An empty array disables an inherited
mapping for that tag. Workspace entries replace global entries with the same key spelling.
The default map is empty. Derived labels are evaluated at each admission,
including tasks filed before the mapping existed; they are never stored in
`requires_labels`. Clearing the task field therefore leaves tag requirements
in force. Local discovery, explicit ship, readiness, placement waits, creation
crew draws, pull admission (including repairs) and executor-census affinity use
the same effective set. The owner applies its mapping to pull requests; a
follower's workspace configuration cannot weaken it. The built-in `ui`
browser-kit requirement remains in force as well.

Orbit's [workspace policy example](../../config/orbit-workspace.toml) maps `ui`
to `visual-fixture`. During rollout, merge that table into the owning workspace
and add `visual-fixture` to `machine.labels` only on hosts where
`scripts/visual-fixture-server.sh busy-workspace` has been verified **inside
the managed worker runtime**, with a disposable home and workspace. A working
browser kit or a host-shell fixture run alone does not establish that capability.
A host whose worker denies the backend must omit the label. If no serving host
carries it, UI tasks wait; the policy never infers a capability from the OS.

## `[machine.github_quota]` — the host's GitHub API quota

Global-only. Every `gh` call Orbit makes against the GitHub API on a host spends the same account quota: GitHub's REST (`core`) and GraphQL (`graphql`) buckets, 5,000 calls an hour each for a user token, counted apart. Orbit keeps one record per host, GitHub account and resource and refuses a read locally, without calling GitHub, before an agent's reads can stop a delivery [ORB-25868].

```toml
[machine.github_quota]
agent_reserve_percent = 20
background_reserve_percent = 10
refresh_interval_seconds = 60
```

| Key | Default | What it is |
|---|---|---|
| `machine.github_quota.agent_reserve_percent` | `20` | Share of each resource's limit agent reads may not spend: an agent's `github.*` tool call is refused once the remaining count is at or below it. `0` disables the refusal; valid range `0..=100`. |
| `machine.github_quota.background_reserve_percent` | `10` | The same for background reads: the CI and security-alert sweeps, auto-task sources and `github.*` calls a deterministic job step makes. `0` disables the refusal; valid range `0..=100`. |
| `machine.github_quota.refresh_interval_seconds` | `60` | **Advanced.** Shortest interval between two reads of GitHub's rate-limit endpoint on the host that the caller's own timeout budget did not cut short (see **Clipped reads** below), however many processes read, delivery completion's reset lookup included; valid range `10..=3600`. The endpoint does not count against the quota. |

- **Caller classes.** Delivery (the private VCS operations behind PR open, completion, landing and merge, and the task PR forge) keeps no reserve and runs the quota to zero. Background reads stop at their reserve, agent reads at theirs. Any other `github.*` call, including attention's CI read, is an agent read.
- **Hold after a rate-limit answer.** After GitHub's authenticated primary rate-limit answer for a resource, or a rate-limit read showing it empty, every read of that resource on the host, delivery included, is refused locally until GitHub's reset (its `Retry-After` when no reset is known, else one refresh interval). The other resource is unaffected.
- **Refusal.** A local refusal is the same typed throttle as GitHub's own answer: it names the resource and the time reads may resume and tells an agent not to retry before then, so a completion or landing waits for the reset under its existing deferral policy and a sweep defers as it does for GitHub's answer. `github.run.logs` returns the refusal as an error even when it refuses the job-log fallback after an empty, not-yet-ready or invalid-zip run log read, for a scoped (step or window) read as for an unscoped one, instead of an empty success or a missing-selection error.
- **Unknown.** An unreadable or unauthenticated rate-limit answer leaves reads allowed and reports the quota `unknown`, never zero remaining. A failure to read or write the record never refuses a read; a process that can read but not write it still applies the refusals.
- **Clipped reads.** A rate-limit read that the caller's own timeout budget cuts short (a zero budget, or a probe given less than the 15-second standard timeout that runs out) records nothing: the record keeps its previous reading and no refresh error, and the read does not use up the refresh interval. The interval is measured from the last read that was not cut short, so the next caller may read the endpoint sooner than one interval after a clipped read. A probe that runs out the full 15-second timeout, or any other failure, is an unknown answer and does start the interval.
- **Record.** `<Orbit data root>/state/github-quota/<host>-<fingerprint>.json` (`$ORBIT_ROOT` when set, else `~/.orbit`), one file per GitHub host and credential; the fingerprint is a truncated SHA-256, never the token. It holds, per resource, the limit, remaining count, reset and observation time, any hold, and the calls each class made and had refused in the current window. Writes take a lock beside the file and land through an atomic rename. A request with a finite timeout waits for that lock, and for the rate-limit read, no longer than what is left of its own budget; once the budget is spent it returns its timeout without calling GitHub, and that request is not counted against the record. A caller with no finite timeout, such as the `github.auth.status` quota report, keeps the standard lock wait. Only the `orbit` binary guards its `gh` calls.
- **Status.** `orbit tool run github.auth.status` reports the record under `quota`: per resource the limit, remaining count, reset time (`state: unknown` when GitHub's answer could not be read), whether agent, background or delivery reads are currently refused, and each class's call and refusal counts in the current window.
