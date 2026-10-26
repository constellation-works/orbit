# Configuration

What is tunable and where. `orbit config keys` and `orbit config show` expose
the installed version's supported keys and effective values without a source
checkout. Check `orbit --version` before applying version-dependent CLI advice.

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

## Common settings

| Key | Purpose |
|---|---|
| `workflow.base_branch` | Config fallback for ship/auto/pilot base branch. A registered workspace `base_branch` takes precedence. |
| `workflow.default_crew` | Crew for any task that doesn't declare one. |
| `workflow.<tier>_complexity_crews` | Automatic crew pool per task complexity (`low`, `medium`, `hard`, `xhard`); entries `name` or `name:weight`. Empty (`[]`) routes that tier to `default_crew`. Orbit's own work draws from them too while its purpose pool is unset: auto-task templates by complexity, the task pilot and PR auto-fix from `low`, failure and PR conflict recovery from `medium`. The retired system-crew key loads with a deprecation warning and is ignored. |
| `workflow.purpose_crews.<purpose>` | Weighted pool for one purpose of work, written like the complexity pools: `task_pilot`, `failure_recovery`, `final_recovery`, `auto_fix`, `pre_landing_review`, `review`, `baseline_red_repair`, `sweep_tasks` (every task the CI failure, Dependabot and code-scanning sweeps file, and merged-head repairs a review reconciliation files), `explorer` (read-only explorers; see `workflow.explore.*`). An explicit task, template or activity crew wins; otherwise the purpose pool is drawn, narrowed by `--allow-crew`; unset or `[]` keeps the earlier route. A provider limit or failure hold on the assigned member redraws from the pool's other enabled members before the complexity pool. `final_recovery` has no earlier route: `final_recovery = []` disables final recovery. `pre_landing_review` falls back to `review` when unset or empty; `review` also supplies after-landing review and is described below. `orbit config show --all` lists each purpose's resolved pool and its source (`crew_purposes` in `--json`). |
| `workflow.explore.enabled` | Let managed implement and review-repair runs delegate read-only questions through `orbit.agent.invoke` to sandboxed explorers drawn from `workflow.purpose_crews.explorer` (default off). Budgets: `workflow.explore.max_calls_per_run` (6), `max_concurrent_per_run` (2), `timeout_seconds` (600) and `max_answer_bytes` (16384). |
| `workflow.regression_risk.enabled` | Mark regression-prone context files in implement and review prompts (default on). A file qualifies with at least `workflow.regression_risk.min_support` touching delivered tasks in `window_days` (defaults 5 and 30) and a regressed share of at least `threshold_percent` (default 40). At most `max_files` marks (default 3), highest rate first. |
| `workflow.generated_paths` | **Advanced: executes commands during conflict recovery.** Default `{}`; table of workspace-relative path globs to non-empty regeneration commands. Applies only to conflicting generated files in a stopped rebase. Verify the recovery's `regenerated_paths` and command result. See [Recovery settings](#recovery-settings). |
| `workflow.unlocked_context_paths` | Default `["**/*.md"]`; array of workspace-relative globs (`*` within a component, `**` across components; no absolute paths or `..`). Matching context entries take no lock in mixed tasks; an all-matching task still locks every entry. `[]` locks all entries. Verify with `orbit run readiness` and `orbit task eligible`. Keep code and build/CI definitions locked. |
| `workflow.lock_modes` | **Advanced: admission policy.** Default `[]` (every overlap blocks); ordered `[[workflow.lock_modes]]` tables, each a `pattern` (Rust regex searched for in the workspace-relative path; anchor with `^`/`$`) and a `mode` of `exclusive` (default) or `shared`. First match wins. An overlap is waived when its point, the narrower selector's path, is shared, so tasks sharing only, say, `(^|/)mod\.rs$` run together and the second to land resolves any conflict. `module:`/`command:` selectors stay exclusive. Set it in the workspace config only for files that merge cleanly or are regenerated. Verify with `orbit run readiness` and `orbit task eligible`. |
| `workflow.unlocked_path_validation_commands` | **Advanced: validation policy.** Default `[]` keeps full validation; array of command strings, trimmed with blank entries dropped. Replaces required commands only when the candidate differs from this run's last fully validated tree solely in unlocked paths. Verify `validation_scope` in validation logs. See [Recovery settings](#recovery-settings). |
| `workflow.admission_affinity` | **Advanced.** Default `band`; choices `band`, `priority`, `off`. Work only the requesting executor can run leads within a dispatch band with `band`, or only within a priority with `priority`; `off` keeps canonical band, priority, expiry and age order. Applies to owner-local and pull admission using the live executor census. Verify the effective choice with `orbit config show --all` and compare `orbit run readiness` ordering with the drain's last pass and live executor capabilities. |
| `workflow.validation_env.cargo_incremental` | **Advanced.** Default `false`; boolean. With a host validation output binding, forces `CARGO_INCREMENTAL=0` on validation children, replacing inherited values. `true` leaves it untouched; without a binding neither value sets it. Verify the child environment during authorized validation; existing incremental directories remain. See [Validation environment](operational-logs.md#worker-resource-containment). |
| `worktree.reclaim_below_free_mib` | **Advanced: reclaims rebuildable output.** Default unset; non-negative integer MiB (`u64`). During admission, reclaim `worktree.reclaim` paths from oldest terminal worktrees while free space under the state directory is below this mark; active runs remain protected. `0` never triggers it. Verify candidates with `orbit gc worktrees --reclaim --dry-run --estimate-bytes` and actual reclamation via the `worktree.reclaim` audit event. See [Worktree GC](maintenance.md#worktree-garbage-collection). |
| `workflow.auto_ship` | Opt-in for `orbit run ship-sweep` unattended ship dispatch. The seeded `ship-sweep` routine does not read it. |
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
change stays inside `workflow.light_delivery_paths`. `ui` gives implementers
the screenshot rule and gives reviewers the check that screenshots are
attached. A mapped file replaces the default for that tag and role.

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
validated pool assignment, or `default` fallback. Otherwise the complexity pool
and default chain apply as described below. A creation without a crew draws
from its complexity pool, falling back to `default_crew`, and stores
`crew_source` as `explicit`, `pool:<complexity>`, `purpose:<purpose>` (a task
whose system tag or `crew-purpose:<purpose>` tag names a configured purpose
pool), or `default`, also recorded
in assignment history. A complexity re-rate redraws a pool-sourced crew from
another tier or a `default` fallback and records the previous and new sources
and crews in `crew_redrawn` history; explicit crews stay pinned. A `default`
fallback is not an explicit pin. Status transitions alone preserve the choice.
Admission revalidates pool assignments and `default` fallbacks against the
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
