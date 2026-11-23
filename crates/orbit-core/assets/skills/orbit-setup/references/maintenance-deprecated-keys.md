# Maintenance: deprecated keys and cleanup verification

## Contents

- [Deprecated keys: translated, ignored or fixed](#deprecated-keys-translated-ignored-or-fixed)
- [Verify the cleanup](#verify-the-cleanup)

## Deprecated keys: translated, ignored or fixed

These keys still load and warn on every load, and `orbit config get` and `set`
refuse them (except `machine.worker_containment`, which stays a settable key;
only its boolean value form is deprecated). `orbit doctor --problems` names each
one with the file that sets it, and `orbit doctor --fix-retired-config-keys` acts
on it as the tables below say. A key past its recognized date becomes an unknown
setting, which the same check reports.

Where a row gives no date, the load
warning gives none either.

**Translated** keys are rewritten by the repair into their replacement in the
same file, then removed. Except for `machine.worker_containment` (whose boolean
value form the repair rewrites in place while keeping the key), the repair writes
a replacement only when that file does not already set it, so an explicit modern
value wins and the old key is simply removed.

| Key | Becomes | Recognized until |
|---|---|---|
| `operation.review_policy` | `before-pr` sets `review.timing = "before_pr"`, a deprecated value that still works (prefer `before_landing` or `off`). `after-landing` and `none` turn neither review on: after-landing review is the `delivery-code-review` auto-task's own flag, set with `orbit auto-task toggle delivery-code-review on` or `off`. A `[review]` table in the same file wins. | None stated |
| `operation.review_minutes` | `review.minutes`, now one candidate's review limit rather than a lineage total | None stated |
| `operation.review_crew` | `workflow.purpose_crews.review`, the same crew or pool | None stated |
| `workflow.final_recovery_crews` | `workflow.purpose_crews.final_recovery`, the same pool | None stated |
| `workflow.purpose_crews.ci_failure_sweep`, `dependabot`, `merged_head_repair` | `workflow.purpose_crews.sweep_tasks`, the one pool every sweep-filed task draws from. Load fails while one file sets them to different pools. | None stated |
| `review.before_pr`, `review.before_landing` | `review.timing`: `before_pr`, `before_landing` or `off`. Both set to `true` fails load. | None stated |
| `machine.worker_containment` as `true` or `false`, with `machine.worker_containment_strict` absent | `"warn"` or `"off"` (rewritten in place; key stays settable) | None stated |
| `machine.worker_containment_strict` | `machine.worker_containment = "strict"` when `true`; `false` keeps the boolean's mode. Refused beside a `false` boolean. | None stated |
| `workflow.crew_performance_types` | `workflow.task_types`, the same ordered entries | 2026-12-07 |

**Ignored** keys have no value to set. Delete them.

| Key | What to do instead | Recognized until |
|---|---|---|
| `workflow.system_crew` | Nothing. System work draws its crew from the `workflow.<tier>_complexity_crews` pools. | None stated |
| `workflow.auto_ship` | Nothing. Dispatch with `orbit run auto` drains or `orbit run ship`. | None stated |
| `workflow.validation_env.login_shell`, `workflow.validation_env.interactive` | Nothing. Required validation always probes the interactive login shell, falling back to a login shell. | 2026-12-08 |
| `commit.planned_by_trailer`, `commit.implemented_by_trailer` | Nothing. Orbit no longer writes `Planned-By` or `Implemented-By` trailers. | None stated |
| `workflow.explore.*`, `workflow.purpose_crews.explorer` | Nothing. Covered by the Explorer row in the removed-features table. | No date set |
| `plugin.legacy_callback_identity` | Nothing. Plugin callbacks need the host-issued session descriptor; the environment token and process ancestry no longer authorize them. | None stated |
| `execution.env.inherit` | Nothing. Inheritance is fixed off. Doctor reports this key as an unknown setting, with a did-you-mean hint. | None (unknown setting) |

**Fixed-bound** keys are ignored because Orbit now uses the value shown. Delete them.

| Keys | Fixed value | Recognized until |
|---|---|---|
| `workflow.admission_affinity` | `band` | 2026-12-08 |
| `workflow.auto_effort.explore_share` | `0.1` | 2026-12-08 |
| `workflow.provider_limit_max_used_pct` | 90% skip threshold. `workflow.provider_limit_overrides` still sets it per provider or crew. | 2026-12-08 |
| `workflow.usage_pressure_cap`, `workflow.usage_pressure_strength` | 8 and 1. `workflow.crew_selection` still chooses the draw. | 2026-12-08 |
| `workflow.resource_throttle.cpu_light_leaves`, `io_full_high_percent`, `io_full_resume_percent` | 2 CPU-light leaves under CPU-only pressure; throttles on Linux disk I/O stalls at 20%, resuming below 10% | 2026-12-08 |
| `workflow.validation_env.shell_probe`, `path_mode`, `cargo_incremental` | Login-shell probe as above; `workflow.validation_env.path` is prepended to the resolved PATH; `CARGO_INCREMENTAL=0` for bound host validation | 2026-12-08 |
| `[attention]` table | Cursors last 24 hours; failed runs are read back 48 hours; a run is stuck after 120 minutes; CI reads are cached 60 seconds; source reads time out after 10 seconds; cross-host fan-out has a 20-second budget; 200 candidates per workspace; severity rises at 24 and 72 hours | 2026-12-08 |
| `automation.stall_window_minutes` | 60 minutes | 2026-12-08 |
| `ci_failure.operator_suppression_hours` | 6 hours | 2026-12-08 |
| `execution.agent_stall_minutes`, `execution.proc_spawn_max_timeout_minutes` | 30 minutes idle before a provider step stalls; 45 minutes per `proc.spawn` call | 2026-12-08 |
| `experiments.max_running`, `experiments.min_delivered_rate` | 3 experiments at once; 0.9 delivered rate for a verdict | 2026-12-08 |
| `jobs.failure_streak_warn` | 5 consecutive failed runs | 2026-12-08 |
| `machine.application_consent_max_days`, `machine.host_shutdown_hold_minutes` | 30 days; 180 minutes. Global `config.toml` only. | 2026-12-08 |
| `machine.auto_concurrency.admit_memory_percent`, `profile_window`, `horizon_cap_minutes`, `head_max_wait_minutes`, `head_stuck_passes` | 75% memory ceiling; 50-delivery profile window; 120-minute horizon cap; 30-minute and 20-pass starvation guard. `mode`, `admit_cpu_percent` and `min_profile_deliveries` stay settable. | 2026-12-08 |
| `resident.evolution.window_days`, `resident.evolution.decline_suppress_days` | 14 days of cost; a declined proposal stays declined 30 days unless the decline names its own length | 2026-12-08 |
| `retention.journal_days`, `retention.store_warn_bytes` | 14 days; `orbit doctor` warns about a store file over 1 GiB. `retention.audit_days` and `retention.runs_days` stay settable. | 2026-12-08 |
| `runtime.log_retention_days`, `runtime.log_max_total_mb`, `runtime.log_max_file_mb` | Archives kept 7 days within 500 MiB; the log rolls at 100 MiB. Delete these keys from `[runtime]`. | 2026-12-08 |
| `security_alert_sweep.min_severity` | Moderate and above. A sweep job's own `min_severity` input still overrides it. | 2026-12-08 |
| `worktree.parked_reclaim_after_hours` | 336 hours (14 days) | 2026-12-08 |
| `[notice]` table | Fleet notice limits at their former defaults; agent posting and brief delivery stay on | 2026-12-08 |
| `pr.close_on_terminal`, `pr.draft`, `pr.freshness_section`, `pr.orchestration_trailer`, `pr.screenshot_section`, `pr.signature`, `commit.orchestrated_by_trailer`, `commit.regression_trailers` | Delivery is fixed: it closes a task's open Orbit PRs when the task ends, opens PRs ready for review (a draft only for a required-section gap), and always adds the Screenshots table for `ui` tasks, the Branch Freshness section, the model signature, and the Orchestrated-By and regression trailers | 2026-12-08 |

Three groups are not enumerated in the tables above, and each needs its own step:

- **Unknown keys and tables** (anything the registry does not know, including a
  mistyped key) are reported as `unknown and ignored`, with a did-you-mean hint
  where one exists. The repair removes them too, so read the list first when a
  newer Orbit binary shares the file.
- **A legacy `after-landing` review policy** is never removed automatically.
  Migrate it as [Diagnose and repair](maintenance-diagnose.md#diagnose-and-repair) describes.
- **`[routines]`** (`role = "source"`) loads with a warning, but `orbit doctor`
  does not list it and the repair does not remove it. Delete the table by hand.
  It has no recognized-until date.

## Verify the cleanup

1. List what still sets a retired key, with the file that sets it. The check
   covers the global and workspace `config.toml`:

   ```bash
   orbit doctor --check retired-config-keys
   orbit doctor --problems      # lists every retired, deprecated and unknown key
   ```

   `--check` prints only the first key it finds, so use `--problems` when several
   are set.

2. Remove the leftover keys in one pass. It backs up each edited `config.toml`,
   writes each translated replacement before removing the old key, and writes
   nothing on a second run. It refuses while a legacy `after-landing` review
   policy is set; migrate that first, as [Diagnose and repair](maintenance-diagnose.md#diagnose-and-repair)
   describes.

   The repair removes keys, not list entries, so delete
   `agent-invoke` from `machine.mcp.toolsets` and from any `--toolset` argument
   by hand. It also leaves a `[routines]` table in place; delete that table by hand.

   ```bash
   orbit doctor --fix-retired-config-keys
   ```

3. Confirm the load is clean and the removed commands are gone:

   ```bash
   orbit doctor --problems      # no retired-config-keys warning should appear
   orbit config show --all      # loads with no deprecation warning; this also
                                # catches a [routines] table, which doctor does not list
   orbit agents --help          # `ladder` is not listed
   orbit visual                 # reports an unrecognized subcommand
   ```

A key that stays past its recognized date becomes an unknown setting. The same
`retired-config-keys` check reports it, so one check covers both states.
