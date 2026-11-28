# Workflow configuration

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
| `workflow.default_crew` | see [resolution](providers.md#resolution-precedence) | Crew for a task with no `crew`. Must name a defined crew. |
| `workflow.system_crew` | — | **Deprecated, ignored.** Loads with a deprecation warning for one release; `config get`/`set` refuse it. System work draws from the complexity pools instead: see [system work](crews.md#system-work-draws-from-the-complexity-pools). Delete the key. |
| `workflow.low_complexity_crews` | `[]` | Pool a crew-less `low` task draws from, and the task pilot and PR auto-fix while their [purpose pools](crews.md#purpose-crew-pools) are unset; empty uses `default_crew`. See [pools](crews.md#automatic-crew-pools-by-complexity). |
| `workflow.medium_complexity_crews` | `[]` | The same, for `medium`; step-failure and PR conflict recovery draw from it too while `workflow.purpose_crews.failure_recovery` is unset. |
| `workflow.hard_complexity_crews` | `[]` | The same, for `hard`. |
| `workflow.xhard_complexity_crews` | `[]` | The same, for `xhard`. |
| `workflow.type_complexity_crews.<type>.<tier>` | unset | Pool a task of [type](#task-types) `<type>` at complexity `<tier>` draws from instead of `workflow.<tier>_complexity_crews`. See [type pools](crews.md#type-pools-by-task-type-and-complexity). |
| `workflow.purpose_crews.<purpose>` | `[]` | Weighted pool for one purpose of work (`task_pilot`, `failure_recovery`, `auto_fix`, `baseline_red_repair`, `sweep_tasks`, `pilot_warning_triage`). Unset or `[]` keeps the purpose's earlier route. `pilot_warning_triage` keeps only crews whose executor declares `inspection_args`, and falls back to `workflow.hard_complexity_crews` while unset or when no member qualifies. `final_recovery` (default `["sol:100", "opus:20"]`, `[]` disables it; see [final recovery pool](crew-recovery.md#final-recovery-pool)), `review` and `pre_landing_review` (see [automatic review](review.md#review-and-operation--automatic-review)) keep their own rules. See [purpose crew pools](crews.md#purpose-crew-pools). |
| `workflow.provider_limit_overrides` | `[]` | Per-provider thresholds replacing the fixed 90% at which admission skips a crew whose provider or model usage window is exhausted or used at or above it (see [provider usage limits](crews.md#provider-usage-limits)), each `provider:percent` (`["claude:80", "grok:100"]`) or `provider:off` (`["codex:off"]`, record readings without gating admission). One entry per provider, percent `1`–`100`, an alias such as `anthropic` stored as its provider. An entry may instead name a crew that sets [`env`](crews.md#per-crew-credentials-env) (`opus-credits:off`), its own account. |
| `workflow.provider_limit_budgets` | `[]` | **Advanced.** Rolling budgets for providers that report no usage, each `provider:<amount><usd\|tokens>/<n><h\|d>` (`["grok:30usd/5h"]`), or for one crew that sets [`env`](crews.md#per-crew-credentials-env) (`opus-credits:200usd/30d`). See [budgets](crews.md#budgets-for-providers-that-report-no-usage). |
| `workflow.provider_limit_explicit_crews` | `wait` | **Advanced.** A task whose explicit crew is limited: `wait` keeps it in the backlog; `pool` draws from the unlimited members of its complexity pool. |
| `workflow.crew_selection` | `static` | **Advanced.** How task creation, complexity re-rate and admission draws weigh pool members: `static` uses the configured weights; `usage_pressure` scales each by its crew's weekly usage pressure and re-draws a system-chosen crew at admission. This is the policy key; the `crew_selection` object in `orbit run show` and run input is the recorded draw. See [usage-pressure draws](crews.md#usage-pressure-draws). |
| `workflow.task_types` | `[]` | **Advanced.** Ordered task types that [type pools](crews.md#type-pools-by-task-type-and-complexity) route by and the implementer scorecard's by-type matrix splits by, each a `[[workflow.task_types]]` table with `name`, `tags` and `paths`. See [task types](#task-types). |
| `workflow.crew_performance_types` | — | **Deprecated.** The former name of `workflow.task_types`, read into it with a deprecation warning until 2026-12-07. A file that also sets `workflow.task_types` keeps that. `orbit doctor --fix-retired-config-keys` rewrites it. |
| `workflow.crew_performance`, `workflow.crew_performance_strength`, `workflow.crew_performance_cap`, `workflow.crew_performance_window_days` | — | **Deprecated, ignored.** Crew-performance weighting, its scorecard snapshots and the `crew_performance_refresh` routine were removed [ORB-22804]. Load with a deprecation warning until 2026-12-07; `config get`/`set` refuse them. Route by type with `workflow.type_complexity_crews`, and delete the keys. |
| `workflow.auto_ship` | — | **Deprecated, ignored.** The ship sweep it opted into was removed. Loads with a deprecation warning; `config get`/`set` refuse it. Dispatch with `orbit run auto` drains or `orbit run ship`, and delete the key. |
| `workflow.required_validation_commands` | `[]` | Commands every delivered candidate must pass before push or merge. Empty runs no required check. See [required validation](workflow-validation.md#required-validation). |
| `workflow.distributed_completion` | `review` | How far this owner takes an accepted distributed-drain handoff: `review` waits for an operator's **Approve handoff**; `done` authorizes it on acceptance and lands it through `task_landing_pipeline`, rechecking this key before the merge. Under `done`, the owner's [`completion.hold`](review.md#completionhold--keep-risky-completions-in-review) rules can still hold a risky candidate in `review`. |
| `workflow.unlocked_context_paths` | `["**/*.md"]` | Path globs whose `context_files` entries take no context lock. See [unlocked context paths](#unlocked-context-paths). |
| `workflow.lock_modes` | `[]` | **Advanced.** Ordered `[[workflow.lock_modes]]` tables, each a `pattern` regex and a `mode` of `exclusive` (default) or `shared`. An overlap between two footprints at a shared path does not block admission. See [footprint lock modes](#footprint-lock-modes). |
| `workflow.unlocked_path_validation_commands` | `[]` | Commands a repaired candidate runs instead of the full required list when its tree differs from the run's last fully validated tree only in `workflow.unlocked_context_paths`. Empty always runs the full list. See [revalidating an unlocked-path repair](workflow-validation.md#revalidating-an-unlocked-path-repair). |
| `workflow.generated_paths` | `{}` | Generated files, as path glob = regenerate command. Conflict recovery regenerates a conflicting match instead of merging it. See [generated paths](#generated-paths-are-regenerated-not-merged). |
| `workflow.light_delivery_paths` | `["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]` | Path globs a candidate must change only within for a `skip-review` or `skip-validation` task to skip agent review or local validation. A leading `!` excludes. `[]` disables both tags. See [light delivery](workflow-tags.md#light-delivery-skip-review-and-skip-validation). |
| `workflow.test_paths` | `["**/test/**", "**/tests/**", "**/__tests__/**", "**/spec/**", "**/testdata/**", "**/*_test.*", "**/*.test.*", "**/test_*.*", "**/*_spec.*", "**/*.spec.*"]` | **Advanced.** Path globs whose changed lines count as tests in landed-delivery diff stats. See [landed diff classification](#landed-diff-classification). |
| `workflow.non_code_paths` | `["docs/**", "**/*.md", "**/*.mdx", "**/*.rst", "**/*.txt", "**/*.json", "**/*.yaml", "**/*.yml", "**/*.toml", "**/*.lock", "**/*.csv", "**/*.golden", "**/*.png", "**/*.jpg", "**/*.jpeg", "**/*.gif", "**/*.webp", "**/*.svg", "**/*.ico", "**/LICENSE*"]` | **Advanced.** Path globs whose changed lines count as other (docs, config, data, assets) in landed-delivery diff stats; everything else is code. See [landed diff classification](#landed-diff-classification). |
| `workflow.non_reviewable_paths` | `[]` | **Advanced.** Path globs left out of the reviewable-lines denominator. Docs and config stay reviewable; list lockfiles, goldens, snapshots and generated files here. Empty counts every line that is not a pure rename. Distinct from `workflow.non_code_paths`. See [landed diff classification](#landed-diff-classification). |
| `workflow.regression_layers` | `ci_tests`, `docs_skills` rows | **Advanced.** Ordered `[[workflow.regression_layers]]` tables, each a `name` and `prefixes`, `dirs` and `suffixes` path rules, that give a regression fix its layer in codebase regression analytics. See [regression layers](#regression-layers). |
| `workflow.review_skip_tags.<tag>.paths` | unset | Path globs (same syntax as `workflow.light_delivery_paths`; `["**"]` allowed) a candidate carrying `<tag>` must change only within to skip agent review. See [review-skip tags](workflow-tags.md#review-skip-tags-workflowreview_skip_tags). |
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

A task takes the first type whose `tags` it carries (any of) or whose `paths` globs match any of its `context_files` (any of); a task matching none is `other`. Names must be unique and non-empty, `other` is reserved, and an entry with neither `tags` nor `paths` is refused with a config error naming the entry. Orbit ships no types beyond `other`, so an empty list puts every task in `other`. Two readers use the list: [type pools](crews.md#type-pools-by-task-type-and-complexity) route a task of a type through `workflow.type_complexity_crews`, and the implementer scorecard's by-type matrix (`orbit agents scorecard --role implementers --by-type`) splits its cells by type. The scorecard classifies historical tasks with the current list at scan time, so editing it re-sorts history.

The list was named `workflow.crew_performance_types` before [ORB-22804]. That spelling still loads into `workflow.task_types` with a deprecation warning until 2026-12-07, with the same entries; a file that sets both keeps `workflow.task_types`. `orbit doctor --fix-retired-config-keys` rewrites it.

### Unlocked context paths

`workflow.unlocked_context_paths` lists workspace-relative globs: `*` matches within one path component and `**` spans components, so `**/*.md` is every Markdown file and `docs/**` is everything under `docs/`. A `context_files` entry whose path matches stays in the task's context, and the agent still receives it, but takes no context lock. Local drain waves, readiness, backlog admission, `orbit task eligible`, lock contention and lock reservations skip it, so two tasks that share only a design doc beside their own code run together and landing reconciles the text by merge and conflict recovery.

A task whose every entry matches, such as a rewrite of one document, still locks them all, because two rewrites of one file are where a merge can silently produce contradictory text. A mixed task touching that file is not held back by the rewrite, since it does not request the file. Leave build and CI definitions (`Makefile`, `.github/workflows/*.yml`) and code out of the list: they define the gates, so a bad merge there breaks every delivery. `[]` locks every entry, as before the setting existed. Owner pull admission still checks a remote claim's full declared footprint. A live execution claim's frozen footprint likewise holds unlocked entries: a task cannot start work while any of its entries, unlocked ones included, overlaps such a claim. Drain admission and readiness therefore defer it as `context_lock_conflict`, naming the claim's task and run, until the claim settles. A claim on known `no-diff-expected` work holds only its own task. The value is read when an Orbit process opens the workspace.

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

### Regression layers

`orbit codebase analytics` and `show` (and the dashboard's Codebase view) give each `regression_from` link a layer: where the regression task's fix sits. Its landed paths, else its declared footprint, map through `workflow.regression_layers`, an ordered list of `[[workflow.regression_layers]]` tables. A path is in the first row with a matching rule: it starts with one of the row's `prefixes`, one of its directory components equals one of its `dirs`, or it ends with one of its `suffixes`. A path no row matches is `other`, and a fix with no path is `unknown`. The fix takes the layer most of its paths map to, ties going to the earlier row; `ci_tests` and `docs_skills` count only when the fix touched nothing else. Breakdowns list the layers in row order, then `other` and `unknown`, and `--layer` (the tool's `layer`) accepts exactly those names.

Unset, the list holds two repository-neutral rows. Setting it replaces them, so repeat them if you want them:

```toml
[[workflow.regression_layers]]
name = "ci_tests"
prefixes = [".github/"]
dirs = ["tests", "testdata", "fixtures"]

[[workflow.regression_layers]]
name = "docs_skills"
prefixes = ["docs/"]
suffixes = [".md"]

[[workflow.regression_layers]]
name = "web"
prefixes = ["web/", "packages/ui/"]
```

Each row needs a unique `name` other than `other` and `unknown` and at least one rule, and each `dirs` entry is a single path component. A refused list is a load error naming the key. `[]` puts every path in `other`. Layers are computed when the analytics scan runs, so a changed list applies to the next scan.

### Generated paths are regenerated, not merged

`workflow.generated_paths` maps a generated file's path glob to the command that rebuilds it:

```toml
[workflow.generated_paths]
"docs/INDEX.md" = "make docs-index"
"docs/design/FEATURES.md" = "make docs-index"
```

When a stopped rebase conflicts on a matching path, the conflict recovery agent is told not to hand-merge it (its input lists it under `regenerated_paths`). After the agent has resolved the other files and exits, the host continuation takes the upstream side of the file, or the replayed side when upstream deleted it. It then runs each distinct command once in the checkout with the validation environment, for at most ten minutes, and stages the output with the rest of the resolution. A command that exits non-zero, times out, leaves conflict markers in the file, or changes HEAD, the branch or the index fails the repair. The stopped rebase then goes to the failure handoff as any refused continuation does. Other files the command rewrites join the continued commit. The certified continuation lists the regenerated paths. Default empty: every conflict is resolved by the agent.

### What `orbit init` seeds

Only the global file, and only when it is absent (or under `--force`):

- every [built-in crew](crews.md#crewsname--which-provider-model-runs-the-task), enabled for each detected provider CLI and `enabled = false` otherwise;
- `default_crew`: the default crew of the first detected family in preference order;
- the four complexity pools, as bare lists:

  | Detected | `low` | `medium` | `hard` | `xhard` |
  |---|---|---|---|---|
  | codex + claude | `["haiku", "luna"]` | `["terra", "sonnet"]` | `["sol", "opus"]` | `["fable", "astra"]` |
  | codex only | `["luna"]` | `["terra"]` | `["sol"]` | `["astra"]` |
  | claude only | `["haiku"]` | `["sonnet"]` | `["opus"]` | `["fable"]` |

  On any row, and alone when neither codex nor claude is detected, a detected `grok` is appended to `medium`; a detected `agy` appends `gemini-flash` to `low`, `hydrogen` to `medium`, `argon` and `neon` to `hard` and `carbon` to `xhard`; a detected `muse` appends `muse-spark`, `muse-wonder` and `muse-wonder-full` to `low`, `medium` and `hard`. With codex, claude, grok, `agy` and `muse` all detected that gives `low = ["haiku", "luna", "gemini-flash", "muse-spark"]`, `medium = ["terra", "sonnet", "grok", "hydrogen", "muse-wonder"]`, `hard = ["sol", "opus", "argon", "neon", "muse-wonder-full"]` and `xhard = ["fable", "astra", "carbon"]`. Copilot leaves the pools as they are, and an empty pool routes to `default_crew`. Init fails rather than write a pool entry it does not seed enabled.
- these [purpose pools](crews.md#purpose-crew-pools), each keeping only the members whose crew is enabled: `review = ["sol", "sonnet", "muse-wonder", "grok", "opus@medium"]`, `task_pilot = ["luna@high", "haiku@high", "muse-spark@high"]`, `failure_recovery = ["luna", "haiku", "muse-spark", "grok"]`, `auto_fix = ["luna", "haiku", "muse-spark"]`, `final_recovery = ["sol", "muse-wonder", "opus@medium"]`, `baseline_red_repair = ["terra", "sonnet", "muse-wonder", "grok"]` and `pre_landing_review = ["luna", "haiku", "muse-spark"]`. A pool left empty is omitted and keeps its earlier route, except `final_recovery`, written as `[]` (final recovery off) so it never falls back to a crew this host does not run. `sweep_tasks` and `pilot_warning_triage` are not written;
- `experiments.enabled = true`, `workflow.crew_selection = "static"`, `workflow.unlocked_context_paths = ["**/*.json", "**/*.txt", "**/*.md", "**/mod.rs", "**/*.toml"]`, `workflow.mid_run_steering.enabled = true` and `workflow.task_pilot_freshness.material_fields = ["description", "criteria"]`;
- when init creates this machine's identity, `machine.auto_concurrency.mode = "enforce"` beside it, unless already set. An existing machine keeps its mode.

These choices apply to newly seeded global configurations. Existing crew tables and complexity or purpose pools keep their explicit values; workspace overrides keep their normal precedence. To use the new defaults in an existing configuration, edit those choices explicitly.

Interactive init asks for the default crew by name, offering only enabled crews. It no longer writes or asks for `workflow.system_crew`: system work draws from the pools. With no supported CLI, every crew is written disabled and `default_crew` stays unset, so it resolves to the disabled `opus` and agent dispatch refuses until you run `orbit config set crews.<name>.enabled true`; deterministic jobs such as store retention still run. Init never writes `[crews.system]`, `[crews.custom]` or `[crews.qa]`.

### `[workflow.resource_throttle]` — host pressure

While CPU, memory, disk I/O (Linux) or a watched filesystem stays at or above its high mark, admission using those thresholds is held until it falls below the resume mark; a local drain may still admit eligible CPU-light auto-tasks while CPU alone is held and reserved slots remain. Memory, disk and I/O pressure also hold CPU-light work, and pull drains do not use the budget. Running work is never cancelled. Drain, ship and readiness behavior: [host resource pressure runbook](../runbooks/distributed-drain.md#host-resource-pressure).

A scheduled host shutdown is a separate hold. It stands new unattended work down only when the schedule is 180 minutes away or sooner, so a reboot set for tomorrow does not idle the host all day. Readiness still names a later schedule. See [scheduled host shutdown](../runbooks/distributed-drain.md#scheduled-host-shutdown-or-reboot).

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

**Dashboard.** The rail foot's host block shows the selected host's `load` (CPU as a multiple of online cores, or `cpu` as a busy percentage when `cpu_source = "utilization"`), `mem` and `disk` readings, and an `io` row on hosts that report disk I/O pressure, with severity and held resources ([UI design §6](../design/user-interface/2_design.md#6-top-level-navigation)); they poll every five seconds and mark aging data unknown even while a request is pending. The top bar shows one warning chip only while a reading is elevated or worse. **Settings › Machine** shows the enable switch and each resource's high and resume marks, with source, live reading, severity and verdict: six marks for CPU, memory and disk, and, on hosts that report disk I/O pressure (Linux only), an `io` row with its fixed 20% / 10% marks, which cannot be edited. Its edits share the topic pages' `PUT /api/config/keys/{key}` validation but write the **global** file by default, because this host's readings and admission read global settings; a workspace override is marked and wins for that workspace's runtimes.

### `[workflow.task_pilot_freshness]` — when a task is piloted again

```toml
[workflow.task_pilot_freshness]
material_fields = ["title", "description", "criteria", "plan", "selectors"]
source_sensitivity = "ignore"
```

| Key | Default | What it does |
|---|---|---|
| `workflow.task_pilot_freshness.material_fields` | `["title", "description", "criteria", "plan", "selectors"]` | **Advanced.** Task inputs checked for drift while a pilot is prepared and applied. For an applied assessment, a description or criteria edit queues a fresh pilot through the task-pilot routine when it admits the task and no live execution claim or unresolved failed pilot holds it, while edits to the other fields never do; an `--approve-proposed` drain still pilots a stale explicit/routine assessment again automatically when its certified description/criteria or other material (selectors, complexity, crew/provenance, relations, configured material fields) drifted and no live execution claim holds it, while scheduled discovery still pilots one again automatically only when its certified description/criteria drifted and no live execution claim holds it. At least one of `title`, `description`, `criteria`, `plan`, `selectors` (`context_files`), `tags`, `crew` (the stored crew and its resolved model/provider), `tools`, `type`, `complexity`, `relations`, `dependencies` (each one's status and meaning) and `instructions` (`AGENTS.md`/`CLAUDE.md` at the pinned revision). |
| `workflow.task_pilot_freshness.source_sensitivity` | `ignore` | **Advanced.** Whether a move of the observed branch head makes an assessment stale. `ignore`: never (the pinned revision is still recorded). `context_files`: when the head changed a path a selector names (`file:` and `dir:` by prefix, `symbol:` through its file). `any`: every move. |

**Unprepared backlog work.** A backlog task with no `context_files` and no `no-diff-expected` tag holds no file lock. A local drain or discovery ship with more than one slot (`max_active_leaf_runs` > 1) therefore holds it: while an enabled `preparation_eligible` routine owned by this host (`trigger.state.owner_machine`) would still prepare it, it waits as `awaiting_footprint` for at most that trigger's `max_wait_minutes + deadline_minutes` from the task's last change. When no such pilot exists, the pilot assessed it and left it empty, or the wait elapsed, it starts only when no other leaf is in flight (`awaiting_exclusive_slot`) and holds the whole tree while it runs. A single-slot drain, an explicit `orbit run ship <id>` and owner pull admission admit it as before. `orbit run readiness` reports both reasons with the fix. An operator's `readiness-waived` tag waives a backlog task's readiness gaps (no context files, unassessed complexity): the drain then admits it like ready work, without the whole-tree reservation and beside other work. A task with no `context_files` still holds no file lock; one with declared selectors keeps its file locks and waives only the complexity gate. Only an operator adds the tag, history records `readiness_waived` and `readiness_waiver_removed`, and host OS, labels, browser kit and human-crew waits still apply.

Pilot preparation reserves its selection atomically inside the task commit boundary. If acquiring that boundary times out before the reservation operation starts, preparation excludes the waiting tasks as `pilot_reservation_lock_busy`, records the readable lock diagnostic in `reservation_deferred`, and returns no pilot partitions. The run launches no pilot agent; apply supersedes that preparation attempt so state-triggered members can be observed afresh, including CI-sweep children. Existing live pilot reservations remain in force. Reservation I/O errors and errors after the operation starts still fail; preparation never retries an operation that may have written state.

A routine's `eligibility` block decides which never-assessed tasks receive their first automatic pilot. Once a task has an applied assessment, a description or criteria edit queues a fresh pilot through the routine when it admits the task and no live execution claim or unresolved failed pilot holds it; other material or freshness-policy edits do not. An `--approve-proposed` drain does queue a fresh pilot automatically when a task's certified description/criteria or other material (selectors, complexity, crew/provenance, relations, or a configured material field) changed after its applied assessment and no live execution claim holds it, and scheduled discovery does so only when its certified description/criteria changed and no live execution claim holds it, the same way they pilot a new proposal; a failed repilot holds the task until its meaning changes again or an operator acts. A person's or agent session's edit to `context_files` on a proposed or backlog task whose latest assessment holds a warning or an admission hold queues a fresh pilot the same way (history records `pilot_context_files_edited` against the assessment it follows); a comment, a plan or execution-summary update, the pilot's own selector writes and a run's `context_files_widened` records do not, so a pilot cannot re-trigger itself. The `pilot_warning` and `pilot_hold` attention items and the dashboard's **Pilot warnings** callout say which edits clear the hold and name `task repilot` only as the operator route. An operator can still request one explicitly with `orbit task repilot <id>`, `orbit.task.recover` action `repilot`, task update `repilot: true`, or a decision option action `re_pilot`. Description/criteria edits hold dispatch as `pilot_assessment_stale` until a new assessment completes, unless an operator skips pilot admission with `no-pilot`; adding the tag after a receipt or while a pilot is pending requires operator authority. Other readiness and delivery holds still apply. A task-pilot routine's `trigger.state.freshness` overrides either key (routine, then this table, then defaults); the resolved value still guards prepare/apply drift. See [automation triggers](../design/automation-triggers/references/operations.md).

The dashboard's Auto-drain card shows a hold breakdown by reason. The
pilot-warning count links to proposed tasks filtered by their holding findings.
Task rows (including the grouped status cards) carry
an amber **⚠ pilot warnings · N** chip, and the detail's **Pilot warnings**
callout lists the full findings by kind. These come from the task's latest
durable pilot verification, so they remain visible without a live drain.
Advisory findings do not count. Approval, leaving proposed, or a clean pilot
verification clears them; an ordinary comment or edit alone does not.
Task list/detail JSON and MCP expose the same `pilot_warnings` array of
`{kind, text}` entries, and CLI readiness uses that source for warning holds.
A proposed task whose referenced pilot assessment artifact is missing, unreadable
or does not match its receipt reports one `pilot_assessment_unavailable` entry in
its own `pilot_warnings`; the other tasks in the same list or page are unaffected.
Automatic pilot discovery likewise excludes only that task, counted under
`pilot_assessment_unavailable` in its `excluded_by_reason`, and keeps preparing
the rest.
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

An `--approve-proposed` drain checks freshness under the approval lock against the exact material certified by the pilot's atomic write. The pilot's OS routing tag, selectors, complexity and automatic crew redraw therefore permit approval; later tag, crew, assignment provenance, relation or configured material edits withhold it. The `no-auto-approve` and `no-drain` opt-outs always win. Replayed writes use their recorded audit rather than the task's current values; a pending approval from an older audit without that record needs a new assessment. Approval preserves routing and native-host admission holds. A fresh assessment from an explicit or routine pilot is approved without another pilot. A stale one whose certified description/criteria or other material (selectors, complexity, crew/provenance, relations, or a configured material field) drifted is selected again for a fresh drain pilot like a new proposal when no live execution claim holds it; a failed repilot, a holding warning, a live claim, or a running pilot keeps it proposed until the task changes or an operator acts.

### `[workflow.activity_timeouts]` — CLI agent wall-clock budgets

```toml
[workflow.activity_timeouts]
agent_implement = 28800 # 8 hours, overriding the shipped 6-hour budget
```

Maps exact activity names to integer seconds (`1..43200`, inclusive; maximum
12 hours). Applies to CLI agent-loop activities, including custom activities,
regardless of provider. Zero, negative, non-integer or excessive values fail
config loading with an error identifying the key. Names must be non-empty and
contain no whitespace. A workspace table replaces the global table; omitted
activities keep their declaration, with the runner fallback for a zero
declaration. An empty table overrides nothing. Restart long-running processes
after editing. `orbit config get workflow.activity_timeouts` shows the effective
table; `orbit config set workflow.activity_timeouts '{ agent_implement = 28800 }'`
replaces it.

The budget is resolved once per activity step: continuations and response
corrections spend what remains rather than renewing it. An admitted review
keeps its `review.minutes` bound ahead of activity overrides. Shipped
`agent_implement` defaults to 21600 seconds (6 hours). The task gate's child
wait and reservation TTL are 46800 seconds (13 hours), covering the maximum
12-hour override plus a bounded 1-hour delivery/review tail. The auto pipeline's
gate wait is 54000 seconds (15 hours), including admission and tail headroom.
Child waits and reservation TTLs accept up to 86400 seconds (24 hours).
Recovery/review budgets are independent and do not enclose implementation.
Custom jobs and explicitly shorter enclosing waits can still cancel a child
first; keep those waits and reservation TTLs consistent with activity budgets.

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

With it on, `task_pr_pipeline` and `task_local_pipeline` gate `commit` ([design](../design/mid-run-steering/2_design.md#3-promotion-gate)). Before committing, the run re-reads the task, and while an operator comment its prompts never carried in full remains, it does not commit. Each refusal spends one steering round, recorded under `steering_rounds` in the run's state: the implement step runs again, even for a carried candidate, with `input.steering_round` set, and its prompt delivers the comments onto the candidate still uncommitted in the worktree. A run that carries an earlier run's candidate judges from the watermark that earlier run's prompts left, so a comment written after that prompt and before the new run is outstanding; a carried candidate whose source cannot be traced fails the run before `commit` (`[steering_gate_unreadable]`). When the run has spent `max_rounds`, or its last round delivered none of the comments still outstanding (one too long for the block, for example), the task is blocked under a `steering_unconsumed` history event naming each outstanding comment, and the run fails without committing. Read the comments with `orbit task show <id>`, then requeue the task, answer them differently, or accept the candidate as it is through the normal review record. A gate that cannot read the task or the run fails the run before `commit` (`[steering_gate_unreadable]`), and a task that is `done` or `archived` stops it there, naming the steering it never received. A resumed run starts with its full budget of rounds. Both claimed pipelines gate `commit` too, reading the comment log and the claim's watermark from the owner; an owner the leaf cannot read fails the run before `commit` (`[steering_gate_unreadable]`), and a replica never certifies delivery. A claim's watermark outlives its settlement, so the task's next claim or owner run never delivers the same comments again. While an execution claim protects a task, the owner allows human operator comment-only writes; other unscoped writes stay refused. The live leaf reads those comments and records delivery through its owner.

Steering settings refresh at prompt and gate checkpoints. Enabling with runs in flight leaves missing watermarks absent: their gates treat post-dispatch operator comments as undelivered (or use an inherited candidate/claim baseline when present). Disabling stops recording and gating at the next checkpoint; existing watermarks and steering-round state stay inert, not deleted. Invalid config retains the last successfully resolved settings. Prefer enabling between drains; enabling does not change admission authority. A new global config seeded by `orbit init` sets `workflow.mid_run_steering.enabled = true`; when the key is absent the built-in default `false` applies. See [migration](../design/mid-run-steering/specs/steering-delivery.md#migration).

### `[workflow.standing_drain]` — the opt-in drain keeper

```toml
[workflow.standing_drain]
enabled = false
window = "24h"
# concurrency = 5            (unset: the drain job's default)
complete = false
approve_proposed = false
auto_fix = false
min_resubmit_interval = "10m"
max_failed_resubmits = 3
```

| Key | Default | What it does |
|---|---|---|
| `workflow.standing_drain.enabled` | `false` | Lets the owner host's clock tick submit one bounded `workspace_auto_pipeline` drain, through the same path as `orbit run auto`, when none is pending, running or held, `backlog` or `proposed` work is waiting, and the workspace's drain intent is `on`. `false` on every workspace and host: nothing is submitted and no keeper attention item appears. |
| `workflow.standing_drain.window` | `"24h"` | **Advanced.** Window of each keeper drain (`1m`–`24h`), the `run auto --for` value. |
| `workflow.standing_drain.concurrency` | unset | **Advanced.** Worker ceiling of each keeper drain (`1`–`64`). Unset uses the drain job's default (5), as `run auto` without `--concurrency` does. The forecast mode stays `machine.auto_concurrency.mode`. |
| `workflow.standing_drain.complete` | `false` | **Advanced.** Keeper drains land shipped tasks and mark them `done`, as `run auto --complete` does. |
| `workflow.standing_drain.approve_proposed` | `false` | **Advanced.** Keeper drains approve `proposed` tasks that pass the task pilot, as `run auto --approve-proposed` does. |
| `workflow.standing_drain.auto_fix` | `false` | **Advanced.** Keeper drains repair failing checks and conflicts on their pull requests, as `run auto --auto-fix` does. |
| `workflow.standing_drain.min_resubmit_interval` | `"10m"` | **Advanced.** Least time from the newest drain's end to the keeper's next submission (`1m`–`24h`). |
| `workflow.standing_drain.max_failed_resubmits` | `3` | **Advanced.** Crash breaker: once this many keeper drains end `failed` or `interrupted` within one hour of each other (an upgrade interruption does not count), the keeper submits nothing, however much time passes, until a person starts a drain (`1`–`100`). |

**Setting `enabled` and `approve_proposed` is the operator's (Daniel's) decision**, as creating a routine is: `enabled` lets the clock start drains with nobody present, and `approve_proposed` lets those drains approve work. No agent session sets them, and no session sets `enabled = true` on a live host for a test; QA runs under a disposable home. Orbit has no operator-only class of config key, so this is a rule, not something the config layer enforces.

The keeper never resumes a pause: a hand stop (`orbit run auto --stop`, the dashboard's Stop) records the intent `paused`, and only a hand start (CLI, dashboard or MCP) sets it back to `on`. A workspace whose intent was never recorded (`off`) is not acted on either. A window end or a crashed drain does not pause; the keeper resubmits after `min_resubmit_interval`. The keeper admits no task: the drain it submits goes through the unchanged admission ladder, its flags come from these keys and never from the last run, and a replica checkout never submits. Its submissions are recorded as the keeper's (trigger routine and actor `standing-drain-keeper`) and leave a `drain.keeper.submit` audit event with the effective window, concurrency, `complete`, `approve_proposed` and `auto_fix`; a skip leaves `drain.keeper.skip` with its `reason` when the reason differs from the keeper's previous decision. Settings are re-read each tick, so turning `enabled` off stops the keeper at its next tick. While it is enabled, `orbit run readiness`, the owner's `orbit doctor` `standing-drain` row and the attention kinds `drain_paused` (paused over 12 hours with work waiting) and `drain_breaker_open` report it. Operation: [distributed-drain runbook](../runbooks/distributed-drain.md#standing-drain-keeper-phase-2).

<a id="task-riskiness-scores"></a>

### Task riskiness scores

The riskiness score is fitted on task size alone over the done tasks of the trailing 30 days: the declared path count before a diff exists, and the lines changed once a review candidate or landed diff resolves if a lines-based fit is available. Otherwise it falls back to the declared path count. A fit needs at least 5 done tasks on its basis. Both numbers are fixed. The Tasks row shows the score as a percentage, prefixed with `~` when some selectors name no path; when no estimate is available, the row shows no risk part. Task detail shows the score with its basis, size and how many recent done tasks of that size or larger regressed, including the reason for no estimate: **no files** (the task has no context files and no diff), **no paths** (its selectors identify no file or directory), **thin history** (too few done tasks in the window to fit), or **unavailable** (the estimate could not be computed). Analytics uses this score. `@auto` effort instead uses configured bands based on complexity and declared files (or review diff lines), falling back to the crew's configured effort when no band matches; it does not read the riskiness score.

Regression-risk prompt marks were removed [ORB-22806]: implement and review prompts no longer carry a `task.regression_risk` section, no task records a new `regression_risk_marked` history event (earlier ones still load), and Analytics › Codebase has no with-marks comparison. `workflow.regression_risk.enabled`, `window_days`, `min_support`, `threshold_percent` and `max_files` are deprecated and ignored ([validation and errors](validation.md#validation-and-errors) lists them).

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
See [workspace agent corpus](../design/workspace-agent-corpus/2_design.md) for document
schema, append materialization and the orbit-office Library boundary.

### Path leases

Path leases are not configurable for now: the `lease.*` keys, `orbit lease` and the `orbit.lease.*` tools were withdrawn, and a `[lease]` table loads with a deprecation warning and no effect ([ORB-22808]; [design](../design/path-leases/1_overview.md)). The lease machinery is retained but stays disabled.
