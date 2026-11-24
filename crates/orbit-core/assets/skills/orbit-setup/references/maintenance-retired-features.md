# Maintenance: retired features and their keys

## Retired features and their keys

Orbit removes features across releases. A removed feature can leave configuration
keys or an MCP toolset name behind; a removed command simply stops existing.
Orbit still loads the leftover keys and toolset names, warns on every load and
ignores them. Delete each leftover, then verify the load is clean. Keys that were
only renamed or moved are translated on load and rewritten by
`--fix-retired-config-keys`.

This table lists removed features; the deprecated-key
[tables below](maintenance-deprecated-keys.md#deprecated-keys-translated-ignored-or-fixed) cover the renamed, ignored and fixed-bound keys, with what each one
becomes.

| Removed feature | Leftovers to delete | Replacement | Recognized until |
|---|---|---|---|
| Explorer and agent invocation | `[workflow.explore]` table; `workflow.purpose_crews.explorer`; `agent-invoke` in `machine.mcp.toolsets` and in any `--toolset`. `orbit run agent` and the `orbit.agent.invoke` tool are gone. Stored runs and experiment readouts stay readable. | None in Orbit: use task workflows or a provider CLI. | No date set; warns until a later release makes it an error |
| Crew ladder | `[workflow.crew_ladder]` table. `orbit agents ladder` is gone. | Route each tier with `workflow.<tier>_complexity_crews`; compare crews with [experiments](configuration-experiments.md#experiments). | 2026-12-07 |
| Regression-risk prompt marks | `[workflow.regression_risk]` table, including `record_sites` | None. Task riskiness scores use fixed defaults: a 30-day window and at least 5 done tasks. | 2026-12-07 |
| Pilot explore brief | `[workflow.explore_brief]` table | None. Existing `explore-brief.json` artifacts stay readable. | 2026-12-07 |
| Visual review | `[visual_review]` table; `workflow.purpose_crews.visual_review`. `orbit visual` and the `orbit.visual.*` tools are gone. | None. `mocks/` and `visual/` are ordinary artifact names now. | 2026-12-07 |
| Crew-performance weighting | `workflow.crew_performance`, `crew_performance_strength`, `crew_performance_cap` and `crew_performance_window_days` | None for the weighting. The older `workflow.crew_performance_types` name is read as `workflow.task_types`; the repair rewrites it. | 2026-12-07 |
| Weekly evolution pass | `resident.evolution.min_occurrences`, `min_distinct_cycles`, `min_distinct_mandates`, `min_cost_minutes` and `max_proposals` | None. `orbit evolution` and `orbit resident digest` stay, and nothing files proposal tasks automatically. The ledger's 14-day window and 30-day decline are fixed, see [deprecated keys](maintenance-deprecated-keys.md#deprecated-keys-translated-ignored-or-fixed). | 2026-12-07 |
| UX rules | `[ux_rules]` table | None. No routine or installed command runs the rules. | 2026-12-07 |
| Finding verification | `review.verify_findings` | None. The task pilot's `finding_invalid` result and an implementer's `finding-invalid.json` still reject an invalid finding. | 2026-12-07 |
| Agent scoreboard | `[scoring]` table (`scoring.enabled`) | None; nothing records scoreboard metrics. | 2026-11-12 (window closed; now an unknown key) |
| Dashboard browser sessions | `[machine.web_terminal]` and `[machine.web_session]` tables; `machine.web_terminal.enabled`, `machine.web_session.ttl`. `orbit web open` is gone. | None for browser sessions. Open the dashboard URL directly; `orbit web serve --operator` still provides operator capability. | 2026-12-08 |
| Routine publication attachments | `publication.routine_attachments`, `publication.routine_allow_unscanned_attachments` | None for the scheduled routine, which always omits attachments. Publish attachments by hand with `orbit task publication publish --attachments`. | 2026-12-08 |
| Selective before-landing review | `[review.selective_before_landing]` table; `review.selective_before_landing.mode`, `risk_threshold`, `paths`, `unknown_risk` | Review before landing with `review.timing = "before_landing"` or `review.before_landing_hosts`. | 2026-12-08 |
| Decision assistance | Global `[assistance]` table. `orbit decision assist` is gone. | None. | 2026-12-08 |
| Path leases | `[lease]` table / `lease.*` keys. `orbit lease` and the `orbit.lease.*` tools are gone. | None. The lease machinery stays disabled. | 2026-12-08 |
| Orchestrator scorecard | `orbit orchestrator`; `--role orchestrators` on `orbit agents scorecard` and `orbit agents show`; the orchestrator scorecard's MCP tool; the `/api/agents/orchestrators` endpoint; any link to the dashboard Orchestrators tab. | None for orchestrators. The dashboard Orchestrators tab is replaced by Rank, which ranks implementer models with `orbit agents rank`. For crew comparison, use [experiments](configuration-experiments.md#experiments). | No window stated; removed in 0.48.0. |
| MCP tool-family names | Per-action tool names such as `orbit_friction_add`, `orbit_friction_update`, `orbit_task_artifact_get` and `orbit_task_artifact_put`, in scripts, prompts and MCP client allow-lists. Stored names in existing records migrate on upgrade, so there is nothing to delete there. | The family tool with an `action` field: `orbit_friction` with `add` or `update`, and `orbit_task_artifact` with `get` or `put`. See [tool surface](../../orbit/references/tool-surface-routing.md). | No window stated; removed in 0.48.0. |
| MCP task schema string forms | A bare string where a list is expected on `orbit_task_add`, `orbit_task_update` and `orbit_task_list`, such as in `acceptance_criteria` or `context_files`. | Send a JSON array for each list field, such as `acceptance_criteria`, `context_files` and `relations`. | No window stated; removed in 0.48.0. |
| Per-call `model` input and task aliases | The `model` input on MCP task calls, which the MCP schema no longer lists; the `agent` alias of `orbit_task_list`; the `context` alias on `orbit_task_add` and `orbit_task_update`. Orbit still accepts a sent `model` as an unadvertised identity key, and the session's own identity takes precedence over it. | Identity comes from the session's crew configuration or the MCP client's `clientInfo`, not from the call. Use `context_files` in place of `context`. For `agent` on `orbit_task_list`, read the list's field names with `orbit tool show orbit.task.list`. | No window stated; removed in 0.48.0. |
| Retired activity-backend repair | `orbit doctor --fix-retired-activity-backends` is gone. A schemaVersion 2 `agent_loop` activity file that still declares the scalar `spec.backend: http` or `spec.backend: auto` warns on `orbit doctor`, naming the file and the rejected value. | None. Delete only that scalar key from the activity file by hand, then verify with `orbit doctor`. Leave a plugin manifest's structured `spec.backend` command backend (timeout and sandbox settings) in place; it is still supported. | No window stated; removed in 0.50.0. |
| Executor type `cli_command` | `spec.executor_type: cli_command` in an Executor definition. After changing it, verify with `orbit doctor providers` (a subcommand of `orbit doctor`): it lists each executor and fails on a definition that does not load. | `spec.executor_type: local_shell`, the deterministic action. The `command`, `args`, `env` and `timeout_seconds` fields are unchanged. | Not recognized: a definition with this value fails to load until the value changes. |

### Retired seeded routines

These retired templates let Orbit recognize routines it seeded in earlier
releases. `orbit workspace sync` retires an unchanged seeded copy and preserves
operator-authored routines.

| Retired routine | Replacement or current behavior | `orbit workspace sync` effect |
|---|---|---|
| `auto_task_scheduler` | Each clock tick evaluates auto-task definitions directly, so this routine has no job to fire. | Retires the unchanged seeded copy; the retained template lets Orbit distinguish it from an operator-authored routine. |
| `blocked_task_triage` | Replaced the retired `blocked-task-triage` auto-task. Final recovery handles blocked tasks; `escalation_triage_pipeline` remains for pilot-warning triage through the dashboard's Triage action or `orbit run job escalation_triage_pipeline`. | Workspace sync retires the unchanged seeded routine, separately from the predecessor auto-task; it does not enable this routine. |
| `crew_memory_dream` | Orbit no longer seeds the weekly routine. The `crew_memory_dream_pipeline` job still ships and can run through an optional routine or an explicit job run. | Retires the unchanged seeded copy; the retained template lets Orbit distinguish it from an operator-authored routine. |
| `crew_performance_refresh` | This enabled hourly snapshot routine and crew-performance weighting were removed. Route by task type with `workflow.type_complexity_crews`. | Retires the unchanged seeded copy. |
| `evolution_pass` | The weekly proposal routine was removed; the evolution ledger uses a fixed 14-day window and 30-day decline. | Retires the unchanged seeded copy. |
| `finding_verification` | The routine was removed; the task pilot's `finding_invalid` result and an implementer's `finding-invalid.json` still reject an invalid finding. | Retires the unchanged seeded copy. |
| `pilot_warning_triage` | Retired through its `pilot_warnings_held` state trigger; use the dashboard's Triage action for a task held on pilot warnings. | Retires the unchanged seeded copy; the pipeline remains available for dashboard triage. |
| `ship_sweep` | The scheduled ship sweep is gone. Dispatch with `orbit run auto` drains or ship on demand with `orbit run ship`; this routine has no job to fire. | Retires the unchanged seeded copy; the retained template lets Orbit distinguish it from an operator-authored routine. |
| `task_triage` | Terminal failed-run triage is gone: failed runs park tasks in `blocked` with the failure attached, and re-backlogging is a deliberate human transition. | Retires the unchanged seeded copy; the retained template lets Orbit distinguish it from an operator-authored routine. |
| `ux-rules` | The routine and installed command were removed; nothing runs UX rules on a schedule. | Retires the unchanged seeded copy. |

A
routine you wrote that targets one of the three removed jobs
(`finding_verification_pipeline`, `evolution_pass_pipeline`,
`ux_rules_pipeline`) loads as retired and never fires unless the workspace
defines a job of the same name, which resolves first and stays active;
otherwise remove it or retarget it.

A routine on the retired
`pilot_warnings_held` trigger loads as retired even though its
`escalation_triage_pipeline` job still ships, so a cron routine targeting that
job without the retired trigger stays active. Triage a
task held on pilot warnings from the dashboard's Triage action.

### Retired activity backends

The `orbit doctor --fix-retired-activity-backends` repair is gone: no flag
cleans up activity files for you. A schemaVersion 2 `agent_loop` activity file
that still declares the retired scalar `spec.backend: http` or
`spec.backend: auto` is a warning naming the file and the rejected value.
Delete only that scalar key from the activity file by hand, then verify with
`orbit doctor`.

Do not remove a plugin manifest's structured `spec.backend`
command backend (with its timeout and sandbox settings): that is a live plugin
field and stays in place.
