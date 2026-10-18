# Configuration

What is tunable and where. `orbit config keys` and `orbit config show` expose
the installed version's supported keys and effective values without a source
checkout. This reference covers operating choices; for additional prose see
the [published configuration reference](https://github.com/constellation-works/orbit/blob/main/docs/CONFIG.md),
checking its release against `orbit --version`.

Contributors adding an executor should use the source checkout's
[executor onboarding runbook](https://github.com/constellation-works/orbit/blob/main/docs/runbooks/executor-onboarding.md).
It covers the v2 integration and test seams; this reference is for configuring
an existing lane.

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
`built-in`, or `environment`, with the file it came from. Reach for it before
assuming a value.

## Common settings

| Key | Purpose |
|---|---|
| `workflow.base_branch` | Config fallback for ship/auto/pilot base branch. A registered workspace `base_branch` takes precedence. |
| `workflow.default_crew` | Crew for any task that doesn't declare one. |
| `workflow.<tier>_complexity_crews` | Automatic crew pool per task complexity (`low`, `medium`, `hard`, `xhard`); entries `name` or `name:weight`. Empty (`[]`) routes that tier to `default_crew`. Orbit's own work draws from them too while its purpose pool is unset: auto-task templates by complexity, the task pilot and PR auto-fix from `low`, failure and PR conflict recovery from `medium`. The retired system-crew key loads with a deprecation warning and is ignored. |
| `workflow.purpose_crews.<purpose>` | Weighted pool for one purpose of work, written like the complexity pools: `task_pilot`, `failure_recovery`, `auto_fix`, `baseline_red_repair`, `ci_failure_sweep`, `merged_head_repair`, `dependabot`. An explicit task, template or activity crew wins; otherwise the purpose pool is drawn, narrowed by `--allow-crew`; unset or `[]` keeps the earlier route. A provider limit or failure hold on the assigned member redraws from the pool's other enabled members before the complexity pool. Final recovery and automatic review keep `workflow.final_recovery_crews` and `operation.review_crew`. `orbit config show` lists each purpose's resolved pool and its source. |
| `workflow.auto_ship` | Opt-in for `orbit run ship-sweep` unattended ship dispatch. The seeded `ship-sweep` routine does not read it. |
| `workflow.provider_limit_max_used_pct` | Skip a crew while a live usage window of its provider or model is exhausted or used at or above this percent (1..=100, default 90; 100 skips only on exhaustion). |
| `workflow.provider_limit_overrides` | Per-provider replacement for that percent: `provider:percent` (1..=100) or `provider:off` (record readings without gating). Each provider once. |
| `workflow.provider_limit_explicit_crews` | When an explicit crew is at its provider's limit: `wait` (default) leaves the task in the backlog until the limit lifts; `pool` draws from unlimited members of its complexity pool. A purpose-sourced crew is not an explicit pin; it redraws within its purpose pool first. |
| `workflow.provider_limit_budgets` | Rolling budgets for providers that report no usage, each `provider:<amount><usd|tokens>/<n><h|d>` (for example `grok:30usd/5h`), one per provider. Spend is this host's invocation ledger. None by default. |
| `review.before_pr` | Before-PR review: hold PR creation for a fresh reviewer that fixes what it finds (default `false`). A run captures it at submission. After-landing review is not a key: toggle the `delivery-code-review` auto-task (`orbit auto-task toggle delivery-code-review on`). |
| `review.before_landing` | Before-landing review: open the PR first and review it while hosted CI runs; it merges only at the reviewed head, and any other outcome leaves the PR open with the task in `review` (default `false`). PR route only. Config load fails while it and `review.before_pr` are both on: one review layer before landing. |
| `review.before_landing_hosts` | Machine ids (`hm_...`) whose claimed leaves review their open PR before it lands, while this owner's own deliveries keep `review.before_landing` off. Probe and pull admission resolve the caller's label and the claim captures it. Ignored when `review.before_landing` is already on for that machine. Loading fails while `review.before_pr` is on and the list is non-empty. The label is caller-chosen, so this is policy, not a security boundary. Default empty. |
| `review.minutes` | Wall-clock limit for one candidate's before-PR or before-landing review (1..=1440, default 30). Each candidate gets one review; a changed candidate, such as a completion rebase, is a new one. |
| `operation.review_crew` | Crew for automatic review: the before-PR or before-landing reviewer and, when set, every review task the after-landing auto-task mints. A pool such as `["sol", "grok"]` (or weighted `["sol:3", "grok:1"]`) draws one crew per review, skipping disabled or provider-limited crews and preferring one that did not implement the work. |
| `tasks.id_start` | Floor for this machine's task-id allocator; forward-only. → [multi-host.md](multi-host.md) |
| `execution.env.pass` | Environment variable names allow-listed into agent subprocesses. |
| `execution.codex.sandbox` | `read-only`, `workspace-write`, or `danger-full-access`. |
| `execution.codex.approval_policy` | `untrusted`, `on-request`, or `never`. |
| `machine.auto_concurrency.mode` | Forecast admission: `off` (default) builds no forecast, `shadow` records what forecasting would admit while retaining ordinary admission, `enforce` uses forecast fit to decide admissions within the worker ceiling. |
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

`[machine]` settings are global-only. An authorized local drain can select its
own forecast mode with `orbit run auto --auto-concurrency <MODE>` (`off`,
`shadow` or `enforce`); MCP `orbit_workflow_auto` `action: "start"` accepts the
same `auto_concurrency` input. When omitted, submission captures the configured
`machine.auto_concurrency.mode`; the captured value and source survive
restart/resume without a configuration write. `orbit run show <id> --json` and
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
still load, translated with a warning (`before-pr` → `review.before_pr = true`;
`after-landing` enables `delivery-code-review` until an operator toggles it;
`none` turns neither on), and a later release makes them errors. Move them to
`[review]` and the auto-task flag. `operation.review_reviewer_starts` and
`operation.review_repair_cycles` are ignored with a warning; delete them.
`orbit config show` reports both review switches with their sources.

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
orbit config set crews.gemini.enabled true
```

`orbit init` writes every built-in crew (Claude: `opus`, `sonnet`, `haiku`, `fable`;
Codex: `astra`, `sol`, `luna`; one crew each for Antigravity, Gemini,
Grok, Copilot, Cursor, Pi, OpenCode, Muse), with `enabled = true` on the crews whose
agent CLI it detects and `enabled = false` on the rest, and points
`workflow.default_crew` at the preferred family's default (`opus` on a Claude
host). Interactive init offers the enabled crews by name; `--non-interactive`
writes the recommendation. Init does not write a `custom` or `system` crew
table, nor a system-crew key: system work draws from the pools. The four
`workflow.*_complexity_crews` pools are seeded from the detected families
(Claude only: `haiku` / `sonnet` / `opus` / `opus`; Codex only: `luna` / `sol` /
`sol` / `astra`; both: `haiku, luna` / `sol, sonnet` / `opus` / `opus, astra`;
grok joins `medium`, `antigravity` or else `gemini` joins `low`; other families
leave them `[]`). A user-authored
legacy `qa` crew remains loadable, but init never creates it. To move system
work, set its `workflow.purpose_crews.<purpose>` pool, or edit the complexity
pool it falls back to. With no supported
agent CLI, every crew is written disabled and no default crew is set, so nothing
dispatches until you enable a crew. Which provider CLIs
this machine can launch, and each executor's sandbox mode:

```bash
orbit doctor providers
```

Executors are how a provider is invoked. You choose crews; executors are
infrastructure.

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

Installed executors include provider-specific CLI integrations; a model name
from another vendor does not change the executor's identity. For example,
Copilot, Cursor, and Pi keep their own authentication and sandbox grants
regardless of the model they route to. Use the installed executor catalog and
provider CLI help for supported model IDs instead of treating examples as a
permanent list.

Executor tool reach is not uniform. Some provider CLIs have no MCP client at
all, so Orbit's tools are reached from the agent's shell through the `orbit`
binary on the child's `PATH` rather than through an injected MCP server. That
path enforces the same allowlist and caller-role gates, but it means an
executor definition that disables the provider's shell tool also removes the
Orbit tool path. Check the provider section of the configuration reference
before assuming MCP-native tool injection.
