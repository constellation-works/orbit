# Keeping a workspace healthy

Day-2 operations. The first section is the one that actually bites busy
workspaces; the rest is periodic hygiene.

## Worktree garbage collection

Implementation pipelines create worktrees; deterministic collection or filing
jobs need not. With the default seeding policy, a fresh workspace seeds the
`worktree-gc` routine enabled, but automatic reclamation needs the host's Orbit
scheduler clock installed and enabled. Without
that clock, a workspace that ships on a schedule accumulates worktrees until the
disk fills. **Make sure the host clock is running before, not after, scheduling
ship traffic.**

```bash
orbit gc worktrees                          # report only — the default is non-destructive
orbit gc worktrees --estimate-bytes         # dry-run plus a recursive byte estimate
orbit gc worktrees --confirm                # actually remove
orbit gc worktrees --older-than-hours 24    # leave recent runs alone
orbit gc worktrees --run <run_id>           # restrict to one run
orbit gc worktrees --target-only            # report only each worktree's target/ build output
orbit gc worktrees --target-only --confirm  # delete target/ and keep the checkout
```

Collection is conservative: it reaps only worktrees whose associated task has
settled to `done`, `rejected`, or `archived`, or that has been parked in
`someday` for 336 hours (14 days), counted from
the status change. A parked worktree also stays while its worker or claim is
live, while Git holds it locked, or while its run branch has commits that
exist on no published ref and in no rescue patch
(`skipped:unpublished_commits`). A worktree belonging to live work
is never a candidate. A worktree with uncommitted changes is kept
(`skipped:dirty_rescue_candidate`), except by the scheduled
`worktree_gc_pipeline`. That pipeline removes a settled run's dirty worktree
once the candidate is recoverable: a carry ref or published branch on `origin`
holding exactly it, or a patch it writes to
`.orbit/state/rescue/worktree-gc/` and verifies first. The patch's base commit
is pinned at `refs/orbit/rescue/worktree-gc/<run>-<tree>` until a deleting
sweep sees that the patch file is gone. `recovery` in the
report names which source. Kept terminal worktrees report `retained_bytes`, and
`orbit run show <gc-run>` lists them with their reasons. Details:
[worktree reclaim runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/worktree-reclaim.md).
Run the report a few times before relying on scheduled reclamation. The default
seeding policy seeds `worktree-gc` enabled for hourly collection; the host clock
must also be running — [automation.md](automation.md).

### Workspace scratch

The same `worktree_gc_pipeline` run also prunes the checkout's own
`.orbit/tmp` (`ORBIT_SCRATCH_DIR`), where operators and
`orbit.task.artifact` action `put` callers stage evidence, build output and CodeQL
databases. Artifacts are copied into the task store, so staged files are
disposable once attached. A top-level entry is removed when its **newest mtime
anywhere inside it** is older than `scratch_older_than_hours` (default `24`);
newer entries are kept, so a file staged moments ago is never at risk.
Override the window for one run:

```bash
orbit run job worktree_gc_pipeline --wait --input scratch_older_than_hours=72
orbit run job worktree_gc_pipeline --wait --input scratch_older_than_hours=0   # everything not in use
```

Set a workspace-specific default by editing `scratch_older_than_hours` in the
workspace copy at `.orbit/resources/jobs/worktree_gc_pipeline.yaml`. To set a
host-wide default, edit the shipped job at
`~/.orbit/resources/jobs/worktree_gc_pipeline.yaml`; local edits there are
preserved instead of refreshed from upstream. A run scoped to one run id
(`target_run_id`) leaves scratch alone.

Symlinks are removed as links and never followed out of `.orbit/tmp`. GC skips,
and reports with a reason, any entry that a live process holds open, uses as its
working directory or executes (read from `/proc` on Linux and `lsof` on macOS —
best effort, so another user's processes are not visible), or whose path a
pending, running or retrying run's record or saved pipeline state names.
Retrying runs keep their scratch while waiting for another attempt. If the
run inventory or any required saved state cannot be read, the scratch sweep
fails before deleting any entry. A failure removing an entry skips that entry
and the sweep continues.

`orbit run show <run> --json` carries the figures under the `reap` step's
`scratch` object, beside the worktree figures: `bytes_reclaimed`,
`entries_removed`, `entries_skipped` (each listed in `entries` with its
`reason`) and `entries_kept`. To empty the directory by hand regardless of age,
use `orbit gc tmp --confirm`. Without `--confirm` the command only reports,
and it refuses while job runs are active.

On a replica checkout (one that pulls work from an owner on another machine),
new claimed tasks live on the owner; historical tasks minted with this machine's
prefix remain local. A claimed leaf whose handoff the owner accepted and whose
claim this follower settled needs no task answer: the owner holds its delivery, so
its worktree is collected and `detail` names the settled claim. For other
worktrees, GC reads tasks carrying this machine's prefix from its local store
and asks the owner only for the owner's prefix, learned from existing claim
admissions or a single foreign prefix in the workspace's stored tasks. Claims
identify the owner even when other foreign task mirrors exist; without claims,
multiple foreign prefixes are ambiguous. An unknown or ambiguous prefix retains the worktree with
`skipped:task_prefix_unroutable` and makes no owner call. Missing local or owner
tasks stay `skipped:task_unresolved`. Owner lookups use the owner's tool
surface, through the claim's own route (the owner must be registered with
`orbit host add`). A transport failure keeps the worktree as
`skipped:owner_unreachable` with the error in `detail`; run GC again once the
owner answers. `skipped:no_owner_route` means the replica has no route to ask
at all — the owner has no host entry (`orbit host add`) or the checkout is
not a registered workspace — and `detail` says which. A status lookup that
fails without a transport error is reported as `skipped:owner_lookup_failed`
with the reason in `detail`; it does not establish that the owner is down.
The pull drain also
reclaims each settled leaf's `target/` on its next pass, so follower disk
does not depend on this schedule.

Removal first restores owner access to read-only directories inside the
worktree (a test fixture can leave one), so Git can delete the whole tree.

A directory under the worktree root that Git does not list as a worktree is
never removed (`skipped:not_registered_worktree`), unless it is the leftover
of a removal that failed partway: its `.git` link is missing or names an
administrative directory that no longer exists, and it maps to a terminal run
with no live worker whose tasks are settled. GC then removes it as `removed`,
with the reason in `detail`. For any other such directory, `detail` gives the
remedy: `git worktree repair <path>` if it was moved, otherwise inspect it and
delete it by hand once nothing in it is needed.

`--target-only` (alias of `--reclaim`) reclaims the declared `worktree.reclaim`
paths, by default `target`, `.orbit/tmp/*-target` and `.orbit/tmp/base-wt`.
These hold most of a worktree's size. It deletes only those paths and keeps
the checkout, including uncommitted changes, so a failed or blocked run can
still be rescued. It needs a terminal run with no live recorded worker, not a
settled task. A running run is never touched, and a path holding anything Git
tracks is kept (`skipped:tracked_content`). The scheduled pipeline does the
same for every kept terminal worktree. `bytes_reclaimed` reports the size of
the removed paths.

`worktree.reclaim_below_free_mib` adds admission-time reclamation: unset by
default, or a non-negative integer MiB (`u64`; `0` never triggers). While free
space beneath the state directory is below it, admission reclaims the declared
`worktree.reclaim` paths from oldest terminal worktrees first, keeping their
checkouts and protecting active runs. Read the effective mark with
`orbit config show --all`. Inspect eligible output without deletion using
`orbit gc worktrees --reclaim --dry-run --estimate-bytes`; this previews GC
eligibility, not a forced low-space admission. Actual admission reclamation
records a `worktree.reclaim` audit event with reclaimed bytes. See
[Configuration](configuration.md#common-settings) for the setting and limits.

With `workflow.validation_env.build_output_env` configured, required validation
binds the workspace's output variables to host-owned output under
`.orbit/state/host-validation-targets/`, outside the checkout. Native Linux and macOS
collectors keep the same layout and lineage for historical output; an empty mapping
allocates nothing and never deletes old output. Other platforms retain configured
output for operator maintenance. Darwin uses readable no-follow directory handles
and `fchmod` on held descriptors; unreadable directories or unprovable identities
refuse reclamation rather than relaxing permissions for measurement. GC attributes
each target to the runs whose recorded worktree path it was built for, and
reclaims it once they are terminal with no live worker and the checkout is gone
(or its kept output is being reclaimed). A scratch checkout's target (base
rerun, host evidence, recovery) is reclaimed only through its base-cache lock
or lease record, while GC holds that lock; a held lock reports
`skipped:host_target_in_use`. Targets with no such record stay
`skipped:unattributed_host_target`. The existing worktree-GC routine and pipeline
use the same collector. Platform support does not authorize production deletion,
resume paused routines or change schedules; validate disposable native fixtures
and obtain the required review before rollout. Byte reports measure logical output,
not physical disk headroom.

## Diagnose and repair

```bash
orbit doctor            # config, database, disk, indexes, locks, runs
orbit doctor --json
```

Run it after an upgrade, after an interrupted run, and whenever something behaves
inexplicably. It has targeted repairs, each narrow on purpose:

| Flag | Repairs |
|---|---|
| `--fix-stale-locks` | Lock files whose recorded holder process is dead. |
| `--fix-stale-task-locks` | Task reservations whose owner and task state are conclusively inactive. |
| `--fix-stale-artifacts` | Retires deprecated skills, jobs, activities, auto-tasks, and routines that Orbit itself wrote. Locally modified ones are preserved, not deleted. |
| `--fix-retired-config-keys` | Backs up each edited `config.toml` and removes retired or deprecated keys, first writing each deprecated key's translated value under its replacement key in the same layer while preserving explicit modern values. A second successful run writes nothing. Any legacy after-landing policy refuses before either config is changed or backed up; migrate it manually first. |
| `--fix-automation-pins` | Deletes this Orbit root and workspace's own state-routine attempt pins that no consumer or live run still names. Legacy shared `refs/orbit/automation/<attempt>` pins and other roots' or workspaces' pins are retained and reported. Refuses while a routine sweep runs. |
| `--fix-state-directory-permissions` | Restricts writable Orbit state directories to owner-only access, excluding run worktrees, Cargo target trees, and child symlinks (Unix only). |
| `--fix-orphan-task-stores` | Deletes empty unclaimed task-store partitions, and populated partitions whose bound checkout is confirmed absent, including their task bundles. Unowned or unreachable populated partitions are left alone. Requires `--confirm`. |
| `--remove-graph` | Removes retired graph state from this worktree and the shared workspace. |

For a retired-key repair refused on `operation.review_policy = "after-landing"`,
choose the intended consumer state with `orbit auto-task toggle delivery-code-review on`
or `off`. Set `review.timing` and `review.minutes` explicitly as needed, remove
the legacy policy manually, then rerun the repair. This includes policies masked
by another layer; toggling the consumer alone does not permit automatic removal.

`--fix-stale-artifacts` is how a workspace catches up after an Orbit upgrade
drops a shipped definition. It works by content provenance — changes to
template-owned fields are local edits that Orbit preserves.

For routines, the two settings Orbit's own surfaces change are not edits:
flipping `enabled` (the documented opt-in, and what the dashboard toggle
writes) and deleting the retired `hosts:` key. Comparison parses the routine,
so comments do not count as local modifications either. A routine that differs
from a shipped template only in those settings or comments still counts as
Orbit-written, so an upgraded workspace converges without you moving files by
hand. Orbit deletes outright
only bytes it can prove it wrote; anything else it copies to
`.retired-managed/routines/` before removing it from the active catalog.
Changing a routine's cadence, target, policy, or description is a real edit
and is preserved and reported; a comment-only edit remains managed.

## Retired features and their keys

Orbit removes features across releases. A removed feature can leave configuration
keys or an MCP toolset name behind; a removed command simply stops existing.
Orbit still loads the leftover keys and toolset names, warns on every load and
ignores them. Delete each leftover, then verify the load is clean. Keys that were
only renamed or moved are translated on load and rewritten by
`--fix-retired-config-keys`. This table lists removed features; the deprecated-key
tables below cover the renamed, ignored and fixed-bound keys, with what each one
becomes.

| Removed feature | Leftovers to delete | Replacement | Recognized until |
|---|---|---|---|
| Explorer and agent invocation | `[workflow.explore]` table; `workflow.purpose_crews.explorer`; `agent-invoke` in `machine.mcp.toolsets` and in any `--toolset`. `orbit run agent` and the `orbit.agent.invoke` tool are gone. Stored runs and experiment readouts stay readable. | None in Orbit: use task workflows or a provider CLI. | No date set; warns until a later release makes it an error |
| Crew ladder | `[workflow.crew_ladder]` table. `orbit agents ladder` is gone. | Route each tier with `workflow.<tier>_complexity_crews`; compare crews with [experiments](configuration.md#experiments). | 2026-12-07 |
| Regression-risk prompt marks | `[workflow.regression_risk]` table, including `record_sites` | None. Task riskiness scores use fixed defaults: a 30-day window and at least 5 done tasks. | 2026-12-07 |
| Pilot explore brief | `[workflow.explore_brief]` table | None. Existing `explore-brief.json` artifacts stay readable. | 2026-12-07 |
| Visual review | `[visual_review]` table; `workflow.purpose_crews.visual_review`. `orbit visual` and the `orbit.visual.*` tools are gone. | None. `mocks/` and `visual/` are ordinary artifact names now. | 2026-12-07 |
| Crew-performance weighting | `workflow.crew_performance`, `crew_performance_strength`, `crew_performance_cap` and `crew_performance_window_days` | None for the weighting. The older `workflow.crew_performance_types` name is read as `workflow.task_types`; the repair rewrites it. | 2026-12-07 |
| Weekly evolution pass | `resident.evolution.min_occurrences`, `min_distinct_cycles`, `min_distinct_mandates`, `min_cost_minutes` and `max_proposals` | None. `orbit evolution` and `orbit resident digest` stay, and nothing files proposal tasks automatically. The ledger's 14-day window and 30-day decline are fixed, see [deprecated keys](#deprecated-keys-translated-ignored-or-fixed). | 2026-12-07 |
| UX rules | `[ux_rules]` table | None. No routine or installed command runs the rules. | 2026-12-07 |
| Finding verification | `review.verify_findings` | None. The task pilot's `finding_invalid` result and an implementer's `finding-invalid.json` still reject an invalid finding. | 2026-12-07 |
| Agent scoreboard | `[scoring]` table (`scoring.enabled`) | None; nothing records scoreboard metrics. | 2026-11-12 (window closed; now an unknown key) |
| Dashboard browser sessions | `[machine.web_terminal]` and `[machine.web_session]` tables; `machine.web_terminal.enabled`, `machine.web_session.ttl`. `orbit web open` is gone. | None for browser sessions. Open the dashboard URL directly; `orbit web serve --operator` still provides operator capability. | 2026-12-08 |
| Routine publication attachments | `publication.routine_attachments`, `publication.routine_allow_unscanned_attachments` | None for the scheduled routine, which always omits attachments. Publish attachments by hand with `orbit task publication publish --attachments`. | 2026-12-08 |
| Selective before-landing review | `[review.selective_before_landing]` table; `review.selective_before_landing.mode`, `risk_threshold`, `paths`, `unknown_risk` | Review before landing with `review.timing = "before_landing"` or `review.before_landing_hosts`. | 2026-12-08 |
| Decision assistance | Global `[assistance]` table. `orbit decision assist` is gone. | None. | 2026-12-08 |
| Path leases | `[lease]` table / `lease.*` keys. `orbit lease` and the `orbit.lease.*` tools are gone. | None. The lease machinery stays disabled. | 2026-12-08 |
| Orchestrator scorecard | `orbit orchestrator`; `--role orchestrators` on `orbit agents scorecard` and `orbit agents show`; the orchestrator scorecard's MCP tool; the `/api/agents/orchestrators` endpoint; any link to the dashboard Orchestrators tab. | None for orchestrators. The dashboard Orchestrators tab is replaced by Rank, which ranks implementer models with `orbit agents rank`. For crew comparison, use [experiments](configuration.md#experiments). | No window stated; removed in 0.48.0. |
| MCP tool-family names | Per-action tool names such as `orbit_friction_add`, `orbit_friction_update`, `orbit_task_artifact_get` and `orbit_task_artifact_put`, in scripts, prompts and MCP client allow-lists. Stored names in existing records migrate on upgrade, so there is nothing to delete there. | The family tool with an `action` field: `orbit_friction` with `add` or `update`, and `orbit_task_artifact` with `get` or `put`. See [tool surface](../../orbit/references/tool-surface.md). | No window stated; removed in 0.48.0. |
| MCP task schema string forms | A bare string where a list is expected on `orbit_task_add`, `orbit_task_update` and `orbit_task_list`, such as in `acceptance_criteria` or `context_files`. | Send a JSON array for each list field, such as `acceptance_criteria`, `context_files` and `relations`. | No window stated; removed in 0.48.0. |
| Per-call `model` input and task aliases | The `model` input on MCP task calls, which the MCP schema no longer lists; the `agent` alias of `orbit_task_list`; the `context` alias on `orbit_task_add` and `orbit_task_update`. Orbit still accepts a sent `model` as an unadvertised identity key, and the session's own identity takes precedence over it. | Identity comes from the session's crew configuration or the MCP client's `clientInfo`, not from the call. Use `context_files` in place of `context`. For `agent` on `orbit_task_list`, read the list's field names with `orbit tool show orbit.task.list`. | No window stated; removed in 0.48.0. |
| Retired activity-backend repair | `orbit doctor --fix-retired-activity-backends` is gone. A schemaVersion 2 `agent_loop` activity file that still declares the scalar `spec.backend: http` or `spec.backend: auto` warns on `orbit doctor`, naming the file and the rejected value. | None. Delete only that scalar key from the activity file by hand, then verify with `orbit doctor`. Leave a plugin manifest's structured `spec.backend` command backend (timeout and sandbox settings) in place; it is still supported. | No window stated; removed in 0.50.0. |

The `finding-verification`, `evolution-pass`, and `ux-rules` routines are retired,
while `pilot-warning-triage` is retired via its `pilot_warnings_held` state
trigger rather than its job. `orbit workspace sync` retires their seeded copies. A
routine you wrote that targets one of the three removed jobs
(`finding_verification_pipeline`, `evolution_pass_pipeline`,
`ux_rules_pipeline`) loads as retired and never fires unless the workspace
defines a job of the same name, which resolves first and stays active;
otherwise remove it or retarget it. A routine on the retired
`pilot_warnings_held` trigger loads as retired even though its
`escalation_triage_pipeline` job still ships, so a cron routine targeting that
job without the retired trigger stays active. Triage a
task held on pilot warnings from the dashboard's Triage action.

### Deprecated keys: translated, ignored or fixed

These keys still load and warn on every load, and `orbit config get` and `set`
refuse them. `orbit doctor --problems` names each one with the file that sets it,
and `orbit doctor --fix-retired-config-keys` acts on it as the tables below say.
A key past its recognized date becomes an unknown setting, which the same check
reports. Where a row gives no date, the load warning gives none either.

**Translated** keys are rewritten by the repair into their replacement in the same
file, then removed. The repair writes a replacement only when that file does not
already set it, so an explicit modern value wins and the old key is simply removed.

| Key | Becomes | Recognized until |
|---|---|---|
| `operation.review_policy` | `before-pr` sets `review.timing = "before_pr"`, a deprecated value that still works (prefer `before_landing` or `off`). `after-landing` and `none` turn neither review on: after-landing review is the `delivery-code-review` auto-task's own flag, set with `orbit auto-task toggle delivery-code-review on` or `off`. A `[review]` table in the same file wins. | None stated |
| `operation.review_minutes` | `review.minutes`, now one candidate's review limit rather than a lineage total | None stated |
| `operation.review_crew` | `workflow.purpose_crews.review`, the same crew or pool | None stated |
| `workflow.final_recovery_crews` | `workflow.purpose_crews.final_recovery`, the same pool | None stated |
| `workflow.purpose_crews.ci_failure_sweep`, `dependabot`, `merged_head_repair` | `workflow.purpose_crews.sweep_tasks`, the one pool every sweep-filed task draws from. Load fails while one file sets them to different pools. | None stated |
| `review.before_pr`, `review.before_landing` | `review.timing`: `before_pr`, `before_landing` or `off`. Both set to `true` fails load. | None stated |
| `machine.worker_containment` as `true` or `false` | `"warn"` or `"off"` | None stated |
| `machine.worker_containment_strict` | `machine.worker_containment = "strict"` when `true`; refused beside `false` | None stated |
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
  Migrate it as [Diagnose and repair](#diagnose-and-repair) describes.
- **`[routines]`** (`role = "source"`) loads with a warning, but `orbit doctor`
  does not list it and the repair does not remove it. Delete the table by hand.
  It has no recognized-until date.

### Verify the cleanup

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
   policy is set; migrate that first, as [Diagnose and repair](#diagnose-and-repair)
   describes. The repair removes keys, not list entries, so delete
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

### Retired activity backends

The `orbit doctor --fix-retired-activity-backends` repair is gone: no flag
cleans up activity files for you. A schemaVersion 2 `agent_loop` activity file
that still declares the retired scalar `spec.backend: http` or
`spec.backend: auto` is a warning naming the file and the rejected value.
Delete only that scalar key from the activity file by hand, then verify with
`orbit doctor`. Do not remove a plugin manifest's structured `spec.backend`
command backend (with its timeout and sandbox settings): that is a live plugin
field and stays in place.

## Task locks

```bash
orbit task locks list                      # files held by active tasks and reservations
orbit task locks release <reservation_id> --confirm  # operator escape hatch
```

The listing also includes active reservations and distinct task counts. The
Drain readiness groups name the holders blocking a waiting task. No-diff tasks
hold no context locks;
other holders use admission's canonical selectors and unlocked-path rules.
Listing locks also expires old reservations and records their audit events.

Release only after confirming the holding task is genuinely inactive, and only
through this surface — never by editing the store. The full diagnostic sequence
for a reservation blocking a run is in
[common-failures.md](../../orbit-orchestrate/references/common-failures.md).

## Managed resources and upgrades

```bash
orbit workspace sync --check --json   # no writes; exit 3 means pending changes
orbit workspace sync --json           # converge managed defaults
orbit workspace sync --all --json     # every workspace registered on this host
orbit workspace sync --check --all    # preview all; writes nothing, exit 3 = pending
orbit skill list
orbit skill doctor
orbit skill link                      # repair supported user skill symlinks
```

Run sync from an initialized, registered checkout after upgrading the binary,
or run `orbit workspace sync --all` once per host from any directory to converge
every registered workspace. `--all` reports `synced`, `already current`,
`skipped` or `failed` per workspace, keeps going past a failure (exit 1), and
skips, never rewrites, a workspace a live drain or run holds; rerun it when the
drain ends. Single-checkout `orbit workspace sync` does not check for live
runs, so run it only when the workspace is idle. `--all` never reaches across
hosts and cannot be combined with `--workspace`.
It reconciles both host-global and workspace-local managed assets, including
skill references, jobs, activities, routines, and auto-task definitions.
Provenance distinguishes untouched shipped content from local edits. Read
`preserved` and `binding_drift` outcomes; a successful sync does not mean a
customized override was overwritten. Never fabricate or edit the managed-asset
manifest to force replacement. Review custom overrides against the installed
catalog and update them deliberately.

Sync updates managed files, not workspace ownership or task publication. The
plugin skill bundle updates through its plugin distribution; global skill
symlinks and a plugin installation are distinct delivery paths. The `orbit skill`
commands that inspect and repair the global skill links are in
[Skill management](#skill-management).

## Skill management

`orbit skill` works with two separate things on a host:

- **The catalog**: each shipped skill's files under the global Orbit root, by
  default `~/.orbit/skills/` (`orbit`, `orbit-orchestrate` and `orbit-setup`).
  `orbit workspace sync` reconciles these managed files and preserves local
  edits. A missing catalog entry is a sync question, not a `link` one.
- **The client links**: a symlink named after each shipped skill in
  `~/.agents/skills/` and `~/.claude/skills/`, each pointing at its catalog
  entry. Client CLIs discover skills through these links.

The plugin skill bundle is a third delivery path that these commands do not
touch; see [Managed resources and upgrades](#managed-resources-and-upgrades).

| Subcommand | What it does | When to run it | How to verify |
|---|---|---|---|
| `orbit skill list` | Lists each installed skill with its ID, the first ten characters of its content hash, its tag count and its summary. | After an upgrade or sync, to confirm every shipped skill is installed. | The three shipped IDs appear, each with a hash. A hash that differs from the same skill on another host means the content differs. |
| `orbit skill show <name>` | Prints one skill's `Path`, full `Content hash`, its SKILL.md behavioral contract and its `meta.json` (or `(none)`). | To read what a skill tells agents to do, or to confirm which file a catalog entry resolves to. | The output starts with `Skill: <name>`, and the `Content hash` begins with the HASH that `list` shows. An unknown name fails with `skill not found` and exit status 1. |
| `orbit skill doctor [<name>]` | Without a name, checks each catalog skill and diagnoses dangling symlinks in existing client link directories. A dangling symlink in `~/.agents/skills/` or `~/.claude/skills/` is a `warning` that names its path. It does not report an expected client link that is absent. With a name, prints only that skill's findings. | After an upgrade, after an interrupted run, or to diagnose catalog problems and dangling client links. If a client cannot find a skill, also inspect the expected links under `~/.agents/skills/` and `~/.claude/skills/` or run `orbit skill link`. | A healthy doctor result ends with `All skills healthy.`, but only confirms the catalog checks and dangling-link checks it performs; it does not certify that every expected client link exists. With findings, the table lists them and `N issue(s) found.` goes to stderr. The exit status stays 0 for findings, so read the STATUS column. An unknown name fails with `no skill diagnostic named '<name>'` and exit status 1. |
| `orbit skill link` | Creates the client links for each shipped skill in `~/.agents/skills/` and `~/.claude/skills/`. It replaces an existing link that points elsewhere or dangles, and does nothing when the links are current. It fails if a shipped skill's catalog entry is missing, and it never creates catalog entries. | When an expected client link is absent, a client stops listing Orbit skills, or `doctor` reports a dangling link. | It prints `Linked N skill(s) in:` with both directories, or `Skill symlinks are already up to date.` when nothing changed. Then `ls -l ~/.agents/skills ~/.claude/skills` shows each shipped skill as a symlink to `~/.orbit/skills/<name>`. `orbit skill doctor` can confirm there are no reported catalog problems or dangling links, but a healthy result alone does not confirm every expected client link exists. |
| `orbit skill unlink` | Removes only the links that `link` creates: entries named after a shipped skill that point at its catalog entry. Plugin links, custom links, regular files and other symlinks stay in place, and a discovery `skills` directory left empty is removed. The catalog under `~/.orbit/skills/` is not changed. | When Orbit should stop offering the shipped skills to clients on this host, such as before moving to the plugin distribution or retiring the host. Clients stop finding them there until `orbit skill link` runs again. Confirm with the operator first, because it changes what clients load. | It prints `Removed N skill symlink(s).` or `No skill symlinks found to remove.`. Then `ls -l ~/.agents/skills ~/.claude/skills` shows no `orbit`-named links, and `orbit skill list` still lists the three skills. |

### Repair a missing or broken client link

1. Run `orbit skill doctor` to check the catalog and diagnose dangling links.
   It does not report an expected client link that is absent, so a healthy
   result does not prove every client can discover every shipped skill.
2. If a client cannot find a skill, inspect the expected links under
   `~/.agents/skills/` and `~/.claude/skills/`. Run `orbit skill link` to create
   absent links or repair links for shipped skills that point elsewhere or
   dangle.
3. If a shipped skill's catalog entry is missing, `orbit skill link` fails, so
   restore the catalog first with `orbit workspace sync` (see
   [Managed resources and upgrades](#managed-resources-and-upgrades)), then
   confirm with `orbit skill list` and run `orbit skill link` again.
4. A dangling link named after something other than a shipped skill is not
   Orbit's to remove. Delete it by hand, as the `doctor` message says.
5. Verify expected links with `ls -l ~/.agents/skills ~/.claude/skills`, then
   run `orbit skill doctor` to confirm it reports no catalog problems or
   dangling links. Doctor still does not verify that all expected links exist.

Do not use `unlink` as a repair: it removes the links that clients use.

## Removing or re-targeting a workspace

Choose by intent:

- To stop tracking a checkout but keep its data, use
  `orbit workspace remove <workspace>`. It removes the workspace and checkout
  records and the local task-publication binding. It keeps the checkout's
  `.orbit/` directory and task-store partition, including task bundles; the
  publication Git repository is not changed. This command has no `--confirm`
  or `--dry-run` option. Require the user's explicit authorization before
  running it.
- To delete Orbit data for a checkout, use
  `orbit workspace teardown <workspace> --confirm`. It deletes the task-store
  partitions bound to that checkout, Orbit-owned skill links under
  `.agents/skills/` and `.claude/skills/`, and its `.orbit/` directory, then
  deregisters the workspace. It leaves the source checkout and global Orbit
  data directory itself in place. Without `--confirm`, it prints the resolved
  deletion plan and
  refuses; there is no `--dry-run` option. Require the user's explicit
  authorization and a verified backup or task export before teardown.
- If the repository moved, use
  `orbit workspace source-remote rebind --remote <URL>`. First inspect and
  save the current identity with `orbit workspace source-remote show --json`,
  then validate the proposed change with
  `orbit workspace source-remote rebind --remote <URL> --dry-run --json`.
  After reviewing that result, apply the same command without `--dry-run` and
  update the checkout's Git `origin` separately. The new URL must be portable;
  credentials, local paths and remote aliases are refused. Rebinding changes
  the registered source remote and repository identity and reconciles the
  task-registry source pairing. Equivalent SSH and HTTPS URLs compare equal.
  Repeat the command with the registered URL to repair a pairing left stale
  by an older Orbit version, even when the source reports `unchanged`.
  Git configuration is updated separately.
  It refuses to rewrite an existing publication binding or its lineage and
  snapshots; record and remove that binding before the move, then use
  `orbit workspace publication rebind` with a new publication ID and an empty
  publication branch. Verify with `orbit workspace publication show --json`.
  See [Publication](publication.md) and [Multi-host](multi-host.md) before
  moving state between hosts.
- If the checkout directory moved on disk and the repository did not change,
  use `orbit workspace rebind --path <DIR>` to re-point this host at it. This
  is not `remove` or `teardown`, and it does not change the source remote. See
  [Rebind a moved checkout](#rebind-a-moved-checkout).

Before `remove` or `teardown`, manually confirm there are no live drains, runs,
or claims, and identify any unpublished tasks. The commands do not enforce
these checks. Inspect `orbit task publication status --json`; a `current`
status does not prove that every recent task change is published. Preserve the
task data you need with a verified publication or export, and back up required
host configuration separately, following [Publication](publication.md),
before teardown.

Capture `orbit workspace show --json` and
`orbit workspace source-remote show --json` before a change. After removal or
teardown, use `orbit workspace list --json` to confirm the workspace is no
longer registered on this host. After a rebind, use
`orbit workspace list --json`, `orbit workspace show --json` and
`orbit workspace source-remote show --json` to verify the registration and
new source identity.

### Rebind a moved checkout

Use `orbit workspace rebind --path <DIR>` after the operator has moved a
registered Git checkout directory to a new path. It re-points this host's
registration at the directory that was already moved. It works on owner and
replica checkouts and keeps their role and owner.

It does not move files, change the source remote, or change another host. Run
it on each host whose checkout moved. If the repository's origin also changed,
use the source-remote bullet under "Choose by intent" for that change; it is a
separate operation. `remove` and `teardown` are not moves: `remove` stops
tracking the checkout and keeps its data, and `teardown` deletes the checkout's
Orbit data.

Preconditions, all before the directory is moved:

- Stop the workspace's runs, settle pulled claims, and release workspace claims
  and path reservations.
- Remove the retained Orbit-managed worktrees first.
  [Worktree garbage collection](#worktree-garbage-collection) describes how
  worktrees are reclaimed.
- Move the entire repository, including `.git` and the checkout-local `.orbit`
  directory. External Orbit data roots are refused.
- Keep the host store outside the moved directory. The store, `orbit.db` under
  `~/.orbit` (or under the selected `--root`), must already exist. On a newly
  initialized checkout, run `orbit tool run orbit.task.list --input '{}'` from
  the registered checkout before moving it. Without the store, both apply and
  `--dry-run` refuse with `workspace_rebind_host_store_required` and write
  nothing.
- The new path must be the checkout's primary Git root, with the registered Git
  origin and the existing `.orbit` task-store identity. A target claimed by
  another workspace is refused.

If the directory has already moved and the store is missing, move it back to
its registered path, run the runtime command there, then move it again and
rebind.

Preview first. `--dry-run` validates and reports the binding without writing
anything. Review its output, then repeat the command without `--dry-run`:

```bash
orbit --workspace <registered-name-or-id> workspace rebind --path /new/checkout --dry-run --json
orbit --workspace <registered-name-or-id> workspace rebind --path /new/checkout --json
```

If the command is interrupted, runtime commands refuse to open the workspace
until the same rebind finishes its recovery. Repeat the same command with the
same `--path`, and finish that recovery before starting another rebind.

Verify from the new directory:

```bash
cd /new/checkout
orbit workspace list --json
orbit workspace show --json
orbit tool run orbit.task.list --input '{}'
```

`workspace list --json` should still list the workspace on this host, and
`workspace show --json` should resolve it from the new directory. The runtime
command confirms that the store opens against the moved checkout. A successful
rebind also appears in `orbit audit list --json`.

## Persistent MCP clients during upgrades

`orbit update --contract --json` describes candidate protocol support without
opening state. The updater requires this protocol before replacing an executable;
a missing/incompatible candidate is refused, including pre-fix downgrades.

Use `orbit update --preflight --json` against the configured executable and
same authorities before a wrapper changes the installation. Exit 0 reports
`schema_version: 1`, `admitted: true`, `reservation: false`,
`contract: executable-generation-v1`, `admission_contract: compatibility-generation-v2`,
and the `admission_roots` it locked;
exit 1 refuses admission on stderr and names the refusing authority.
`--root`, then `ORBIT_ROOT`, otherwise isolated `HOME=` / the host-global
root selects the first authority, reported as `global_root` (scratch init in
a read-only `~/.orbit` sandbox stays unblocked). When an override names
something other than the host-global root, the host-global root is locked
*as well*: the replaced executable is the running host binary, which no root
override moves, and clients started without an override pin the host-global
root. The initialized workspace that convergence would use is locked too
when it is a different directory — the initialized override, or, when that
override is only an uninitialized generation root, the workspace discovered
from the working directory. `--preflight` probes that uninitialized root without
requiring `orbit workspace init`; `orbit update` itself still refuses it
before convergence. Spellings of the same directory are one authority. A root
override therefore isolates state, not host-binary replacement — a live client
on any of those authorities refuses the upgrade, and so does an authority
whose `.generation.lock` this process cannot write (a read-only `~/.orbit`,
say): the update could never record the candidate there, so it is refused
before anything is staged rather than after the binary is replaced.
`orbit update` admits against that same set for the invocation; a green
preflight is not evidence for an update that would resolve different roots. It opens no runtime or
stores and may create coordination lock files. It is an observation, not a
reservation. `orbit update` reacquires and holds admission through
replacement, then pins the candidate in every locked authority through
convergence. External installers must quiesce clients; a standalone preflight
is not race-free, so preflight plus a raw copy is never a way to deploy a
local build — use `--local-candidate` (below). `orbit update --check` checks releases, not running-client
compatibility.

Participating CLI/MCP processes hold a generation lock for their entire
lifetime. Admission depends on the update: for a staged local candidate, only
a live long-lived participant that registered a resume capability the candidate
resumes is admitted beside the updater and pinning waits for it to hand
over — in this tree that is a non-remote stdio `mcp serve` whose stdin can be
polled and whose capability is `mcp-stdio-v1` — and short-lived participants,
including clock ticks and one-shot commands, are waited for; a live dashboard
(`orbit web serve`), a drain (`orbit job run-pipeline-worker`), `orbit mcp listen`,
an `mcp serve` that did not register a capability the candidate resumes, and a
holder that never registered (a v1 binary or a sandboxed child that cannot write
the Orbit root) still block the update and make it refuse before the installed
executable is replaced. Without a candidate, a live long-lived participant still
blocks. Do not kill an idle handover-capable client to make room. Ordinary v2 runtime admission instead compares store,
layout and feature compatibility: compatible builds may run and apply additive
migrations side by side despite different executable digests. A breaking newer
writer records a pending switch and waits up to 120 seconds by default for
participants to yield at safe points; an incompatible older build is refused.
With a v1-owned generation record, a read-only command whose compiled store schema
equals the live store schema may join that pin without rewriting `.generation.lock`
(`task show`/`list`/`flow`, `run history`/`show`, `search`, `workspace list`/`show`,
`tool list`, `friction list`). That v1 joiner still holds the shared flock, so
`orbit update` stays refused until it exits. Writers, MCP/web serve, `migrate --confirm`,
and a differing schema are still refused; schema equality is exact, not
additive-newer. Additive-newer read-only compatibility still applies
to a matching digest. This covers stdio/operator, TCP listener, federated local,
destination SSH and managed processes without changing their authority. Quiesce
via the owning client/operator when replacement is refused. After an installed
executable changes, supported long-lived processes can re-exec at idle boundaries;
MCP stdio preserves its session and drain coordinators reattach to their run.
The clock resumes eligible runs interrupted by the current upgrade at most once,
leaving elapsed, stopped or superseded drains alone. It does not reclaim claims
or replay uncertain mutations. For a lost reply, inspect the
durable operation/audit before any retry. Never delete the root's
`.generation.lock` or `.generation-admission.lock` to force admission.

Existing pre-fix processes do not hold these locks: the first installation needs
explicit quiescence and reconnection to the same configured authority. A desktop
restart alone does not prove an unmanaged backend exited. No shadow stores or
ad-hoc MCP servers are part of this contract. Additive-newer compatibility permits
some unaudited CLI reads; MCP tool calls, including workspace discovery, require
durable audit writes and cannot use that read-only fallback.

## Deploying a local build pinned to a source commit

To ship an unreleased fix, build a clean checkout of the full commit SHA and run
the **candidate's** updater so an older installed build is bootstrapped:

```sh
set -eu
SHA='replace-with-the-full-40-or-64-hex-commit'
WORKSPACE='/absolute/path/to/the-intended-workspace'
test -z "$(git status --porcelain)" # start in a clean Orbit source checkout
git fetch --all
git cat-file -e "$SHA^{commit}"
git checkout --detach "$SHA"
test "$(git rev-parse HEAD)" = "$SHA"
test -z "$(git status --porcelain)"
cargo build --release --locked -p orbit-cli
C="$(pwd -P)/target/release/orbit"
"$C" update --local-candidate "$C" --source-commit "$SHA" \
  --write-candidate-manifest ~/orbit-candidate-"$SHA".json
# Handover-capable clients need no quiescing; run from the intended workspace for discovery/convergence.
cd "$WORKSPACE"
"$C" update --local-candidate "$C" --candidate-manifest ~/orbit-candidate-"$SHA".json \
  --source-commit "$SHA" \
  --install-target ~/.orbit/bin/orbit --json
```

Trust is reported literally as `operator_attested` with `signed_release: false`:
Orbit computes the digest from the accepted bytes and the target from the
executable header, but the source commit is the operator's attestation. Release
signature verification is unchanged and never satisfied by a local candidate;
each platform builds its own candidate from the same commit.

`--install-target` must be the managed `orbit` executable (not a symlink, owned
by the invoking user); package-manager or unknown installs are refused. The
update admits the invocation, host-global, and selected workspace roots for the
whole staging, backup, swap and convergence sequence. Workspace discovery
follows the current directory independently of `HOME`; run from the intended
workspace, and use an isolated checkout as well as isolated `HOME` and install
target for smoke checks. The JSON report's `workspace_root` and
`admission_roots` show what was selected and held. The staged copy is what
installs even if the candidate path changes, an equal version with a different
digest replaces, and only a live long-lived client that registered a resume
capability the candidate resumes is admitted beside it and hands over after the
swap — in this tree that is a non-remote stdio `mcp serve` whose stdin can be
polled and whose capability is `mcp-stdio-v1` — while short-lived clients,
including clock ticks and one-shot commands, are waited for — do not
quiesce idle handover-capable clients. A live dashboard (`orbit web serve`), a
drain (`orbit job run-pipeline-worker`), `orbit mcp listen`, an `mcp serve` that
did not register a capability the candidate resumes, and a holder that never
registered (a v1 binary or a sandboxed child that cannot write the Orbit root)
make it refuse before the installed executable is replaced; quiesce that holder,
retry, then reconnect. Rerunning the
same command is idempotent and finishes partial convergence; `needs_recovery`
(exit 4) carries the exact retry command in `local_candidate.retry_command`.
See the upgrades runbook for the full procedure.

### Reclaim what deploys leave behind

Build directories under `~/.orbit/deployments/`, older executable copies in
`~/.orbit/bin/` (`orbit.bak-*`, `orbit.before-*`, `*.stale-*` and similar) and
`~/.orbit/orbit.db.bak*` store copies are not removed by any other GC class.
Once the upgrade is verified:

```sh
orbit gc deployments            # report each entry, its bytes and whether it is kept
orbit gc deployments --confirm  # remove what the report lists as would_remove
```

Each class keeps its newest `--keep` entries (default 2). `orbit.previous`, and
any deployment whose candidate manifest names the installed or rollback
executable's digest, are always kept and do not count toward `--keep`. Links are
unlinked, never followed. The upgrades runbook's "Reclaim old deployments and
backups" section has the full rule.

## Database and layout upgrades

```bash
orbit migrate               # inspect pending migrations without applying
orbit migrate --confirm     # apply
orbit migrate --backfill-diff-stats [--limit N]  # apply, then record diff stats for done tasks landed earlier
orbit migrate --backfill-task-crews  # apply, then set task.crew from each task's latest delivery (run or claimed-leaf settlement)
orbit migrate --backfill-dispatch-effort  # apply, then record the launch effort of agent invocations that predate dispatch identities
orbit migrate --backfill-claimed-effort  # on a pull follower: apply, then report to the owner the effort and model of claimed leaves it settled without them
orbit migrate --adopt-authority ~/.orbit  # record this home as the authority protecting a workspace that recorded none (stop its clients first)
```

Both ledgers — the SQLite schema and the workspace layout — auto-apply when a
runtime opens, so most upgrades need nothing. The bare command and `--dry-run`
are the only way to *see* what is pending without applying it, which is what you
want before upgrading a machine that matters.

Layout version 9 fences older Orbit clients that cannot preserve skipped-step
checkpoint markers: older writers drop the marker, and older readers restore a
skipped step's null as an output on resume, changing downstream template
behavior. The layout fence protects the workspace; the marker field itself is
not necessarily rejected by older binaries. Layout version 8 also protects
script output files and parallel script-branch checkpoints. For a workspace
used from multiple hosts, update every host that can open it before any host
opens or migrates it to version 9; do this as one coordinated rollout and keep
old clients stopped. See [Upgrading a workspace together](multi-host.md#upgrading-a-workspace-together)
for the host-by-host order. On each host, `orbit --version` confirms the
installed binary, while `orbit update --check` only reports available releases
and does not verify layout compatibility. `orbit update` run inside an
initialized workspace immediately runs `migrate --confirm` followed by
`workspace sync` as post-replacement convergence, so during the coordinated
rollout run the per-host updates from a directory that is not inside any
initialized workspace (with no `--root` or `ORBIT_ROOT` override); the JSON
report showing `"workspace_root": null` with both steps skipped confirms no
migration ran yet. Use `orbit migrate` to inspect pending
changes, then apply them with `orbit migrate --confirm` when needed. After the
upgrade, use `orbit doctor` on each host and `orbit migrate` to confirm no
migration remains pending.

When Orbit reports that safe SQLite permission repair is unavailable, it
refuses to change database permissions in place. Stop every process using that
database first, including workers, MCP servers, timers, dashboards, and other
Orbit commands. Locate the database and its existing `-wal` and `-shm` sidecars
in the configured state root. While all users are stopped, restrict the main
file and each existing sidecar to owner-only access: use `chmod 600` for
writable state, or retain `chmod 400` for a read-only publication. Do not
create sidecars that are absent. Check the actual files and their permissions,
then restart Orbit and retry the command. Run `orbit doctor` and `orbit migrate`
afterward to verify the state opens cleanly and no migration is pending.

## Audit events

```bash
orbit audit list --since 1h --status failure
orbit audit stats --since 7d
orbit audit export --output audit.json
orbit audit prune --older-than 90d --confirm
```

The audit store is persistent invocation metadata: who called what, when, and
whether it was denied. It grows without bound until pruned. Prune requires
`--confirm`; export first if the history matters.

## Store retention

The host store (`~/.orbit/orbit.db`) also keeps every run's audit events and
pipeline state, and the commit decision of every task transition (the task
commit journal). Each workspace keeps the blobs those events name under
`.orbit/state/audit/blobs/`. Three commands bound them. Each only reports
until you pass `--confirm`:

```bash
orbit gc audit                        # rows and bytes past retention.audit_days, and unreferenced blobs
orbit gc audit --confirm              # delete them
orbit gc runs                         # terminal runs past retention.runs_days that still keep pipeline state
orbit gc runs --confirm               # drop that state; the run, its steps and summary stay
orbit gc journal                      # applied commit journal rows older than 14 days
orbit gc journal --confirm            # delete them; unsettled rows and each task's newest stay
orbit gc audit --older-than-days 30   # one-off window instead of the configured one
```

`retention.audit_days` and `retention.runs_days` default to 60 days. On a large
store, consider `retention.runs_days = 30` before enabling the sweep. Values
below 30 are refused for new config writes and command overrides because
Reliability Pipeline echo dedupe and forecast-accuracy limiter shares need
30 days of run state. Older global or workspace files with a shorter window
load with an effective 30 days and a warning; startup leaves the file intact.
Persist the correction with `orbit config set retention.runs_days 30` (add
`--global` for the global file). Leave audit retention at 60 unless you have a reason to shorten it. `gc audit` prunes the host-wide command audit,
this workspace's run audit, and run audit whose workspace id is no longer
registered, then removes blobs that no remaining audit row, run step or
pipeline state names. Another registered workspace keeps its own window. A blob
written in the last 24 hours, or one a write in progress has marked as pending,
is always kept. `gc runs` never touches a held run or one still in flight, so
`orbit run show`, run history and the Cost page keep working, and it reaches
the same unregistered workspace ids. The same pass prunes the host's
per-minute run and host resource history (`run_resource_minutes`,
`host_resource_minutes`) and admission forecasts older than 30 days, whatever the
run window, except the rows its counts keep at any age: each workspace, work class
and complexity's newest 200
deliveries, the newest 10,000 forecasts, and host minutes from the oldest kept
forecast onward. Auto-concurrency profiles and forecast accuracy keep working on
those rows; older raw samples outside them are lost.
Durable lock-denial facts and commit outcomes preserve Lock contention and succeeded implement_one durations after pruning; upgrade backfills
existing evidence before GC. Health Events, Incidents, Denials, `orbit audit` and
the diagnostics friction view lose raw history past the audit cutoff with no
rollups. Task-store Codebase rankings, `orbit task flow` and friction stats keep
working. See the retention runbook for each setting's history boundary.

`gc journal` deletes applied task commit journal rows of this workspace, and of
unregistered workspace ids, older than 14 days.
Recovery replays only prepared and committed rows; an applied row is a decision
already published to its task, kept only to diagnose recent transitions. So
prepared, committed and aborted rows stay, and so does each task's newest
applied row. The window is shorter than the others because the journal grows
fastest: at about 600 rows of about 80 KB a day, 60 days would pass 2.5 GiB. Its
rows are selected by one read that takes no write lock, then deleted by key in
batches. The `store-gc` routine does not run it; apply it yourself.

Deletes run in batches of 1,000 rows, each its own short write transaction, so
running workers are not starved. Freed pages stay inside the database file; each
report shows the freelist, and `VACUUM` returns it to the filesystem. Run
`VACUUM` yourself while nothing is using Orbit:

```bash
sqlite3 ~/.orbit/orbit.db 'VACUUM;'
```

`orbit doctor` reports what audit and run retention could reclaim now
(`store-retention`); it does not count journal rows. It warns
when the store file is larger than 1 GiB
and no store-gc routine is enabled, and it names the enable command. To run the
audit and run sweeps daily, enable the seeded `store-gc` routine ([automation.md](automation.md)).
On a replica checkout the same cron routine runs against that host's own store
and does not call the owner. Enable it there when this machine is the only
place those rows live. A delivery- or state-triggered copy stays owner-only.

## Admission receipt storage

Workspaces that admit tasks for replica checkouts keep admission request
history apart from ordinary store garbage collection. Logical payload
accounting and permanent replay tombstones are not file-size reclaim; for
file size, see Store retention above.

```bash
orbit doctor admission-receipts          # read-only inspection
orbit doctor admission-receipts --json
```

Run the inspection on the owning checkout, or through the owner's federated
selector. A replica checkout refuses owner tools and names the owner to
contact instead. The report carries three separate totals, each with a count
and logical payload bytes: full admission receipts, permanent tombstones, and
claim-mutation receipts. Logical bytes count UTF-8 JSON payload lengths only;
they exclude row keys, indexes, free pages and write-ahead log state, and are
not filesystem usage.

The read-only inspection refuses while a commit is pending recovery rather
than repairing state. Recover first, then re-run the inspection.

Compaction is explicit maintenance, never automatic: admitting tasks does not
scan receipt history, so idle payloads accumulate until an operator runs it.
Capture the read-only report before and after to verify the change. Run
compaction only when deliberately authorized:

```bash
orbit doctor admission-receipts --compact
```

Under the exclusive admission boundary it first recovers pending commits,
then replaces eligible full receipts with permanent tombstones. A tombstone
keeps only the originating machine identity, request identity and input
digest in the same persisted encoding as replay. Eligible are idle receipts
with no claim and receipts whose claim has settled, except failed or revoked
claims that kept a pull request: those keep their full receipt to authorize
later external-merge settlement. Unsettled claims — claimed, running,
handed-off and repair-pending — keep their full receipts, as does a receipt
whose claim record is missing. Reservation expiry and age never count as
settlement, and there is no age-based tombstone deletion.

Replay stays refused: an exact replay of a tombstoned request reports expiry,
and a replay with changed input reports a mismatch; neither admits again.
Repeated runs are idempotent and report how many receipts were compacted plus
the resulting storage snapshot.

Claim-mutation receipts are reported alongside but pruned separately: they are
kept for seven days after settlement and this command does not run that
pruning. Never delete receipts by age or edit the store directly; use only the
inspection and `--compact` above.

## Logs

The global JSONL trace at `~/.orbit/state/logs/orbit.jsonl` rotates from
long-lived processes (`orbit mcp serve`, `orbit sweep`, `orbit web serve`)
and when the active file exceeds its budget (one `metadata()` check on first
write). Short-lived commands, including `orbit --help`, do not open the file
or walk the log directory. These budgets are fixed in code: seven days of
archives, a 500 MiB total budget, a 100 MiB active-file threshold. They are
not tunable. The former `runtime.log_retention_days`,
`runtime.log_max_total_mb` and `runtime.log_max_file_mb` keys are ignored
when present (recognised only until 2026-12-08); delete them from
`config.toml`.

```bash
orbit log tail -n 120
orbit log tail --level warn --since 1h
```

For diagnosing a host-level incident rather than tuning retention, see
[operational-logs.md](operational-logs.md).

## Search indexes

```bash
orbit doctor                              # search-index coverage
orbit search reindex                       # rebuild task chunks
```

Indexing is idempotent and safe to re-run. Reindex after bulk task imports or
a restore. → [search.md](../../orbit/references/search.md)

## What is evidence and must not be edited

Files under `.orbit/state/job-runs/`, `.orbit/state/audit/`, and
`~/.orbit/state/` are the record of what happened. Never edit them to make a
warning disappear or a run look successful. If a run is wrong, fix the cause and
re-run; the failed run stays as history.

For a validated task snapshot and same-authority recovery, use
[publication.md](publication.md). It does not back up logs, credentials, or
scheduler state.

Task bundles can also be moved deliberately — `orbit task export` and
`orbit task import` handle portable archives, and `orbit task reindex` rebuilds
the registry index from on-disk bundles after a restore. Never delete
`tasks/index.sqlite` to fix drift: it holds the allocator's issued-id high-water
mark for every workspace. When it is lost, stop Orbit processes, reindex every
workspace, and set the mark with `orbit workspace init --force --task-id-start <N>`
above every id ever issued; Orbit refuses new task ids until then.

For archive migration, `orbit task export --output <archive.tar.zst> --ids <id>,<id>`
selects tasks (omitting IDs exports all). `orbit task import <archive.tar.zst>`
defaults to renumbering collisions and rewriting references; choose
`--on-conflict fail` or `skip` deliberately when appropriate. Inspect the returned
ID mapping. To keep a read-only mirror of another host's tasks in step, use
`--on-conflict owner-wins`: it replaces only bundles whose task-ID prefix belongs
to that host, never touches locally minted IDs, and is safe to re-run. Import is a different operation from publication's strict
same-authority, identical-retry recovery; do not substitute it to bypass a
publication ownership or divergence error.
