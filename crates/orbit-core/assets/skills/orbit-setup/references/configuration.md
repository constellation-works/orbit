# Configuration

What is tunable and where. `orbit config keys` and `orbit config show` expose
the installed version's supported keys and effective values without a source
checkout. Check `orbit --version` before applying version-dependent CLI advice.
To choose values for a repository, in dependency order and from its own CI,
build and backlog evidence, follow [workspace tuning](workspace-tuning.md).

## Where config lives

| Path | Scope | Created by |
|---|---|---|
| `<workspace>/.orbit/config.toml` | Workspace-local | Hand-authored, optional |
| `~/.orbit/config.toml` | Global / user | `orbit init` |

Ordinary settings inherit per key: workspace overrides global, global fills
omissions, built-in defaults fill the rest. Tables layer down to individual
settings; scalars and arrays replace wholesale.

**Three security-sensitive settings deliberately do not inherit** once a
workspace policy file exists — `execution.codex.sandbox`,
`execution.codex.approval_policy`, and `execution.env.pass`. If the workspace
file omits one, its built-in default applies rather than the user's global
value. This keeps repository sandboxing deterministic instead of dependent on
whoever's machine is running. A file containing only `[plugin_enablement]`
is stripped before layering and does not count as a workspace policy layer:
those three settings still inherit global values.

```bash
orbit config show                  # effective merged view, with source provenance
orbit config show --scope global   # one physical file
orbit config get <key>
orbit config set <key> <value>
orbit config keys                  # every registered key with its allowed values; machine.id and machine.task_prefix are listed and read-only
orbit config path
```

`orbit config show` labels configured values by source: `workspace`, `global`,
or `environment`; `workflow.default_crew` shows `environment` when the
`CONSTELLATION_DEFAULT_PROVIDER` variable supplies it (no provider detection
runs at config load). A value from a compiled-in default shows `default`; a key with
no configured value and no compiled-in default shows `unset`. `machine.id` and
`machine.task_prefix` render as identity, not as settable rows. The text rows
do not include file paths: see the `Layers (later
wins)` banner for layer paths, or use `orbit config show --json` for per-key
path provenance. Reach for it before assuming a value. Use `orbit config show
--all` to include unset advanced keys;
the defaults below are built-ins, not necessarily this host's effective policy.

## MCP roles and toolsets

`machine.mcp.role` and `machine.mcp.toolsets` are global-only host settings
used by `orbit mcp serve` and `orbit mcp listen`. A role selects the MCP
toolsets the server advertises and accepts calls for. The `core` toolset,
including workspace discovery, is always present. Roles are presets:

| Role | Included toolsets |
|---|---|
| `operator` | `tasks-read`, `tasks-write`, `task-repair`, `decisions`, `runs`, `dispatch`, `automation`, `codebase`, `search`, `ui`, `exec`, and `plugins` |
| `manager` | Task and run reads, decisions, notices, resident mandates and search |
| `resident` | The manager surface plus task writes and dispatch |
| `agent` | Task reads and writes, decisions, codebase, search, UI and notices |
| `reader` | Task and run reads, codebase and search |

The `operator` role selects `tasks-read`, `tasks-write`, `task-repair`,
`decisions`, `runs`, `dispatch`, `automation`, `codebase`, `search`, `ui`,
`exec`, and `plugins` but grants no operator authority. `--operator` is a
separate server choice, and selects the operator profile when no `--role` is
given; tools that require operator capability still require that capability
under every role.

Set and read back the global defaults with:

```bash
orbit config set --global machine.mcp.role reader
orbit config set --global machine.mcp.toolsets '["experiments"]'
orbit config get machine.mcp.role
orbit config get machine.mcp.toolsets
orbit config show --scope global
```

The configured toolset names are additions to the chosen role. At server
startup, `--role <role>` replaces `machine.mcp.role`, and `--toolset <list>`
replaces `machine.mcp.toolsets`; each flag overrides only its corresponding
setting. Passing both flags replaces both configured values. The requested
toolsets are then added to the selected role, or to the core profile
when no role is selected. With no selected role, that core profile (`core`
plus `tasks-read`, `tasks-write`, and `search`) remains the base;
configured or command-line toolsets can extend it. `default` names
the core profile and `all` names every toolset. Restart a long-lived MCP
server or listener after changing the host defaults; an open session keeps the
selection it started with.

Check `orbit --version` before relying on role support. Inspect
`orbit mcp serve --help`, `orbit mcp listen --help`, `orbit tool list --help`
and `orbit config keys` from that same installed version. A source checkout or
newer documentation does not add settings and flags to an older installed
binary; upgrade to a version that includes role support before using this
guidance.

For the built-in role baseline and its size, run
`orbit tool list --role <role>` with that matching-version binary. This shows
the role's tools, count and approximate token size; it does not include
configured or command-line additions. To inspect the exact surface of a
server, use an MCP client connected to that server, initialize the session,
send `notifications/initialized`, then call `tools/list`. The response is the
effective advertised surface, including any additional toolsets. For a local
check, have a disposable foreground stdio client start `orbit mcp serve` with
the intended selection, use a short timeout, request `tools/list`, and exit
after the response. Do not add `--operator` just to inspect the tool list.

## Seeding policy

`seeding.routines` and `seeding.auto_tasks` choose which shipped defaults a
workspace carries, independently of presets. Both default to `"all"`. Set them
in `<workspace>/.orbit/config.toml` for one workspace, or use `orbit config set
--global` for `~/.orbit/config.toml`. Each workspace key overrides its global
counterpart; a name list replaces the inherited list rather than adding to it.

| Value | Effect |
|---|---|
| `"all"` | Seed every shipped default of that kind, including additions in later releases. |
| `"none"` | Seed none of that kind. |
| A TOML name list | Seed exactly those shipped defaults; later additions stay excluded. Routine names are file stems such as `["worktree_gc", "store_gc"]`, not the rendered names from `orbit routine list`. Auto-task names use names such as `["doc-duties", "qa-sweep"]`. |

A name Orbit has never shipped is refused by config loading, `orbit config
set` and seeding commands; the error names the key, bad name and shipped
choices. A name a later release retired or demoted still loads but seeds
nothing. This
policy selects Orbit's shipped defaults, not user-authored or plugin definitions.

The policy applies to `orbit workspace init`, `orbit workspace init --force`,
`orbit workspace sync`, and the create-only seeding that `orbit init` performs
over registered workspaces after an upgrade. Use workspace sync to reconcile
an existing workspace: the upgrade's create-only pass leaves existing files
alone and does not retire them.

### Changing an existing workspace

Run from the intended registered checkout. Inspect effective values and their
sources, then save the current config and any local definition edits before
narrowing. Review the definitions you intend to keep; this example selects
garbage collection and the crew snapshot refresh, plus two auto-tasks whose
shipped defaults are disabled:

```bash
orbit workspace show
orbit config keys
orbit config show --all
orbit config get seeding.routines
orbit config get seeding.auto_tasks
orbit routine list
orbit auto-task list

orbit config set seeding.routines '["worktree_gc", "store_gc", "task_pilot"]'
orbit config set seeding.auto_tasks '["doc-duties", "qa-sweep"]'
orbit config get seeding.routines
orbit config get seeding.auto_tasks
orbit workspace sync --check --json
```

The two config writes are separate, not an atomic preset application. Check
their effective values before applying. `sync --check` writes nothing and
exits 3 when changes are pending. Review the whole report, including warnings
and host-global asset changes, before running:

```bash
orbit workspace sync --json
orbit config show --all
orbit workspace show
orbit workspace show --json
orbit doctor
```

- **Narrowing retires managed defaults by recorded digest.** Sync removes an
  unedited file Orbit seeded. It moves a locally edited excluded default under
  `.orbit/.retired-managed/` and warns to migrate it under a new user-authored
  name. A file Orbit never recorded is left alone. Save and review edits before
  sync; preservation does not keep the excluded definition active.
- **Widening reseeds on the next sync.** Add a name to the list or set a key
  back to `all`, then preview and apply sync again. This seeds the shipped
  definition, not the preserved edited copy. Selecting a default is separate
  from enabling it: shipped auto-tasks start disabled, except `host-health`,
  which ships enabled. The shipped `worktree-gc`, `task-pilot` and
  `crew-memory-dream` routines seed enabled; other shipped routines seed
  disabled. `crew-memory-dream` mints nothing while `crew_memory.enabled`
  is false or no running memory-arm experiment applies, so its seeded
  enablement does not turn the experimental feature on. The switch,
  inspection and both kill switches are in [Crew memory](crew-memory.md).
  Preserve existing enablement choices.
- **Auto-task deletion remains an opt-out.** An opt-out recorded by `orbit
  auto-task delete` survives exclusion and later widening, so sync still leaves
  that default absent. First include it in `seeding.auto_tasks`, then use
  `orbit auto-task restore <name>` to clear the opt-out and restore its shipped
  content with the shipped `enabled` value: disabled for most defaults, enabled
  for `host-health`. Restore refuses an excluded name or an existing definition.
  See [Deleting a definition](auto-tasks.md#deleting-a-definition). For routines,
  a name-list policy is the durable per-name opt-out.

**Verify the result.** `orbit config show --all` reports the effective keys and
their provenance. `orbit workspace show` reports both policies and the defaults
left out; JSON carries `seeding.routines`, `seeding.auto_tasks` and
`seeding.left_out`, with the last applied preset separately under `preset`.
These describe policy, so also inspect `orbit routine list`, `orbit auto-task
list` and sync warnings for actual definitions and enablement. `orbit doctor`
does not report excluded defaults as missing. Its `seeding-policy` row explains
non-default policies; no such row is emitted when both keys are `all`.

For applying policy together with config and plugin enables from a reviewed
file, retain the [workspace preset procedure](first-run.md#workspace-presets).

## Common settings

| Key | Purpose |
|---|---|
| `workflow.base_branch` | Config fallback for ship/auto/pilot base branch. A registered workspace `base_branch` takes precedence. |
| `workflow.default_crew` | Crew for any task that doesn't declare one. |
| `workflow.<tier>_complexity_crews` | Automatic crew pool per task complexity (`low`, `medium`, `hard`, `xhard`); entries `crew[@effort]` or `crew[@effort]:weight` (`opus@medium:100` runs opus at medium effort as its own member). Empty (`[]`) routes that tier to `default_crew`. Orbit's own work draws from them too while its purpose pool is unset: auto-task templates by complexity, the task pilot and PR auto-fix from `low`, failure and PR conflict recovery from `medium`. The retired system-crew key loads with a deprecation warning and is ignored. |
| `workflow.purpose_crews.<purpose>` | Weighted pool for one purpose of work, written like the complexity pools: `task_pilot`, `failure_recovery`, `final_recovery`, `auto_fix`, `pre_landing_review`, `review`, `baseline_red_repair`, `sweep_tasks` (every task the CI failure, Dependabot and code-scanning sweeps file, and merged-head repairs a review reconciliation files). An explicit task, template or activity crew wins; otherwise the purpose pool is drawn, narrowed by `--allow-crew`; unset or `[]` keeps the earlier route. A provider limit or failure hold on the assigned member redraws from the pool's other enabled members before the complexity pool. `final_recovery` has no earlier route: `final_recovery = []` disables final recovery. `pre_landing_review` falls back to `review` when unset or empty; `review` also supplies after-landing review and is described below. `orbit config show --all` lists each purpose's resolved pool and its source (`crew_purposes` in `--json`). |
| `workflow.generated_paths` | **Advanced: executes commands during conflict recovery.** Default `{}`; table of workspace-relative path globs to non-empty regeneration commands. Applies only to conflicting generated files in a stopped rebase. Verify the recovery's `regenerated_paths` and command result. See [Recovery settings](#recovery-settings). |
| `workflow.unlocked_context_paths` | Default `["**/*.md"]`; array of workspace-relative globs (`*` within a component, `**` across components; no absolute paths or `..`). Matching context entries take no lock in mixed tasks; an all-matching task still locks every entry. `[]` locks all entries. Verify with `orbit run readiness` and `orbit task eligible`. Keep code and build/CI definitions locked. |
| `workflow.lock_modes` | **Advanced: admission policy.** Default `[]` (every overlap blocks); ordered `[[workflow.lock_modes]]` tables, each a `pattern` (Rust regex searched for in the workspace-relative path; anchor with `^`/`$`) and a `mode` of `exclusive` (default) or `shared`. First match wins. An overlap is waived when its point, the narrower selector's path, is shared, so tasks sharing only, say, `(^|/)mod\.rs$` run together and the second to land resolves any conflict. `module:`/`command:` selectors stay exclusive. Set it in the workspace config only for files that merge cleanly or are regenerated. Verify with `orbit run readiness` and `orbit task eligible`. |
| `workflow.unlocked_path_validation_commands` | **Advanced: validation policy.** Default `[]` keeps full validation; array of command strings, trimmed with blank entries dropped. Replaces required commands only when the candidate differs from this run's last fully validated tree solely in unlocked paths. Verify `validation_scope` in validation logs. See [Recovery settings](#recovery-settings). |
| `workflow.ci_infrastructure_retry.max_attempts` | Total failed-jobs reruns for one pull-request head whose failures are all logged infrastructure/transient. Default `3`; integer `0`..`10`. `0` disables this recovery. See [CI infrastructure reruns](#ci-infrastructure-reruns). |
| `workflow.ci_infrastructure_retry.backoff_seconds` | Initial delay between infrastructure reruns, doubled after each reservation. Default `60`; integer `1`..`3600` seconds; the first rerun is immediate. See [CI infrastructure reruns](#ci-infrastructure-reruns). |
| `workflow.ci_infrastructure_retry.handoff_steps` | Failed-step name patterns of CI dependency handoffs (a step that awaits or downloads another job's output). When every failed job stopped in a matching step, the set gets the infrastructure rerun without reading logs. Default `[]`; whole-name, case-sensitive, `*` matches any characters. See [CI infrastructure reruns](#ci-infrastructure-reruns). |
| `worktree.reclaim_below_free_mib` | **Advanced: reclaims rebuildable output.** Default unset; non-negative integer MiB (`u64`). During admission, reclaim `worktree.reclaim` paths from oldest terminal worktrees while free space under the state directory is below this mark; active runs remain protected. `0` never triggers it. Verify candidates with `orbit gc worktrees --reclaim --dry-run --estimate-bytes` and actual reclamation via the `worktree.reclaim` audit event. See [Worktree GC](maintenance.md#worktree-garbage-collection). |
| `workflow.provider_limit_overrides` | Delivery admission skips a crew while a live usage window of its provider or model is exhausted or used at or above 90%. Each entry replaces that percent for one provider: `provider:percent` (1..=100; 100 skips only on exhaustion) or `provider:off` (record readings without gating). Each provider once. |
| `workflow.provider_limit_explicit_crews` | When an explicit crew is at its provider's limit: `wait` (default) leaves the task in the backlog until the limit lifts; `pool` draws from unlimited members of its complexity pool. A purpose-sourced crew is not an explicit pin; it redraws within its purpose pool first. |
| `workflow.crew_selection` | `static` (default) draws on configured weights; `usage_pressure` scales task creation, re-rate and admission draws by each crew's weekly usage pressure (`weight × clamp(pressure, 1/8, 8)`) and re-draws a system-chosen crew at admission, keeping its crew source. A pull follower re-draws a claimed leaf's system-chosen crew on its own readings and reports per-crew pressure so the owner offers it first, among tied tasks, the ones whose pools hold its highest-pressure crews. Explicit crews are never re-drawn. Not the run's recorded `crew_selection`. |
| `workflow.provider_limit_budgets` | Rolling budgets for providers that report no usage, each `provider:<amount><usd|tokens>/<n><h|d>` (for example `grok:30usd/5h`), one per provider, or for a crew that sets `env`, its own account (`opus-credits:200usd/30d`). Spend is this host's invocation ledger. None by default. |
| `workflow.task_types` | Ordered task types that `workflow.type_complexity_crews` routes by and the implementer scorecard's by-type matrix (`orbit agents scorecard --role implementers --by-type`) splits by. Each entry has a unique non-empty `name` (never `other`) and at least one of `tags` or `paths`; a task takes the first matching type, else `other`. Empty (default) means every task is `other`. The former name `workflow.crew_performance_types` loads into it with a deprecation warning until 2026-12-07; `orbit doctor --fix-retired-config-keys` rewrites it. `workflow.crew_performance`, `_strength`, `_cap` and `_window_days` are deprecated and ignored: crew-performance weighting was removed. |
| `review.timing` | Review before landing, one layer at most (default `off`). `before_pr` holds PR creation for a fresh reviewer that fixes what it finds. `before_landing` opens the PR first and reviews it while hosted CI runs; it merges only at the reviewed head, and any other outcome leaves the PR open with the task in `review` (PR route only). A run captures it at submission. After-landing review is not a key: toggle the `delivery-code-review` auto-task (`orbit auto-task toggle delivery-code-review on`). |
| `review.before_landing_hosts` | Machine ids (`hm_...`) whose claimed leaves review their open PR before it lands, while this owner's own deliveries keep `review.timing` off. Probe and pull admission resolve the caller's label and the claim captures it. Ignored when `review.timing` is already `before_landing`. Loading fails while `review.timing` is `before_pr` and the list is non-empty. The label is caller-chosen, so this is policy, not a security boundary. Default empty. |
| `review.minutes` | Wall-clock limit for one candidate's before-PR or before-landing review (1..=1440, default 30). Each candidate gets one review; a changed candidate, such as a completion rebase, is a new one. |
| `review.baseline_commands` | Default `[]`; commands the owner trusts review settlement to rerun on the host for a reviewer's red-base claim, in addition to required validation commands. Entries are trimmed strings; blank entries are dropped. A configured command is binding during settlement. |
| `[[review.host_evidence]]` | Default `[]`; owner policy for checks owed on claimed reviews when changed paths match. This TOML array replaces the whole global list at workspace scope; see [review baseline commands and owed host evidence](#review-baseline-commands-and-owed-host-evidence). |
| `workflow.purpose_crews.review` | Crew or pool for after-landing review tasks the `delivery-code-review` auto-task mints. Before-PR and before-landing review use `workflow.purpose_crews.pre_landing_review` when non-empty, falling back to this pool when it is unset or empty. A pool such as `["sol", "grok"]` (or weighted `["sol:3", "grok:1"]`) draws one crew per review, skipping disabled or provider-limited crews and preferring one that did not implement the work. |
| `workflow.distributed_completion` | Standing owner authority for accepted distributed-drain handoffs: `review` (default, each handoff waits for operator approval) or `done` (acceptance authorizes owner landing). Owner configuration only; landing rechecks it. See [Distributed-drain completion policy](#distributed-drain-completion-policy). |
| `tasks.id_start` | Floor for this machine's task-id allocator; forward-only. → [multi-host.md](multi-host.md) |
| `execution.env.pass` | Environment variable names allow-listed into agent subprocesses. |
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
| `pr.task_url_template` | URL template linking a task ID in PR descriptions. |
| `pr.delivery_authors` | Forge logins whose PRs count as Orbit-authored for closing them (Orbit closes a task's open delivery and `[BLOCKED]` PRs when it lands, is rejected or archived, moves back to backlog or someday, or a newer attempt opens its own PR) and for settling a task to done from its delivery PR merged outside Orbit. Empty uses the `gh` login on this machine. |
| `pr.title_template`, `pr.body_template` | Single-task PR title and body templates with `{{placeholder}}` values (`task_id`, `title`, `type`, `description`, `acceptance_criteria`, `execution_summary`, `validation`, `freshness`, `task_url`, `ext.<system>`). The body is `"repo"` (the repository's `pull_request_template.md`) or a repository-relative path, read at the delivery's base revision. An unknown placeholder fails the config load. A rendered body ends with an Orbit comment naming the task, so a template that omits `{{task_id}}` still identifies the delivery. |
| `pr.agent_sections`, `pr.required_sections`, `pr.missing_required_section` | Body headings the implementer fills through the `pr-sections.json` task artifact. An unfilled required section opens the PR as a draft (`draft`, default) or opens none (`refuse`), recorded in task history. |
| `pr.labels`, `pr.reviewers` | Labels and reviewers applied when Orbit opens a PR. Orbit opens PRs ready for review, as a draft only for a required-section gap; a draft is never merged automatically. |
| `pr.task_tag_labels`, `pr.task_type_labels` | Opt-in task labels after PR creation or reuse: exact, case-sensitive tag allowlist (array, default `[]`), and a table mapping `feature`, `bug`, `refactor`, `chore` to label names (default `{}`, e.g. `{ bug = "bug", feature = "enhancement" }`). Names must be non-empty and cannot start with `-`; unknown types are refused. Unioned over every delivered task; missing labels are created and existing labels accepted. Failures warn and are recorded in `pr_open.task_labels.applied` / `.failed` without failing the PR step. Empty settings add no commands; labels are never removed or synced after later task edits. |
| `commit.subject_template`, `commit.body_template` | Single-task commit templates; the subject must contain `[{{task_id}}]` and keeps a 72-character budget. |
| `branch.name_template` | Single-task delivery branch name (default `{{prefix}}/{{task_id}}-{{hash}}`); must contain `{{task_id}}` and `{{hash}}`. Names with `..` or a leading `-` are refused. |

Host pressure holds admission after high readings span ten seconds and releases
only below the resume mark; every resume mark must be strictly below its high
mark. Running work is never cancelled or paused. Use `orbit config show --all`
for all effective marks, `orbit run readiness --json` for the held resource,
reading, threshold and CPU source (`capacity.resource_cpu_source`), and
`orbit doctor` for the effective source and host diagnosis. Explain a hold from
those readings and let pressure clear; do not raise marks or disable the
throttle to push waiting work through.

Some limits are fixed rather than configured. A provider with no
stdout/stderr and no CPU use for 30 minutes fails its step as stalled, entering
normal retry/recovery. One `proc.spawn` call inside a managed activity may run
at most 45 minutes, also capped at the activity's remaining wall-clock budget
(60 seconds outside an activity). Forecast admission keeps each profile key's
last 50 completed deliveries, forecasts at most 120 minutes ahead, protects a
queue head that has failed to fit for 30 minutes, and reports it stuck after 20
protected passes without improvement; a stuck report does not change
admission. New unattended admissions hold when a scheduled shutdown or reboot
is 180 minutes away or sooner; later schedules are reported while admission
continues. The JSONL log rolls past 100 MiB, keeps archives for 7 days and
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
restart/resume without a configuration write. Host thresholds and effective
provider-limit policy refresh next pass, caching unchanged config by mtime;
invalid config warns and retains the previous policies. `orbit run show <id> --json` and
`orbit run readiness --json` expose `mode`, `source` (`explicit` or
`configured`), and the current `configured_mode`. Pull drains refuse the
override and use the executor host's configured mode.
Inspect `orbit config keys --json` and `orbit config show` for the installed
schema and effective policy. Forecasting does not raise the drain's
`--concurrency` worker ceiling or bypass resource throttles. A per-drain mode
choice does not authorize dispatch or changes to global policy, host thresholds,
or shutdown holds. Use read-only `orbit run profiles`, `orbit run profiles
--accuracy`, and `orbit run readiness --json` to explain waiting work; preserve
operator settings unless an authorized change is requested. See
[operational-logs.md](operational-logs.md#delivery-profiles-and-admission-diagnostics).

`operation.review_policy` and `operation.review_minutes` are deprecated: they
still load, translated with a warning (`before-pr` → `review.timing = "before_pr"`;
`after-landing` enables `delivery-code-review` until an operator toggles it;
`none` → `review.timing = "off"`), and a later release makes them errors. Move
them to `[review]` and the auto-task flag. `operation.review_reviewer_starts` and
`operation.review_repair_cycles` are ignored with a warning; delete them.
`orbit config show --all` reports the review timing and the after-landing switch with
their sources; `--json` includes the same `review` object, and the default text view prints the `review.*` rows without that summary.

## Activity wall-clock timeouts

Set CLI agent activity budgets by exact activity name in the workspace's
`.orbit/config.toml` (or globally for a host-wide policy):

```toml
[workflow.activity_timeouts]
agent_implement = 28800 # 8 hours; shipped default is 21600 seconds (6 hours)
```

`workflow.activity_timeouts` is a table of activity names to integer seconds,
from `1` through `43200` (12 hours), inclusive. Zero, negative, non-integer and
larger values fail configuration loading; the error names the setting. Names
must be non-empty and contain no whitespace. Match the activity name, such as
`agent_implement`, rather than a pipeline step ID or provider name. Custom CLI
agent activities use the same table. Unmapped activities retain their declared
timeout, including the runner fallback when the declaration is zero. An empty
table overrides nothing. A workspace table replaces the global table, so copy
any global entries you want to retain.

The override is resolved once per activity step. Implementation continuations
and response corrections consume the remaining resolved budget rather than
starting a fresh full budget. An admitted review keeps its `review.minutes`
bound ahead of activity overrides. Restart long-running Orbit processes after an
edit so they load the new policy.

The shipped task gate allows 13 hours for child delivery and reserves its
footprint for the same interval: the maximum 12-hour implementation override
plus a bounded 1-hour delivery/review tail. The auto pipeline allows 15 hours
for the gate (1-hour admission, 13-hour delivery and 1-hour tail). Child waits
and reservation TTLs admit up to 86400 seconds (24 hours). Recovery and review
activities keep their separate declared budgets; they do not enclose
implementation. Custom jobs and explicit shorter wait inputs can still cancel
a child earlier; keep enclosing waits and reservation TTLs consistent with the
activity budgets you choose.

Inspect or replace the effective table with installed commands:

```bash
orbit config get workflow.activity_timeouts
orbit config set workflow.activity_timeouts '{ agent_implement = 28800 }'
orbit config show --all
```

## Mid-run steering

Mid-run steering lets an operator's task comments guide a run at a later prompt
and prevents delivery from committing while an operator comment remains
undelivered. It works within the task's admitted scope and authority; enabling
it does not widen either. A new global config seeded by `orbit init` sets `workflow.mid_run_steering.enabled = true`; when the key is absent it resolves to `false`.

| Key | Default | Effect |
|---|---|---|
| `workflow.mid_run_steering.enabled` | `false` | Record steering watermarks and deliver later operator comments at task-context prompts; gate delivery while comments remain undelivered. |
| `workflow.mid_run_steering.max_rounds` | `2` | **Advanced.** Maximum steering rounds before a delivery with unconsumed comments is held. Accepts integers from `1` through `10`. |

For one workspace, set the values from its registered checkout with
`orbit config set` (which writes workspace configuration by default):

```bash
orbit config set workflow.mid_run_steering.enabled true
orbit config set workflow.mid_run_steering.max_rounds 2
```

The equivalent workspace `.orbit/config.toml` is:

```toml
[workflow.mid_run_steering]
enabled = true
max_rounds = 2
```

Read back both values and their configuration source, then check the doctor
row:

```bash
orbit config get workflow.mid_run_steering.enabled
orbit config get workflow.mid_run_steering.max_rounds
orbit config show --all
orbit doctor --check mid-run-steering
```

The doctor check reports `workflow.mid_run_steering.enabled` and
`max_rounds`; its row is `ok` whether steering is enabled or not, so inspect
the reported values. Settings refresh at prompt and gate checkpoints. Prefer
enabling between drains: when enabled during an existing run, missing
watermarks stay absent and comments since dispatch can be outstanding at the
next prompt or gate unless an inherited candidate or claim supplies a baseline.
Disabling takes effect at the next checkpoint and makes recorded watermarks
and round state inert without deleting them. While on, if comments remain
undelivered when steering rounds are exhausted or a round delivers none of
them, delivery stops before commit in the `steering_unconsumed` hold. See the
[mid-run steering operator procedure](../../orbit-orchestrate/SKILL.md#mid-run-steering)
for posting and verifying steering comments.

## Completion holds

`--complete` (`orbit run ship --complete`, `orbit run auto --complete`, the
dashboard and the desktop drain) merges each task's pull request and moves the
task to `done` with nobody looking, and under
`workflow.distributed_completion = "done"` an owner lands claimed leaves the
same way. `[[completion.hold]]` rules keep a task in
`review` instead when the change is risky. A rule only lowers `done` to
`review`; it never grants completion, and a run without `--complete` is
unchanged.

Each rule sets at least one of:

- `paths` — globs matched against the files the delivery actually changes
  (the pull request diff, or the local/claimed candidate's diff against its
  base; a `!` prefix excludes), never the task's declared context files;
- `complexity_at_least` — `low`, `medium`, `hard` or `xhard`;
- `types` — `feature`, `bug`, `refactor`, `chore`;
- `tags` — task tags, compared case-insensitively;

and an optional `name` that labels it. A rule holds when any of its fields
matches; the first matching rule, in order, is recorded. Put the rules in the
workspace config (`.orbit/config.toml`) of the owner that runs `--complete`.

Example — CI, release and installer files, store migrations, sandbox and
credential code, and the hardest tasks; replace the paths with your
repository's:

```toml
[[completion.hold]]
name = "ci-workflows"
paths = [".github/workflows/**"]

[[completion.hold]]
name = "release-and-installers"
paths = ["install.sh", "RELEASING.md", "scripts/release*", "packaging/**"]

[[completion.hold]]
name = "store-migrations"
paths = ["**/migrations/**", "**/migration/**", "db/schema.sql"]

[[completion.hold]]
name = "sandbox-and-credentials"
paths = ["src/sandbox/**", "src/auth/**", "**/secrets.rs", "!**/*.md"]

[[completion.hold]]
name = "xhard"
complexity_at_least = "xhard"
```

A held run leaves the pull request open and ready and the task in `review`,
as a run without `--complete` would (with `--auto-fix` it still watches and
repairs the PR, but never merges it). A held local (`--mode local`)
candidate is not merged onto the base and not pushed, and a held owner
landing under `workflow.distributed_completion = "done"` stops before
merging or fast-forwarding anything; both leave their task in `review`.
Under the `review` policy nothing is held: a person already approved it. The task gets a comment and a
`completion_held` history entry such as
`held for review: paths .github/workflows/** matched .github/workflows/ci.yml (rule ci-workflows)`;
`orbit task show` prints it as `Completion Hold:` (JSON `completion_hold`),
the dashboard shows it under **Held for review**, and the run's `complete_pr`
step succeeds with the task in `held_task_ids`. Approve it as any task in
review: merge the PR, then `orbit task update <id> --approve`.

A delivery whose changed files cannot be read is held by any rule with
`paths`. No-diff and already-landed completions have no diff of their own, so
only complexity, type and tag rules apply to them. The `task_complete`
transition also refuses a task matching a complexity, type or tag rule, so
no completion route reaches `done` past it. For an owner landing, the rules
are the owner's, from the owner's workspace config.

Loading fails on an unknown key, an empty rule, a blank name, glob or tag,
a glob that is not a confined relative path, an exclude-only `paths`, an
unknown type, or a complexity outside the four levels (`unassessed` is also
refused). Only errors found after a rule decodes name the rule by its
1-based position (`completion.hold[2]`): an empty rule, a blank name, glob
or tag, a bad glob, an exclude-only `paths` list, or
`complexity_at_least = "unassessed"`. Schema errors name only
`completion.hold` and the bad field or value, without a rule position: an
unknown key, or an unknown complexity or type value. Verify with
`orbit config show completion.hold` and `orbit config keys`.

## Distributed-drain completion policy

`workflow.distributed_completion` decides how far the owner takes an accepted
distributed-drain handoff. It admits only `review` (the default) or `done`;
any other value fails config load. The value in force is always the owner's
own configuration: admission captures it into the claim, acceptance records
it as the handoff's authorization, and the landing pipeline rechecks it
before merging. A follower cannot grant itself this authority, and nothing a
follower sends changes what the owner requires.

| Value | Behavior |
|---|---|
| `review` | Every accepted handoff waits for an operator's **Approve handoff** on the owner's dashboard before anything lands. |
| `done` | Acceptance authorizes the handoff and the owner's landing pipeline merges and completes it, as `--complete` does for owner tasks. |

Set it in the owner's workspace, from the owner's registered checkout:

```bash
orbit config set workflow.distributed_completion done
```

Stay on the default until the drain and its landing path are routinely clean,
then opt in one workspace at a time. The key is read when each owner runtime
opens, so opting in or withdrawing reaches long-lived owner processes only
after they restart: restart `orbit web serve`, any active drain, and any
running landing worker's owner process after changing the key (a landing
worker already running keeps the value it opened with until it finishes),
then verify with `orbit config get workflow.distributed_completion`.
Reverting is the same write with `review`; because landing rechecks the key,
withdrawing `done` stops handoffs authorized by the owner policy that have
not landed yet. It does not stop a handoff an operator already approved,
which lands through the operator authorization path. Claims admitted before
opting in keep their `review` contract and still need **Approve handoff** on
the owner's dashboard.

`done` changes only who authorizes completion. It skips none of the gates
captured at admission: the owner's required validation commands, any review
gate in effect, and owed post-deploy criteria still hold — a handoff with
outstanding post-deploy measurements still settles in `review` until an
operator records each criterion met. For execution mechanics, see
[Distributed drain](../../orbit/references/setup/distributed-drain.md).

Verify the readback and the effective layer with its source:

```bash
orbit config get workflow.distributed_completion
orbit config get --scope workspace workflow.distributed_completion
orbit config show
orbit config show --all
```

`orbit config show` labels configured rows with their `workspace` or `global`
source, and rows with no configured value show `default`. The text rows do not
include file paths: see the `Layers (later wins)` banner for layer paths, or
use `orbit config show --json` for per-key path provenance. A workspace value
overrides the global one. Use `orbit config show --scope workspace` to inspect
one file's values without inheriting the other layer.

## Fleet notices, attention, evolution ledger and failing jobs

Fleet notice limits are fixed (below). The other settings are ordinary layered settings: a workspace value overrides
the global value, and the built-in default fills the rest. Set one for a
workspace with `orbit config set <key> <value>`, or for every workspace on
the host with `orbit config set --global <key> <value>`. Verify any one
with `orbit config get <key>`, and inspect the effective values with their
sources with `orbit config show` or `orbit config show --all`. The types,
defaults and ranges below match `orbit config keys --json` for the
installed version; check that command before applying version-dependent
values.

### Failing-job warning

The dashboard Jobs catalogue shows a failing badge on a job after 5
consecutive failed runs. The badge is display only: it never pauses, disables
or changes a job. `jobs.failure_streak_warn` loads with a deprecation warning,
is ignored, and stops being recognised after 2026-12-08. Delete it.

### Resident evolution ledger

Resident checkpoints record capability gaps in the evolution ledger, which
`orbit evolution` and `orbit resident digest` read. Nothing files evolution
proposal tasks automatically: the weekly evolution pass was removed.
A declined record reopens early only when its cost inside the window doubles.
The resident digest reads 14 days of ledger cost by default, a decline
measures the same 14 days, and a record stays declined for 30 days after its
proposal task is rejected. `resident.evolution.window_days` and
`decline_suppress_days` load with a deprecation warning, are ignored, and stop
being recognised after 2026-12-08.
`min_occurrences`, `min_distinct_cycles`, `min_distinct_mandates`,
`min_cost_minutes` and `max_proposals` belonged to the removed pass: they load
with a deprecation warning, are ignored, and stop being recognised after
2026-12-07. Delete them.

### Fleet notices

Fleet notices are short, expiring, attributed announcements. Their limits
are fixed; no key changes them:

- Default expiry: 72 hours for an incident, 12 for maintenance, 168 for
  info. A managed agent's notice defaults to, and is capped at, 24 hours.
- Longest expiry: 720 hours for an operator; 168 (incident), 72
  (maintenance) and 168 (info) for an orchestrator.
- Size: titles up to 120 characters, bodies up to 4096 UTF-8 bytes, at most
  8 evidence references.
- Open notices: 20 per workspace, 10 global per owner host, 2 per managed
  task.
- Post rates: 5 per managed task in 24 hours (corroborations included), 30
  per orchestrator crew per hour.
- Retention: closed and expired notices stay listable for 30 days.
- Agents: managed agents always may post and close their own notices
  through their bound task.
- Brief: opted-in managed activities always receive up to 5 open notices,
  each body cut to 600 characters; a read that takes over 2000 ms gives an
  unavailable brief.

The former `notice.*` keys load with a deprecation warning, are ignored, and
stop being recognised after 2026-12-08. Delete the `[notice]` table.

### Attention

The attention feed that reports work needing a human or operator has fixed
bounds: a delta-polling cursor stays valid for 24 hours, failed runs are read
over the last 48 hours, a run counts as stuck after 120 minutes of wall clock,
the base-branch CI reading is cached for 60 seconds, each source read has 10
seconds, a cross-host call has 20 seconds, each workspace contributes at most
200 candidates, and an item's severity steps up at 24 and 72 hours. The
`attention.*` keys load with a deprecation warning, are ignored, and stop being
recognised after 2026-12-08. Delete them.

## Host capability labels

`machine.labels` declares the capabilities available on a host. Set it in the
global configuration file under `[machine]`:

```toml
[machine]
labels = ["gpu", "cuda-12"]
```

Each host also carries its operating system as an implicit label: `linux`,
`macos` or `windows`. You do not need to add the OS label to `machine.labels`.
Orbit does not detect capabilities; it uses the labels you configure and the
host's OS.

Run `orbit doctor` to verify the host's effective labels in the `host-labels`
row. The row reports `machine.labels` together with the implicit OS label.

A task's `requires_labels` lists every label a host must carry to start it. A
host must match **all** of those labels. If no live host carries them, task
show (`Placement:`), task list and `orbit run readiness` report a placement
wait such as:

> waits for a host labelled `gpu` (requires_labels): no registered serving host carries them

The task stays in the backlog while it waits.

## Experiments

Experiments compare a control arm with one or more treatments for agent work.
Each treatment can vary instructions, skills, tools, crew, an allowlisted
configuration value, or crew memory. Crew memory is its own surface and stays
off until `crew_memory.enabled` is set; the procedure is
[Crew memory](crew-memory.md). The `[experiments]` table is workspace policy:

| Key | Default | Bound or effect |
|---|---|---|
| `experiments.enabled` | `false` | Boolean switch that permits experiments to start. |
| `experiments.config_keys` | `[]` | Advanced list of config keys an experiment may override. Empty allows no config overlays. |

For workspace policy, add or merge this table in
`<workspace>/.orbit/config.toml`:

```toml
[experiments]
enabled = true
```

At most 3 experiments run at once in a workspace, and a readout withholds its
verdict when any treatment arm's delivered rate is below 0.9.
`experiments.max_running` and `experiments.min_delivered_rate` load with a
deprecation warning, are ignored, and stop being recognised after 2026-12-08.

Each overlay config key needs two permissions: the built-in typed allowlist
and `experiments.config_keys`. The workspace list alone enables nothing else;
only allowlisted keys are supported, and any other key, a value of the wrong
type, or a number outside its bounds is refused when the experiment starts.
The built-in allowlist holds exactly two keys, and neither has an effect
today; both are kept so historical experiments still load:

- `workflow.explore.max_calls_per_run`, an integer from `1` to `100`,
  ignored since explorer removal.
- `visual_review.reviewer`, a boolean, ignored since visual review was
  removed.

To opt in, list only the keys the arms will override.
Read the workspace permission back with
`orbit config get experiments.config_keys`, and use
`orbit config show --all` to inspect effective values with their source, or
`orbit config get <key>` for one value.

An operator manages experiment definitions and lifecycle with these commands.
The syntax shows each command's required arguments; use `--help` for optional
flags and their values. Replace `<ID>` with the experiment's ID or name.

| Command | Use it when | Verify with |
|---|---|---|
| `orbit experiment create [OPTIONS] <NAME>` | Define a new draft. Options include `--hypothesis`, `--arms`, `--population`, `--primary-metric`, `--guardrails`, `--min-sample`, `--max-days` and `--mode`. | `orbit experiment show <ID>` |
| `orbit experiment update [OPTIONS] <ID>` | Change a draft's definition before starting it. | `orbit experiment show <ID>` |
| `orbit experiment clone [OPTIONS] --name <NAME> <ID>` | Copy an existing definition into a separate draft with a new name. | `orbit experiment show <NAME>` |
| `orbit experiment start [OPTIONS] <ID>` | Start a ready draft, opening its first segment and freezing its definition. | `orbit experiment show <ID>` |
| `orbit experiment pause [OPTIONS] --reason <REASON> <ID>` | Pause a running experiment when its enrollment should stop temporarily. | `orbit experiment show <ID>` |
| `orbit experiment resume [OPTIONS] <ID>` | Resume a paused experiment. | `orbit experiment show <ID>` |
| `orbit experiment reweight [OPTIONS] --reason <REASON> <ID>` | Change arm weights or retire arms in a running or paused experiment; this opens a new segment. Supply `--weights <WEIGHTS>` and/or repeat `--retire <RETIRE>` as needed. | `orbit experiment show <ID>` |
| `orbit experiment assign [OPTIONS] --task <TASK_ID> --arm <ARM> <ID>` | Force a nonterminal task into an active arm of a running experiment in shadow or live mode, when it has no assignment for that experiment or only an `explicit_crew` exclusion. | `orbit experiment show <ID>` |
| `orbit experiment conclude [OPTIONS] --reason <REASON> <ID>` | Finish an experiment and record the chosen arm with `--arm <ARM>`, or omit `--arm` to record no choice. | `orbit experiment readout <ID>` |
| `orbit experiment abandon [OPTIONS] --reason <REASON> <ID>` | End an experiment without recording a conclusion. | `orbit experiment show <ID>` |
| `orbit experiment delete [OPTIONS] --reason <REASON> <ID>` | Remove a finished experiment that never applied a treatment. | `orbit experiment list` (confirm it is absent) |

Existing arm assignments and exclusions remain sticky. Enrollment remains
historical, but concluded and abandoned experiments apply no treatment to a
fresh admission. Existing owner runs and claim receipts keep their frozen
treatment on resume. For a fresh crew admission, an explicit run choice or a
later audited task pin wins; a force supersedes an older task pin (and wins an
exact timestamp tie). A crew override suppresses the whole crew-bearing arm and
records the intended digest and actual crew without rewriting enrollment. An
agent session may only use these read-only commands:

- `orbit experiment list`
- `orbit experiment show <id-or-name>`
- `orbit experiment readout <id-or-name>`

The other experiment operations are operator-only.

Choose `shadow` to record assignments without applying treatments. Choose
`live` to apply the assigned arm's instructions, skills, tools, crew and
allowlisted config values to work. A memory overlay is
`{"memory":{"roles":["implement"],"global":false}}`. `roles` lists
`implement`, `review` or `recovery` and defaults to `implement`. `global`
defaults to false and includes the shared global space when true. Memory is
its own surface, so one experiment per task may own it. `start` refuses a
memory arm while `crew_memory.enabled` is false. That key defaults to false
and is independent of `experiments.enabled`. In `shadow` mode the owner
records the memory capture and delivers nothing. In `live` mode the assigned
arm delivers it. The seeded `crew-memory-dream` routine does not turn the
key on. Inspection, write rules, proposals and both kill switches are in
[Crew memory](crew-memory.md). Verify state from the command line with
`orbit experiment list [--status <status>]`,
`orbit experiment show <id-or-name>` (arms and event log) and
`orbit experiment readout <id-or-name> [--metric <name>]` (per-arm outcomes
and intervals reported throughout; verdict reads `collecting` until the
horizon and `withheld` when a sample-ratio or treatment-delivery check fails).
Readouts calculate per-arm outcomes from the recorded assignments
and work records. Or open the dashboard's **Analytics › Experiments** page
for the list with per-arm enrollment and each experiment's readout: the
detail renders from the experiment record immediately while the readout
loads beside it, so a slow or failed readout holds only the **Result** and
**Trust** tabs, which show their own loading or error state with a Retry
instead of failing the page. The header and **Setup** tab stay available.

## Review baseline commands and owed host evidence

These settings govern the checks review settlement may rerun and the checks an
owner may owe after a claimed review. They do not turn on before-PR or before-landing review; set
`review.timing` separately when that review workflow is wanted.

### Set the policy

`review.verify_findings` was removed with the finding-verification routine in
this release. It loads with a deprecation warning and is ignored until
2026-12-07, and nothing holds a review-filed bug as
`unverified_review_finding`. An invalid finding is still rejected by the task
pilot's `finding_invalid` result or an implementer's `finding-invalid.json`,
which record a REJECTED verdict.

`review.baseline_commands` is an array of owner-trusted commands, default
`[]`. Review settlement may use them, along with
`workflow.required_validation_commands`, to rerun an eligible failed check on
the host and determine whether a reviewer's red-base claim also fails on the
base. The reviewer cannot authorize an arbitrary host command by naming it in
the report. If a listed command cannot be confirmed on the base, the review
settles incomplete; the list is therefore binding policy, not a hint. These
commands also count as required validation gates an implementer may hand off
when its sandbox deferred only an admitted sandbox-dependent test.

`review.baseline_commands` can be written with `orbit config set`, for example:

```sh
orbit config set review.baseline_commands '["cargo test -p service --test api"]'
```

`[[review.host_evidence]]` has no `config set` key. Add its array-of-tables
directly to `<workspace>/.orbit/config.toml` or `~/.orbit/config.toml`.
The workspace list replaces the entire global list, including when set to
`[]`. Replace the example paths, package, target and analysis command with
ones that exist in your project:

```toml
[review]
baseline_commands = ["cargo test -p service --test api"]

[[review.host_evidence]]
kind = "host_sandbox_test"
name = "Linux sandbox tests"
paths = ["src/sandbox/**", "tests/sandbox/**"]
os = "linux"
command = "cargo test -p service --test sandbox_tests"
artifact = "evidence/linux-sandbox.json"

[[review.host_evidence]]
kind = "codeql"
name = "Linux code analysis"
paths = ["src/**"]
os = "linux"
command = "scripts/run-analysis.sh"
artifact = "evidence/linux-code-analysis.json"
hosted_check = "CodeQL / Analyze"
```

Each rule admits only these fields: `kind`, `name`, `paths`, `os`,
`command`, `artifact`, and optional `hosted_check`; unknown fields are
rejected. `kind` is exactly `codeql` or `host_sandbox_test`, `os` is
`linux` or `macos`, `name` and `command` must be non-empty, and `paths`
must contain at least one valid workspace-relative glob. The `artifact` must
be a unique relative `.json` path across rules. A `host_sandbox_test`
command must use the admitted
`cargo test -p <package> --test <target> [<one-filter>]` form (or use
`--package` in place of `-p`); shell metacharacters and other command
shapes are refused.

A `codeql` rule is owed when the claimed review host has a different OS from
the rule's `os`, and the owner fulfils it on the named OS. A
`host_sandbox_test` rule is owed on a host of its named OS and is run outside
the agent sandbox at settlement. Windows is not a valid evidence OS. A
`hosted_check` is allowed only on a `codeql` rule with `os = "linux"`, and
it must be non-empty and name a pull-request check that runs exactly the rule's
`command` on every candidate. For a before-landing claim on a different known
OS, this lets the owner treat that CodeQL check as owner-owed evidence and
wait for explicit success on the reviewed head before landing. A skipped or
neutral check is not success. When a CodeQL check is owed on another OS,
without `hosted_check` the review remains held until the owner supplies the
required evidence.

At claim admission, the owner captures validation gates including
`review.baseline_commands`; when the claim includes a before-PR or
before-landing review, it also captures `review.host_evidence`. The claimed
leaf follows those owner snapshots, not its own config. Updating owner policy
affects new claims. If the captured policy no longer matches the owner's
current policy at acceptance, the handoff fails closed and needs a fresh claim.

### Verify the effective policy and its effect

Run `orbit config show --all` to inspect the effective values and provenance
for `review.baseline_commands`. Use
`orbit config keys --json` to confirm which settings the installed version
supports; `review.host_evidence` is TOML-only and is not a `config set` key.
Check the global and workspace config files when reviewing host-evidence
rules, since a workspace list replaces the global one, and run `orbit doctor`
to catch configuration errors before a claim.

For a claimed review, inspect the
task's `review-gate.json` artifact with
`orbit task artifact get <task-id> review-gate.json --out <file>`:
`owed_evidence` records checks the reviewer did not run and are pending host
fulfilment, not failed checks. Compare with validation records or
host-evidence outcomes: an executed command with a failed result is a failed
check and follows normal review recovery.

Use `orbit config keys` to distinguish fixed registry keys from settings
authored as TOML. Read-only identity keys are listed and refused by
`config set`. Named crew fields are also settable as
`crews.<name>.<field>` (for example `orbit config set crews.sol.effort high`)
even though those keys are not listed by `orbit config keys`. Creating a crew
still requires a `[crews.<name>]` table with `model` and `provider`. Set
workspace ship mode with
`orbit workspace init --ship-mode pr|local`, or rebind a registered workspace
without re-initializing it with `orbit workspace ship-mode pr|local` (no
argument prints the current mode); verify with `orbit workspace show`. PR mode
needs a Git remote on a forge host: when no remote names a network host, ship
and the drain refuse untagged tasks before dispatch and `orbit doctor` warns on
its `forge-remote` row. Base branch and ship mode govern source delivery,
not task snapshot publication.

## Scheduled task publication

The disabled weekly `task-publication-<workspace>` routine publishes only its
owned workspace and always omits attachments; it has no settings. Manual
publication keeps its CLI attachment policy.
See [publication.md](publication.md#weekly-publication-routine) for the schedule,
owner-only behavior, credentials and enablement.

## Recovery settings

Read effective values with `orbit config show --all` before editing workspace
policy. These examples use installed commands; use the workspace's actual
generator and authorized documentation checks for its own files.

```toml
[workflow.generated_paths]
"generated-config-keys.json" = "orbit config keys --json > generated-config-keys.json"
```

Generated paths apply only to conflict recovery for a stopped rebase, not
ordinary edits or merges without conflicts. Relative globs use `*` within a
component and `**` across components; absolute paths and `..` are refused.
The agent resolves other files; the host takes upstream's generated file (the
replayed side if upstream deleted it), runs each distinct command once with the
validation environment and stages its output. Each command has ten minutes;
nonzero exit, timeout, remaining conflict markers, or a change to HEAD, branch
or index refuses the repair. Commands must be trusted and must not stage or
commit. Verify the certified continuation's `regenerated_paths` and output.

```toml
[workflow]
unlocked_context_paths = ["**/*.md"]
# Example for config-documentation repairs: checks supported/effective policy.
unlocked_path_validation_commands = ["orbit config keys", "orbit config show"]
```

Unlocked paths control context locks, not permission to edit a file. An
all-matching task still locks its entries; owner pull admission still checks a
remote claim's full footprint. The globs are read when the process opens the
workspace. Keep code, build and CI definitions outside them.

For prose repairs, including conflict, step and final recovery, narrowing
compares the entire candidate tree with this run's last full pass of
`workflow.required_validation_commands`. Every changed path must be unlocked;
a code change brought in by rebase requires the full list. The first validation,
a changed required list, another process resuming the run and a claimed leaf
all require a full pass. Empty narrowed commands keep the full list.
Each validation log and step output records `validation_scope` (`full` or
`narrowed`, reason and compared trees); a narrowed pass never becomes the
comparison point and cannot satisfy completion evidence requiring the full
list. Verify that record, rather than an agent's account of what changed.

### CI infrastructure reruns

When every authenticated failure on a pull-request head is logged
infrastructure/transient, the watcher reruns the failed jobs instead of
starting a code-repair agent. Two keys bound that recovery; the read-only
[GitHub evidence sweep](automation.md#github-evidence-sweeps) lists such jobs
under `infrastructure` without rerunning them.

- `workflow.ci_infrastructure_retry.max_attempts`: total failed-jobs reruns for
one pull-request head. Default `3`; integer `0`..`10`. `0` disables this
recovery, so logged infrastructure failures keep their usual route.
- `workflow.ci_infrastructure_retry.backoff_seconds`: initial delay between
reruns. Default `60`; integer `1`..`3600` seconds. The first rerun is
immediate, then each reservation doubles the wait (by default `60`, then
`120` seconds, and so on).

`workflow.ci_infrastructure_signatures` selects what counts: a failed job
matches only when a line it emitted starts with one of those prefixes.
Declare only failures a rerun can clear; anything else keeps its repair
route, and no repair agent runs for a match.

`workflow.ci_infrastructure_retry.handoff_steps` covers a job that failed
while obtaining another job's output, whose log is often truncated or names
no failing command. Each entry is a step-name pattern matching the whole
name, case-sensitively, with `*` for any characters, such as
`["Download nextest archive", "Await the *"]`. When every failed job's one
failed step, as the provider's job listing reports it, matches a pattern,
the set gets these reruns from the step names alone, without reading logs.
A failed job in any other step, or with several failed steps, leaves the set
to its usual classification. Default `[]` declares none.

Behavior notes:

- The whole failure set must be logged infrastructure/transient. Any
candidate failure excludes the set, including a candidate sibling beside a
transient failure. Flaky failures, conclusion-only cancellation or timeout
sets, and a failure whose log is only a ci-await timeout keep their existing
single rerun rather than this budget.
- Reservations are durable per head across restarts and workflow runs,
including unknown send outcomes; an unchanged provider attempt is never
resent. A new head gets its own budget.
- The existing watch or completion deadline is never extended. Backoff uses
durable reservation timestamps and avoids re-reading completed-job logs or
authenticating another rerun while one is owed. Provider throttling uses the
recorded read deferral and spends no rerun attempt.
- Once the budget is spent, `ci_infrastructure_retry_exhausted` appears in
task history naming `classification=transient` and the attempt count; no
repair agent runs and the candidate stays in review. A passing retry resumes
the existing observation with its merge authority unchanged.
- Inspecting this policy, or changing it where authorized, grants no
repair or merge authority by itself.

Discover, read and — only where authorized to change policy, for example a
disposable fixture — set the keys with the installed commands:

```sh
orbit config keys | grep ci_infrastructure_retry
orbit config get workflow.ci_infrastructure_retry.max_attempts
orbit config get workflow.ci_infrastructure_retry.backoff_seconds
orbit config show --all | grep ci_infrastructure_retry
# Only where authorized to change policy:
orbit config set workflow.ci_infrastructure_retry.max_attempts 3
orbit config set workflow.ci_infrastructure_retry.backoff_seconds 60
```

Confirm the exhausted-budget record with `orbit task show <task-id>` history.
Use `orbit config show --all` for effective values and provenance, and
`orbit doctor` to catch configuration errors before relying on a change.

## Crews

A crew is one named provider-and-model assignment. A task's `crew` field selects
its executor; `workflow.default_crew` covers the rest.

Optional `effort` tunes the existing provider-mapped reasoning vocabulary.
Any crew reference (pools, `default_crew`, task `crew`, `--crew`) may
override it as `<crew>@<effort>`: the reference wins over the crew's `effort`, is a
distinct pool member and scorecard row, shares the crew's account, and an effort the
provider rejects is refused at load (never ignored). Crew names may not contain `@`.
Each launch records the effective value after crew/inline precedence and
admission, alongside its resolved crew and model. Run inspection with
`orbit run show --json` or the operator-only `orbit.run` action `show` MCP tool
exposes `agent_executions` on the run and displayed steps:
`reasoning_effort: null` means provider default; `"unknown"` means historical
execution without a record. Editing a crew never rewrites history.
Scorecards, experiment arms and managed usage include effort
breakdowns; inspect those populations when tuning a crew. Scorecard rankings
retain their crew/model score, with effort shown as descriptive execution usage.

### Automatic effort selection: `<crew>@auto`

A pool entry can leave the effort to each draw instead of fixing it:
`<crew>@auto`, or `<crew>@auto:100` in a weighted pool. At admission the draw
resolves the entry through the crew's ordered effort bands and runs at the
resolved reference. The run's crew identity is that resolved reference:
`<crew>@<effort>`, or the bare crew when the crew's configured effort applies
and names none. The resolution record keeps the pool entry:
`auto_effort.requested` is `<crew>@auto`, and the draw's source note still
contains `<crew>@auto` (see Verify below).

**Where `@auto` is allowed.** Only these pools accept it:

- the implementer complexity pools, `workflow.low_complexity_crews`,
  `workflow.medium_complexity_crews`, `workflow.hard_complexity_crews` and
  `workflow.xhard_complexity_crews`;
- the per-type implementer pools, `workflow.type_complexity_crews`;
- `workflow.purpose_crews.task_pilot`;
- the review pools, `workflow.purpose_crews.review` and
  `workflow.purpose_crews.pre_landing_review`.

Everywhere else it is refused: any other pool, `workflow.default_crew`, a
task's `crew` field, and `--crew` or other run-time crew overrides. The
refusal happens when configuration loads, on `orbit config set`, and on run
overrides, so an unsupported field never silently accepts it. A crew also
needs bands (below) before any pool may name it with `@auto`; a crew without
bands is refused the same way.

**Bands.** `workflow.auto_effort.bands` maps each crew — by its bare
`[crews.<name>]` name, never a `<crew>@<effort>` reference — to an ordered
list of bands. Bands default to empty (`{}`): no crew has bands, and every
`@auto` entry is refused, until an operator configures them. Each band sets
an `effort` the crew's provider and model accept, plus at least one
condition. The first band whose conditions the draw's signal all meets
chooses the effort; later bands are not consulted.

| Condition | Meets it when |
|---|---|
| `complexity = ["hard", "xhard"]` | The signal's tier is listed. Only `low`, `medium`, `hard` and `xhard` are valid; `unassessed` is refused. |
| `below_files = N` | The signal counts fewer than `N` files (`N` above 0). |
| `below_lines = N` | The signal counts fewer than `N` reviewable lines (`N` above 0). |

A band with no condition, a bound of 0, an unknown crew key, an empty band
list for a crew, or an effort the crew's provider and model reject all fail
configuration load with a message naming the band. To drop a crew's bands,
remove its key; an empty list is refused, not treated as removal.

Each role resolves from the best signal it has at admission:

| Role | Complexity signal | Size signal |
|---|---|---|
| Implementer | The task's assessed tier | `below_files` counts the distinct files the task's declared context files name (the pre-dispatch footprint). An implementer never has a line count, so `below_lines` never matches it. |
| Task pilot | The task's filed tier only | None; the pilot is what produces a size. |
| Reviewer | The reviewed tasks' highest settled tier | `below_lines` counts the candidate's reviewable lines and `below_files` its changed files. |

A condition on a signal the role does not have does not match. With no
signal at all, or when no band matches, the draw falls back to the crew's
own configured effort: the bare crew when its table sets no `effort`,
otherwise `<crew>@<effort>` from the table. A task keeps its `@auto` pool
member while the effort it resolves to moves; the next admission re-resolves
it against the task's current tier and footprint.

**Exploration.** A fixed share of `0.1` (10%) of seeded draws is dispatched at
another effort the crew can resolve to — one its bands name, or its
configured effort — so each side of a cutover keeps getting data. The roll
is seeded per draw (by the task for implementers, by the run and partition
for pilots, by the run and candidate head for reviewers), so a retried
selection resolves alike. A task's creation draw never explores, and a draw
with no subject to seed it never explores. An explored draw records
`explored: true` together with the policy reference it replaced.

**Bounded pilot escalation.** Escalation applies only to a task pilot that
ran at an `@auto` resolution, and only once. When the pilot raises the task
above its filed tier to a tier whose band maps to a higher effort than it
ran at, or reports confidence `medium` or `low` (the target is then the
highest effort the crew's bands name), the assessment is held back and the
run re-pilots the task once at the higher effort; that second assessment is
the one applied. A target at or below the effort the pilot ran at never
escalates (a crew whose table sets no effort counts as below every band
effort for that comparison), and the re-pilot never escalates again.

**Example.** Replace `example-crew` with a crew defined on the host, and
name that same crew with `@auto` in one of the supported pools:

```toml
[workflow.auto_effort]
bands.example-crew = [
  { complexity = ["hard", "xhard"], effort = "xhigh" },
  { below_files = 4, effort = "medium" },
]

[workflow]
medium_complexity_crews = ["example-crew@auto:100", "example-crew@medium:50"]
```

Order matters: a hard task with few files runs at `xhigh`, because the
complexity band comes first. Keep the fixed-effort member beside the `@auto`
member only when both shares are wanted; they are distinct pool members.

**Verify without dispatching production work.** Configuration alone proves
the policy is admitted, not that any draw has used it:

1. Read the policy back: `orbit config get workflow.auto_effort.bands` and
   `orbit config show --all`. The show must load without error and report
   each value with its source. A band against an undefined crew, or a band
   with no condition fails here, before any
   dispatch.
2. Confirm each `@auto` entry sits in a supported pool above and names a
   crew that has bands: `orbit config show` lists the pools. Do not test
   an unsupported field by setting it there; load and `orbit config set`
   refuse it by design.
3. Treat a live rollout as proven only by a recorded resolution. After a
   real draw, inspect it (`orbit run show --json` for a run, or the task's
   recorded crew selection). The implementer's crew selection stores the
   resolution record under `auto_effort`: `requested` (`<crew>@auto`),
   `resolved`, `policy`, `basis` (`band`, `fallback` or `escalation`), `band`,
   `explored` and `signal`. `band` is the zero-based index of the matching
   band and is present only for basis `band`. The draw's source note reads
   `auto effort <crew>@auto -> <resolved> (<basis>)`, where basis is `band N`
   (N is the one-based band), `configured effort` or `pilot escalation`. An
   explored draw reads `auto effort <crew>@auto -> <resolved> (explored;
   policy <policy> by <basis>)`. A pilot partition draw appends this note to
   its `source`, and an admitted reviewer appends it to its `crew_source`.
   Until such a record exists, report the feature as configured, not rolled
   out.


```toml
[crews.reviewer]
provider = "claude"
model = "opus"
description = "Deep review passes"
tags = ["review"]
```

Crews layer by name *and* field, so a workspace can override just the model of a
globally defined crew. A `default_crew` naming an undefined crew
fails config load — deliberately, since the alternative is silently dispatching
to the wrong model.

`enabled = false` on a crew keeps it defined and listed (`orbit config show`
has an `ENABLED` column; `orbit.workspace.list` with `include: ["crews"]`
returns `enabled` per crew) but takes it out
of execution. Omitting `enabled` means enabled, so older configs are unchanged.
A disabled crew is never drawn from a complexity pool; a pool whose members are
all disabled routes the task to `default_crew`, like an empty pool. A task
`crew`, explicit crew or `default_crew` that
lands on a disabled crew refuses dispatch with an error naming the crew — no
substitution. Config still loads; `orbit doctor` warns when a lane key names a
disabled crew. Enable one with:

```bash
orbit config set crews.argon.enabled true
```

A crew can also opt out of image input with `vision = false`; see
[Executor image input](#executor-image-input-image_input).

A crew can run on its own provider account through `env`, a list of variables
its agents receive on top of `execution.env.pass` (names only, never values):

```toml
# ~/.orbit/config.toml only
[crews.opus-credits]
provider = "claude"
model = "claude-opus-5-5"
env = ["ANTHROPIC_API_KEY=ANTHROPIC_CREDITS_API_KEY_1"]   # or a bare "NAME"
```

`NAME=SOURCE` gives the agent `NAME` with the value of `SOURCE` from the
launching shell (or `~/.orbit/clock.env` for clock-started runs); a bare `NAME`
passes that variable to this crew only. `NAME=file:/abs/path` (or `file:~/path`)
reads the value from an owner-only file the host opens when it starts the agent,
so no shell, service or `clock.env` has to export it (a symlink, a file owned by
another user, a group- or world-readable, missing or empty file leaves the crew
unavailable). The entry wins over `pass`, and setting
one Claude credential removes the other from that crew. `env` is global-only:
a workspace `config.toml` that sets it fails to load, and `orbit config set`
needs `--global`. With the source unset, drain, ship and job starts warn, the
drain leaves the crew out of its window, the crew's activities are refused
rather than run on the default login, and `orbit doctor`'s `env-pass` row
reports it. A crew with `env` is its own account: its usage readings, failure
holds, pull-drain auth exclusions and probes are its own, and
`workflow.provider_limit_budgets` / `provider_limit_overrides` may name it
(`opus-credits:200usd/30d`).

`orbit init` writes every built-in crew (Claude: `opus`, `sonnet`, `haiku`, and `fable` pinned to `claude-fable-5-2`;
Codex: `astra`, `sol`, `terra`, `luna`; Antigravity: `gemini-flash`, `argon`;
Grok: `grok`; Copilot: `copilot`; Muse: `muse-spark`, `muse-wonder`, `muse-wonder-full`;
no built-in Gemini CLI crew; the `cursor`, `pi`, and `opencode` lanes were [retired](providers.md#provider-identity-and-resolution)), with `enabled = true` on the crews whose
agent CLI it detects and `enabled = false` on the rest, and points
`workflow.default_crew` at the preferred family's default (`opus` on a Claude
host, `gemini-flash` on an Antigravity host). Interactive init offers the enabled crews by name; `--non-interactive`
writes the recommendation. Init does not write a `custom` or `system` crew
table, nor a system-crew key. The four
`workflow.*_complexity_crews` pools are seeded from the detected families
(Claude only: `haiku` / `sonnet` / `opus` / `fable`; Codex only: `luna` / `terra` /
`sol` / `astra`; both: `haiku, luna` / `terra, sonnet` / `sol, opus` / `fable, astra`;
grok joins `medium`; a detected `agy` appends `gemini-flash` to `low` and `argon` to `hard`;
a detected `muse` appends `muse-spark`, `muse-wonder`, and `muse-wonder-full` to `low`, `medium`, and `hard`;
Copilot and other families leave them `[]`). A user-authored
legacy `qa` crew remains loadable, but init never creates it. Init also writes the
`workflow.purpose_crews` pools as fixed lists filtered to the enabled crews: `review` from
`sol, sonnet, muse-wonder, grok, opus@medium`; `task_pilot` from `luna@high, haiku@high,
muse-spark@high`; `failure_recovery` from `luna, haiku, muse-spark, grok`; `auto_fix` and
`pre_landing_review` from `luna, haiku, muse-spark`; `final_recovery` from `sol, muse-wonder,
opus@medium`; `baseline_red_repair` from `terra, sonnet, muse-wonder, grok`. A pool left empty
is omitted, except `final_recovery`, written as `[]` (off). Editing
`workflow.*_complexity_crews` later does not move those purpose pools; to move system
work, edit its `workflow.purpose_crews.<purpose>` pool directly. Creating this machine's
identity also sets `machine.auto_concurrency.mode = "enforce"` (an existing machine keeps
its mode), and the seed sets `experiments.enabled = true`,
`workflow.crew_selection = "static"`, `workflow.mid_run_steering.enabled = true`,
`workflow.task_pilot_freshness.material_fields = ["description", "criteria"]` and
`workflow.unlocked_context_paths = ["**/*.json", "**/*.txt", "**/*.md", "**/mod.rs", "**/*.toml"]`. With no supported
agent CLI, every crew is written disabled and no default crew is set, so nothing
dispatches until you enable a crew. Which provider CLIs
this machine can launch, and each executor's sandbox mode:

```bash
orbit doctor providers
```

Executors are how a provider is invoked. You choose crews; executors are
infrastructure.

For provider IDs, dispatch precedence and refusal diagnostics, see
[Provider identity and resolution](providers.md#provider-identity-and-resolution).
To add a lane or fix its login, follow [Provider setup](providers.md#provider-setup).

### Crew names containing a colon

The pool grammar reserves `:` for weights: `workflow.*_complexity_crews` and
`workflow.purpose_crews.*` entries are written `crew[@effort]` or `crew[@effort]:weight`. Orbit
therefore refuses any crew whose name contains a colon, both where it is
defined and in `orbit config set crews.<name>.<field>`. It checks each config
file on its own as it loads, before the layers merge. A `config.toml` that
already defines such a crew, such as `[crews."gpt-5:codex"]`, stops every
command that loads configuration, including `orbit config show`,
`orbit config set` and `orbit doctor`. The refusal names the defining file:

```
error: invalid input: [crews]: crew name 'gpt-5:codex' must not contain ':'; ... The crew is defined in '~/.orbit/config.toml'; Orbit refuses to load that config, so `orbit config set` cannot repair it either. Edit '~/.orbit/config.toml' and rename or remove the [crews."gpt-5:codex"] table, then rerun the command
```

Two routes that look like repairs do not work:

- `orbit config set` loads the same files first, so it is refused too. Edit the
  file by hand.
- A workspace override does not help. The global file is still checked before
  the merge, so a valid `<workspace>/.orbit/config.toml` leaves the refusal in
  place. A workspace table with the same colon name is refused by the same check.

Repair it by editing only the file the refusal names:

1. Rename the table to a name without a colon, such as `[crews."gpt-5-codex"]`,
   or delete it if nothing uses it. Keep the crew's other fields, every other
   table, and any `env` entries unchanged; those entries hold variable names,
   not values, so leave them as they are.
2. Update every reference to the old name:
   - `workflow.default_crew`. When any crew is defined, Orbit requires this key
     to name one, and a value naming an undefined crew fails the load with
     `crew '<name>' is not defined in [crews.*]`.
   - Each entry of `workflow.*_complexity_crews`, `workflow.type_complexity_crews`
     and `workflow.purpose_crews.<purpose>`: replace the old name and keep any
     `:weight` suffix on that entry.
   - A task's crew, with `orbit task update <task-id> --crew <new-name>`.
     Changing it to an empty value draws a fresh crew instead.
3. Run `orbit config show`. It should exit 0 and list the renamed crew with its
   `ENABLED` state. Then run `orbit doctor`. Once configuration loads, a later
   row about a provider CLI, sandbox or permissions is a separate finding, not
   a symptom of the crew rename; for a missing CLI, follow
   [Provider setup](providers.md#provider-setup).

## Executor definitions

Four executor settings decide whether a crew can run on a host and how it
recovers. They live in the installed executor definition (YAML, beside
`sandbox` and `model_flag`), not in `config.toml`, so `orbit config` never
shows them. Edit the installed definition to change them: reseeding preserves
an operator-set value and only fills in fields the install omits, and custom
executors outside the shipped set are never rewritten.

### `host_os`: which hosts may run this executor

A non-empty subset of `linux`, `macos` and `windows`. An empty list or any
other name is refused when the definition loads. Omit `host_os`, or set it
null, and the executor runs on every host; no shipped executor pins it.

```yaml
spec:
  host_os:
    - macos
```

A task's effective OS requirement is its `os:` tags intersected with its
assigned crew's executor `host_os`. An empty intersection is refused when the
task is created or updated, and an explicit crew this host cannot run stays
assigned and waits instead of being redrawn onto another crew.

Verify with `orbit doctor providers`: an executor `host_os` excludes this
host reports `unsupported on this OS`, even when its binary is absent
(`not installed` is only for an executor this host may run whose command is
missing), and the full `orbit doctor` agrees. Waiting tasks name the mismatch
(`host_os_mismatch`); filter candidacy with
`orbit task list --runs-on linux|macos|windows`.

### `auth_probe`: re-admitting a pull-drain provider after auth failures

Only a pull drain's auth-excluded providers run the probe: never at drain
start, for healthy crews, after the window closes, or for capacity or refusal
exclusions. The first attempt is due ten minutes after the failed leaf, then
backs off (twenty, then thirty minutes, capped). A pass re-admits that
provider account's crews within the window and never clears any other
exclusion; a crew on its own account is excluded, probed and re-admitted
alone. Without a probe the exclusion lasts the whole window: re-login and
start a new drain. Only the shipped definition for one provider declares one.

Opt in by adding the declaration to the installed definition and starting a
drain. The probe reuses the executor's resolved command, replaces the activity
argv with `args`, and requires exit zero plus a marker line on stdout;
`timeout_seconds` is 1–120. Verify with `orbit doctor`
(`provider-auth:<provider>`, or `provider-auth:<provider>/<crew>` for a crew
account) and `orbit run show <drain>`: both name the account, exclusion time,
re-login hint and next probe time. Doctor reads the record and calls no
provider.

### `inspection_args`: tools for pinned-source inspection runs

The arguments appended to the executor's argv for an inspection run (task
pilot, escalation triage), so the CLI's own file-read
and shell tools work unattended. An empty list means the static `args`
already enable those tools; omitting the field means no known inspection
surface. Inspection draws skip crews without it, and an explicit crew that
names one fails before launch with `InspectionToolsUnavailable`. Read-only is
enforced by the `reviewer` filesystem profile, not by these arguments.

Two shipped definitions declare it (an empty list for one, feature flags for
the other). Declare the arguments on the installed definition once the CLI's
native tools are known to work non-interactively under the `reviewer`
profile; an installed omission takes the shipped default of the same name,
while a set value wins.

### Shared Cargo-cache sandbox grants

Both OS sandboxes let workers share one Cargo cache: the registry, git
checkouts and download locks under `$CARGO_HOME` are read-write, `bin` is
read and execute only, credentials are read-denied, and the cache root itself
is read-only. `$CARGO_HOME` is the child's variable when
`[execution.env].pass` names it, else `~/.cargo`. There is no executor field
for this: the grant applies only to profiles that already grant some write
(`implementer`, `unrestricted`, `docs_writer`, …) and never to read-only
profiles. On Linux a cache path missing on the host is skipped, not created.

To use a non-default cache location, pass it through
`[execution.env].pass`; to change what a worker may write, change the
activity's filesystem profile, not the executor. Dry-run a path with
`orbit doctor fs-access <profile> <path>`.

## Executor image input (`image_input`)

Image input lets a crew's model read image files. Any activity whose input
asks for images needs it. Images never travel in prompt text: Orbit stages each one as a
file in the run's scratch directory, and the executor presents it to the model.

### Declare it on the executor

Declare `spec.image_input` in the executor's installed definition, the YAML file
`resources/executors/<executor>.yaml` under the Orbit home (`~/.orbit`), beside
`sandbox` and `model_flag`. Like the other executor settings, it is not in
`config.toml`, so `orbit config` does not show it.

```yaml
spec:
  image_input:
    mode: attach_args          # or native_read
    formats: [png, jpeg, webp] # png is required: it is the canary's format
    max_images: 40             # canary included; at least 2
    attach_args: ["--image", "{path}"]   # attach_args mode only
```

- `mode: native_read` presents the staged images through the CLI's own
  file-read tool. The prompt lists their paths and tells the agent to read each
  one. It takes no `attach_args`.
- `mode: attach_args` appends `attach_args` to the argv once per image, with
  `{path}` replaced by that image's staged path. Only scratch paths reach argv,
  never image content. The template must contain `{path}`.
- `formats` must include `png` and must not repeat a format.
- `max_images` is at least 2, and it counts the canary.

A definition with an invalid declaration fails to load with
`invalid spec.image_input: …`, naming the problem. An unknown key is refused the
same way.

**Omission and inheritance.** An executor that omits `image_input` cannot take
images. A request for images on a crew that uses it fails before launch with the
non-retryable `image_input_unavailable` error, which names the executor and
`spec.image_input`. An installed definition that omits the field takes the
shipped default executor's declaration of the same name, so existing installs
inherit it without reseeding. A value set on the installed definition wins.

The shipped declarations are:

- **claude**: `native_read`, for PNG, JPEG, WebP and GIF.
- **codex**: `attach_args` with `--image {path}`, for PNG, JPEG and WebP.
- **muse**: `attach_args` with `--image {path}`, for PNG.
- **gemini**, **antigravity**, **grok** and **copilot** declare none.

Each shipped declaration rests on a lane that passed the canary probe below.
To test a lane that declares none, add a candidate declaration to its installed
definition, probe that crew, and keep the declaration only if its row reads `seen`.

### Opt a crew out with `vision = false`

A crew can take images only when its executor declares `image_input` and the
crew's `vision` is not `false`. `vision` defaults to `true`. Set `vision = false`
on a crew whose model keeps returning `image_not_seen` while the same executor
passes for other crews, such as a text-only model:

```bash
orbit config set crews.<crew>.vision false   # set it back to true to re-enable
```

A `vision = false` crew still does ordinary work. Its image requests fail with
`image_input_unavailable` and its probe row reads `vision_off`.

### The canary and the visual hold

Every image-input invocation first gets a canary: a generated PNG that shows a
random six-character code. Orbit creates it for that invocation in scratch and
never writes it to a task. The agent must return the code as `result.canary`.

- A delivery result without the code, or with another code, is a canary miss:
  a retryable `[image_not_seen]` step failure. The image-input probe records
  the same miss on that crew's row and the probe run still completes, so one
  crew does not fail the probe batch. Each check emits an
  `activity.image_canary_checked` audit event with the crew, the mode and whether
  the image was seen. The canary proves the image arrived, not that the model
  judged it well.
- A crew whose newest two or more canary checks all missed shows a visual
  hold in `orbit agents vision-probe` until 30 minutes after its last miss. Each further consecutive miss
  doubles the hold, up to 24 hours. The next passing check clears it. The hold
  is read from the audit events, so probes and deliveries both count.

### Probe with `orbit agents vision-probe`

```bash
orbit agents vision-probe                         # every enabled crew
orbit agents vision-probe --crew <crew>           # repeatable: only the named crews
orbit agents vision-probe --timeout-seconds 900   # the default wait
```

The probe runs one canary per crew outside any delivery and waits for each run
to settle. Without `--crew`, every enabled crew gets a row, including crews that
cannot take images; those rows give the reason. A crew named with `--crew` that
cannot take images fails the command before anything launches, so fix its
declaration or leave it out.

Each row has these columns:

- `CREW`, `EXECUTOR` and `MODEL` name the crew and what runs it.
- `MODE` is the executor's declared mode, or `-` when it declares none.
- `RESULT` is the outcome, described below.
- `HOLD` reads `N misses, until <time>` while the crew is held, else `-`.
- `DETAIL` carries the error text, when there is one.

The `--json` output adds the `run_id` of each probe run that launched; read a
run with `orbit run show <run_id>`.

| RESULT | Meaning | What to do |
|---|---|---|
| `seen` | The model returned the canary code. | Keep the declaration. The crew can take images. |
| `image_not_seen` | The run finished without the code. The probe run itself still completes. | If the same model keeps missing, set `vision = false` on that crew. |
| `no_image_input` | The crew's executor declares no `image_input`. | Add a candidate declaration as above, or leave the crew out of image work. |
| `vision_off` | The crew sets `vision = false`, so it was not probed. | Set `vision` back to `true` and probe again. |
| `no_cli_runtime` | The crew's provider has no CLI runtime on this host. | Check `orbit doctor providers`, then follow [Provider setup](providers.md#provider-setup). |
| `failed` | The run failed before the canary was checked. | Read the run with `orbit run show <run_id>` and fix what it reports. |
| `timeout` | The run had not settled when the probe stopped waiting. | Probe again with a larger `--timeout-seconds`. If it still times out, read the run with `orbit run show <run_id>`. |

## Tag instructions

A task tag can select extra instructions for the activities that work on
it. Map the tag to committed instruction files, one per role:

```toml
[workflow.tag_instructions.migration]
implement = "docs/agents/migration-implement.md"
review = "docs/agents/migration-review.md"

[workflow.tag_instructions.ui]
review = false            # switch off the shipped reviewer default
```

The three roles are `implement` (`agent_implement`, which also runs QA
sweep tasks), `review` (`agent_review_repair` and
`review_reconciliation_review`) and `recovery` (`step_failure_recovery`,
`final_recovery` and `pr_conflict_recovery`). Map only the roles a tag
needs; an unmapped tag and role adds nothing.

Each value is a path to a file relative to the repository root, with `/`
separators, no `..` and no absolute path — or `false` to switch off the
shipped default for that tag and role. Tags match ignoring case.

Orbit ships defaults for three tags. `no-diff-expected` gives
implementers the read-only handoff rule. `skip-validation` gives
implementers the light-delivery rule: run no local validation while the
change stays inside `workflow.light_delivery_paths`. `ui` routes implementers
to a host with a verified browser kit and selects the optional PR screenshots
section; an empty section never blocks opening. Visual task artifact evidence
follows the workspace guide and actual diff, not the tag. A mapped file
replaces the default for that tag and role.

The global file layers under the workspace file: a workspace entry
replaces the global one for the same tag and role and leaves the other
tags and roles. Edit `config.toml` directly; `orbit config set` does not
write these keys.

Instruction text comes only from the shipped defaults or from the mapped
file as committed at the delivery's base revision. Uncommitted worktree
edits never change the instructions a delivery runs under, and nothing
written on the task (its description, comments or tags) supplies text: a
tag only selects. Tags apply in sorted order, once each, after the
activity's own instructions. They add to those instructions and cannot
override them.

A mapped file the activity cannot use fails the activity by name before
its provider starts, and only for tasks carrying that tag:

- `tag_instruction_unreadable`: the file is missing at the base
  revision, is not UTF-8, or is empty.
- `tag_instruction_too_large`: the file is larger than 8 KiB.
- `tag_instructions_too_large`: one prompt's tag text would exceed
  16 KiB.

Verify what applied with `orbit run show`: each dispatch that received
tag instructions prints a `Tag instructions:` line, and the audit trail
records an `activity.tag_instructions_applied` event naming the step,
activity, task, and each instruction's tag, role, source, SHA-256 and
size.

## Activity-scoped agent corpus

`[workflow.agent_corpus]` maps exact workflow activity names to a shared,
committed workspace corpus index (`INDEX.md`) so mapped activities receive
bounded, activity-scoped workspace knowledge (issue mechanisms, current
decision rules and workspace guidelines) at dispatch. The table is unset by
default, which maps no activities and injects nothing. Do not enable or
populate it on a live workspace unless the operator asks for it.

```toml
[workflow.agent_corpus]
agent_implement = "docs/agents/corpus/INDEX.md"
final_recovery = "docs/agents/corpus/INDEX.md"
task_pilot = "docs/agents/corpus/INDEX.md"
triage_escalation = "docs/agents/corpus/INDEX.md"
```

- **Authoring and layering:** edit `<workspace>/.orbit/config.toml` (or
  `~/.orbit/config.toml`) directly. Like `[workflow.tag_instructions]` and
  `[workflow.review_skip_tags]`, `[workflow.agent_corpus]` is TOML-only: it is
  not listed by `orbit config keys`, and `orbit config get` and
  `orbit config set` do not read or write it, so a key-only audit will miss
  it. Global and workspace files layer per activity key (a workspace entry
  replaces the global entry for that activity), and the merged table is
  validated whenever configuration loads (`orbit config show`, `orbit doctor`).
- **Limits and admission rules:** at most `32` activities may be mapped. Each
  key must be a non-empty exact activity name containing only ASCII letters,
  digits and `_`. Every mapped activity must point to the **same**
  repository-relative path ending in `/INDEX.md` (at most `512` bytes, `/`
  separators, a leading `./` stripped, and no leading `/`, `\`, NUL, empty,
  `.` or `..` segments). Mapping different activities to different index paths
  or violating any limit fails configuration loading.
- **Exact activity matching and pinned source:** only an activity whose exact
  name is mapped in `[workflow.agent_corpus]` is checked; unmapped activities
  receive nothing. On an owner or local run, the host reads the configured
  `INDEX.md` (and any referenced decision documents) from the Git commit at
  the run's recorded worktree base revision (`base_sha` from the `worktree`
  step, or `HEAD` only when a run has no recorded base). Uncommitted worktree
  edits, task fields, comments, tags and agent output never supply injected
  corpus text. The committed `INDEX.md` blob at that revision must be valid
  UTF-8, at most `1 MiB`, and carry `<!-- agent-corpus-schema: 1 -->` with
  generated `<!-- agent-corpus-row {...} -->` JSON row comments (`type` of
  `issue`, `decision` or `guideline` with relative `.md` paths under
  `issues/`, `decisions/` or `guidelines/`, `status`, `applies_to`, `title`,
  `links` and optional `match_infra_class`); a missing, oversized or malformed
  index refuses dispatch for the mapped activity.
- **Claimed runs:** at claim admission, the owner resolves each mapped
  activity's corpus injection at the claim's authoritative base revision
  alongside tag instructions and records the result in the claim receipt. A
  claimed worker uses the owner's captured rows from that receipt rather than
  its own local `[workflow.agent_corpus]` table or checkout (falling back to
  the replica's local configuration with a warning only when an older owner's
  receipt captured none).
- **Filtering and 8 KiB byte bound:** from the index rows at the pinned base
  revision, dispatch selects only rows whose `applies_to` list includes the
  exact activity name, and omits `issue` rows whose `status` is `resolved`. A
  `decision` row contributes one line per `current` citable rule in that
  decision document at the pinned base revision (where `status: current`,
  `review_by` has not passed, and every `depends_on` path or config digest
  matches); a decision document with no current rules (or that cannot be read)
  is skipped without failing dispatch, and raw decision history entries are
  never injected as rules. Whole selected rows are appended in index order
  under `Workspace agent corpus (<index> at <revision>)`. The injected text is
  capped at `8 KiB` (`8,192` bytes) independently of tag instructions: when
  the next row would exceed `8,192` bytes (including room for the truncation
  notice), remaining rows are omitted, `truncated` is set to `true`, and
  `[Corpus truncated at 8192 bytes; remaining rows omitted.]` is appended. If
  no rows match after filtering, nothing is injected and no audit event is
  emitted.

### Verify a mapped activity

1. Confirm configuration loads cleanly:

   ```bash
   orbit config show
   orbit doctor
   ```

2. After a run executes a mapped activity, inspect its `activity.agent_corpus_applied`
   event in the run's v2 audit trail (`orbit run show` does not print a header
   line for corpus injection):

   ```bash
   orbit run events <run_id> --type activity.agent_corpus_applied --json
   orbit run events <run_id> -s <step_id> --type activity.agent_corpus_applied --json
   orbit run trace <run_id>
   ```

   Each `activity.agent_corpus_applied` event (`body_kind: "agent_corpus_applied"`)
   records `step_id`, `activity_name`, `index` (the configured `/INDEX.md`
   path), `revision` (the pinned base commit SHA read by the host), `paths`
   (the relative document paths of the delivered rows), `sha256` and `bytes`
   (the lowercase hex SHA-256 digest and byte length of the injected block;
   the prompt text itself stays out of the audit store), and `truncated`
   (`true` when rows were omitted at the 8,192-byte bound, `false` otherwise).
   An unmapped activity or a mapped activity with zero matching active rows
   writes no `activity.agent_corpus_applied` event.

### Proposed corpus updates versus reviewed repository changes

While `[workflow.agent_corpus]` is configured, two deterministic host paths
record structured `agent_corpus_append` entries in task history (`orbit task show <task-id> --json`):

- **Human decision answers:** when a human resolves a decision request
  (`decision_answered`, excluding automated or precedent answers, model-answer
  reviews and resident-authority grants), the same task update records an
  `agent_corpus_append` proposal targeting `decisions/orbit-requests.md`
  (`Entries` section).
- **Typed recovery matches:** when `final_recovery` is mapped and the engine's
  typed `TransientInfraBlock` classification matches an unresolved `issue`
  row's `match_infra_class` for `final_recovery`, the task update records an
  `agent_corpus_append` occurrence proposal targeting that issue document
  (`Occurrences` section).

These `agent_corpus_append` history records are sidecar proposals, **not**
automatic commits, merges or live corpus mutations. Because dispatch reads
only committed blobs at the run's pinned base revision, proposed appends — and
any unmerged corpus document or decision-rule edits — do not change what
activities receive until they are materialized into the repository's corpus
files, `INDEX.md` is regenerated by the repository's own documentation
tooling, and the change lands through ordinary code review. Orbit does not ship
an installed subcommand or script that generates `INDEX.md`.

## PR screenshot section

A single-task pull request body for a task tagged `ui` (matched ignoring
case) carries the optional `## Screenshots` table that Orbit builds from the
task's `screenshots/` artifacts. An untagged task gets no section: `pr_open`
reads no screenshots and pushes no evidence ref. This is fixed; the former
`pr.screenshot_section` key is ignored with a deprecation warning.

A bundle of several tasks keeps Orbit's batch body and never gets the
section, and a `body` supplied to `pr_open` is published as given, without
it. The `ui` tag's implementer routing is described in
[Tag instructions](#tag-instructions).

This rule governs only PR publication. Task-artifact evidence is a
separate duty: a diff that changes something a user sees still attaches its
screenshots as task artifacts when the workspace guide requires them,
whatever its tags. Name each artifact
`screenshots/<view>-<theme>-<width>[-before|-after].<ext>`. The width is the
last number in the name, the theme is the token before it, and the view is
the rest. A name without a suffix is an After shot; the Before shot is
optional. Only PNG, JPEG, GIF and WebP files within the 1 MiB artifact limit
are published. Any other artifact under `screenshots/` is skipped with a
warning in the run's log.

When the section applies and no screenshot is publishable, the body carries
the neutral note `No screenshots attached.` That note is not a required-section
gap: `pr.missing_required_section` never drafts or refuses the PR for it.
Only `pr.required_sections` gaps do that.

### Verify the outcome

Check a real delivery, without opening a PR for the test:

- Before `pr_open`, `orbit task show <task-id> --fields artifacts` lists
  the task's `screenshots/` artifacts.
- After the PR opens, `gh pr view <pr-number> --json body --jq .body`
  should show the expected body:
  - a `## Screenshots` table with one row per view, width and theme (the
    theme is named only when the shots carry several), with Before and
    After columns, when publishable shots exist;
  - the `No screenshots attached.` note, when the section applies and no
    shot is publishable;
  - no `## Screenshots` section on an untagged task.

## Review-skip tags

A workspace can map its own tag to a review skip for low-risk work that
touches paths outside `workflow.light_delivery_paths`. The workspace file
or the global file may carry the mapping, layered per tag like
`[workflow.tag_instructions]`:

```toml
[workflow.review_skip_tags.comment-hygiene]
paths = ["docs/**", "**/*.md"]
guard = "scripts/check-comment-only-diff.sh"
```

- `paths` lists the path globs a candidate carrying the tag must change
  only within (the `workflow.light_delivery_paths` glob syntax, with `!`
  excludes; `["**"]` allows every path). `guard` is an optional
  repository-relative executable, read from the delivery's base revision,
  that must exit 0 for the tag to skip review.
- Edit `config.toml` directly; `orbit config set` does not write these
  keys (they are unknown keys, unlisted by `orbit config keys`). The
  seeded template is not migrated into existing config files, so an
  existing install opts in by adding the tables to its `config.toml`.
- A mapped tag skips every agent review layer and nothing else:
  required validation, workspace gates and hosted checks still run. The
  built-in `skip-review` and `skip-validation` tags cannot be remapped.
- Only filing a task or an operator update may add a mapped tag; an
  agent's update that adds one is refused, while removal is always
  allowed.
- Verify with `orbit config show` (effective mappings under
  `Review skip tags:`, `review_skip_tags` in `--json`) and with
  `orbit task show`: each candidate's decision is recorded once in the
  task's history as `light_delivery_applied` or
  `light_delivery_ignored`, naming the tag and the failed condition.

## Landed diff classification

Each landed delivery records the lines it added and deleted, and the files it
changed, in three classes shown under Analytics › Metrics › Lines changed. Each
changed path is classed in this order:

1. A path matching `workflow.test_paths` counts as **tests**.
2. Otherwise, a path matching `workflow.non_code_paths` counts as **other**.
3. Anything else counts as **code**.

Both keys are arrays of path globs with the `workflow.light_delivery_paths`
syntax: `*` stays within one path component, `**` spans components, and a
leading `!` excludes what it matches. A path passes a list when some positive
glob matches it and no `!` glob does, whatever their order. A list with no
positive glob matches nothing, so `[]` counts no path in that class. A
`!` glob removes a path from its list only; the path then falls through to the
next class, so `!**/fixtures/**` in `test_paths` makes fixtures count as other
or code, depending on `non_code_paths`.

Unset keys use language-neutral naming conventions:

- `workflow.test_paths`: `test/`, `tests/`, `__tests__/`, `spec/` and
  `testdata/` directories at any depth, and files named `*_test.*`, `*.test.*`,
  `test_*.*`, `*_spec.*` or `*.spec.*`.
- `workflow.non_code_paths`: `docs/**`, and prose, data, configuration, lock,
  golden, image and `LICENSE*` files at any depth (for example `*.md`, `*.rst`,
  `*.json`, `*.toml`, `*.lock`, `*.png`).
- `workflow.non_reviewable_paths`: empty. Paths here are left out of the
  reviewable-lines denominator. Docs and config stay reviewable, so this is
  not the non-code class and it is not a built-in list of one toolchain's
  generated files. Set lockfiles, goldens, snapshots and generated paths
  when those lines should not count.

Most projects need no change. Set a key only when the repository's layout
differs. An array replaces the inherited value wholesale, so a workspace list
replaces the default rather than adding to it: copy any default you still want.
Use the workspace file for one repository and `--global` for every repository
on the host:

```toml
[workflow]
test_paths = ["**/tests/**", "e2e/**", "**/*_test.*"]
non_code_paths = ["docs/**", "**/*.md", "assets/**"]
```

```bash
orbit config set workflow.test_paths '["**/tests/**", "e2e/**"]'
```

A rename counts only its changed lines. A binary file counts as one changed file
with no lines.

**Inspect the effective lists.** `orbit config keys --json` confirms both keys
(`array<string>`, section `delivery`). `orbit config get workflow.test_paths`
prints the effective value, built-in default included, and
`orbit config show --all` labels configured values by source (`workspace`,
`global`, or `environment`); `workflow.default_crew` shows `environment` when
the `CONSTELLATION_DEFAULT_PROVIDER` variable supplies it (no provider
detection runs at config load). Values from compiled-in defaults
show `default`; keys with no configured value and no compiled-in default show
`unset`. File paths appear in the `Layers (later wins)` banner or in `--json`
per-key provenance.

**Verify a change on a new delivery.** The lists are compiled when a runtime
opens the workspace, so restart long-running processes such as `orbit web serve`
and any active drain first. Then note the Lines changed totals for the current
window, let a delivery land whose files span the classes, and compare the totals
after it lands. Analytics › Metrics aggregates by bucket and does not list
individual deliveries, so expect the change in the bucket where the delivery
landed, not a row for it. Verify against a delivery that lands after the change;
do not edit recorded rows to test it.

**Changes do not reclassify history.** A change to either list applies only to
deliveries recorded after it. Rows already recorded keep their classes, even
when the new lists would classify their files differently.

**Reviewable lines are recorded forward only.** A landed row also stores the
reviewable lines of that task's own commits: added and deleted lines, minus
pure renames and minus `workflow.non_reviewable_paths`. A row written before
that denominator existed stays unavailable. So does a row whose commits
cannot be isolated to the one task. Nothing recomputes those rows by
re-diffing a shared landing range. `cost_per_line` and `lead_time_per_line`
leave both kinds out and count them separately per arm. A change to
`workflow.non_reviewable_paths` applies only to deliveries recorded after it.

**Backfill is not a policy change.** `orbit migrate --backfill-diff-stats
[--limit N]` applies pending migrations, then records rows only for done tasks
that landed before diff stats were recorded and have no row yet. It classifies
those rows with the lists in effect when it runs, so set the lists first if the
backfilled history should follow them. It records reviewable lines only when
the landing's commits can be isolated to that task; otherwise the new row's
denominator is unavailable. It does not rewrite existing rows. Run
`orbit migrate` without flags to see pending migrations before applying.

## Environment passthrough

Agent subprocesses always start from a **cleared** environment plus the
allowlist — the Orbit process's environment is never inherited, and that is not
configurable. The seeded baseline covers `HOME`, `PATH`, `TMPDIR`, `USER`, and
the provider home directories. Credentials are opt-in: add `GITHUB_TOKEN` or a
provider API key explicitly if agents need them.

The shipped default includes none of `CLAUDE_CODE_OAUTH_TOKEN`,
`ANTHROPIC_API_KEY` or `CODEX_API_KEY`. Choose shared `pass` or a global
per-crew `env` mapping for the intended authentication route; see
[Provider credentials](providers.md#provider-credentials) for variable names,
Codex API-key examples, unset-name warnings and service verification.

```toml
[execution.env]
pass = ["HOME", "PATH", "TMPDIR", "USER", "GITHUB_TOKEN"]
```

## Policies and filesystem profiles

A policy defines the filesystem profiles activities run under.

```bash
orbit doctor fs-access <profile> <path>    # dry-run a workspace-relative path against the profile rules
```

Shipped profiles: `reviewer` and `pure_compute` (read-only), `docs_writer`
(scoped writes), `implementer` (workspace writes), `unrestricted`.

The trap worth knowing: an activity that omits `fsProfile` falls back to
the `unrestricted` profile. A read-only activity must declare its profile
explicitly — silence is not a safe default here.

The operating-system boundary matters: macOS uses `sandbox-exec`; Linux uses
Bubblewrap to enforce allowed writes while leaving host reads and network
available. Other platforms have no backend: dispatch refuses a shipped agent
executor there unless its `spec.sandbox` is `off`, and Windows runs Orbit
inside WSL2. A read-only profile is not a claim of network isolation or
private host reads.
See [first-run.md](first-run.md) for the Linux prerequisite.

## Crew selection and actual execution

For ship dispatch, an explicit run crew overrides the task's explicit pin,
validated pool assignment, or `default` fallback. Otherwise the purpose, type,
complexity and default chain apply as described below. A creation without a
crew draws in this order: its purpose pool, when its task carries a system tag
or `crew-purpose:<purpose>` tag naming a configured purpose pool with an enabled
member; else its type pool, `workflow.type_complexity_crews.<type>.<tier>`, when
its type and tier have one; else its complexity pool, falling back to
`default_crew`. It stores `crew_source` as `explicit`, `purpose:<purpose>`,
`type:<type>/<tier>`, `pool:<complexity>` or `default`, also recorded in
assignment history. A complexity re-rate, or a tag or context-file edit that
changes the task's type pool, redraws a pool-, type- or default-sourced crew
and records the previous and new sources and crews in `crew_redrawn` history;
explicit and purpose-sourced crews stay pinned. A `default` fallback is not an
explicit pin. Status transitions alone preserve the choice. Admission
revalidates pool and type-pool assignments and `default` fallbacks against the
current tier and enabled pool members, recovering legacy provenance from
assignment history when needed, without writing to the task. An empty crew
string on task update re-draws for the task's current complexity rather than
leaving the field empty. Discover actual
crew names through the connected server's crew discovery when available, or
inspect effective configuration; executor names are not a list of crew names.

The task's `resolved_crew` and `crew_model`, when returned, describe the recorded
run rather than today's default. Unsupported/unavailable explicitly selected
providers fail rather than silently switching vendors. Provider login and model
availability must be checked on the execution host before dispatch.

For lane-specific authentication, sandbox grants and MCP versus shell access,
follow [Provider setup](providers.md#provider-setup). Shell tool access through `orbit tool run`
enforces the same allowlist and caller-role gates as MCP; disabling that shell
also removes Orbit tool reach for a shell-only lane.
