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
orbit config keys                  # every registered key; machine.id and machine.task_prefix are listed and read-only
orbit config path
```

`orbit config show` annotates every setting as `workspace`, `global`,
`built-in`, or `environment`, with the file it came from. `machine.id` and
`machine.task_prefix` render as identity, not as settable rows. Reach for it before
assuming a value. Use `orbit config show --all` to include unset advanced keys;
the defaults below are built-ins, not necessarily this host's effective policy.

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
choices. A name a later release retired still loads but seeds nothing. This
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

orbit config set seeding.routines '["worktree_gc", "store_gc", "crew_performance_refresh"]'
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
  from enabling it: shipped auto-tasks and every routine except
  `crew-performance-refresh` start disabled; that one starts enabled and no-ops
  while `workflow.crew_performance` is `off`. Preserve existing enablement choices.
- **Auto-task deletion remains an opt-out.** An opt-out recorded by `orbit
  auto-task delete` survives exclusion and later widening, so sync still leaves
  that default absent. First include it in `seeding.auto_tasks`, then use
  `orbit auto-task restore <name>` to clear the opt-out and restore its shipped,
  disabled content. Restore refuses an excluded name or an existing definition.
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

If `workflow.crew_performance` is anything other than `off` and the routine
policy excludes `crew_performance_refresh`, doctor adds a `seeding-policy`
warning: nothing refreshes the snapshot crew draws read. Either keep crew
performance off or include that routine and sync; also verify the host clock
and routine state as described in [Crew performance snapshot](automation.md#crew-performance-snapshot).
For applying policy together with config and plugin enables from a reviewed
file, retain the [workspace preset procedure](first-run.md#workspace-presets).

## Common settings

| Key | Purpose |
|---|---|
| `workflow.base_branch` | Config fallback for ship/auto/pilot base branch. A registered workspace `base_branch` takes precedence. |
| `workflow.default_crew` | Crew for any task that doesn't declare one. |
| `workflow.<tier>_complexity_crews` | Automatic crew pool per task complexity (`low`, `medium`, `hard`, `xhard`); entries `name` or `name:weight`. Empty (`[]`) routes that tier to `default_crew`. Orbit's own work draws from them too while its purpose pool is unset: auto-task templates by complexity, the task pilot and PR auto-fix from `low`, failure and PR conflict recovery from `medium`. The retired system-crew key loads with a deprecation warning and is ignored. |
| `workflow.purpose_crews.<purpose>` | Weighted pool for one purpose of work, written like the complexity pools: `task_pilot`, `failure_recovery`, `final_recovery`, `auto_fix`, `pre_landing_review`, `review`, `baseline_red_repair`, `sweep_tasks` (every task the CI failure, Dependabot and code-scanning sweeps file, and merged-head repairs a review reconciliation files), `explorer` (read-only explorers; see `workflow.explore.*`). An explicit task, template or activity crew wins; otherwise the purpose pool is drawn, narrowed by `--allow-crew`; unset or `[]` keeps the earlier route. A provider limit or failure hold on the assigned member redraws from the pool's other enabled members before the complexity pool. `final_recovery` has no earlier route: `final_recovery = []` disables final recovery. `pre_landing_review` falls back to `review` when unset or empty; `review` also supplies after-landing review and is described below. `orbit config show --all` lists each purpose's resolved pool and its source (`crew_purposes` in `--json`). |
| `workflow.explore.enabled` | Let managed implement and review-repair runs delegate read-only questions through `orbit.agent.invoke` to sandboxed explorers drawn from `workflow.purpose_crews.explorer` (default off). Budgets: `workflow.explore.max_calls_per_run` (6), `max_concurrent_per_run` (2), `timeout_seconds` (600) and `max_answer_bytes` (16384). |
| `workflow.regression_risk.enabled` | Mark regression-prone context files in implement and review prompts (default on). This switch controls prompt marks only. A file qualifies with at least `min_support` touching done tasks in `window_days` (defaults 5 and 30) and a regressed share of at least `threshold_percent` (default 40); at most `max_files` marks (default 3), highest rate first. These are file-level prompt marks. Task riskiness is a separate estimate fitted on task size alone, without per-file or crew attribution, and continues to use `window_days` and `min_support` when marks are off. Tasks without a usable diff-based fit use the number of distinct file and directory paths in their context selectors. A review or done task with a resolvable diff and enough matching landed-diff history uses inserted plus deleted lines instead. The task detail breakdown shows this basis and its size band: recent done tasks on the same basis at least this size, and how many regressed. A `~` before the percentage means some selectors have no path and are omitted from the declared-file count, making it a lower bound. `threshold_percent` and `max_files` apply to prompt marks only. A Tasks row without an estimate has no risk part. Task detail shows **No estimate** with its reason: no files (no context selectors and no usable diff-based estimate), no paths (selectors identify no file or directory), thin history (not enough usable done-task observations on the selected basis to fit), or unavailable (the estimate could not be computed). Only an estimated task shows the collapsible Regression risk breakdown. |
| `workflow.generated_paths` | **Advanced: executes commands during conflict recovery.** Default `{}`; table of workspace-relative path globs to non-empty regeneration commands. Applies only to conflicting generated files in a stopped rebase. Verify the recovery's `regenerated_paths` and command result. See [Recovery settings](#recovery-settings). |
| `workflow.unlocked_context_paths` | Default `["**/*.md"]`; array of workspace-relative globs (`*` within a component, `**` across components; no absolute paths or `..`). Matching context entries take no lock in mixed tasks; an all-matching task still locks every entry. `[]` locks all entries. Verify with `orbit run readiness` and `orbit task eligible`. Keep code and build/CI definitions locked. |
| `workflow.lock_modes` | **Advanced: admission policy.** Default `[]` (every overlap blocks); ordered `[[workflow.lock_modes]]` tables, each a `pattern` (Rust regex searched for in the workspace-relative path; anchor with `^`/`$`) and a `mode` of `exclusive` (default) or `shared`. First match wins. An overlap is waived when its point, the narrower selector's path, is shared, so tasks sharing only, say, `(^|/)mod\.rs$` run together and the second to land resolves any conflict. `module:`/`command:` selectors stay exclusive. Set it in the workspace config only for files that merge cleanly or are regenerated. Verify with `orbit run readiness` and `orbit task eligible`. |
| `workflow.unlocked_path_validation_commands` | **Advanced: validation policy.** Default `[]` keeps full validation; array of command strings, trimmed with blank entries dropped. Replaces required commands only when the candidate differs from this run's last fully validated tree solely in unlocked paths. Verify `validation_scope` in validation logs. See [Recovery settings](#recovery-settings). |
| `workflow.admission_affinity` | **Advanced.** Default `band`; choices `band`, `priority`, `off`. Work only the requesting executor can run leads within a dispatch band with `band`, or only within a priority with `priority`; `off` keeps canonical band, priority, expiry and age order. Applies to owner-local and pull admission using the live executor census. Verify the effective choice with `orbit config show --all` and compare `orbit run readiness` ordering with the drain's last pass and live executor capabilities. |
| `workflow.validation_env.cargo_incremental` | **Advanced.** Default `false`; boolean. With a host validation output binding, forces `CARGO_INCREMENTAL=0` on validation children, replacing inherited values. `true` leaves it untouched; without a binding neither value sets it. Verify the child environment during authorized validation; existing incremental directories remain. See [Validation environment](operational-logs.md#worker-resource-containment). |
| `worktree.reclaim_below_free_mib` | **Advanced: reclaims rebuildable output.** Default unset; non-negative integer MiB (`u64`). During admission, reclaim `worktree.reclaim` paths from oldest terminal worktrees while free space under the state directory is below this mark; active runs remain protected. `0` never triggers it. Verify candidates with `orbit gc worktrees --reclaim --dry-run --estimate-bytes` and actual reclamation via the `worktree.reclaim` audit event. See [Worktree GC](maintenance.md#worktree-garbage-collection). |
| `workflow.provider_limit_max_used_pct` | Skip a crew while a live usage window of its provider or model is exhausted or used at or above this percent (1..=100, default 90; 100 skips only on exhaustion). |
| `workflow.provider_limit_overrides` | Per-provider replacement for that percent: `provider:percent` (1..=100) or `provider:off` (record readings without gating). Each provider once. |
| `workflow.provider_limit_explicit_crews` | When an explicit crew is at its provider's limit: `wait` (default) leaves the task in the backlog until the limit lifts; `pool` draws from unlimited members of its complexity pool. A purpose-sourced crew is not an explicit pin; it redraws within its purpose pool first. |
| `workflow.crew_selection` | `static` (default) draws on configured weights; `usage_pressure` scales task creation, re-rate and admission draws by each crew's weekly usage pressure (`weight × clamp(pressure^strength, 1/cap, cap)`) and re-draws a system-chosen crew at admission, keeping its crew source. A pull follower re-draws a claimed leaf's system-chosen crew on its own readings and reports per-crew pressure so the owner offers it first, among tied tasks, the ones whose pools hold its highest-pressure crews. Explicit crews are never re-drawn. `workflow.usage_pressure_cap` (default 8, at least 1) and `workflow.usage_pressure_strength` (default 1.0, at least 0; 0 disables) tune it. Not the run's recorded `crew_selection`. |
| `workflow.provider_limit_budgets` | Rolling budgets for providers that report no usage, each `provider:<amount><usd|tokens>/<n><h|d>` (for example `grok:30usd/5h`), one per provider, or for a crew that sets `env`, its own account (`opus-credits:200usd/30d`). Spend is this host's invocation ledger. None by default. |
| `workflow.crew_performance_types` | Ordered task types for the implementer scorecard's by-type matrix (`orbit agents scorecard --role implementers --by-type`). Each entry has a unique non-empty `name` (never `other`) and at least one of `tags` or `paths`; a task takes the first matching type, else `other`. Empty (default) means every task is `other`. |
| `workflow.crew_performance` | `off` (default), `absolute`, `comparative`, `shadow_absolute` or `shadow_comparative`. A live mode weighs task creation, re-rate and admission draws by each pool member's implementer score on the task's type and tier from the hourly snapshot (`weight × pressure_factor × clamp(exp(strength × advantage / 100), 1/cap, cap)`) and records `applied: true`. `absolute` favours the best crew for the type; `comparative` the type each crew does best against its own mean. Shadow computes and records the same factor with `applied: false` and draws as `off` would; usage pressure still applies, and the snapshot still refreshes. Before enabling a live mode, `orbit agents scorecard --role implementers --by-type --shadow --window 30d` shows each crew's static share, the share actually drawn, and the would-be share with the recorded factor. Draws with no performance record are listed separately and counted as static. Composes with `workflow.crew_selection`; never re-draws an assigned or explicit crew by itself. A missing, stale or mismatched snapshot gives factor 1 with the reason recorded. `workflow.crew_performance_strength` (default 3, at least 0; 0 disables) and `workflow.crew_performance_cap` (default 4, at least 1) tune it. Only `off` skips the hourly snapshot scan. Fed by the seeded `crew-performance-refresh` routine — [automation.md](automation.md#crew-performance-snapshot). |
| `workflow.crew_performance_window_days` | Evidence window in days (1..=90, default 30) the crew-performance snapshot is built from. Fed by the seeded `crew-performance-refresh` routine — [automation.md](automation.md#crew-performance-snapshot). |
| `review.timing` | Review before landing, one layer at most (default `off`). `before_pr` holds PR creation for a fresh reviewer that fixes what it finds. `before_landing` opens the PR first and reviews it while hosted CI runs; it merges only at the reviewed head, and any other outcome leaves the PR open with the task in `review` (PR route only). A run captures it at submission. After-landing review is not a key: toggle the `delivery-code-review` auto-task (`orbit auto-task toggle delivery-code-review on`). |
| `review.before_landing_hosts` | Machine ids (`hm_...`) whose claimed leaves review their open PR before it lands, while this owner's own deliveries keep `review.timing` off. Probe and pull admission resolve the caller's label and the claim captures it. Ignored when `review.timing` is already `before_landing`. Loading fails while `review.timing` is `before_pr` and the list is non-empty. The label is caller-chosen, so this is policy, not a security boundary. Default empty. |
| `review.minutes` | Wall-clock limit for one candidate's before-PR or before-landing review (1..=1440, default 30). Each candidate gets one review; a changed candidate, such as a completion rebase, is a new one. |
| `review.verify_findings` | **Advanced.** Default `false`; gates review-filed bug findings on a confirmed verification for their current material before approval, promotion or drain admission. Does not enable the verification routine. |
| `review.baseline_commands` | Default `[]`; commands the owner trusts review settlement to rerun on the host for a reviewer's red-base claim, in addition to required validation commands. Entries are trimmed strings; blank entries are dropped. A configured command is binding during settlement. |
| `[[review.host_evidence]]` | Default `[]`; owner policy for checks owed on claimed reviews when changed paths match. This TOML array replaces the whole global list at workspace scope; see [review finding verification and owed host evidence](#review-finding-verification-and-owed-host-evidence). |
| `workflow.purpose_crews.review` | Crew or pool for after-landing review tasks the `delivery-code-review` auto-task mints. Before-PR and before-landing review use `workflow.purpose_crews.pre_landing_review` when non-empty, falling back to this pool when it is unset or empty. A pool such as `["sol", "grok"]` (or weighted `["sol:3", "grok:1"]`) draws one crew per review, skipping disabled or provider-limited crews and preferring one that did not implement the work. |
| `tasks.id_start` | Floor for this machine's task-id allocator; forward-only. → [multi-host.md](multi-host.md) |
| `execution.agent_stall_minutes` | **Advanced.** Default `30`; integer `0..=1440` minutes. A provider with no stdout/stderr and no CPU use for this long fails its step as stalled, entering normal retry/recovery. `0` disables this check; the wall-clock timeout remains. Verify a stalled step's failure in `orbit run show` and provider logs; do not induce a production stall. |
| `execution.proc_spawn_max_timeout_minutes` | **Advanced.** Default `45`; integer `1..=1440` minutes. Ceiling for one `proc.spawn` call inside a managed activity, also capped at the activity's remaining wall-clock budget. Outside an activity the ceiling is 60 seconds. Verify `timeout_ceiling_ms` and its source in the tool result. |
| `execution.env.pass` | Environment variable names allow-listed into agent subprocesses. |
| `execution.codex.sandbox` | `read-only`, `workspace-write`, or `danger-full-access`. |
| `execution.codex.approval_policy` | `untrusted`, `on-request`, or `never`. |
| `machine.auto_concurrency.mode` | Forecast admission: `off` (default) builds no forecast, `shadow` records what forecasting would admit while retaining ordinary admission, `enforce` uses forecast fit to decide admissions within the worker ceiling. |
| `workflow.resource_throttle.enabled` | **Advanced.** Default `true`; boolean. Controls whether host pressure holds new admissions; `false` still reports telemetry and severity. Preserve the operator's policy when diagnosing a hold. |
| `workflow.resource_throttle.cpu_source` | Default `load`; choices `load`, `utilization`. Linux load is one-minute load divided by online CPUs (includes I/O wait and can exceed 100%); utilization uses busy tick deltas excluding idle and I/O wait. macOS uses Mach utilization for either choice. Verify the source in readiness and doctor. |
| `workflow.resource_throttle.cpu_high_percent`, `workflow.resource_throttle.cpu_resume_percent` | CPU hold/resume marks; defaults `90` / `85`, integers `1..=100`, resume strictly below high. |
| `workflow.resource_throttle.memory_high_percent`, `workflow.resource_throttle.memory_resume_percent` | Non-reclaimable memory hold/resume marks; defaults `90` / `85`, integers `1..=100`, resume strictly below high. |
| `workflow.resource_throttle.disk_high_percent`, `workflow.resource_throttle.disk_resume_percent` | Space-used hold/resume marks for each watched filesystem; defaults `90` / `85`, integers `1..=100`, resume strictly below high. |
| `workflow.resource_throttle.cpu_light_leaves` | **Advanced.** Default `2`; integer `0..=32`. Local-drain `no-diff-expected` auto-task leaves still admitted while CPU alone is held. Memory, disk or I/O holds stop these too; `0` holds all new leaves. |
| `workflow.resource_throttle.io_full_high_percent`, `workflow.resource_throttle.io_full_resume_percent` | Linux disk I/O stall marks on `/proc/pressure/io` `full avg60` (defaults 20 and 10; 1..=100, resume below high). Admission-only: running work is untouched, and hosts without PSI are not gated. |
| `machine.auto_concurrency.admit_cpu_percent`, `machine.auto_concurrency.admit_memory_percent` | Forecast limits as percentages of host cores and non-reclaimable memory (defaults 90 and 75; 1..=100). Memory must be below global `workflow.resource_throttle.memory_high_percent`. |
| `machine.auto_concurrency.profile_window`, `machine.auto_concurrency.min_profile_deliveries` | Recent completed deliveries per profile key (default 50) and minimum before using it without fallback (default 10); the window must be at least the minimum. |
| `machine.auto_concurrency.horizon_cap_minutes` | Maximum candidate forecast horizon (default 120 minutes). |
| `machine.auto_concurrency.head_max_wait_minutes`, `machine.auto_concurrency.head_stuck_passes` | Queue-head starvation guard: protect the head after its maximum wait (default 30 minutes), then report it stuck after consecutive protected passes without improvement (default 20; at least 1). A stuck report does not change admission. |
| `machine.host_shutdown_hold_minutes` | Hold new unattended admissions when a scheduled shutdown or reboot is this many minutes away or sooner (default 180; at least 1). Later schedules are reported while admission continues. |
| `runtime.log_retention_days` | Delete JSONL archives older than N days. |
| `runtime.log_max_total_mb` | Total archive budget; oldest pruned first. |
| `runtime.log_max_file_mb` | Roll the active log past N MiB. Must not exceed the total budget. |
| `pr.task_url_template` | URL template linking a task ID in PR descriptions. |
| `pr.close_on_terminal` | Close a task's open Orbit-authored delivery and `[BLOCKED]` PRs when it lands, is rejected or is archived, when an operator moves it back to backlog or someday with no run delivering them, and when a newer attempt opens its own PR (default `true`). Branches are kept; a forge error is only a warning. |
| `pr.delivery_authors` | Forge logins whose PRs count as Orbit-authored for closing them (`pr.close_on_terminal`) and for settling a task to done from its delivery PR merged outside Orbit. Empty uses the `gh` login on this machine. |
| `pr.title_template`, `pr.body_template` | Single-task PR title and body templates with `{{placeholder}}` values (`task_id`, `title`, `type`, `description`, `acceptance_criteria`, `execution_summary`, `validation`, `freshness`, `task_url`, `ext.<system>`). The body is `"repo"` (the repository's `pull_request_template.md`) or a repository-relative path, read at the delivery's base revision. An unknown placeholder fails the config load. A rendered body ends with an Orbit comment naming the task, so a template that omits `{{task_id}}` still identifies the delivery. |
| `pr.agent_sections`, `pr.required_sections`, `pr.missing_required_section` | Body headings the implementer fills through the `pr-sections.json` task artifact. An unfilled required section opens the PR as a draft (`draft`, default) or opens none (`refuse`), recorded in task history. |
| `pr.freshness_section`, `pr.signature`, `pr.orchestration_trailer` | Toggle Orbit's own PR body sections (default `true`). |
| `pr.labels`, `pr.reviewers`, `pr.draft` | Labels, reviewers and draft state applied when Orbit opens a PR. A draft is never merged automatically. |
| `pr.task_tag_labels`, `pr.task_type_labels` | Opt-in task labels after PR creation or reuse: exact, case-sensitive tag allowlist (array, default `[]`), and a table mapping `feature`, `bug`, `refactor`, `chore` to label names (default `{}`, e.g. `{ bug = "bug", feature = "enhancement" }`). Names must be non-empty and cannot start with `-`; unknown types are refused. Unioned over every delivered task; missing labels are created and existing labels accepted. Failures warn and are recorded in `pr_open.task_labels.applied` / `.failed` without failing the PR step. Empty settings add no commands; labels are never removed or synced after later task edits. |
| `commit.subject_template`, `commit.body_template` | Single-task commit templates; the subject must contain `[{{task_id}}]` and keeps a 72-character budget. |
| `commit.planned_by_trailer`, `commit.implemented_by_trailer`, `commit.orchestrated_by_trailer` | Toggle Orbit's commit trailers (default `true`). |
| `branch.name_template` | Single-task delivery branch name (default `{{prefix}}/{{task_id}}-{{hash}}`); must contain `{{task_id}}` and `{{hash}}`. Names with `..` or a leading `-` are refused. |

Host pressure holds admission after high readings span ten seconds and releases
only below the resume mark; every resume mark must be strictly below its high
mark. Running work is never cancelled or paused. Use `orbit config show --all`
for all effective marks, `orbit run readiness --json` for the held resource,
reading, threshold and CPU source (`capacity.resource_cpu_source`), and
`orbit doctor` for the effective source and host diagnosis. Explain a hold from
those readings and let pressure clear; do not raise marks or disable the
throttle to push waiting work through.

`[machine]` settings are global-only. An authorized local drain can select its
own forecast mode with `orbit run auto --auto-concurrency <MODE>` (`off`,
`shadow` or `enforce`); MCP `orbit_workflow_auto` `action: "start"` accepts the
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

## Explore briefs

An explore brief is the task pilot's bounded map of code a task may change,
including targets, call paths, patterns, covering tests and gotchas. Configure
whether the pilot makes one and whether an implementer receives it in
`<workspace>/.orbit/config.toml`:

```toml
[workflow.explore_brief]
enabled = true
tags = ["explore-brief"]
deliver = true
```

| Key | Default | Effect |
|---|---|---|
| `workflow.explore_brief.enabled` | `false` | Ask the task pilot for a brief for requested tasks. |
| `workflow.explore_brief.tags` | `[]` | Select tasks by tag; an empty list selects every piloted task. |
| `workflow.explore_brief.deliver` | `true` | Give a stored brief to the implementer; `false` keeps it in shadow, where it is produced and stored but never delivered. |

Use `orbit config show --all` to inspect the effective values and their
provenance. With `enabled = true`, tags are read when a task is piloted, so add
the selected tag before piloting it. A valid brief is stored as the reserved
task artifact `explore-brief.json`; agents and operators cannot write that
artifact themselves.

To verify storage, run `orbit task show <task-id>` and inspect the task-pilot
comment: its `explore_brief` receipt says `written` with a digest and byte count,
or records a drop reason. To verify an implementation dispatch, run
`orbit run show <run-id>` and inspect its `Explore brief:` line; JSON carries
the same records under `tag_instructions.explore_briefs`.

At dispatch, `fresh` means none of the brief's covered files changed since its
inspection revision. `drifted` means one to three covered files changed by at
most 200 lines and no `change` target was deleted or renamed; a drifted brief
can still be delivered, with the changed-file details available to the
implementer. Changes beyond those limits, or deletion or rename of a `change`
target, produce `dropped: stale`; an inspection revision that is not an
ancestor of the run base produces `dropped: revision_unrelated`; and changed
task material without a new pilot, or material whose fingerprint cannot be
recomputed, produces `dropped: material_changed`.
The run's `Explore brief:` line reports these dispatch outcomes. Only a `fresh`
or `drifted` brief reaches the implementer, and only when both `enabled` and
`deliver` are on. In shadow mode, the pilot still stores the brief, while the
dispatch does not pass it to the implementer.

## Experiments

Experiments compare a control arm with one or more treatments for agent work.
Each treatment can vary instructions, skills, tools, crew or an allowlisted
configuration value. The `[experiments]` table is workspace policy:

| Key | Default | Bound or effect |
|---|---|---|
| `experiments.enabled` | `false` | Boolean switch that permits experiments to start. |
| `experiments.max_running` | `3` | Advanced integer from `1` to `20`; maximum running experiments in the workspace. |
| `experiments.config_keys` | `[]` | Advanced list of config keys an experiment may override. Empty allows no config overlays. |

For workspace policy, add or merge this table in
`<workspace>/.orbit/config.toml`:

```toml
[experiments]
enabled = true
max_running = 2
config_keys = ["workflow.explore.max_calls_per_run"]
```

Each overlay config key must be in both the built-in typed allowlist and
`experiments.config_keys`; the workspace list alone does not add support. The
current built-in allowlist contains `workflow.explore.max_calls_per_run`, an
integer from `1` to `100`. Use `orbit config show --all` to inspect effective
values with their source, or `orbit config get <key>` for one value.

An operator creates an experiment as a draft, edits a draft with `update`, or
copies a definition into a new draft with `clone`. `start` opens the first
segment and freezes the definition. Operators may `pause` and `resume`,
`reweight` a running or paused experiment to open a new segment, or `assign` a
nonterminal task to an active arm of a running experiment in either mode when
the task has no assignment for that experiment or has only an
`explicit_crew` exclusion for it. Existing arm assignments and other exclusions
for that experiment remain sticky. They finish with `conclude` (recording a
chosen arm or no choice) or
`abandon` (without a conclusion). An agent session may only use these
read-only commands:

- `orbit experiment list`
- `orbit experiment show <id>`
- `orbit experiment readout <id>`

The other experiment operations are operator-only.

Choose `shadow` to record assignments without applying treatments. Choose
`live` to apply the assigned arm's instructions, skills, tools, crew and
allowlisted config values to work. Readouts calculate per-arm outcomes from
the recorded assignments and work records. For the list and readouts, use
`orbit experiment list`, `orbit experiment show <id>` and
`orbit experiment readout <id>`, or open the dashboard's **Analytics ›
Experiments** page for per-arm enrollment and readouts.

## Decision assistance

Decision assistance is an optional, host-owned foundation for bounded model
annotations. It ships off. The whole `assistance` table is one advanced setting
and is **global-only**: workspace files and workspace-scoped writes refuse it,
including an empty table, and `orbit config set --seed-from-global` never copies
it into a workspace file.

Nothing installs a task or report consumer or a background worker that uses the
table. Assisted calls happen only when an application integrates the library
service explicitly, and a configured grant does not activate any consumer. No
mode grants workflow authority: an annotation never approves, blocks, lands or
dispatches work.

### Inspect the off state

These commands read local configuration only. None of them contacts the
provider or resolves a credential.

```bash
orbit config keys | grep '^assistance'      # registered in this build
orbit config get assistance --scope global  # the global file; built-in defaults fill omissions
orbit config get assistance                 # effective value
orbit config show --all                     # text view, including unset advanced keys
orbit config show --json                    # effective JSON, including the assistance table
```

If `orbit config keys` does not list `assistance`, this build predates the table.
Upgrade first, following [maintenance](maintenance.md), before applying anything
in this section.

The off state is an empty policy list. With `policies` at `[]`, the host enqueues
nothing and reads no credential. The defaults are `budget.epoch = "unfunded"`
with zero ceilings and no pricing.

### Set the table

Only `--global` can write the table. A set replaces the whole table, and any
field you omit returns to its built-in default, including every limit and
`budget.epoch`. Read the current table first and carry forward each field you
keep.

```bash
orbit config get assistance                               # read the current table
orbit config set --global assistance '{ policies = [] }'  # explicit off state; limits and budget revert to defaults unless you include them
orbit config get assistance                               # confirm policies is []
```

- A set without `--global` is refused, and the error names `--global` as the
  flag that targets the global file.
- Nested dotted keys are not registered. `orbit config get
  assistance.limits.concurrency` and `orbit config set --global
  assistance.limits.concurrency 4` both fail with an unknown-key error. Limits,
  budget and policies are fields of the one table.

### Modes

| Mode | Effect |
|---|---|
| `off` (default) | Enqueues nothing and reads no credential. |
| `shadow` | Retains results privately for operator comparison; the advisory read returns nothing. |
| `advisory` | Permits annotations for the matching current source and policy version. |

Moving a grant from `shadow` to `advisory` changes its cache identity. Setting a
grant's `mode` to `off` stops it without removing the grant.

### Policy grants

`policies` lists exact grants, at most 128, unique by `workspace_id` and
`use_case`. Lookups match both exactly, so a grant for one workspace does nothing
in another. Each grant binds one consumer's frozen question and evidence
contract. An identifier below is 1 to 128 characters of letters, digits, `_`,
`-`, `.`, `/` or `:`.

| Field | Rule |
|---|---|
| `workspace_id`, `use_case` | Identifiers supplied by the owning application. |
| `mode` | `off`, `shadow` or `advisory`. Optional; defaults to `off`. |
| `model` | Must be `gpt-6-luna`. Any other model is refused. |
| `model_revision` | Identifier. An operator-maintained cache epoch; advance it when the provider model changes. |
| `policy_version` | Identifier for the consumer policy, including its thresholds. |
| `contract_digest` | `sha256:` followed by 64 lowercase hex digits. Uppercase or a bare digest is refused. The consumer computes it from its serialized contract; do not hand-write it. |
| `evidence_fields` | Non-empty list of identifiers, at most 128. Only these packet fields may leave the host; others are omitted. |
| `evidence_ref_prefixes` | Up to 128 prefixes of 1 to 256 characters with no control characters. References are provenance strings only and are never fetched. |
| `credential_env` | Host environment variable name: starts with `ORBIT_ASSISTANCE_`, uses only uppercase letters, digits and `_`, at most 128 characters. Config holds this name, never the value. |

Calls go only to the provider's fixed Decisions endpoint. No endpoint, redirect,
proxy or agent-crew route is configurable. Explicit secret-value and shared
credential-pattern redaction run before anything is stored or sent. The agent
subprocess environment builder refuses the `ORBIT_ASSISTANCE_` namespace, even
through passthrough or crew `env` mappings, so agents cannot receive the
credential.

### Limits

Every limit has a built-in default. Out-of-range values refuse the whole table.

| Key | Default | Allowed |
|---|---|---|
| `limits.concurrency` | `2` | 1–64 active claims across the host store |
| `limits.queue_capacity` | `128` | 1–10000 pending items |
| `limits.max_records` | `1024` | `queue_capacity`–100000 retained items; overflow records a coverage-gap counter |
| `limits.queue_age_seconds` | `300` | `timeout_seconds`–86400 maximum age of queued work, including retry-pending work |
| `limits.timeout_seconds` | `15` | 1–120 per attempt |
| `limits.retry_delay_seconds` | `5` | 1–300 |
| `limits.max_attempts` | `2` | 1–5, including the first attempt |
| `limits.max_request_bytes` | `16384` | 1024–65536 serialized outbound request |
| `limits.max_response_bytes` | `65536` | 1024–1048576 received response; oversize is reported, never clipped |
| `limits.max_input_tokens` | `32768` | 1024–262144 conservative reservation per attempt |
| `limits.retention_seconds` | `604800` | 1–2592000 terminal payload and attempt retention |

Queue age does not expire a completed result. A claim refused before any request
is sent releases its reservation. A claim whose request entered the transport
keeps its full reservation, even when the attempt fails.

### Budget and pricing

| Key | Default | Meaning |
|---|---|---|
| `budget.epoch` | `unfunded` | Host allocation identity. Its cumulative counters survive payload retention. |
| `budget.token_ceiling` | `0` | Cumulative reserved input tokens for the allocation. |
| `budget.spend_ceiling_microusd` | `0` | Cumulative reserved spend for the allocation, in micro-USD. |
| `budget.input_microusd_per_million_tokens` | absent | Uncached input price ceiling, 1 to 1000000000. Absent pricing refuses paid calls; it never means free. |
| `budget.pricing_version` | absent | Identifier recording the price source the operator verified. |

Pricing is an external prerequisite. Before a paid call can run, the operator
checks the current uncached input price on the provider's published price list
and records it with a `pricing_version` that names the source and the date
checked. Do not copy a rate from documentation, an earlier setup or memory.
Orbit takes the price from this table and does not fetch it. The field covers
uncached input tokens only.

### Evaluation and activation prerequisites

Writing configuration does not evaluate or activate anything. Live evaluation or
advisory activation needs a separate operator decision that records:

- the consumer and its question and policy versions;
- the mode, with `shadow` before `advisory`;
- the workspace scope, the evidence destination and the owner of redaction;
- per-run and total budget caps, with a pricing version;
- timeout and retry limits;
- the targets the consumer must meet on fresh held-out data;
- who owns the off switch.

A grant does not authorize paid or live calls by itself.

### Verify the effective policy

```bash
orbit config get assistance   # policies lists exactly the intended grants, with each mode
orbit config show --json      # the same table in effective JSON
```

The table shows configuration only. It does not report that a consumer is
running, because none ships with Orbit.

## Review finding verification and owed host evidence

These settings govern review findings and the checks an owner may owe after a
claimed review. They do not turn on before-PR or before-landing review; set
`review.timing` separately when that review workflow is wanted.

### Set the policy

`review.verify_findings` is an advanced boolean, default `false`. When true,
a review-filed task of type `bug` tagged `code-review`,
`delivery-code-review`, `security-review` or `qa-sweep` must have a
`CONFIRMED` finding-verification verdict for its current material before it
can be approved, promoted to `backlog` or admitted by a drain. Until then,
readiness reports the `unverified_review_finding` hold. A `REJECTED` verdict
still rejects the task even when verification is not required. The gate reads
this setting each time it checks, so an authorized configuration change applies
to a running drain without a restart.

This switch does not enable or run `finding_verification_pipeline`. The seeded
`finding-verification` routine is a separate, disabled-by-default schedule
that picks up findings still owed a verdict. Inspect its workspace-specific
name and state with `orbit routine list` and `orbit routine show <name>`.
To run a verdict on demand, use
`orbit run job finding_verification_pipeline --input 'task_ids=["<task-id>"]'`.
For automatic runs, enable the routine definition separately as described in
[Automation](automation.md#turning-the-scheduler-on); routine enablement and a
host-local pause are separate controls. A disabled routine does not prevent a
manual job run.

`review.baseline_commands` is an array of owner-trusted commands, default
`[]`. Review settlement may use them, along with
`workflow.required_validation_commands`, to rerun an eligible failed check on
the host and determine whether a reviewer's red-base claim also fails on the
base. The reviewer cannot authorize an arbitrary host command by naming it in
the report. If a listed command cannot be confirmed on the base, the review
settles incomplete; the list is therefore binding policy, not a hint. These
commands also count as required validation gates an implementer may hand off
when its sandbox deferred only an admitted sandbox-dependent test.

Both settings can be written with `orbit config set`, for example:

```sh
orbit config set review.verify_findings true
orbit config set review.baseline_commands '["cargo test -p service --test api"]'
```

`[[review.host_evidence]]` has no `config set` key. Add its array-of-tables
directly to `<workspace>/.orbit/config.toml` or `~/.orbit/config.toml`.
The workspace list replaces the entire global list, including when set to
`[]`. Replace the example paths, package, target and analysis command with
ones that exist in your project:

```toml
[review]
verify_findings = true
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
for `review.verify_findings` and `review.baseline_commands`. Use
`orbit config keys --json` to confirm which settings the installed version
supports; `review.host_evidence` is TOML-only and is not a `config set` key.
Check the global and workspace config files when reviewing host-evidence
rules, since a workspace list replaces the global one, and run `orbit doctor`
to catch configuration errors before a claim.

Use `orbit run readiness --json` or `orbit doctor` to see an
`unverified_review_finding` hold while a qualifying finding lacks a current
confirmation. After a verification job runs, `orbit run show <run-id>`
reports the run and `orbit task show <task-id>` shows the task status and
`finding_verified` history. A confirmed verdict for current material clears
that verification hold; changing claim or evidence selectors makes the old
verdict stale. For a claimed review, inspect the
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

| Key | Behavior |
|---|---|
| `publication.routine_attachments` | `omit` (default), `fail`, or `include`. The disabled weekly `task-publication-<workspace>` routine publishes only its owned workspace; manual publication keeps its CLI policy. |
| `publication.routine_allow_unscanned_attachments` | `false` (default). Explicitly accepts unscanned attachments with routine policy `include`; sensitive path and size limits still apply. Without this opt-in and a scanner, included attachments fail closed. |

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

## Crews

A crew is one named provider-and-model assignment. A task's `crew` field selects
its executor; `workflow.default_crew` covers the rest.

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
passes that variable to this crew only. The entry wins over `pass`, and setting
one Claude credential removes the other from that crew. `env` is global-only:
a workspace `config.toml` that sets it fails to load, and `orbit config set`
needs `--global`. With the source unset, drain, ship and job starts warn, the
drain leaves the crew out of its window, the crew's activities are refused
rather than run on the default login, and `orbit doctor`'s `env-pass` row
reports it. A crew with `env` is its own account: its usage readings, failure
holds, pull-drain auth exclusions and probes are its own, and
`workflow.provider_limit_budgets` / `provider_limit_overrides` may name it
(`opus-credits:200usd/30d`).

`orbit init` writes every built-in crew (Claude: `opus`, `sonnet`, `haiku`, `fable`;
Codex: `astra`, `sol`, `luna`; Antigravity: `gemini-flash`, `argon`;
Grok: `grok`; Copilot: `copilot`; Muse: `muse-spark`, `muse-wonder`, `muse-wonder-full`;
no built-in Gemini CLI crew; the `cursor`, `pi`, and `opencode` lanes were [retired](providers.md#provider-identity-and-resolution)), with `enabled = true` on the crews whose
agent CLI it detects and `enabled = false` on the rest, and points
`workflow.default_crew` at the preferred family's default (`opus` on a Claude
host, `gemini-flash` on an Antigravity host). Interactive init offers the enabled crews by name; `--non-interactive`
writes the recommendation. Init does not write a `custom` or `system` crew
table, nor a system-crew key. The four
`workflow.*_complexity_crews` pools are seeded from the detected families
(Claude only: `haiku` / `sonnet` / `opus` / `opus`; Codex only: `luna` / `sol` /
`sol` / `astra`; both: `haiku, luna` / `sol, sonnet` / `opus` / `opus, astra`;
grok joins `medium`; a detected `agy` appends `gemini-flash` to `low` and `argon` to `hard`;
a detected `muse` appends `muse-spark`, `muse-wonder`, and `muse-wonder-full` to `low`, `medium`, and `hard`;
Copilot and other families leave them `[]`). A user-authored
legacy `qa` crew remains loadable, but init never creates it. Init also writes every
`workflow.purpose_crews` pool explicitly: `task_pilot` and `auto_fix` copy `low`, and
`failure_recovery` and `review` copy `medium`. Because a fresh init writes them as explicit lists,
editing `workflow.*_complexity_crews` later does not move those purpose pools; to move system
work, edit its `workflow.purpose_crews.<purpose>` pool directly. With no supported
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
`workflow.purpose_crews.*` entries are written `name` or `name:weight`. Orbit
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
pilot, finding verification, escalation triage), so the CLI's own file-read
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

## PR screenshot section

`pr.screenshot_section` (advanced, default `auto`) decides whether a
single-task pull request body carries the optional `## Screenshots` table
that Orbit builds from the task's `screenshots/` artifacts.

| Value | A single-task delivery gets the section when |
|---|---|
| `auto` (default) | the task carries the `ui` tag, matched ignoring case |
| `always` | every single-task delivery |
| `off` | never: `pr_open` reads no screenshots and pushes no evidence ref |

Any other value fails the configuration load, so `orbit doctor` reports it
before a claim. A bundle of several tasks keeps Orbit's batch body and never
gets the section, and a `body` supplied to `pr_open` is published as given,
without it.
The `ui` tag's implementer routing is described in
[Tag instructions](#tag-instructions).

This setting governs only PR publication. Task-artifact evidence is a
separate duty: a diff that changes something a user sees still attaches its
screenshots as task artifacts when the workspace guide requires them,
whatever this value is. Name each artifact
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

### Set and verify the value

1. Confirm the installed version supports the key:
   `orbit config keys --json` lists `pr.screenshot_section`.
2. Read the effective value: `orbit config get pr.screenshot_section`
   (add `--json` for an object). `orbit config show --all` lists it in the
   delivery section with its source.
3. When provenance matters, resolve one file in isolation with
   `orbit config get pr.screenshot_section --scope global` or
   `--scope workspace`.
4. Only with the operator's authorization, change it:
   `orbit config set pr.screenshot_section always`. This writes the workspace
   `config.toml`; add `--global` to write the global file. Use `auto`, `always`
   or `off`; the value is parsed as a TOML literal, and these three fall back
   to plain strings.
5. Read the value back with step 2 and run `orbit doctor`.
6. Check the outcome on a real delivery, without opening a PR for the test:
   - Before `pr_open`, `orbit task show <task-id> --fields artifacts` lists
     the task's `screenshots/` artifacts.
   - After the PR opens, `gh pr view <pr-number> --json body --jq .body`
     should show the expected body:
     - a `## Screenshots` table with one row per view, width and theme (the
       theme is named only when the shots carry several), with Before and
       After columns, when publishable shots exist;
     - the `No screenshots attached.` note, when the section applies and no
       shot is publishable;
     - no `## Screenshots` section, when the value is `off`, or `auto` on an
       untagged task.

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
`orbit config show --all` adds the source: `workspace`, `global` or `built-in`.

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

**Backfill is not a policy change.** `orbit migrate --backfill-diff-stats
[--limit N]` applies pending migrations, then records rows only for done tasks
that landed before diff stats were recorded and have no row yet. It classifies
those rows with the lists in effect when it runs, so set the lists first if the
backfilled history should follow them. It does not rewrite existing rows. Run
`orbit migrate` without flags to see pending migrations before applying.

## Path-lease policy

A path lease lets a named principal, a person or an agent, reserve workspace
selectors for coordination. Leases are off until `lease.enabled` is `true`.
While it is off, every lease operation is refused with an error naming the key.

| Key | Built-in default | Accepted values | Governs |
|---|---|---|---|
| `lease.enabled` | `false` | `true`, `false` | Whether lease operations are accepted at all. |
| `lease.default_mode` | `advisory` | `advisory`, `hard` | Mode of a request that names none. |
| `lease.hard.human` | `allow` | `allow`, `never` | Whether a human principal's hard requests stay hard. `never` grants them as advisory. |
| `lease.hard.agent` | `allow` | `allow`, `never` | The same rule for agent principals. |
| `lease.max_priority.human` | `high` | `low`, `normal`, `high` | Highest priority a human request may name. |
| `lease.max_priority.agent` | `normal` | `low`, `normal`, `high` | Highest priority an agent request may name. |
| `lease.aging_minutes` | `30` | `1`–`10080` | Minutes a queued hard request or expansion waits before it moves up one priority level. |
| `lease.intake.peer_user` | unset | an OS user name | Global only. The lease listener's OS user that `orbit web serve` accepts lease operations from over `~/.orbit/run/lease-intake.sock`. Unset serves no socket. |

These keys inherit per key like other settings. Set the policy and read it back:

```bash
orbit config keys --json                 # confirm the lease.* keys in the installed version
orbit config set lease.enabled true      # workspace config; add --global for ~/.orbit/config.toml
orbit config get lease.enabled
orbit config show --all                  # effective values with their source
```

The same policy as TOML, with each dotted key written as a table:

```toml
[lease]
enabled = true
default_mode = "advisory"

[lease.hard]
human = "allow"
agent = "never"

[lease.max_priority]
human = "high"
agent = "normal"
```

Enabling leases changes live coordination for everyone on the workspace, so
make that choice deliberately and record it.

`lease.intake.peer_user` is read only from the global `~/.orbit/config.toml`.
Set it only for a lease listener user that meets the listener-user
prerequisite in the path-leases design. Orbit Web refuses to start when it
names root, the operator's own user or a missing user. The socket's directory
and file are group-owned by that user's primary group, so the operator's user
must belong to that group. Every connection from another user, the operator
included, is refused and logged with its uid.

### How requests behave

- **Advisory** requests are granted whenever leases are enabled. They block
  nothing. The answer lists the `overlaps` holders so the caller can coordinate.
- **Hard** requests are granted only when no selector overlaps a task context
  lock, a task reservation or another hard lease. Paths matching
  `workflow.unlocked_context_paths` leave the hard surface and come back as
  `unlocked_selectors`. An overlapping request waits in the lease queue:
  `granted: false`, `queued: true`, its `queue_position`, every holder in
  `conflicts`, and a lease in state `queued` that holds nothing, so drains and
  reservations ignore it. It gets a `queued` notice now and a `granted` notice
  when its holders release, expire or settle; a task holder is never preempted.
  The queue orders by priority, then age, and a waiter older than
  `lease.aging_minutes` moves up one level. Send `wait: false` (CLI
  `--no-wait`) to be refused instead (`queued: false`, `reason: held`).
- **Class downgrade:** a hard request from a principal class whose
  `lease.hard.*` is `never` is granted as advisory, with `downgraded: true`
  and a `notice`.
- **Priority** is stored with the lease and checked only against the class
  ceiling. A request above `lease.max_priority.*` is refused with
  `reason: priority_ceiling`, and the refusal is audited. Priority orders the
  queue; it never preempts a holder. A request that names no priority gets
  `normal`, lowered to the ceiling if the ceiling is below it.
- **Principal class** is `agent` when the process running the operation declares
  an agent or managed run, and `human` otherwise. Over REST, that is the serving
  process, not the remote caller. Over SSH the class and its ceilings are
  attribution plus a guard, not a security boundary.
- **TTL and expiry:** `ttl_seconds` defaults to `1800` and must be between `1`
  and `14400`. `renew` moves expiry to now plus the TTL, and `release` ends a
  lease. An expired lease ends lazily when a request, renew, release, show or list
  touches it, and each ending emits a `lease.expired` audit event. `check` only
  reads: it neither expires nor audits. Renewing an inactive lease fails, and
  releasing one returns `released: false`. Either way, request a new lease.
- **Expansion** (`orbit lease expand <id> <selector>…`, `orbit.lease.expand`,
  `POST /api/leases/{id}/expand`) adds selectors to a granted lease. Free paths
  are added at once; on a hard lease, blocked ones queue while the lease keeps
  what it holds. When two leases each wait on the other's expansion, the
  expansion ranked last in queue order (lowest effective priority, then
  youngest) is refused with a `deadlock` notice and keeps what it holds.

### Transports

CLI and MCP expose request, renew, expand, release, show, list, check and notices through
the same owner operations. Check `orbit lease --help` and the installed tool catalog
when coordinating with an older binary. `orbit lease notices --after <seq>` and
`orbit.lease.notices` return `{notices, cursor}`; add `--lease-id <id>` to filter.
`--follow` polls continuously and emits JSONL in JSON modes. Save the last returned
cursor to resume after a disconnect; reads never acknowledge notices or expire leases.

- **Config:** the `orbit config` commands above.
- **REST:** `orbit web serve` (default `127.0.0.1:7878`) serves `/api/leases`:
  `GET` and `POST /api/leases`, `GET` and `DELETE /api/leases/{id}`,
  `POST /api/leases/check`, `POST /api/leases/{id}/renew` and
  `POST /api/leases/{id}/expand`, plus
  `GET /api/leases/notices?after=<seq>` and `GET /api/leases/events` (SSE).
  SSE accepts `Last-Event-ID` as the resume cursor; each ID is the notice sequence. Select a workspace
  with `?workspace=<name-or-id>`. Reads need no operator session. Request, renew,
  expand and release need the server started with `--operator`; otherwise they are denied.
  The routes accept only localhost origins.
- **Audit:** `orbit audit list --tool orbit.lease.request --since 1h`, or
  `--kind lease` for every lease event. Granted requests record
  `lease.request.granted`, waiting ones `lease.request.queued` and, once granted,
  `lease.request.granted` with `from_queue: true`; refusals record
  `lease.request.refused` with status `denied`, expansions `lease.expanded`,
  and expiries record `lease.expired`.

### Claude Code edit hook

After requesting a session lease, export its ID as `ORBIT_LEASE_ID`. Put this
command hook in the project's `.claude/settings.json` (merge with existing hooks):

```json
{
  "hooks": {
    "PreToolUse": [{
      "matcher": "Edit|Write",
      "hooks": [{"type": "command", "command": "python3 .claude/lease-check.py"}]
    }]
  }
}
```

Save the following as `.claude/lease-check.py`. The hook reads the target path
from Claude Code's stdin JSON and invokes the public check command with an argv
list, so spaces and shell punctuation in paths stay literal:

```python
import json
import os
import pathlib
import subprocess
import sys

request = json.load(sys.stdin)
path = request.get("tool_input", {}).get("file_path")
lease_id = os.environ.get("ORBIT_LEASE_ID")
warning = None
if not lease_id:
    warning = "No session lease. Request one before editing shared paths."
elif path:
    try:
        target = pathlib.Path(path).resolve()
        root = subprocess.run(["git", "rev-parse", "--show-toplevel"],
                              capture_output=True, text=True, check=True).stdout.strip()
        selector = "file:" + target.relative_to(pathlib.Path(root).resolve()).as_posix()
        result = subprocess.run(
            ["orbit", "lease", "check", selector, "--lease-id", lease_id, "--json"],
            capture_output=True, text=True, cwd=root)
        if result.returncode:
            warning = "Lease check failed: " + result.stderr.strip()
        else:
            check = json.loads(result.stdout)
            if not check["lease_active"] or any(not p["covered"] for p in check["paths"]):
                warning = "Outside the session lease. Ask to expand it, or coordinate before editing."
            elif any(p["holders"] for p in check["paths"]):
                warning = "Other holders overlap this path. Coordinate before editing."
    except (OSError, subprocess.CalledProcessError, ValueError, KeyError) as error:
        warning = "Lease check could not cover this target: " + str(error)
if warning:
    print(warning, file=sys.stderr)
    if os.environ.get("ORBIT_LEASE_HOOK_DENY") == "1":
        sys.exit(2)
```

The default is advisory: a warning exits zero and permits the edit. Set
`ORBIT_LEASE_HOOK_DENY=1` explicitly to deny on warnings (Claude Code command hooks
use exit 2 to block). To cover an added path, run `orbit lease expand "$ORBIT_LEASE_ID"
<selector>` and wait for its `granted` notice. This hook does not expand or release
leases automatically. Test both an inside and outside path before enabling denial.

### Verify the policy

Use a disposable workspace for the first check. Enable leases there, start the
dashboard with `orbit web serve --operator --no-open`, and then:

1. `orbit config show --all` shows each `lease.*` value and its source.
2. Post an advisory request. Expect `"granted": true` and a `lease` object with a `lease_id`:

   ```bash
   curl -s -X POST 'http://127.0.0.1:7878/api/leases?workspace=<workspace>' \
     -H 'Content-Type: application/json' \
     -d '{"principal":"<person-or-agent>","selectors":["<workspace-relative-path-or-glob>"],"mode":"advisory","ttl_seconds":600}'
   ```

3. `GET /api/leases` lists the lease, and `orbit audit list --tool orbit.lease.request --since 1h` shows the granted event.
4. To see the queue, send a `"mode":"hard"` request over a path held by another active hard lease. Expect `granted: false`, `queued: true` and `conflicts` naming the holder; release the holder and expect a `granted` notice for the queued lease. Add `"wait": false` to see a refusal instead, with a `lease.request.refused` event with status `denied`.
5. To see a priority refusal, set `lease.max_priority.human` to `low` in the disposable workspace, then request `"priority":"high"`. Expect a refusal with `priority_ceiling` in the audit event.
6. To see class downgrade, set `lease.hard.human` to `never`, then send a hard request. Expect `granted: true`, `downgraded: true` and a `notice`.
7. Release the test lease with `DELETE /api/leases/<lease_id>`, and restore the previous `lease.*` values with `orbit config set`.

## Environment passthrough

Agent subprocesses always start from a **cleared** environment plus the
allowlist — the Orbit process's environment is never inherited, and that is not
configurable. The seeded baseline covers `HOME`, `PATH`, `TMPDIR`, `USER`, and
the provider home directories. Credentials are opt-in: add `GITHUB_TOKEN` or a
provider API key explicitly if agents need them.

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
