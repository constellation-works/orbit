# Crews and crew selection

## `[crews.<name>]` — which provider-model runs the task

A crew is one provider-model assignment. An activity uses the crew named in its rendered input, otherwise the run's resolved crew.

| Field | Required | Values |
|---|---|---|
| `provider` | Yes | `claude`, `codex`, `antigravity`, `gemini`, `grok`, `copilot`, `muse`. See [provider identity](providers.md#provider-identity-and-resolution). |
| `model` | Yes | Model ID passed to the provider CLI. |
| `enabled` | No | Default `true`. `false` keeps the crew defined and listed but refuses to run it; see [disabled crews](#disabled-crews). |
| `effort` | No | Reasoning effort (below). Omitted leaves the provider's default. |
| `description` | No | Summary. Trimmed; blank becomes absent. |
| `tags` | No | Discovery labels. Trimmed, blanks dropped, sorted and deduplicated. |
| `env` | No | Variables this crew's agents receive on top of `execution.env.pass`, as `NAME` or `NAME=SOURCE`. Global config only; see [per-crew credentials](#per-crew-credentials-env). |
| `vision` | No | Default `true`. `false` opts a text-only model out of image input on an image-capable lane; see [executor image input](providers.md#executor-image-input-image_input). |

```toml
[crews.sol]
model = "gpt-6.2-sol"
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
| `claude` | `claude` | `opus` (`opus`), `sonnet` (`sonnet`), `haiku` (`haiku`), `fable` (`claude-fable-5-2`) | `opus` |
| `codex` | `codex` | `astra` (`gpt-6.2-astra`), `sol` (`gpt-6.2-sol`), `terra` (`gpt-6.1-terra`), `luna` (`gpt-6-luna`) | `astra` |
| `antigravity` | `agy` | `gemini-flash` (`gemini-3.8-flash-high`), `argon` (`gemini-4-argon-high`), `neon` (`gemini-4-neon-high`), `hydrogen` (`gemini-4-hydrogen-high`), `carbon` (`gemini-4-carbon-high`) | `gemini-flash` |
| `grok` | `grok` | `grok` (`grok-4.7`) | `grok` |
| `copilot` | `copilot` | `copilot` (`claude-sonnet-5`) | `copilot` |
| `muse` | `muse` | `muse-spark` (`muse-spark-1.3`), `muse-wonder` (`muse-wonder-1.0`), `muse-wonder-full` (`muse-wonder-full-1.0`) | `muse-spark` |

`opus`, `sonnet` and `haiku` are Claude CLI aliases, so they follow the installed CLI's current release. `fable` is pinned to the Fable 5.2 model id, because the CLI's `fable` alias can trail the newest Fable model (CLI 2.1.319 resolves it to `claude-fable-5-1`).

**Reasoning effort.** Choose capability with the model, then tune `effort` within it.

| Provider | Accepted `effort` | Rendered as |
|---|---|---|
| `claude`, `codex`, `muse` | `low`, `medium`, `high`, `xhigh`, `max` | `--effort` · `model_reasoning_effort` · `--reasoning-effort` |
| `antigravity` | `low`, `medium`, `high` | `--effort` |
| `grok` (`grok-4.7`, `grok-4.6`) | `low`, `medium`, `high`, `xhigh` | `--reasoning-effort` |
| `grok` (`grok-4.5`) | `low`, `medium`, `high` | `--reasoning-effort` |
| others | none | — |

An unsupported `effort` (`max` on Grok, `effort = "hard"`) is ignored for that crew, never remapped, with a warning naming the file, crew, value and accepted values. `orbit doctor` lists ignored properties; `orbit config set` refuses a value load would drop. A selected crew's provider, model and effort each override an activity's inline baseline.

**Crew references.** Wherever a crew is named — the complexity, type, purpose, review and final-recovery pools, `workflow.default_crew`, a task or auto-task `crew`, `--crew` and MCP `crew` inputs — it can carry an effort: `<crew>@<effort>`, followed in pools by the usual `:<weight>` (`hard_complexity_crews = ["opus@medium:100", "opus:100", "sol:100"]`). The reference runs that crew at that effort, ahead of the crew's own `effort` and the activity's inline baseline; without `@effort` the crew's own `effort` applies, as before. `opus@medium` and `opus` are distinct pool members with their own weights, and runs, scorecards and experiments record them under those names. They share the crew's provider account, holds and limits: a [provider failure hold](crew-recovery.md#provider-failure-holds) or pull-drain exclusion on either one covers the whole family. Unlike a crew table's `effort`, a reference effort the provider or model does not accept (`grok@max`, `luna@hard`) is refused at load and by `orbit config set`, naming the reference and the accepted values. Crew names may therefore contain neither `:` nor `@`.

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
`orbit run show --json` and `orbit.run` action `show` expose `agent_executions`
on the run and its displayed steps, including retries and interrupted launches.
The dashboard shows the same provenance on run detail and step rows.

Scorecard rows are one per model and recorded effort. Each row's
`effort_breakdown` lists completed invocations for that model and its
contributing crews at every recorded effort: counts, linked tasks, duration,
tokens and provider-reported cost. Those counts are not the row's scored
units, which count each unit once under its main invocation's effort. The
dashboard and `orbit agents show` label the list that way and name each
crew. The top-level scorecard `effort_breakdown` covers completed workspace
invocations in the selected window (`effort_population` names that population).
Experiment arms include the completed invocations
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
| [`orbit run auto --auto-fix`](crew-recovery.md#pr-auto-fix-repairs) repairs (failed required checks and merge conflicts on the drain's PRs) | `low` | The repair is bounded to one named blocker on a candidate that already passed its gates. |

A pilot, recovery or auto-fix activity names its purpose as `crew_config_key: workflow.purpose_crews.<purpose>` (an older asset may still name `workflow.<tier>_complexity_crews`, which draws that pool directly). The run draws once per pool on its first such dispatch: the pool's weights apply, disabled crews and those outside the run's `allowed_crews` are skipped, and crews at a [provider usage limit](#provider-usage-limits) are skipped while another member is left. Every later dispatch of that pool in the run, and a resume, reuses the draw, which the run's state records with the eligible members. An empty or wholly disabled pool falls back to `workflow.default_crew`, as an unassigned task does, and the recorded draw says so. The task pilot and escalation triage inspect pinned source with a CLI's own native file-read and command tools, so their draws keep only crews whose provider has a CLI runtime and whose executor declares [`inspection_args`](providers.md#executor-inspection-tools-inspection_args). A pool with no such member passes to the next tier up, then to `workflow.default_crew`; when that cannot inspect either, the activity fails with `inspection_crew_unavailable` before a provider launches. The recorded draw names the filter and the crews it skipped. Trust in an inspection still comes from its result: each result reports the `HEAD` and tree it observed in its checkout, and the apply step (or, for triage, `orbit.task.block_triage`) refuses a result whose proof is missing or differs from the pinned source.

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
| Final recovery | `workflow.purpose_crews.final_recovery` | the [final-recovery](crew-recovery.md#final-recovery-pool) activity | its own default; there is no other route |
| PR auto-fix | `workflow.purpose_crews.auto_fix` | [`--auto-fix`](crew-recovery.md#pr-auto-fix-repairs) repairs | `workflow.low_complexity_crews` |
| Automatic review | `workflow.purpose_crews.review` | [automatic review](review.md#review-and-operation--automatic-review): after-landing review tasks, and the reviewer before landing while the next row is unset | the review's existing rules |
| Pre-landing review | `workflow.purpose_crews.pre_landing_review` | before-PR and before-landing review | `workflow.purpose_crews.review` |
| Red-base repair | `workflow.purpose_crews.baseline_red_repair` | tasks the red-base re-check files (`baseline-red-repair` tag) | the task's matching [type pool](#type-pools-by-task-type-and-complexity), when it has one, then its complexity pool, then `default_crew` |
| Sweep tasks | `workflow.purpose_crews.sweep_tasks` | tasks the CI failure sweep files (`ci-failure-sweep` tag), the Dependabot and code-scanning sweeps file, and merged-head repairs a review reconciliation files (its follow-up tag) | the same |
| Pilot-warning triage | `workflow.purpose_crews.pilot_warning_triage` | the triager of each held task-pilot assessment (`escalation_triage_pipeline`, subject `pilot_warning`) | `workflow.hard_complexity_crews` |

A triager inspects pinned source natively, so only members whose executor declares [`inspection_args`](providers.md#executor-inspection-tools-inspection_args) are drawn, by weight and seeded by task, and for a pilot warning the crew that wrote the held assessment is passed over while the pool offers another. A pool with no such member keeps the hard-tier route (the next tier up, then `workflow.default_crew`, as above). The recorded draw names its source as `purpose:pilot_warning_triage` (`pool:<tier>` or `default` on the hard-tier route) in each prepared task row (`crew_source`) and partition (`draws`).

Final recovery keeps its own rules (no fallback; `[]` turns it off), and automatic review keeps its own (required for a review layer before landing; it avoids the crew that implemented the reviewed work).

**Deprecated pool keys.** `workflow.final_recovery_crews` and `operation.review_crew` load into `final_recovery` and `review`, and the `ci_failure_sweep`, `dependabot` and `merged_head_repair` pools load into `sweep_tasks`, each with a deprecation warning. A file that also sets the replacement keeps the replacement. A file whose old sweep pools name different pools fails to load until you merge them into `sweep_tasks`, because every sweep-filed task now draws from one pool. Tasks and runs recorded under the old purposes keep resolving: a `crew_source` of `purpose:ci_failure_sweep`, `purpose:dependabot` or `purpose:merged_head_repair`, and a `crew-purpose:` tag naming one, read as `sweep_tasks`. The final-recovery activity's `crew_config_key` binding keeps the old `workflow.final_recovery_crews` name, so the draws earlier runs froze under it resume.

**Resolution order.** The first rule that applies wins:

1. **Explicit crew.** A `crew` on the task, the auto-task template or the activity input is used as it is and skips the purpose. An activity's explicit `crew` also beats its `crew_config_key`.
2. **Purpose pool.** The purpose's pool, drawn by weight once per run for an activity and at creation for a task. Disabled crews are skipped, and `--allow-crew` narrows the draw (a pool with no permitted member fails as a complexity pool does). For activity draws, [provider usage limits](#provider-usage-limits) and [failure holds](crew-recovery.md#provider-failure-holds) skip crews while another member is left. At task creation, limited members are skipped; if none remain, the task stays unassigned for admission to retry. The task pilot keeps only members that [inspect natively](#system-work-draws-from-the-complexity-pools).
3. **Earlier route.** A purpose whose pool is unset, `[]`, wholly disabled, or (for the pilot) has no inspection-capable member resolves to the right-hand column above. Only the tasks the red-base re-check and the sweeps file (`baseline_red_repair`, `sweep_tasks`) consult a matching [type pool](#type-pools-by-task-type-and-complexity) on that route, before their complexity pool. The activity purposes `task_pilot`, `failure_recovery` and `auto_fix` go straight to their complexity pool.

**A task's purpose** comes from its tags: a `crew-purpose:<purpose>` tag naming one of the `workflow.purpose_crews` purposes, else the tag the system filed it with. An auto-task template names a purpose for the tasks it mints by adding a `crew-purpose:<purpose>` tag. A task drawn from its purpose pool records `crew_source: purpose:<purpose>`; admission keeps that crew while it is still an enabled positive-weight member of the pool, and otherwise redraws as for a stale pool assignment. A [provider usage limit](#provider-usage-limits) or a [failure hold](crew-recovery.md#provider-failure-holds) that covers the assigned crew redraws from the pool's other enabled positive-weight members, keeping their weights and the run's allowlist, before the complexity pool. Admission writes the crew it runs back to the task (see [Admission write-back](#automatic-crew-pools-by-complexity)). A purpose task whose pool is unset, `[]` or wholly disabled falls back to its matching type pool, when one exists, then to its complexity pool. A pool whose members are all held or limited instead retries its other members, then the complexity pool, and never the type pool. A complexity change keeps a purpose-sourced crew, because the purpose does not depend on the tier.

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
- `default_crew` must name a defined crew. Left unset with crews defined, Orbit uses `opus`, else refuses to load.
- A crew name may not contain `:` or `@`, which the crew-reference grammar reserves ([repair](#repairing-a-config-that-already-names-a-crew-with-a-colon)).
- An Antigravity crew needs an `agy models` slug; a bare Gemini CLI ID such as `gemini-3.8-flash` fails with migration guidance.

**Retired crew shapes** fail load with migration guidance: `backend` (`cli` is ignored, `http` and `auto` refused, likewise in `ORBIT_BACKEND` and `[runtime] backend`; remove it).

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
- **Never piloted.** The task pilot excludes the task (`human_crew`), refuses an explicit request (`task_pilot_human_crew`), and records no assessment, warning or hold on it. A task-pilot run that names one still starts: that crew is not the run's agent crew, prepare leaves the task out, and the run keeps an agent crew. An approve-proposed drain does not select the task.
- **Closed by the operator.** An agent-attributed `orbit.task.update` (one that passes `model`) cannot move a human-crew task to `done`. The operator's own update or the dashboard can, and without the lifecycle gates: `orbit task update <id> --status <status>` or the dashboard's status control (which offers every status for such a task) moves a human-crew task from any status to any other, for example `proposed` to `done`, with no plan, review, completion evidence or `--force`. A note stays optional. Each move is recorded as a `forced` status entry with the actor, the from and to statuses and a note naming the human crew (`human-crew bypass`). Only the operator's CLI and dashboard get this; `orbit.task.update` and MCP keep the normal lifecycle on the same task, so assigning a human crew never lets an agent skip review. A non-terminal local run that owns the task still refuses the move, and leaving `done` or `archived` still needs `--force`.
- **Out of agent analytics.** Crew analytics, performance rankings and provider usage leave human crews out.
- **Listed.** `orbit config show` lists a human crew with provider `human`, and `orbit config get crews.<name>.kind` answers `human`. For an agent crew, `kind` is absent. The dashboard tags each human-crew task `for <name>`, classifying it by the task's own workspace: task reads and write responses carry `human_crew` (`{name, description}`) for a task assigned to a human crew, so the tag, the operator status control and the missing Ship control hold in the All workspaces view too, even when another workspace configures the same name as an agent crew. The **Waiting on you** chip, under **More** on the Tasks page, counts open human-crew tasks and tasks with open decisions, never a `done`, `archived`, `rejected` or cancelled one. The **My work** chip beside it (`#tasks?crew=mine`) keeps the open tasks of the viewer's human crew: the only human crew, or else the one matching the OS user that runs `orbit web serve`.
- **Seeded by `orbit init`.** When the global config has no human crew, `orbit init` adds one named after the OS user, normalized to lowercase letters, digits, `_` and `-`. At a terminal it prompts with the OS user as the default; without a terminal, or with `--non-interactive`, it takes the OS user as-is. `--human-name <NAME>` sets the name. A name an agent crew already holds is asked again at a terminal and refused otherwise, before init writes anything. A re-run leaves an existing human crew unchanged.

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

- **`NAME`** passes `NAME` from the launching process to this crew's agents only, whether or not [`pass`](execution.md#executionenv--the-agent-subprocess-environment) lists it.
- **`NAME=SOURCE`** gives this crew's agents `NAME` with the value of `SOURCE` from the launching process, or from `~/.orbit/clock.env` for [clock-started runs](execution.md#executionenv--the-agent-subprocess-environment). Keep the source under a name no CLI reads (`ANTHROPIC_CREDITS_API_KEY_1`), so interactive shells and other crews are unaffected and different crews can use different keys. The agent never receives `SOURCE` itself.
- **`NAME=file:PATH`** gives this crew's agents `NAME` with the contents of the file at `PATH`, with the trailing newline trimmed. The host opens the file itself each time it starts an agent, outside the agent sandbox, and sets only `NAME` in the agent's environment, so no login shell, service unit or `~/.orbit/clock.env` has to export anything: one config line works for CLI, dashboard and clock-started drains alike. `PATH` must be absolute or start with `~/` (the launchers have different working directories). The `file:` prefix cannot be mistaken for a variable name, which never contains `:`.
- **Names and paths only, never values.** `NAME` and a variable `SOURCE` must be variable names (letters, digits and `_`, not starting with a digit), and neither may be an `ORBIT_*` name. A malformed entry (a `file:` path that is empty, relative or ends in `/` included), or two entries for one `NAME`, fails load naming the crew and entry.
- **A file source is held to the `clock.env` rules.** The file must be a regular file owned by the user running Orbit, mode `600` or stricter, and not a symlink (a symlinked parent directory is fine). A missing, empty, symlinked, foreign-owned or group- or world-readable file is not read: the crew is unavailable exactly like a crew whose source variable is unset (the drain's window leaves it out and its activities are refused), and the warning names the crew, the variable, the path and the file's state (`does not exist`, `refusing mode 644 … run chmod 600`), never the value.
- **Global config only.** A workspace `config.toml` that sets `crews.<name>.env` fails load naming the crew, and `orbit config set` refuses it without `--global`, so a checkout cannot choose which host secrets its agents read. Set it with `orbit config set --global crews.opus-credits.env '["ANTHROPIC_API_KEY=ANTHROPIC_CREDITS_API_KEY_1"]'`.
- **The crew's entry wins.** It replaces the same name from `pass`. When it sets a Claude credential (`ANTHROPIC_API_KEY` or `CLAUDE_CODE_OAUTH_TOKEN`), the other one is removed from that crew's agents, so exactly the credential the crew chose is present. Other crews keep the shared `pass` environment.
- **Services have their own environment.** Dashboard-started drains inherit `orbit-web.service` (or the web LaunchAgent), and clock-started drains inherit `orbit-sweep.service` / `com.orbit.sweep` plus `~/.orbit/clock.env`. A variable exported by a login shell does not reach an already running service. On Linux, put sources in an owner-only environment file and reference it with `EnvironmentFile=/absolute/path/to/crew.env` in a `[Service]` drop-in created with `systemctl --user edit orbit-web.service`; run `systemctl --user daemon-reload`, restart the service, and start a new drain. Configure the sweep unit similarly if needed; clock runs can instead read the sources from owner-only `~/.orbit/clock.env`. On macOS, provide the source in the LaunchAgent's `EnvironmentVariables` and reload it, or use `~/.orbit/clock.env` for clock runs. A `file:` source needs none of this: the host reads it directly in every launcher. Keep values out of config and command arguments. `orbit config set crews.<crew>.env` and `orbit doctor` warn separately about missing shell sources and installed managed services (a `file:` source is reported by its file's state instead); a stopped or unreadable service is reported as unverified, never assumed to inherit the shell. Linux checks the running unit's process environment; macOS checks launchd's service environment. Clock sources in `~/.orbit/clock.env` are checked separately and never assumed to supply the web service.
- **An unset source excludes the crew.** A drain or ship start (`orbit run auto`, `--pull` included, `orbit run ship`, MCP and the dashboard) reports each crew whose source is unset or empty in its `warning`, and `orbit run job` warns on stderr; each names the crew, source and variable, and records them on the run (`Unset crew env:` in `orbit run show`, `run.crew_env_unset` in JSON, the dashboard run detail). The drain's crew window leaves the crew out (a pull drain's preflight lists it as excluded), and an activity dispatched as that crew anyway, such as an explicit `--input crew=`, is refused before its provider starts rather than run on the provider's default login. `orbit doctor`'s `env-pass` row reports it.
- **Readiness names the cause.** Tasks excluded for missing sources show `crew_env_unset` with the crew and source names. With a live drain, readiness uses that drain's recorded missing sources; otherwise it checks the current launching environment. A positive-weight complexity/type pool crew excluded on every observed admission pass for at least two minutes (and two passes) produces a run-level warning in `orbit run show` and readiness and, on an owner drain, a deduplicated automation friction (a structured `crew_pool_excluded` warning signal on a follower). An exclusion by a provider usage window with a known reset time keeps the run-level warning but files no friction, since the window lifts at its reset by itself. So does a crew that an operator's `--allow-crew` window intentionally leaves out; a crew that is also unset, or excluded for another reason, still files its friction. The streak resets when the crew is available again or leaves the pool; reading readiness alone never advances it.
- **Everywhere the agent environment goes.** The overlay applies to implementer, reviewer and recovery agents and to a pull drain's [authentication probe](providers.md#executor-authentication-recovery-probes) for that crew. A detached worker receives each source variable by name (name-only `systemd-run --setenv` under [containment](machine.md#worker-containment)) and composes the crew's environment itself, reading a `file:` source from disk as the same user.
- **Its own account.** A crew that sets `env` is a provider account of its own; every other crew of the provider shares the provider's default login. [Usage-limit readings](#provider-usage-limits), [provider failure holds](crew-recovery.md#provider-failure-holds), host holds of the task-pilot routine, pull-drain [authentication exclusions and probes](providers.md#executor-authentication-recovery-probes), `--allow-crew` and `provider_limit_budgets` / `provider_limit_overrides` key on the provider and that crew for such a crew, and on the provider alone for the rest, which behave as before. The subscription reaching its 5-hour limit does not stop `opus-credits`, and an expired or invalid key on `opus-credits` neither holds nor excludes the subscription's crews. Surfaces name such an account `provider/crew` (`claude/opus-credits`). Records written before accounts were named carry none, and read as the provider's default login's.

`orbit config show` and `config get crews.<name>.env` print the entries as written, and the dashboard shows them under the crew's name; none of them, `orbit doctor`, run records or logs ever print a value.

### Repairing a config that already names a crew with a colon

Every command, `orbit config set` included, refuses to load, so edit the file by hand. Layers are checked before merging, so the error names the defining file:

```
error: invalid input: [crews]: crew name 'gpt-5:codex' must not contain ':' or '@'; ...
Edit '~/.orbit/config.toml' and rename or remove the [crews."gpt-5:codex"] table, then rerun the command
```

Rename the table (or delete it), update every reference (`default_crew`, pool entries, a task's `crew`), and rerun.

---

## Per-task crew override

`workflow.default_crew` is only the fallback. Each task has an optional `crew`, and `orbit run ship` resolves each task by the [resolution precedence](providers.md#resolution-precedence), so one ship can mix crews; each child run records its crew (`resolved_crew` in `orbit run show`). A single child pipeline whose tasks name different crews, or mix set and unset crews, fails rather than falling back to the default.

### Automatic crew pools by complexity

The pilot's `recommended_crew` is optional and advisory; it may be omitted or
null when workspace configuration is unavailable in the inspection checkout.
Orbit selects the implementation crew from the assessed complexity pool and
preserves explicit crew assignments. The task-pilot purpose pool selects the
agent doing the assessment, separately from the task's implementation crew.

```toml
[workflow]
low_complexity_crews = ["luna"]
medium_complexity_crews = ["grok", "sol"]
hard_complexity_crews = ["astra"]
xhard_complexity_crews = ["fable", "astra"]
```

- **Selected at creation.** A task created without `crew` (`orbit task add`, `orbit.task.add`, an auto-task mint with no template crew, an import) draws from its complexity's pool, else `default_crew`, storing `task.crew` and its provenance `task.crew_source`; a `crew_assigned` history entry records the source (`explicit`, `pool:<complexity>`, `type:<type>/<tier>`, `purpose:<purpose>` or `default`). A task with a [purpose](#purpose-crew-pools) whose pool is configured draws from that pool instead of its complexity's, and a task whose type has a [type pool](#type-pools-by-task-type-and-complexity) for its tier draws from that. That entry is a second write after the insert; if it fails, the task is still created and returned, and the failure is logged. With no crews configured the field stays unset.
- **Provider limits at creation.** Creation, crew clearing and complexity re-rates skip pool members covered by this host's live [provider usage limits](#provider-usage-limits), before image capability filtering and weighting. If every positive-weight member is limited, the task stays unassigned for admission to retry; it does not fall back to another pool or the default crew. Once the reading resets, that member can be drawn again. An explicitly named crew keeps its assignment; admission applies the configured explicit-crew limit policy.
- **Complexity changes before admission redraw automatic crews.** When the pilot or `task update --complexity` changes the tier of unreserved proposed, backlog or someday work, a crew sourced from another pool or the `default` fallback is redrawn in the same mutation, logged as `crew_redrawn` with previous and new source and crew. Explicit crews stay pinned, even when an update names the crew a pool already chose; status transitions keep the selection. `task update --crew <name>` pins; [`--crew ""`](#setting-taskcrew) redraws. Every stored change records the actor, prior and new crew, and whether it was named or drawn. Each `crew_assigned` and `crew_redrawn` history entry also carries a typed `decision` (`kind: crew_assignment`) with the crew and `crew_source` before and after the change; Orbit reads entries recorded before that field from their note.
- **Tiers:** `low`, `medium`, `hard`, `xhard`. Unset or `unassessed` complexity uses the default chain. The task pilot never demotes a task out of `xhard`. An `unassessed` recommendation for a task that already has an assessed tier keeps that tier and its pool, and holds the task on its unresolved footprint until the pilot reassesses it.
- **Empty pool** (`[]`, the init scaffold) means no pool: the task gets `default_crew`, as it does when all members are [disabled](#disabled-crews). Blank entries and unknown crew names fail before dispatch.
- **Preferences, not allowlists.** An explicit `task.crew`, an explicit run crew, and review and preparation jobs keep the crew they name, except that a standing [provider failure hold](crew-recovery.md#provider-failure-holds) redirects a task-crew draw away from the crews it excludes, and a [provider usage limit](#provider-usage-limits) removes limited crews from the draw.
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

`workflow.type_complexity_crews` routes crews by task kind. A task's type is the one [`workflow.task_types`](workflow.md#task-types) gives it from its current tags and context files, the same lens the implementer scorecard measures by. When `workflow.type_complexity_crews.<type>.<tier>` is set for that type and the task's complexity, the pool replaces `workflow.<tier>_complexity_crews` for that draw; any other task, `other` included, draws from its complexity pool exactly as before.

- **Entries** are written like a complexity pool's (`crew` or `crew:weight`), but every entry must name a defined, enabled crew with a positive weight, and a pool must not be empty: remove the key to fall back to the complexity pool. The type must be a `workflow.task_types` name and the tier one of `low`, `medium`, `hard` or `xhard`. Each refusal is a load error naming the full key.
- **Layering.** Each `<type>.<tier>` pool is one setting: a workspace pool replaces the same type and tier from global, and the global's other pools still apply.
- **Precedence.** An explicit crew, then a [purpose pool](#purpose-crew-pools), then the type pool, then the complexity pool, then `default_crew`. A type pool whose members are all excluded by executor `host_os` or the task's `os:` tags falls through to the complexity pool. OS filtering and [usage-pressure](#usage-pressure-draws) weights apply to type-pool members as to complexity-pool members.
- **Provenance.** A type-pool draw records `crew_source: type:<type>/<tier>` (such as `type:ui/hard`), and its `crew_assigned` or `crew_redrawn` history names that source. `orbit task show` prints the source. Admission keeps that `type:` source in `crew_selection.source` in `orbit run show` while the task keeps its assigned member; a draw from the whole type pool records the pool's key, `workflow.type_complexity_crews.<type>.<tier>`, with `; usage pressure` appended when usage pressure weighs it. A provider hold or usage limit adds its own note to the source it applies to (`; provider hold excludes <crews> until <time>` or `; provider limit: <detail>`), ahead of any pressure suffix.
- **Where it draws.** Creation, `task update --crew ""`, a complexity re-rate before admission and fresh admission. A tag or context-file edit that moves an unreserved proposed, backlog or someday task with a pool, type or default source to another type pool redraws it in the same update, as a re-rate does. Implicit edits preserve the crew and source of admitted deliveries, including stopped runs and preserved candidates, in-progress or review work, and work reserved by a non-terminal delivery run or protecting execution claim, even before setup binds the run to the task. Scope metadata still changes under its existing authority rules; it never rewrites recorded run crew/model facts or emits a redraw for a crew that did not run. Local admission serializes selection, run insertion and crew writeback with task edits. An authorized recovery or requeue permits a genuinely fresh admission to evaluate the updated classification and record its selected crew; a candidate resume keeps its original admission provenance. Explicit audited crew corrections retain their existing authority rules.
- **Admission** keeps a `type:` crew while the task still classifies to that type and tier and the crew is an enabled, positive-weight member of the pool. Otherwise the task draws from the pool it now routes to. A [provider usage limit](#provider-usage-limits) or [failure hold](crew-recovery.md#provider-failure-holds) covering the crew redraws from the pool's other members, then the complexity pool, then `default_crew`. A claimed leaf on a follower keeps the owner's type-pool crew unless the follower has a verified Muse model refusal for that crew. The follower then draws from the type and tier named in the snapshot's crew source.
- **Seeing it.** `orbit config show` and `orbit config keys` list the table, and Settings › Crews names each `workflow.type_complexity_crews.<type>.<tier>` pool a crew is in under **Used by**.

**Allowlists.** With `--allow-crew`, the draw renormalizes over permitted members, keeping their ratios. A pool with no permitted positive-weight member is disjoint, and `orbit run readiness --allow-crew <crew>` reports `crew_not_allowed`. Without `--allow-crew`, readiness (and the dashboard Drain card) applies the live drain's own set and reports it as `capacity.allowed_crews`. When a previously drawn purpose, type, or complexity pool crew is excluded, admission redraws from permitted members of that same pool and records the actual crew on the task and run. An explicit or legacy pinned crew still waits; a pool with no permitted member also waits. The allowlist still applies to system and review activities at dispatch.

**Frozen per run.** The admitting run captures the effective pools in run input `auto_crew_pools`, inherited by descendants. Each admitted leaf records `crew` and `crew_selection` (task ID, complexity, source, and the eligible `[{name, weight}]` after renormalization, with `pressure` and `effective_weight` under [usage-pressure draws](#usage-pressure-draws); a run admitted before [ORB-22804] may also carry a `performance` factor per member), shown as `Crew Selection:` in `orbit run show`. Retries and resumes keep it.

### Setting `task.crew`

| Surface | How |
|---|---|
| Dashboard | The crew dropdown on each task card. `default: <crew>` means the task has no `crew` and inherits `default_crew`. |
| CLI | `orbit task add --crew <name>`, or `orbit task update <id> --crew <name>`. `--crew ""` on `update` redraws for the current complexity. |
| MCP | `crew` on `orbit.task.add` / `orbit.task.update`. An empty string on update redraws; `null` is rejected. Omitting `crew` keeps an explicit pin; a complexity change can redraw a pool assignment or default fallback. |

### What "ran" vs what "was selected"

`orbit.task.show` returns `crew` (the stored selection) and `crew_source` (provenance, absent on legacy records), plus `resolved_crew` and `crew_model` when this host can resolve it; an associated run's persisted resolution takes precedence over current config. A `fields: ["crew"]` projection returns the stored selection. `task.crew` is validated on write. Deleting an explicitly pinned crew makes `orbit run ship` fail at run start, before any agent dispatches. Pool assignments are revalidated at admission, so keep pools valid when deleting members.

---

## Provider usage limits

Limits are read and applied per provider account on this host: the provider's default login, which every crew of it without [`env`](#per-crew-credentials-env) shares, and each crew that sets `env`, which is an account of its own.

A hold learns of a limit only after a run fails on it. Delivery admission also reads this host's [limit record](crew-recovery.md#provider-failure-holds) before dispatch and stops routing to a crew whose provider account, as logged in here, is at or near a window's limit.

```toml
[workflow]
provider_limit_overrides = ["claude:80", "grok:100", "codex:off"]
provider_limit_budgets = ["grok:30usd/5h"]  # none by default
provider_limit_explicit_crews = "wait"      # or "pool"
```

**Limited crews.** A crew is limited while a live, gating reading of its provider account is exhausted or at or above that account's threshold (its `provider_limit_overrides` entry, else the fixed 90%). A reading that names a model (Claude's `seven_day_opus`) limits only crews whose model contains it, case-insensitively; any other reading limits every crew of the account (aliases included). A reading belongs to the account it was read from: one taken under the default login, such as Claude's `five_hour` and `seven_day` subscription windows, never limits a crew that sets `env`, and one taken by such a crew limits only it. A crew that sets `env` and has no reading is gated only by its own budget, if it has one. Its threshold is its own override (`opus-credits:95` or `opus-credits:off`), else its provider's, else 90%. An override naming a crew that is not defined, or that sets no `env`, fails load naming it. At `100` only an exhausted window gates (99% gates nothing). An overage window (`gating` off) never gates.

**Gate off.** `provider:off` (for example `codex:off`) turns that provider's pre-dispatch gate off. Every live reading is still recorded and shown, with `threshold` `off` and `gated` false. No reading of that provider — an event reading, a usage-limit failure observation, or a ledger budget reading — limits a crew at admission, in a drain or ship draw, or in the pull window. `orbit doctor`'s `provider-limits:<provider>` row says gating is off, and the dashboard marks the reading the same way. A run that then fails on a usage limit still records the task's provider failure hold: `off` disables only the pre-dispatch gate. Removing the entry gates the same readings again at 90%. `off` is not a percent, so it does not change [`provider_limit_budgets`](#budgets-for-providers-that-report-no-usage) arithmetic.

**Lapse.** A reading counts until its `resets_at`. Without one it counts for its window's length (60 minutes when that is unknown), or, for an exhausted error reading, for the first usage-limit backoff, 30 minutes. The crew is eligible again at the next admission pass, with no provider probe.

**Gated draws.** The pre-dispatch provider-limit gate applies only to delivery admission (the crew draw of drains and `orbit run ship`, and the pull window); system, review, final-recovery and task-pilot crews are not subject to that gate. Ordinary complexity/purpose activity draws (including the task-pilot, step-failure and PR-conflict-recovery complexity-pool draws) and review draws still skip provider-limit candidates before weighting when another eligible member remains. This draw-level narrowing never waits or redirects the way the delivery gate does. Final-recovery and escalation-triage draws do not narrow by provider threshold. Those complexity-pool recovery draws also skip crews under a host provider failure hold for unavailability or capacity, and that hold alone can withhold the task-pilot routine ([Routines](crew-recovery.md#provider-failure-holds) above).

- **Purpose tasks** (`crew_source` `purpose:<purpose>`): a limited assigned crew is redrawn from the unlimited enabled positive-weight members of that purpose pool. Their weights and the run's allowlist are kept, and admission writes the crew it runs back to the task as `redraw:provider_limit`. With none left, the draw continues as a pool task does.
- **Pool and default tasks** (`crew_source` `pool:<tier>`, `type:<type>/<tier>` or `default`, or no crew): limited members are removed and the rest renormalized, as with `--allow-crew`. A limited assigned crew is redrawn from the unlimited members of the current complexity pool; a `type:` crew is redrawn first from its type pool's other unlimited members, as [type pools](#type-pools-by-task-type-and-complexity) describes. Admission writes a changed crew back to the task as `redraw:provider_limit`, except that a weighted [usage-pressure re-draw](#usage-pressure-draws) keeps the task's `crew_source`.
- **No default fallback.** With every member limited the task waits; filtering never falls through to `workflow.default_crew`, usually the most expensive crew and often on the same provider.
- **Explicit crews** (`crew_source` `explicit`, or a legacy pin): `wait` (default) keeps the task in the backlog; `pool` draws from the unlimited members of its complexity pool.
- **Explicit run crews** (`--input crew=<name>` on `orbit run job`) are the operator's decision and not gated; `crew_selection.provider_limit` records the limit.

A redrawn task's `crew_selection.source` names the limit. A task left with no unlimited crew waits with `reason: "provider_limit"` in `orbit run readiness --json` and the detail `<provider> <window> at <used>% (limit <threshold>%) until <reset>; crews <list> skipped`, lifting by itself after the reset. An `orbit run ship` naming such a task still runs it on its usual draw. A standing [provider failure hold](crew-recovery.md#provider-failure-holds) that leaves no crew reports `provider_backoff` first. Operator steps: [stuck job runs](../runbooks/stuck-job-runs.md#a-task-waiting-on-a-provider-usage-limit).

**Pull drains.** Each pass of a follower's pull drain re-reads its own store and excludes each limited crew from its crew window with source `provider_limit` and an `until` time; unlike other exclusions, it lifts at `until` within the same drain. The owner never hands the follower a task on that crew meanwhile. The owner's own provider readings apply only to owner-local work; neither the owner's admission answer nor a settlement supplies readings to the follower's window. Each provider-limit exclusion names the host whose local store supplied it. The owner's before-PR reviewer is not gated.

**Surfaces** all share admission's view of live readings (liveness, threshold, crew rules), so none shows a crew runnable while admission skips it.

- `orbit run readiness`: a `Provider limits:` line per gated reading (`claude five_hour 93% >= 90% until 15:00Z: opus, sonnet skipped`, `claude/opus-credits 30d budget …` for a crew account; a reset on another UTC day includes its date) and each waiting task's detail. `--json` has a top-level `provider_limits` array of every live reading: `provider`, `account` (present only for a crew account's reading), `scope` (the model a reading names, else `null`), `window`, `used_percent`, `exhausted`, `resets_at`, `source`, `observed_at`, `age_seconds` (never negative), `stale` (true when `observed_at` is an earlier UTC date than the snapshot), `gating`, `partial`, `threshold` (a percent, or the string `off` when that provider's gate is off), `gated` (false for every reading of a provider set to `off`), `until` and `crews` (enabled crews covered). The additive `provider_pressures` array lists every enabled crew account (`provider`, optional `account`, `reports_usage`, `threshold`, `gated`, `crews`, `pressure`, `pressure_window`, and `shares_default_login` when that env account shares another enabled crew's credential and no crew of the provider uses the default login); `crew_pressures` lists every enabled `crew`, `pressure` and `pressure_window`. Text shows `Provider pressure:` and `Crew pressure:` lines. `pressure_window` names the binding `window`, model `scope`, `used_percent` and `resets_at`, or is `null` for neutral unknown usage. `provider_auth_failures` lists each host and provider with a `provider_unavailable` authentication failure in the last 7 days (at most 200 recent failures and 200 recent successes) that no successful run of that provider on that host, started after the failure, has cleared. Each item is `host`, `provider`, `account` (present only for a crew that sets `env`, which is grouped and cleared on its own), `runs` (`run_id`, `at`, oldest first) and `fix_command` (the executor's auth-probe hint, else the provider's re-login command; for a crew account, the variables its `env` passes, by name). Crew attribution uses the same recorded draws and verifier partitions as the routine's [provider failure hold](crew-recovery.md#provider-failure-holds), so a successful pilot or recovery on another provider cannot clear the card. A failure or success outside that window is not part of the projection.
- `orbit run show <pull-drain>`: `excluded <crew> (provider_limit until <time>): <reading>; reading from host <machine-id>` (`unregistered` when the runtime has no machine identity).
- `orbit doctor`: one `provider-limits:<provider>` row per provider an enabled crew without `env` uses, and one `provider-limits:<provider>/<crew>` row per enabled crew that sets `env` (a provider with no usage windows and no budget, which is every provider but Codex, Claude, Muse and Grok, shows `info`; a budgeted one shows its `ledger` reading, such as `grok 5h budget 67%`; a provider set to `off` says gating is off and lists its readings), plus a `provider-limits:workflow.purpose_crews.review` warning when that ungated lane uses a gated provider (and a `provider-limits:workflow.purpose_crews.pre_landing_review` warning for that pool while it is set). Each account row appends its raw weekly pressure and binding window/reset (or neutral unknown usage). Row semantics: [health checks](../runbooks/health-checks.md#run-orbit-doctor). `orbit doctor providers` shows the same per executor (`LIMITS` column, `provider_limits` in `--json`).
- Dashboard: the provider strip shows one chip per provider login. A crew that sets its own credential is its own chip. Crews that set the same credential, when none of that provider's crews uses the default login, are marked `shares_default_login` and merged into the provider's chip; that chip's pressure is the default login's weekly pressure, including weekly readings stored on those crews, never another login's. A login with no reading stays its own chip, reading `no usage reported`. The strip shows that pressure beside the readings. Open a provider chip to see its dialog, which lists that login's usage windows and resets, including the binding window. When a reset is available, its relative time appears in the window row and its absolute time in the reset label's tooltip. On narrow bars, open the summary chip to see a summary dialog listing every login with its worst window. The Settings › Crews Limit column (tightest window, used percent, reset, `gated` badge, or `gating off` when the provider's gate is off); the Drain card names gated readings, lists every live reading with its age (`today` on the observation's UTC date, otherwise how long ago) and a stale marker when `stale` is set (a reading whose gate is off is marked `gating off`), lists each standing `provider_auth_failures` item with its host, provider, run links and fix command, and lists tasks waiting with `provider_limit`.

**Weekly usage pressure** is shown raw on these surfaces; it changes draws only under [`workflow.crew_selection = "usage_pressure"`](#usage-pressure-draws). For live provider-reported gating windows of exactly 10080 minutes, pressure is `max(0, 1 - used_percent / 100) / clamp(time_to_reset / 7 days, ε, 1)`, shown uncapped (for example `claude ×25.2 (seven_day 10%, resets 18:00Z)` with six hours left). Five-hour and overage windows do not count. Account pressure uses account-wide weekly readings; when a provider has none that counts (Antigravity reports every window under a model), it takes the minimum across its model-scoped weekly readings instead. Crew pressure takes the minimum across account-wide weekly readings and weekly model windows covering its model. Crews with `env` use only their own account. With no valid weekly reading, a missing/past reset, an error reading or a ledger budget, pressure is neutral `1`. A neutral unknown does not constrain other valid weekly windows. Muse's `weekly` window is normalized to 10080 minutes when parsed.

### Usage-pressure draws

```toml
[workflow]
crew_selection = "usage_pressure"   # default "static"
```

With `workflow.crew_selection = "usage_pressure"`, crew pool draws spend weekly capacity that is about to reset before capacity that is not: a Claude week 10% used with six hours left (pressure about 25) wins over a fresh Codex week (pressure 1).

- **Effective weight.** Each pool member draws on `weight × clamp(pressure, 1/8, 8)`, where pressure is its crew's [weekly usage pressure](#provider-usage-limits) on this host. Tickets are whole numbers, ×1000 and rounded, so a weight-1 member at the `1/8` floor keeps 125 tickets; a positive weight never rounds to zero, and a weight of `0` stays undrawable. The multiplier follows each caller's existing membership filters. Task admission applies it before the run's `--allow-crew` list; other draws apply it after their existing filters. Provider-threshold narrowing depends on the caller, as described under [draws it covers](#usage-pressure-draws). A draw with a single candidate has nothing to weigh.
- **Exhaustion.** A member whose live reading is exhausted (flagged exhausted or at 100%, with its reset still ahead) holds zero tickets, not the floor, while another member that the draw can actually take still has quota. "Can actually take" means a positive weight that the run's `--allow-crew` list permits; a crew the list excludes is never that alternative, so the only allowed crew keeps its floor tickets when an excluded crew has quota. If no such alternative exists, the exhausted member keeps its weighted tickets and the pool draws as it would without exhaustion. A non-exhausted reading above the provider threshold is different from exhaustion: in a draw without threshold prefiltering, a reading at 95% with a 90% threshold keeps its pressure-weighted tickets and may still win. `provider_limit_overrides = ["<provider>:off"]` turns the provider's reading off, so it is never exhausted.
- **Draws it covers.** Task creation, `task update --crew ""` and the re-draw after a complexity change (each a fresh draw from the purpose, type or complexity pool, recorded under that pool's source), delivery admission, and a pull-drain follower's claimed leaf: a leaf whose snapshot crew came from a `pool:`, `purpose:` or `default` source is re-drawn inside that source's pool, as the follower captures it, while a `type:` crew stays the owner's (see the admission re-draw below), with the follower's own readings and under the follower's hold, limit and `--allow-crew` narrowing. The owner's readings never weigh a follower's run, and a follower never rewrites the owner's task; the leaf simply runs on the crew its own host drew. Ordinary complexity/purpose activity draws (including task pilots) and review draws narrow provider-limit candidates before weighting when another eligible member remains. The final-recovery activity and escalation-triage purpose/inspection fallback draws use the same multiplier without provider-threshold narrowing, so a non-exhausted crew at 95% with a 90% threshold can still win unless the exhaustion rule above removes it. The original shared-seam rollout also covered finding-verification pools and agent explore [ORB-17771]; finding-verification was retired with [ORB-22844], and the explorer path was removed with [ORB-20383].
- **Admission re-draw.** At admission, a task whose crew the system chose (`crew_source` `pool:<tier>`, `purpose:<purpose>`, `type:<type>/<tier>` or `default`) is drawn again from that source's whole pool instead of keeping its assigned member, under the same OS, hold, usage-limit and allowlist narrowing. For a claimed type-pool leaf, the follower keeps the owner's crew under usage pressure alone. If the follower has a verified refusal for that crew's model, it opens the named type pool and applies usage-pressure weights when configured. When the weighted draw picks another crew, the task record takes the new crew and keeps its `crew_source` unchanged, and its history gains a `crew_redrawn` entry naming both crews and their pressures (``crew redrawn from pool:hard (`sol`) to pool:hard (`opus`) at admission under usage pressure: `sol` ×1 (…), `opus` ×25.2 (…)``). An explicit crew (`crew_source` `explicit`, a legacy pin, or an explicit run-input `crew`) is never re-drawn.
- **Provenance.** A weighted admission's `crew_selection.source` ends in `; usage pressure`, and each `eligible_pool` member gains `pressure` (raw) and `effective_weight` (the weight it drew on) beside its configured `weight`. Activity draws freeze these fields in `activity_crew_draws`; admitted reviewers record them in `reviewer.eligible_pool`. Retries reuse frozen activity records, including their original weights. Singleton draws keep configured weights without pressure fields.
- **Pull offer order.** A follower under `usage_pressure` also reports each crew's weekly pressure in its pull capability (recomputed each pass; absent under `static`, and an absent report reads as neutral `1`). The owner orders that follower's offers by it only below band, priority, expiry and census reach/affinity: of tasks tied on all of those, the one whose pool contains the follower's highest-pressure crew is offered first. Pressure never overrides a higher priority or an expiring task, never gates admission, and owner-local dispatch, which has no pressure input, orders exactly as before.

Under `static`, or with the keys unset, every seeded draw uses its original configured weights and consumes the same random tickets; recorded members omit pressure fields. The pressure is this host's reading; a drain admitting on this host weighs its own readings.

### Budgets for providers that report no usage

Gemini doesn't report how close an account is to its limit; Orbit learns it only from the failure. Antigravity's structured `/usage` command and Grok's CLI billing API provide live windows when available; accounts or CLI versions without an available reading can use the same fallback. `provider_limit_budgets` declares the operator's allowance per rolling window so Orbit gates the provider before it runs out.

- **Format.** `provider:<amount><unit>/<window>`: a positive amount in `usd` or whole `tokens`, a window of `<n>h` or `<n>d` up to 31 days, each provider once (`xai` is stored as `grok`). No defaults. Load and `orbit config set` reject a malformed entry.
- **Crew accounts.** An entry may name a crew that sets [`env`](#per-crew-credentials-env) instead of a provider (`opus-credits:200usd/30d`): it reads only that crew's own spend and gates only that crew, and a provider's budget reads only its default login's spend, the invocations of its crews without `env`. A name that is a provider is the provider; an entry naming a crew that is not defined, or that sets no `env`, fails load naming it.
- **Reading** (source `ledger`, account-wide): this host's spend on the provider account in the trailing window ÷ the amount, gated at the account's threshold. `usd` sums each invocation's reported cost; `tokens` sums input plus output tokens from the invocation ledger. Its reset is the earliest moment enough old spend ages out to drop below the threshold (at `100`, below the budget). `provider:off` does not change this arithmetic: the reset stays on the fixed 90%, because `off` is not a percent. The reading is still shown, with `threshold` `off` and `gated` false, and it does not limit crews.
- **Partial.** A `usd` reading where some invocation recorded no cost is `partial` (`"partial": true`; surfaces note `partial: some invocations report no cost`), and its spend is a lower bound.
- **Precedence.** A live provider-reported reading or limit failure for the provider account wins; the ledger applies again when it lapses. The ledger reading is computed on each read, never stored.
- **Attribution.** An invocation counts for the provider it ran on, and for the crew account it ran under (the invocation ledger's `account`, shown with each invocation record) when its crew sets `env`. Older invocations are attributed by agent name, so an earlier Antigravity run, recorded under its model's family, does not count toward an `antigravity` budget, and every invocation recorded before accounts were named is the default login's.
- **This host only.** Interactive use and other hosts on the same login are invisible. Set the budget below the account's allowance and keep the provider's own failure as the backstop.
