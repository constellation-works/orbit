# Configuration: common settings for the host and execution

## Common settings: host, execution and machine

| Key | Purpose |
|---|---|

| `workflow.distributed_completion` | Standing owner authority for accepted distributed-drain handoffs: `review` (default, each handoff waits for operator approval) or `done` (acceptance authorizes owner landing). Owner configuration only; landing rechecks it. See [Distributed-drain completion policy](configuration-completion.md#distributed-drain-completion-policy). |
| `tasks.id_start` | Floor for this machine's task-id allocator; forward-only. → [multi-host.md](multi-host.md) |
| `execution.env.pass` | Environment variable names allow-listed into agent subprocesses. |
| `execution.env.login_auth` | Providers (`claude`, `codex`) authenticated by the CLI's own login; their unset credential names in `pass` are not reported. |
| `execution.codex.sandbox` | `read-only`, `workspace-write`, or `danger-full-access`. |
| `execution.codex.approval_policy` | `untrusted`, `on-request`, or `never`. |
| `machine.auto_concurrency.mode` | Forecast admission: `off` (default) builds no forecast, `shadow` records what forecasting would admit while retaining ordinary admission, `enforce` uses forecast fit to decide admissions within the worker ceiling. |
| `workflow.resource_throttle.enabled` | **Advanced.** Default `true`; boolean. Controls whether host pressure holds new admissions; `false` still reports telemetry and severity. Preserve the operator's policy when diagnosing a hold. |
| `workflow.resource_throttle.cpu_source` | Default `load`; choices `load`, `utilization`. Linux load is one-minute load divided by online CPUs (includes I/O wait and can exceed 100%); utilization uses busy tick deltas excluding idle and I/O wait. macOS uses Mach utilization for either choice. Verify the source in readiness and doctor. |
| `workflow.resource_throttle.cpu_high_percent`, `workflow.resource_throttle.cpu_resume_percent` | CPU hold/resume marks; defaults `90` / `85`, integers `1..=100`, resume strictly below high. |
| `workflow.resource_throttle.memory_high_percent`, `workflow.resource_throttle.memory_resume_percent` | Non-reclaimable memory hold/resume marks; defaults `90` / `85`, integers `1..=100`, resume strictly below high. |
| `workflow.resource_throttle.disk_high_percent`, `workflow.resource_throttle.disk_resume_percent` | Space-used hold/resume marks for each watched filesystem; defaults `90` / `85`, integers `1..=100`, resume strictly below high. |
| `machine.auto_concurrency.admit_cpu_percent` | Forecast CPU limit as a percentage of host cores (default 90; 1..=100). The memory limit is fixed at 75% of non-reclaimable memory, so global `workflow.resource_throttle.memory_high_percent` must be above 75. |
| `machine.auto_concurrency.min_profile_deliveries` | Completed deliveries a profile key needs before admission uses it without fallback (default 10; at most the fixed 50-delivery profile window). |
| `machine.github_quota.agent_reserve_percent` | Share of the host's GitHub API quota per resource that agents' `github.*` reads may not spend (default 20; 0 disables). Delivery keeps no reserve; after a GitHub rate-limit answer every read of that resource on the host waits for the reset. `orbit tool run github.auth.status` shows the record under `quota`. |
| `machine.github_quota.background_reserve_percent` | The same for background reads: sweeps, auto-task sources, deterministic job steps (default 10; 0 disables). |
| `machine.github_quota.refresh_interval_seconds` | Shortest interval between two rate-limit endpoint reads on the host that the caller's own timeout did not cut short (default 60; 10..=3600). A cut-short read records nothing and does not start the interval. |

Host pressure holds admission after high readings span ten seconds and releases
only below the resume mark; every resume mark must be strictly below its high
mark. Running work is never cancelled or paused. Use `orbit config show --all`
for all effective marks, `orbit run readiness --json` for the held resource,
reading, threshold and CPU source (`capacity.resource_cpu_source`), and
`orbit doctor` for the effective source and host diagnosis.

Explain a hold from
those readings and let pressure clear; do not raise marks or disable the
throttle to push waiting work through.

Some limits are fixed rather than configured. A provider with no
stdout/stderr and no CPU use for 30 minutes fails its step as stalled, entering
normal retry/recovery. One `proc.spawn` call inside a managed activity may run
at most 45 minutes, also capped at the activity's remaining wall-clock budget
(60 seconds outside an activity).

Forecast admission keeps each profile key's
last 50 completed deliveries, forecasts at most 120 minutes ahead, protects a
queue head that has failed to fit for 30 minutes, and reports it stuck after 20
protected passes without improvement; a stuck report does not change
admission. New unattended admissions hold when a scheduled shutdown or reboot
is 180 minutes away or sooner; later schedules are reported while admission
continues.

The JSONL log rolls past 100 MiB, keeps archives for 7 days and
prunes the oldest past a 500 MiB total. The keys that once set these
(`execution.agent_stall_minutes`, `execution.proc_spawn_max_timeout_minutes`,
`machine.auto_concurrency.admit_memory_percent`, `profile_window`,
`horizon_cap_minutes`, `head_max_wait_minutes` and `head_stuck_passes`,
`machine.host_shutdown_hold_minutes` and `runtime.log_*`) load with a
deprecation warning, are ignored, and stop being recognised after 2026-12-08.

Delete them.

`[machine]` settings are global-only. An authorized local drain can select its
own forecast mode with `orbit run auto --auto-concurrency <MODE>` (`off`,
`shadow` or `enforce`); MCP `orbit_workflow` action `auto` `mode: "start"` accepts the
same `auto_concurrency` input. When omitted, each admission pass follows the
configured `machine.auto_concurrency.mode`. Explicit overrides survive
restart/resume without a configuration write.

Host thresholds and effective
provider-limit policy refresh next pass, caching unchanged config by mtime;
invalid config warns and retains the previous policies. `orbit run show <id> --json` and
`orbit run readiness --json` expose `mode`, `source` (`explicit` or
`configured`), and the current `configured_mode`. Pull drains refuse the
override and use the executor host's configured mode.
Inspect `orbit config keys --json` and `orbit config show` for the installed
schema and effective policy.

Forecasting does not raise the drain's
`--concurrency` worker ceiling or bypass resource throttles. A per-drain mode
choice does not authorize dispatch or changes to global policy, host thresholds,
or shutdown holds. Use read-only `orbit run profiles`, `orbit run profiles
--accuracy`, and `orbit run readiness --json` to explain waiting work; preserve
operator settings unless an authorized change is requested.

See
[operational-logs-cost.md](operational-logs-cost.md#delivery-profiles-and-admission-diagnostics).
