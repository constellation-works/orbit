---
title: Configuration
description: "config.toml locations, the host registry, crews, and the settable configuration keys."
sidebar:
  order: 5
---

## File locations

| Path | Scope |
|---|---|
| `~/.orbit/config.toml` | Global |
| `~/.orbit/clock.env` | Global, machine only — credentials for clock-started runs; see [Scheduled credentials: clock.env](#scheduled-credentials-clockenv) |
| `.orbit/config.toml` | Workspace, per user (gitignored) |

Each key resolves on its own. A workspace value overrides the global value, a
global value fills a workspace omission, and built-in defaults fill the rest.
Tables merge setting by setting; scalars and arrays replace the global value
whole. Named crews merge by crew name and field, so a workspace can override
one crew's `model` without restating the crew.

Three security keys never inherit from global once a workspace file exists:
`execution.codex.sandbox`, `execution.codex.approval_policy`, and
`execution.env.pass`. A workspace file that omits one gets its built-in
default. `orbit config show` labels every value with its source.

`orbit init` seeds the global file. `orbit init --force` resets the global root
first, including the machine identity. A reset creates a new machine ID;
non-interactive resets require both `--machine-name` and `--task-prefix`.
Missing non-interactive inputs or invalid supplied identity flags are refused
before deleting the existing root.

```bash
orbit config path              # the config.toml in effect
orbit config show              # values with their source
orbit config show --all        # also Paths, Crew purposes and Review
orbit config get workflow.default_crew
orbit config set workflow.default_crew opus
orbit config keys              # every settable key, with type and description
```

`orbit config show` hides unconfigured **Advanced** keys and counts them in each
section heading. A key set in any layer remains visible, even if that layer is
shadowed or its security value is not inherited. Use `--all` (alias `--advanced`)
to list every key, expand unset sections, and append Paths, Crew purposes and
Review. Those three blocks restate rows the sections already print, so the
default view omits them. `--json` always includes `crew_purposes` and `review`.
`machine.id` and `machine.task_prefix` render under Identity, not as Machine
keys; `machine.name` stays a Machine key. `--scope global|workspace` applies
the same filter to that file. JSON always includes all keys; each provenance
record has `tier: "standard"` or `tier: "advanced"`. Tiers affect display only;
loading, `config get`, and `config set` keep their existing behavior. Row
descriptions are one sentence; the tables below keep the defaults and ranges.

`orbit config set` writes the workspace file; `--global` writes the global one.
If the workspace file does not exist yet, add `--fresh` to start it empty or
`--seed-from-global` to start from a copy of the global file.

## Scheduled credentials: clock.env

`~/.orbit/clock.env` holds the credentials for runs no login shell starts.
launchd (macOS) or a systemd user timer (Linux) starts `orbit clock tick`
with no login environment, so the tick reads this file and supplies its
values as defaults to the child runs it starts; its own process environment
is never changed, and the installed unit files never contain a secret. It is
machine-global, beside the global `config.toml`: there is no workspace
`clock.env`, and no checkout or sync carries it. For the setup procedure,
see [Schedule recurring work](../../how-to/recurring-work/#scheduled-credentials).

`orbit routine init --install-clock` creates the file comment-only with
owner-only permissions when it is absent, and never rewrites an existing
file. Keep it that way:

```bash
chmod 600 ~/.orbit/clock.env
```

```ini
# ~/.orbit/clock.env — placeholders; put real values only in the file
CLAUDE_CODE_OAUTH_TOKEN=<token-from-claude-setup-token>
ANTHROPIC_CREDITS_API_KEY_1=<api-key-for-the-credits-crew>
GITHUB_TOKEN=<github-token>
```

Loading rules:

- One `NAME=value` per line. Blank lines and `#` comments are skipped, a
  leading `export ` is accepted, and one pair of surrounding single or
  double quotes is stripped. Nothing is expanded. An empty value, a name
  starting with `ORBIT_`, and a name nobody admits are dropped; a repeated
  name keeps its last value.
- A name is admitted only when a discovered workspace's effective
  `execution.env.pass` lists it, or a crew's `env` entry reads it as its
  source (below). Each workspace's runs receive the names that workspace's
  own effective `pass` admits; `pass` is a security key, so a workspace
  file's list replaces the global one for that workspace.
- A non-empty value already in the tick's environment wins; an empty or
  absent one is filled from the file. A dry run (`orbit clock tick
  --dry-run`) loads nothing.
- The file must be a regular file, not a symlink, owned by the user running
  the tick, and readable by its owner alone. A symlink, another owner's
  file, or a group- or world-readable mode is refused as a load error
  naming the `chmod 600` fix; the tick still runs and loads nothing from
  it. Sandboxed agents cannot read the file itself.

Per-crew sources work the same way. A crew's `env` entry
`NAME=SOURCE` (global config only; see [Crews](#crews)) makes that crew's
agents receive `NAME` with the value of `SOURCE`, read from the launching
process or, for clock-started runs, from `clock.env`. Keep the source under
a name no CLI reads, such as `ANTHROPIC_CREDITS_API_KEY_1`, so other crews
and interactive shells are unaffected; the agent never receives `SOURCE`
itself. When a crew entry sets a Claude credential (`ANTHROPIC_API_KEY` or
`CLAUDE_CODE_OAUTH_TOKEN`), the other Claude credential is removed from that
crew's agents. An unset or empty source excludes that crew from drains and
is reported by name — never by value — on the run and by `orbit doctor`.

Verification reports names only. `orbit clock tick --json` lists what the
file supplied as `clock_env_loaded`, `orbit doctor`'s `env-pass` row names
`pass` variables unset in doctor's own environment, and on macOS its
`claude-worker-token` row names the `pass` list or `clock.env` lacking a
Claude credential, or a crew source `clock.env` lacks. No command, run
record, or log prints a value.

On macOS, a Claude activity with neither `CLAUDE_CODE_OAUTH_TOKEN` nor
`ANTHROPIC_API_KEY` in its composed environment is refused before `claude`
starts, rather than fall back to the Claude Desktop login, which the
Desktop revokes mid-run. Create a dedicated token with `claude setup-token`,
store it as `CLAUDE_CODE_OAUTH_TOKEN` above, and admit that name in
`execution.env.pass`; an admitted `ANTHROPIC_API_KEY`, or a per-crew `env`
mapping supplying either name, also satisfies the check. A token stored
under a name `pass` omits counts as lacking.

## Unknown settings

Unknown keys and tables load with a warning naming their file and path, plus a
nearby known key when available. Each file and path warns once per process.
Unknown tables, including empty ones, are reported as a whole. Known section
headers, plugin settings and registered table-valued settings stay under their
own validation. Unknown `[operation]` and `[review]` fields also warn; known
values still validate normally.

`orbit doctor` lists unknown settings alongside retired keys.
`orbit doctor --fix-retired-config-keys` backs up each affected file and removes
those entries, preserving known settings and translated review preferences.
Check the list first when sharing config with a newer binary: that version
may still use an entry this version does not recognise.

## Host registry

`~/.orbit/hosts.toml` registers the remote Orbit installations this machine
can reach over SSH. It is machine-global, beside `workspaces.json`; workspace
configuration does not override or merge it. The local host's identity stays
in `[machine]` in the global `config.toml`, outside this file.

Manage entries with `orbit host add`, `rename`, and `remove`, or the
dashboard's [Settings › Hosts](../../how-to/dashboard/#hosts). For setup and
prefix routing, see [Run Orbit across hosts](../../how-to/multi-host/).

The following illustrates the schema written by `orbit host add`; use the
command to discover the remote identity rather than inventing its values:

```toml
schema_version = 1

[[hosts]]
name = "owner"
machine_id = "hm_example_owner"
ssh = "orbit-owner"
task_prefix = "BOX"
```

| Field | Meaning and validation |
|---|---|
| `schema_version` | Required integer. This build accepts only `1`. |
| `hosts` | Array of remote entries (`[[hosts]]`); omission means no remote hosts. |
| `name` | Required host name, defaulting to the remote's `machine.name`. Non-empty, at most 128 bytes, with no surrounding whitespace, control characters, or path separators. Unique ignoring case, including against this machine's name. |
| `machine_id` | Required remote identity: `hm_` followed by ASCII letters, digits, `_`, or `-`, at most 128 bytes overall. Unique and different from this machine's ID. |
| `ssh` | Required SSH alias or `user@host`. No leading `-`, whitespace, control characters, or shell metacharacters. |
| `task_prefix` | Required task namespace. Two to five uppercase ASCII letters, unique across registered hosts and this machine. Stored legacy `ORB` remains valid; fresh initialization reserves `ORB`, `ADR`, `L`, and `F`. |

Unknown keys, malformed entries, unsupported schema versions, and duplicate
identities or prefixes fail loading. Orbit keeps the file's bytes intact on
a load failure. Mutations validate the resulting entries, sort them by name,
and replace the file atomically. A refused mutation preserves the prior file.

Reachability, Orbit version, protocol, and workspaces are read live by
`orbit host list` and `show`; none is persisted in this file. Registration is
one-way and stores no SSH credentials.

### Legacy migration

`~/.orbit/mcp-destinations.toml` is the legacy destination file. Its reader
remains for one release. Use `orbit host` to migrate it; do not add new rows
to the legacy file.

- **Only the legacy file exists:** federated MCP, pull drains, and replica
  worktree cleanup still read it. `orbit host list` marks rows **legacy**.
  They have no stored task prefix, so they cannot supply prefix routes.
- **First add, rename, or remove:** Orbit probes every retained legacy host,
  writes `hosts.toml`, applies the operation, and deletes the legacy file.
  `orbit host add <existing-ssh-target>` migrates an already-listed host
  successfully, preserving its SSH target and chosen migration name;
  `--name` applies only to a new host. Migrated names use the remote's
  `machine.name`, falling back to its SSH target when the name is taken.
- **A retained host does not answer:** `legacy_host_unreachable` refuses the
  mutation and neither file changes. Restore SSH access or remove the
  decommissioned row with `orbit host remove <ssh-target-or-machine-id>`.
  The removed legacy host is never contacted; all remaining hosts must
  answer. `--force` only overrides replica and pull-drain dependents.
- **Both files exist:** every consumer, including `orbit host`, refuses
  with `host_file_conflict`. The diagnostic compares them by machine ID
  and lists missing routes with their `orbit host add` commands. Preserve
  a backup, retire the legacy file, then run those commands to register
  any missing hosts. Orbit does not choose one file for you.

`orbit doctor` warns while only the legacy file exists and gives an
`orbit host add` command using an existing SSH target. The next release
drops the legacy reader and retains the both-files conflict check for one
additional release. The older singular `host.toml` identity file is
unrelated to this migration.

## Crews

A **crew** is a named provider and model. A task runs on its own `crew`, or on
`[workflow] default_crew` when it names none.

```toml
[crews.sol]
provider = "codex"
model = "gpt-6.1-sol"
effort = "high"
description = "Systems implementation"
tags = ["implementation", "review"]

[crews.opus]
provider = "claude"
model = "opus"

[crews.argon]
enabled = false
provider = "antigravity"
model = "gemini-4-argon-high"

[workflow]
base_branch = "main"
default_crew = "sol"
```

| Field | Purpose |
|---|---|
| `provider` | Required. Agent CLI family: `claude`, `codex`, `antigravity`, `gemini`, `grok`, `copilot`, or `muse`. |
| `model` | Required. Model ID passed to the provider CLI. |
| `enabled` | Default `true`. `false` keeps the crew listed but refuses to run it. See [Disabled crews](#disabled-crews). |
| `effort` | Reasoning effort. See [Reasoning effort](#reasoning-effort). |
| `description` | Short summary for people. |
| `tags` | Discovery labels. |
| `env` | Variables this crew's agents receive on top of `execution.env.pass`, as `NAME` or `NAME=SOURCE`. Global config only. For clock-started runs the source is read from `clock.env`; see [Scheduled credentials: clock.env](#scheduled-credentials-clockenv). |

`ollama` and `openai_compat` are recognized provider IDs with no shipped
executor: a crew naming either one loads, then fails at dispatch. See
[Providers](../../concepts/agents/#providers). `gemini` is the legacy Google
CLI lane; `orbit init` seeds Antigravity's `gemini-flash` and `argon` crews when `agy` is installed. For deprecated
provider aliases and a starter crew per executor, see
[Set up an executor](../../concepts/agents/#set-up-an-executor).

At dispatch, the first crew that is set wins: an explicit crew on the run or
activity input, then the task's `crew`, then `[workflow] default_crew`.

`default_crew` must name a defined crew. If you define crews and leave it
unset, Orbit falls back to an `opus` (or legacy `claude`) crew, and otherwise
refuses to load.

### Disabled crews

`orbit init` writes every built-in crew. It sets `enabled = true` on the crews
whose provider CLI it detects and `enabled = false` on the rest. To turn a
provider on later:

```bash
orbit config set crews.argon.enabled true
```

A crew table without `enabled` is enabled. A disabled crew:

- stays listed: `orbit config show` (`ENABLED` column), `orbit config get
  crews.<name>.enabled`, `orbit.workspace.list` with `include: ["crews"]`
  (`enabled` per crew, schema version 3), and the dashboard all show it;
- is never drawn from a complexity pool. A pool whose members are all disabled
  acts as an empty pool, so the task gets `default_crew`;
- is refused, never substituted, when a task's `crew`, an explicit crew, or
  `default_crew` selects it. The error names the crew and the
  `orbit config set … enabled true` command;
- is reported by `orbit doctor` when `default_crew` names it. Config still
  loads.

With no supported agent CLI detected, every crew is seeded disabled and
`default_crew` stays unset, so nothing dispatches until you
enable a crew.

`orbit init` also seeds the four `workflow.*_complexity_crews` pools from the
detected providers. Claude only: `haiku` / `sonnet` / `opus` / `opus`. Codex
only: `luna` / `sol` / `sol` / `astra`. Both: `haiku, luna` / `sol, sonnet` /
`opus` / `opus, astra`. Grok adds `grok` to `medium`; a detected `agy` appends
`gemini-flash` to `low` and `argon` to `hard`; a detected `muse` appends `muse-spark`, `muse-wonder`,
and `muse-wonder-full` to `low`, `medium`, and `hard`. Other
providers leave the pools empty, which routes to `default_crew`. Init only
seeds a new file; an existing config is not rewritten.

### Reasoning effort

`effort` is unset by default, which keeps the provider's own default. A
supported value is passed through the provider's documented flag.

| Provider | Accepted values | Rendered as |
|---|---|---|
| `claude` | `low`, `medium`, `high`, `xhigh`, `max` | `--effort` |
| `codex` | `low`, `medium`, `high`, `xhigh`, `max` | `--config model_reasoning_effort` |
| `muse` | `low`, `medium`, `high`, `xhigh`, `max` | `--reasoning-effort` |
| `antigravity` | `low`, `medium`, `high` | `--effort` |
| `grok` with `grok-4.6` or `grok-4.7` | `low`, `medium`, `high`, `xhigh` | `--reasoning-effort` |
| `grok` with `grok-4.5` | `low`, `medium`, `high` | `--reasoning-effort` |
| others | None | — |

An invalid or unsupported `effort` is **ignored for that crew**, as if unset,
so a slip such as `effort = "hard"` (a task-complexity word) cannot stop every
command. Loading logs a warning with the config path, crew, value, and accepted
values, and `orbit doctor` lists each ignored value with the fix. Orbit never
remaps an unsupported value onto a nearby one. `orbit config set` refuses to
write a value that loading would drop. A missing `provider` or `model`, a
retired backend, or an unknown pool reference still fails config loading.

The narrower sets follow each CLI. `agy --effort` defines no `xhigh` or `max`.
Grok effort is verified only for `grok-4.5`, `grok-4.6`, and
`grok-4.7`; on any other Grok model it is ignored with a warning. The provider
can still reject a value for a particular model.

Choose capability with the model first, then use `effort` to tune the reasoning
budget within it.

### System work

Orbit's own work draws its crew from the complexity pools, like a task with no
crew, until you give a purpose a pool of its own (see
[purpose pools](#purpose-pools)). Shipped auto-task templates name no crew, so
each minted task draws from the pool for its complexity; a `template.crew`
setting, an explicit template crew, and `workflow.purpose_crews.review` for
`delivery-code-review` still win. While their purpose pools are unset, the
task pilot draws from `low`, step-failure and PR conflict recovery from
`medium`, and repairs authorized by `orbit run auto --auto-fix`, conflict
repairs included, from `low`. Repairs are narrowed only by `--allow-crew`, and a
pool the run cannot use stops the repair rather than escalating. A run draws
once per pool, keeping the pool's weights and skipping disabled and
usage-limited crews, and records the draw in its state. An empty pool falls
back to `default_crew`, and the recorded draw says so. Recovery never inherits
a failed task's crew.

### Purpose pools

Each purpose of work can name a weighted pool of its own under
`[workflow.purpose_crews]`, written like the complexity pools:

```toml
[workflow.purpose_crews]
task_pilot = ["luna:3", "sonnet:1"]
failure_recovery = ["sol"]
baseline_red_repair = ["opus"]
```

The purposes are `task_pilot`, `failure_recovery`, `auto_fix`,
`baseline_red_repair` and `sweep_tasks` (every task the CI failure,
Dependabot, code-scanning and merged-head repair sweeps file), `explorer`
(the read-only explorers an implementing agent delegates, which have no
earlier route: with no eligible member, delegation is not offered), plus
`final_recovery` and `review`, which keep their own rules: final recovery
draws only from its pool, and `[]` turns it off; automatic review needs its
pool set and avoids the crew that implemented the reviewed work. For the
first five, a crew is chosen in this order:

1. An explicit crew on the task, auto-task template or activity input, used as
   it is.
2. The purpose's pool, when it has an enabled member. A provider usage limit
   or failure hold on the assigned member redraws from the pool's other
   enabled members, keeping their weights, before the route below.
3. The route the purpose had before purpose pools: the complexity pool named
   above, else `default_crew`.

`--allow-crew` narrows a purpose draw the same way it narrows a complexity
draw. A task's purpose comes from the tag it was filed with
(`baseline-red-repair`, `ci-failure-sweep`, the Dependabot and code-scanning
tags, a merged-head follow-up tag), or from a `crew-purpose:<purpose>` tag an
auto-task template adds. `orbit config show --all` lists each purpose's resolved
pool and where it comes from (the default view already prints the purpose
keys; `--json` includes `crew_purposes`), and `orbit run show` names the rule behind each
activity crew a run drew.

`[workflow] system_crew` is deprecated: it still loads for one release with a
warning, but is ignored. Orbit no longer defines a built-in `system` crew, so a
template or job still naming `crew: system` routes through the pools with a
warning. A `[crews.system]` table you define is an ordinary crew.

## Settable keys

`orbit config set` accepts these keys, except the two read-only `machine.*` identity keys. `orbit config keys` prints the same list, and the docs test suite fails when this table and that list diverge.

| Key | Type, default and purpose |
|---|---|
| `workflow.base_branch` | **Advanced.** string · **Default:** `main`<br>Base branch for ship, auto, and the task pilot when the registered workspace has none. |
| `workflow.default_crew` | string · **Default:** See [Crews](#crews)<br>Crew for a task that names none and gets no override. |
| `workflow.auto_ship` | bool · **Default:** `false`<br>Opt this workspace in to `orbit run ship-sweep`. The seeded `ship-sweep` routine does not read it; its `enabled:` flag is its switch. |
| `workflow.low_complexity_crews` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for low-complexity tasks with no crew, and for the task pilot and PR auto-fix while their purpose pools are unset. |
| `workflow.medium_complexity_crews` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for medium-complexity tasks with no crew, and for step-failure and PR conflict recovery while their purpose pool is unset. |
| `workflow.hard_complexity_crews` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for hard-complexity tasks with no crew. |
| `workflow.xhard_complexity_crews` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for xhard-complexity tasks with no crew. |
| `workflow.purpose_crews.auto_fix` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for repairs authorized by `orbit run auto --auto-fix`. Unset or `[]`, they draw from `workflow.low_complexity_crews`. An explicit crew still wins, and `--allow-crew` still applies. See [purpose pools](#purpose-pools). |
| `workflow.purpose_crews.baseline_red_repair` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for the tasks the red-base re-check files. Unset or `[]`, they draw from their complexity pool. An explicit crew still wins, and `--allow-crew` still applies. See [purpose pools](#purpose-pools). |
| `workflow.purpose_crews.explorer` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for the read-only explorers an implement or review-repair agent delegates through `orbit.agent.invoke` while `workflow.explore.enabled` is on. Members disabled here, or whose executor `host_os` excludes this host, are skipped. Unset, `[]` or with no eligible member, delegation is not offered and every call is refused. A named crew must be a member. See [purpose pools](#purpose-pools). |
| `workflow.purpose_crews.failure_recovery` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for step-failure and PR conflict recovery. Unset or `[]`, they draw from `workflow.medium_complexity_crews`. An explicit crew still wins, and `--allow-crew` still applies. See [purpose pools](#purpose-pools). |
| `workflow.purpose_crews.final_recovery` | array&lt;string&gt; · **Default:** `["sol:100", "opus:20"]`<br>Crew pool for final recovery, drawn once per run after step recovery is exhausted. The default keeps only members you define; `[]` disables final recovery. Replaces the deprecated `workflow.final_recovery_crews`. |
| `workflow.purpose_crews.pre_landing_review` | string or array · **Default:** Unset<br>Crew for before-PR and before-landing review, including the reviewer a claim from a `review.before_landing_hosts` machine captures. Written and admitted like `workflow.purpose_crews.review`, and drawn by the same rules. Unset or `[]`, that reviewer comes from `workflow.purpose_crews.review`, so after-landing review can keep a different pool. A review layer that resolves no crew from either key refuses to start, naming both. |
| `workflow.purpose_crews.review` | string or array · **Default:** Unset<br>Crew for automatic review: every review task the after-landing auto-task mints, and the reviewer `review.timing` runs while `workflow.purpose_crews.pre_landing_review` is unset. Unset, after-landing review uses that auto-task's template crew. An array is a pool written like the complexity pools (`["sol", "grok"]` or `["sol:3", "grok:1"]`, all bare or all weighted): each review draws one crew by weight, skipping crews that are disabled or at a provider usage limit and then preferring one that did not implement the reviewed work, each step falling back to the pool when it would leave none. A distributed owner resolves its pool to one crew per claim. `[]` unsets it; a crew the registry does not define fails the load. Replaces the deprecated `operation.review_crew`. |
| `workflow.purpose_crews.sweep_tasks` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for every task a sweep files: the CI failure sweep, the Dependabot and code-scanning sweeps, and the merged-head repairs a review reconciliation files. Unset or `[]`, they draw from their complexity pool. Replaces the deprecated `ci_failure_sweep`, `dependabot` and `merged_head_repair` pools, which still load into it; a file whose old pools differ fails to load until you merge them here. An explicit crew still wins, and `--allow-crew` still applies. See [purpose pools](#purpose-pools). |
| `workflow.purpose_crews.task_pilot` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for the task pilot. Unset or `[]`, it draws from `workflow.low_complexity_crews`. An explicit crew still wins, and `--allow-crew` still applies. See [purpose pools](#purpose-pools). |
| `workflow.provider_limit_max_used_pct` | **Advanced.** integer · **Default:** `90`<br>Delivery admission skips a crew while a live usage window of its provider, or of its model, is exhausted or used at or above this percent. `100` skips only on exhaustion. |
| `workflow.provider_limit_overrides` | array&lt;string&gt; · **Default:** `[]`<br>Per-provider thresholds replacing `workflow.provider_limit_max_used_pct`, each `provider:percent` or `provider:off` (record readings without gating admission), such as `["claude:80", "grok:100", "codex:off"]`. |
| `workflow.provider_limit_budgets` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Rolling budgets for providers that report no usage, each `provider:<amount><usd\|tokens>/<n><h\|d>`, such as `["grok:30usd/5h"]`. Counts this host's Orbit runs only. |
| `workflow.provider_limit_explicit_crews` | **Advanced.** string · **Default:** `wait`<br>A task whose explicit crew is limited waits in the backlog (`wait`), or is drawn from the unlimited members of its complexity pool (`pool`). A purpose-sourced crew is not an explicit pin; it redraws within its purpose pool first. |
| `workflow.crew_selection` | **Advanced.** string · **Default:** `static`<br>How task creation, complexity re-rate and admission draws weigh pool members. `static` uses the configured weights. `usage_pressure` scales each member's weight by its crew's weekly usage pressure, clamped by `workflow.usage_pressure_cap`, and at admission draws a crew the system chose again from its whole pool, keeping the task's automatic crew source. Explicit crews are never re-drawn. The `crew_selection` object a run records is the draw itself, not this key. |
| `workflow.usage_pressure_cap` | **Advanced.** number · **Default:** `8`<br>Under `usage_pressure`, the weight multiplier is clamped to `[1/cap, cap]`. At least `1`. |
| `workflow.usage_pressure_strength` | **Advanced.** number · **Default:** `1.0`<br>Under `usage_pressure`, the exponent applied to pressure before the cap. At least `0`; `0` disables weighting. |
| `workflow.crew_performance_types` | **Advanced.** array&lt;table&gt; · **Default:** `[]`<br>Ordered task types for the implementer scorecard's by-type matrix. Each `[[workflow.crew_performance_types]]` entry has a unique non-empty `name` (never `other`) and at least one of `tags` or `paths`; a task takes the first type whose tags it carries or whose path globs match its context files, else `other`. Empty means every task is `other`. Read with `orbit agents scorecard --role implementers --by-type`. |
| `workflow.crew_performance` | **Advanced.** string · **Default:** `off`<br>How task creation, complexity re-rate and admission draws weigh pool members by each crew's implementer score on the task's type within its complexity tier: `off`, `absolute` (advantage `score − 100`), `comparative` (advantage `score − crew mean`), `shadow_absolute` or `shadow_comparative`. A live mode draws on `weight × pressure_factor × performance_factor`, the factor `clamp(exp(strength × advantage / 100), 1/cap, cap)`, and records `applied: true`. Shadow computes and records the same factor with `applied: false` and draws as `off` would, usage pressure still included. Before enabling a live mode, read `orbit agents scorecard --role implementers --by-type --shadow --window 30d`: static share is the configured weight, drawn share is the weight the draw used, and would-be share applies the recorded factor. Draws with no performance record are listed separately and counted as static. A large gap between drawn and would-be is work that would move. Independent of `workflow.crew_selection`; performance alone never re-draws an assigned crew, and explicit crews are never re-drawn. A missing, stale or mismatched snapshot gives every factor 1 and records why in the run's `crew_selection`. Only `off` makes the seeded hourly `crew_performance_refresh` routine write nothing and skip the scan. |
| `workflow.crew_performance_strength` | **Advanced.** number · **Default:** `3`<br>The strength `k` of the crew-performance factor. At least `0`; `0` disables the factor. |
| `workflow.crew_performance_cap` | **Advanced.** number · **Default:** `4`<br>The crew-performance factor is clamped to `[1/cap, cap]`. At least `1`. |
| `workflow.crew_performance_window_days` | **Advanced.** integer · **Default:** `30`<br>Evidence window in days (`1`–`90`) the crew-performance snapshot is built from. The seeded `crew_performance_refresh` routine rebuilds the snapshot hourly over this window; `orbit run readiness` shows its age, window and type list, or why it is unusable (`missing`, `stale` after 24 hours, or `type_list_changed`). |
| `workflow.required_validation_commands` | array&lt;string&gt; · **Default:** `[]`<br>Commands every delivered candidate must pass. Owner PR and local deliveries run them before push or merge; a distributed claim must pass them before this owner accepts its handoff. Empty means no required check on any path: nothing runs and a claimed handoff carries no validation logs. |
| `workflow.distributed_completion` | string · **Default:** `review`<br>How far this owner takes an accepted distributed handoff: `review` waits for an operator's Approve handoff; `done` lands it through the owner's landing job, as `orbit run auto --complete` does. |
| `workflow.admission_affinity` | **Advanced.** string · **Default:** `band`<br>How a distributed owner orders admission against its live executors, so each takes work only it can run first. `band` puts that work ahead of priority within a dispatch band, so critical and corrective work still leads; `priority` puts it first only within a priority; `off` keeps the plain band, priority, expiry and age order. Applies to followers' pulls and the owner's own drain. |
| `workflow.unlocked_context_paths` | array&lt;string&gt; · **Default:** `["**/*.md"]`<br>Path globs (`*` within a component, `**` across components) whose `context_files` entries take no context lock. The agent still receives them as context; admission, readiness and lock reservations skip them. A task whose every entry matches still locks them all. `[]` locks every entry. |
| `workflow.lock_modes` | **Advanced.** array&lt;table&gt; · **Default:** `[]`<br>Ordered `[[workflow.lock_modes]]` entries, each a `pattern` (a Rust regex searched for in the workspace-relative path; anchor it with `^` and `$`) and a `mode` of `exclusive` (default) or `shared`. The first matching entry wins. Two tasks whose context overlaps only at a shared path, judged at the narrower selector's path, run together, and a conflict between them is resolved when the second lands. `module:` and `command:` selectors stay exclusive. Empty locks every overlap. |
| `workflow.unlocked_path_validation_commands` | array&lt;string&gt; · **Default:** `[]`<br>Commands a repaired candidate runs instead of `workflow.required_validation_commands` when its tree differs from the last tree its run passed the full list on only in paths matching `workflow.unlocked_context_paths`, such as `["make ci-fast"]`. Any other change runs the full list. Each validation records which list ran and why as `validation_scope`. Empty always runs the full list. |
| `workflow.generated_paths` | table&lt;string,string&gt; · **Default:** `{}`<br>Generated files, as path glob = regenerate command, such as `"docs/INDEX.md" = "make docs-index"`. When a stopped rebase conflicts on a match, conflict recovery takes one side, runs the command in the checkout and stages its output instead of merging it. A command that fails or leaves conflict markers fails the repair. |
| `workflow.light_delivery_paths` | array&lt;string&gt; · **Default:** `["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]`<br>Path globs (`*` within a component, `**` across components; a leading `!` excludes) a candidate must change only within for a task tagged `skip-review` or `skip-validation` to skip agent review or local validation. Any other changed path ignores both tags, and the task's history records why. `[]` disables both tags. |
| `workflow.test_paths` | **Advanced.** array&lt;string&gt; · **Default:** `["**/test/**", "**/tests/**", "**/__tests__/**", "**/spec/**", "**/testdata/**", "**/*_test.*", "**/*.test.*", "**/test_*.*", "**/*_spec.*", "**/*.spec.*"]`<br>Path globs (`*` within a component, `**` across components; a leading `!` excludes) whose changed lines count as tests in each landed delivery's diff stats, charted under Analytics › Metrics › Lines changed. Checked before `workflow.non_code_paths`. Changes apply to deliveries recorded afterwards. |
| `workflow.non_code_paths` | **Advanced.** array&lt;string&gt; · **Default:** `["docs/**", "**/*.md", "**/*.mdx", "**/*.rst", "**/*.txt", "**/*.json", "**/*.yaml", "**/*.yml", "**/*.toml", "**/*.lock", "**/*.csv", "**/*.golden", "**/*.png", "**/*.jpg", "**/*.jpeg", "**/*.gif", "**/*.webp", "**/*.svg", "**/*.ico", "**/LICENSE*"]`<br>Path globs whose changed lines count as other (docs, config, data, assets) in landed-delivery diff stats. A path matching neither this nor `workflow.test_paths` counts as code. |
| `workflow.task_pilot_freshness.material_fields` | **Advanced.** array&lt;string&gt; · **Default:** `title`, `description`, `criteria`, `plan`, `selectors`<br>Task fields whose edit makes an accepted task-pilot assessment stale. Also accepts `tags`, `crew`, `tools`, `type`, `complexity`, `relations`, `dependencies`, `instructions`. |
| `workflow.task_pilot_freshness.source_sensitivity` | **Advanced.** string · **Default:** `ignore`<br>Whether a branch-head move makes an accepted task-pilot assessment stale: `ignore`, `context_files` (only when the move changed a path the task's selectors name), or `any`. |
| `workflow.mid_run_steering.enabled` | bool · **Default:** `false`<br>Record in each run's state the newest task comment every agent prompt with task context included, the run's steering watermark, and lead the run's later prompts with the operator comments written after it. The shipped delivery pipelines do not commit while one is undelivered. Claimed leaves read steering from their owner and fail closed before commit when it cannot answer. |
| `workflow.mid_run_steering.max_rounds` | **Advanced.** integer · **Default:** `2`<br>Steering rounds a run may spend at the promotion gate before it holds as `steering_unconsumed` (`1`–`10`). |
| `workflow.regression_risk.enabled` | bool · **Default:** `true`<br>Mark the `file:` context paths of a task that regressed often in the trailing window in its implement and review prompts: each mark gives the path, how many of its touching tasks regressed, their most common natures and escapes (`unclassified` when unrecorded) and one lesson line, and the prompt adds a check for each. Each prompt that carries marks records them on the task; Analytics › Codebase compares marked paths' regression rates with marks against the window before the first mark. Off, prompts carry no marks. |
| `workflow.regression_risk.window_days` | **Advanced.** integer · **Default:** `30`<br>Trailing days of done tasks a mark is computed over (`1`–`365`). A path's regressions drop out once their tasks are older. |
| `workflow.regression_risk.min_support` | **Advanced.** integer · **Default:** `5`<br>Done tasks in the window whose context files named the path before it can be marked (`1`–`10000`). |
| `workflow.regression_risk.threshold_percent` | **Advanced.** integer · **Default:** `40`<br>Share of those tasks a `regression_from` link names, in percent, at or above which the path is marked (`1`–`100`). |
| `workflow.regression_risk.max_files` | **Advanced.** integer · **Default:** `3`<br>Most paths one prompt marks, highest regression rate first (`1`–`20`). |
| `workflow.explore.enabled` | bool · **Default:** `false`<br>Let a managed implement or review-repair agent delegate read-only questions through `orbit.agent.invoke`. Each call runs a sandboxed explorer from `workflow.purpose_crews.explorer` in the caller's own worktree: it cannot write, change Orbit state, dispatch or invoke another agent, never gets the operator's trusted-host admission, and answers with the paths it read. Every other agent is refused, and the operator's invocation is unchanged. Refusals name their reason and the caller continues. Run detail lists each explorer with its crew, duration, tokens and cost, and Analytics › Cost compares implement runs with and without explorers per crew. Off, the agent is offered no delegation. |
| `workflow.explore.max_calls_per_run` | **Advanced.** integer · **Default:** `6`<br>Explorer calls one implement or review-repair run may make (`1`–`100`). A retry with the same `idempotency_key` spends none. |
| `workflow.explore.max_concurrent_per_run` | **Advanced.** integer · **Default:** `2`<br>Explorers one run may have queued or running at once (`1`–`16`). |
| `workflow.explore.timeout_seconds` | **Advanced.** integer · **Default:** `600`<br>Default and ceiling of one explorer's provider wall clock, in seconds (`1`–`3600`). |
| `workflow.explore.max_answer_bytes` | **Advanced.** integer · **Default:** `16384`<br>Largest explorer answer returned to the calling agent, in bytes (`167`–`262144`); a longer one is shortened and flagged `answer_truncated`. |
| `workflow.resource_throttle.enabled` | **Advanced.** bool · **Default:** `true`<br>Start no new task while host CPU, memory, disk space, or (on Linux) disk I/O pressure stays high. Disabled, pressure is still reported. |
| `workflow.resource_throttle.cpu_source` | string · **Default:** `"load"`<br>`load` or `utilization`. Linux utilization uses busy CPU tick deltas excluding idle and I/O wait; macOS uses Mach utilization for either choice. Both use the existing ten-second sustain window; I/O pressure remains independently gated. The active source appears in readiness and doctor. |
| `workflow.resource_throttle.cpu_high_percent` | integer · **Default:** `90`<br>CPU high-water mark. With the default `cpu_source = "load"`, Linux uses load average divided by online CPUs, includes the run queue and I/O wait, and can exceed 100%; this differs from `machine.auto_concurrency.admit_cpu_percent`, which measures forecast utilization. |
| `workflow.resource_throttle.cpu_resume_percent` | integer · **Default:** `85`<br>Resume below this CPU percentage. |
| `workflow.resource_throttle.cpu_light_leaves` | **Advanced.** integer · **Default:** `2`<br>While CPU alone holds admissions, a local drain still starts up to this many `no-diff-expected` auto-tasks (reviews, curation). Memory, disk and I/O pressure hold them too; `0` holds them with everything else. |
| `workflow.resource_throttle.memory_high_percent` | integer · **Default:** `90`<br>Memory high-water mark. It and `machine.auto_concurrency.admit_memory_percent` measure non-reclaimable memory; the global auto-concurrency threshold must stay below this global high mark. |
| `workflow.resource_throttle.memory_resume_percent` | integer · **Default:** `85`<br>Resume below this memory percentage. |
| `workflow.resource_throttle.disk_high_percent` | integer · **Default:** `90`<br>Disk high-water mark, per observed filesystem. |
| `workflow.resource_throttle.disk_resume_percent` | integer · **Default:** `85`<br>Resume below this disk percentage. |
| `workflow.resource_throttle.io_full_high_percent` | **Advanced.** integer · **Default:** `20`<br>Linux only. High-water mark for disk I/O stall: `full avg60` in `/proc/pressure/io`, the share of the last minute in which every runnable task was stalled on I/O. `full`, not `some`, because `some` fires whenever any one process waits on disk, which is normal on a busy build host. Hosts without pressure stall information are not gated. |
| `workflow.resource_throttle.io_full_resume_percent` | integer · **Default:** `10`<br>Resume below this `full avg60` percentage. |
| `workflow.validation_env.shell_probe` | string · **Default:** `interactive`<br>How the `PATH` (and allowlisted toolchain locators such as `CARGO_HOME`) that required validation and `local_shell` steps see is resolved from the owner's shell: `interactive` tries an interactive login shell (`-i -l -c`) so toolchains exported in rc files are found, falling back to `-l -c` on startup failure, nonzero exit, timeout, or a missing marker; `login` probes only `-l -c`; `off` never probes the shell. Each probe is bounded to 10 seconds and its outcome is cached for two minutes. Replaces the deprecated `login_shell` (`false` is `off`) and `interactive` (`false` is `login`) booleans. |
| `workflow.validation_env.path` | array&lt;string&gt; · **Default:** `[]`<br>`PATH` entries for required validation and `local_shell` steps, combined with the resolved `PATH` per `path_mode`. A leading `~/` expands to `HOME`. Empty adds nothing. |
| `workflow.validation_env.path_mode` | **Advanced.** string · **Default:** `prepend`<br>How `workflow.validation_env.path` combines with the resolved `PATH`: `prepend` puts it first, `replace` makes it the whole `PATH`. |
| `workflow.validation_env.build_output_env` | array&lt;string&gt; · **Default:** `[]`<br>Environment variable names bound to managed host output for required validation and host evidence. Configured bindings replace inherited values; commands must honor them. Empty allocates nothing. See [Workspace build output](#workspace-build-output). |
| `workflow.validation_env.cargo_incremental` | **Advanced.** bool · **Default:** `false`<br>When a host validation target is bound, force `CARGO_INCREMENTAL=0` on required validation, host evidence, baseline reruns and recovery. `true` leaves the variable untouched. No binding does not set it. |
| `machine.name` | string · **Default:** Set by `orbit init`<br>Global only. This machine's display name, and the one `[machine]` value you can change. |
| `machine.labels` | array of strings · **Default:** `[]`<br>Global only. Capability labels this machine carries (`["gpu", "xcode"]`) for tasks whose `requires_labels` name them; a host may start such a task only when it carries every label. Its OS (`linux`, `macos` or `windows`) is an implicit label. Labels are trimmed and lowercased, and use ASCII letters, digits, `-`, `_`, `.`, `:` and `/`, up to 64 bytes. `orbit doctor` prints the effective set. |
| `machine.id` | string · **Default:** Set by `orbit init`<br>Global only, read-only. This machine's stable generated identity (`hm_…`), written once by `orbit init` and never reused. `orbit config set` refuses it. |
| `machine.task_prefix` | string · **Default:** Set by `orbit init`<br>Global only, read-only. The task-ID namespace for IDs minted on this machine (2–5 uppercase ASCII letters), chosen once by `orbit init`. `orbit config set` refuses it. |
| `machine.host_shutdown_hold_minutes` | **Advanced.** integer · **Default:** `180`<br>Global only. Hold new unattended admissions only when a scheduled host shutdown or reboot is this many minutes away or sooner (`>= 1`). A later schedule is still reported and admission continues. |
| `machine.worker_containment` | string · **Default:** `warn`<br>Global only. How detached pipeline workers are bounded: `warn` runs each in its own systemd user scope, bounded by the `machine.worker_*` limits, and runs it uncontained with a warning where no user manager is reachable; `strict` refuses that launch instead; `off` never uses a scope. Needs Linux with a user manager for a scope. A boolean still loads with a warning (`true` is `warn`, `false` is `off`), as does the deprecated `machine.worker_containment_strict` (`true` is `strict`). |
| `machine.worker_memory_high` | **Advanced.** string · **Default:** `40%`<br>Global only. Worker scope `MemoryHigh=` (throttle point): a size such as `6G`, a percentage of RAM, or `infinity`. |
| `machine.worker_memory_max` | **Advanced.** string · **Default:** `50%`<br>Global only. Worker scope `MemoryMax=` (OOM point), same format. |
| `machine.worker_tasks_max` | **Advanced.** integer · **Default:** `4096`<br>Global only. Worker scope `TasksMax=`. |
| `machine.worker_cpu_quota` | **Advanced.** integer · **Default:** unset<br>Global only. Worker scope `CPUQuota=` as a percentage of one core (`400` = four cores); `0` or unset sets no CPU limit. Linux only. |
| `machine.auto_concurrency.mode` | string · **Default:** `off`<br>Global only. Forecast admission mode: `off`, `shadow`, or `enforce`. `off` leaves admission unchanged. `shadow` records what gap fit would admit in each local drain pass and admits as `off`. `enforce` admits local candidates whose profile fits under the forecast. Under `orbit run auto --pull`, `enforce` sends the fit list with each pull request and requests nothing while nothing fits; `shadow` records that list and requests as with `off`. |
| `machine.auto_concurrency.admit_cpu_percent` | integer · **Default:** `90`<br>Global only. Forecast CPU utilization as a percentage of host cores (1–100); distinct from the default Linux load-based throttle signal (`workflow.resource_throttle.cpu_source = "load"`). |
| `machine.auto_concurrency.admit_memory_percent` | integer · **Default:** `75`<br>Global only. Forecast non-reclaimable memory as a percentage of host memory (1–100), strictly below global `workflow.resource_throttle.memory_high_percent`. |
| `machine.auto_concurrency.profile_window` | **Advanced.** integer · **Default:** `50`<br>Global only. Most recent completed deliveries retained per profile key; at least `min_profile_deliveries`. |
| `machine.auto_concurrency.min_profile_deliveries` | integer · **Default:** `10`<br>Global only. Minimum completed deliveries before using a profile key rather than falling back. |
| `machine.auto_concurrency.horizon_cap_minutes` | **Advanced.** integer · **Default:** `120`<br>Global only. Maximum forecast horizon for a candidate, in minutes. |
| `machine.auto_concurrency.head_max_wait_minutes` | **Advanced.** integer · **Default:** `30`<br>Global only. Maximum queue-head wait before the starvation guard stops backfilling, in minutes. |
| `machine.auto_concurrency.head_stuck_passes` | **Advanced.** integer · **Default:** `20`<br>Global only. Consecutive protected passes whose miss has not improved before a drain reports the queue head stuck: an `enforce` drain files one friction, and `orbit run show` and `orbit run readiness` print a `Stuck head:` line while it lasts. Admission is unchanged. At least `1`. |
| `tasks.id_start` | integer · **Default:** Unset<br>Floor for this machine's task-ID allocator. Moves only forward, so machines can hold disjoint ID ranges. |
| `ci_failure.operator_suppression_hours` | **Advanced.** integer · **Default:** `6`<br>Hours an archived/rejected CI sweep finding without `covered_by` holds its exact failure key (0–720). Explicit task/PR covers hold while open or until a failing checkout contains the landed fix; operator holds report their owner and reason. |
| `publication.routine_attachments` | string · **Default:** `omit`<br>Weekly task publication attachment policy: `omit`, `fail`, or `include`. See [Scheduled publication](../../how-to/task-publication/#scheduled-publication). |
| `publication.routine_allow_unscanned_attachments` | **Advanced.** boolean · **Default:** `false`<br>Explicitly accept unscanned attachments when the routine policy is `include`; sensitive path and size limits still apply. |
| `automation.stall_window_minutes` | **Advanced.** integer · **Default:** `60`<br>Minutes a delivery-automation deferral may persist before Orbit logs a warning and files one friction (1–1440). |
| `decision_channel.enabled` | bool · **Default:** `false`<br>Raise decision requests for work held on a human decision. |
| `pr.close_on_terminal` | **Advanced.** bool · **Default:** `true`<br>Close a task's open Orbit-authored pull requests, including preservation PRs for blocked tasks, when the task lands, is rejected, or is archived, with a comment naming the landing or decision. Branches are kept; a forge error is a warning, never a failure. |
| `pr.delivery_authors` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Forge logins whose pull requests count as Orbit-authored for `pr.close_on_terminal` and for settling a task to done from its delivery pull request merged outside Orbit. Empty means the login the forge CLI is authenticated as on this machine. |
| `pr.title_template` | **Advanced.** string · **Default:** Unset<br>Template for a single-task pull request title, using the [delivery template placeholders](#delivery-templates). Unset uses the task title. |
| `pr.body_template` | **Advanced.** string · **Default:** Unset<br>Pull request body template: `"repo"` (the repository's `pull_request_template.md`) or a repository-relative path, read at the delivery's base revision. Unset uses Orbit's own layout. |
| `pr.agent_sections` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Body headings the implementer may fill through the `pr-sections.json` task artifact. |
| `pr.required_sections` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Body headings the implementer must fill. An unfilled one is handled by `pr.missing_required_section` and recorded in task history. |
| `pr.missing_required_section` | **Advanced.** string · **Default:** `draft`<br>What happens when a required section is unfilled: `draft` opens the pull request as a draft with a note under the heading; `refuse` opens no pull request. |
| `pr.screenshot_section` | **Advanced.** string · **Default:** `auto`<br>When a single-task pull request body carries a `## Screenshots` table of the task's `screenshots/` image artifacts: `auto` for tasks tagged `ui`, `always`, or `off`. |
| `pr.freshness_section` | **Advanced.** bool · **Default:** `true`<br>Include the Branch Freshness section (and the `{{freshness}}` value) in pull request bodies. |
| `pr.signature` | **Advanced.** bool · **Default:** `true`<br>Append the authoring model's signature to pull request bodies. |
| `pr.orchestration_trailer` | **Advanced.** bool · **Default:** `true`<br>Append the `Orchestrated-By` trailer to pull request bodies. |
| `pr.labels` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Labels applied to each pull request Orbit opens. |
| `pr.task_tag_labels` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Exact, case-sensitive allowlist of task tags mirrored as PR labels after creation or reuse. |
| `pr.task_type_labels` | **Advanced.** table&lt;string,string&gt; · **Default:** `{}`<br>Map `feature`, `bug`, `refactor` or `chore` to label names, e.g. `{ bug = "bug", feature = "enhancement" }`. Unknown types are refused. Task labels are unioned across the delivery; missing labels are created and existing labels accepted. Names must be non-empty and cannot start with `-`. Failures warn and are recorded as `task_labels.applied` / `.failed` in the PR step output without failing the step. Empty settings add no commands. Later task edits do not sync or remove labels. |
| `pr.reviewers` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Reviewers (logins or `org/team`) requested on each pull request Orbit opens. |
| `pr.draft` | **Advanced.** bool · **Default:** `false`<br>Open every pull request as a draft. A draft is never merged automatically. |
| `commit.subject_template` | **Advanced.** string · **Default:** Unset<br>Template for single-task commit subjects; must contain `[{{task_id}}]`. Subjects keep a 72-character budget: the title is shortened first, with an ellipsis, and kept whole in the body. |
| `commit.body_template` | **Advanced.** string · **Default:** Unset<br>Template for single-task commit bodies, before the trailers. |
| `commit.planned_by_trailer` | **Advanced.** bool · **Default:** `true`<br>Add the `Planned-By` trailer to commit messages that carry it. |
| `commit.implemented_by_trailer` | **Advanced.** bool · **Default:** `true`<br>Add the `Implemented-By` trailer to commit messages that carry it. |
| `commit.orchestrated_by_trailer` | **Advanced.** bool · **Default:** `true`<br>Add the `Orchestrated-By` trailer to commit messages. |
| `branch.name_template` | **Advanced.** string · **Default:** `{{prefix}}/{{task_id}}-{{hash}}`<br>Template for single-task delivery branch names; must contain `{{task_id}}` and `{{hash}}`. A name containing `..` or starting with `-` is refused. |
| `pr.task_url_template` | **Advanced.** string · **Default:** Unset<br>URL template that links a task ID in PR descriptions. |
| `execution.env.pass` | array&lt;string&gt; · **Default:** `HOME`, `PATH`, `CODEX_HOME`, `TMPDIR`, `USER`<br>Environment variables passed to agent subprocesses. Replaces the default list rather than extending it. macOS also passes `__CF_USER_TEXT_ENCODING` by default. A name you add that the launching shell does not set reaches no agent: drains, `orbit run ship` and `orbit run job` warn on stderr and record it on the run, and `orbit doctor` reports it (start drains from a login shell). Clock-started runs read admitted names from `~/.orbit/clock.env`; see [Scheduled credentials: clock.env](#scheduled-credentials-clockenv). |
| `execution.codex.sandbox` | string · **Default:** `workspace-write`<br>Codex sandbox mode: `read-only`, `workspace-write`, or `danger-full-access`. The global file `orbit init` writes sets `danger-full-access`. |
| `execution.codex.approval_policy` | **Advanced.** string · **Default:** Unset<br>Codex approval policy: `untrusted`, `on-request`, or `never`. |
| `execution.proc_spawn_max_timeout_minutes` | **Advanced.** integer · **Default:** `45`<br>Longest timeout one `proc.spawn` call may use inside a managed activity, also capped by the activity's remaining wall-clock budget (1–1440). Outside an activity the ceiling stays 60 seconds. |
| `execution.agent_stall_minutes` | **Advanced.** integer · **Default:** `30`<br>Minutes a provider process may produce no output and use no CPU before its step fails as stalled and is retried. `0` disables the check and leaves only the wall-clock timeout. At most 1440. |
| `runtime.log_max_file_mb` | **Advanced.** integer · **Default:** `100`<br>Roll the operational `orbit.jsonl` past this size (MiB). At least 1 and at most `runtime.log_max_total_mb`. |
| `runtime.log_max_total_mb` | **Advanced.** integer · **Default:** `500`<br>Operational `orbit.jsonl` archive budget in MiB; the oldest are pruned first. |
| `runtime.log_retention_days` | integer · **Default:** `7`<br>Delete archives in both operational and agent feeds older than this. |
| `retention.audit_days` | integer · **Default:** `60`<br>Days `orbit gc audit` keeps audit rows (1–36500). Older command-audit rows, this workspace's run-audit rows, and run-audit rows whose workspace id is no longer registered become reclaimable, along with audit blobs no remaining row names. Another registered workspace keeps its own window. Health > Events, Incidents, Denials, `orbit audit` and diagnostics friction lose older raw history, with no rollups. Durable lock-denial facts preserve Lock contention. Nothing is deleted until you run `orbit gc audit --apply` or enable the `store-gc` routine. |
| `retention.runs_days` | integer · **Default:** `60`<br>Days after a run finishes before `orbit gc runs` may drop its pipeline state (30–36500). Run rows, steps, summaries and commit outcomes stay, preserving succeeded implement_one durations. Unregistered workspace ids are included. Consider 30 days on a large store; smaller values, including command overrides, are refused because Reliability Pipeline echo dedupe and forecast-accuracy per-drain limiter shares need 30 days of run state. Older raw checkpoints are lost. Resource minutes and admission forecasts separately retain a fixed 30 days, preserving the supported auto-concurrency and forecast-accuracy window. |
| `retention.journal_days` | **Advanced.** integer · **Default:** `14`<br>Days after a task commit is applied before `orbit gc journal` may delete its commit journal row (1–36500). Recovery replays only prepared and committed rows, so an applied row is kept only for diagnosis; prepared, committed and aborted rows, and each task's newest applied row, are never deleted. Unregistered workspace ids are included. Older applied-transition diagnostics are lost; published task history and recovery inputs stay. Nothing is deleted until you run `orbit gc journal --apply`. |
| `retention.store_warn_bytes` | **Advanced.** integer · **Default:** `1073741824`<br>Warn from `orbit doctor` when the host store file is larger than this many bytes and no store-gc routine is enabled (1 GiB). Zero warns for any non-empty file. The check stays ok under the threshold or while the routine is enabled. |
| `worktree.reclaim` | **Advanced.** array&lt;string&gt; · **Default:** `["target", ".orbit/tmp/*-target", ".orbit/tmp/base-wt"]`<br>Rebuildable paths in kept terminal run worktrees. Relative globs use `*` within a component and whole-component `**` across components. An explicit list replaces earlier layers; `[]` disables reclamation. Absolute, parent and root-matching patterns fail config load. Only ignored or untracked paths without tracked content are deleted, after registration, confinement, symlink and worker checks. Inspect with `orbit gc worktrees --reclaim`, apply with `--confirm`. |
| `worktree.reclaim_below_free_mib` | **Advanced.** integer · **Default:** Unset<br>Reclaim during admission when free MiB under the state directory falls below this threshold, oldest terminal worktrees first. Active runs and live or undecidable workers stay protected. |
| `worktree.parked_reclaim_after_hours` | **Advanced.** integer · **Default:** `336`<br>Hours a task must stay parked in `someday`, counted from that status change, before worktree GC removes a terminal run's worktree serving it (0–876000). A live worker, an unsettled claim or a Git worktree lock keeps it. A clean worktree whose run branch holds commits no other ref reaches and `origin` lacks stays, branch included, as `skipped:unpublished_commits`. |
| `security_alert_sweep.min_severity` | **Advanced.** string · **Default:** `moderate`<br>Lowest severity the security alert sweep files for Dependabot and code-scanning alerts: `low`, `moderate`, `high`, or `critical`. Run input overrides the workspace value, which overrides the global one. Secret-scanning alerts are always filed. |
| `review.timing` | string · **Default:** `off`<br>When automatic review runs before landing, by a fresh reviewer that fixes what it finds: `off`; `before_pr` holds PR creation for the review; `before_landing` opens the PR first, reviews it while hosted CI runs, and merges only the head the review settled, leaving the PR open with the task in `review` on any outcome but an approve. Refused for local-only delivery. A run keeps the value it was submitted with. Replaces the deprecated `review.before_pr` and `review.before_landing` booleans, which still load (both `true` fails the load: there is one review layer before landing). |
| `review.before_landing_hosts` | array&lt;string&gt; · **Default:** `[]`<br>Owner policy for the distributed drain: machine ids (`hm_…`) whose claimed leaves run before-landing review while this owner's own deliveries do not. The owner resolves it for the caller's machine when a follower probes or pulls, and the claim captures the result. Orbit fails to load config while `review.timing` is `before_pr` and the list is non-empty. The machine label is chosen by the caller, so this is policy, not a security boundary. |
| `review.minutes` | integer · **Default:** `30`<br>Time limit for one candidate's before-PR or before-landing review (1–1440). Each candidate gets one review; a changed candidate is a new one. |
| `review.baseline_commands` | array&lt;string&gt; · **Default:** `[]`<br>Commands before-PR review may rerun on the host to confirm that a failed required check fails the same way on the pinned base. `workflow.required_validation_commands` always count. A confirmed claim holds the task in the backlog until the base passes; a claim about any other command settles the review incomplete. A listed command's failure cannot be recorded as a diagnostic: it needs a passing record or a confirmed claim. The list is captured when a delivery or claim is admitted. |
| `review.verify_findings` | **Advanced.** bool · **Default:** `false`<br>Review findings must be verified before approval. When on, a review-filed bug (tagged `code-review`, `delivery-code-review`, `security-review` or `qa-sweep`) is refused at approval and promotion and kept out of drains until `finding_verification_pipeline` confirms it. When off, verification state is ignored. Read each time the gate is checked; toggle it with `orbit config set` or the dashboard's Settings › Effective switch. Turning it on does not enable the `finding_verification` routine. |

Agent stdout/stderr relay uses `~/.orbit/state/logs/orbit-agent.jsonl` with a separate 200 MiB archive budget and 50 MiB file limit. Its output cannot evict operational events from `orbit.jsonl`. A full agent queue blocks the relay instead of dropping lines; the operational feed can still drop a line when its own queue is full. The dashboard log dock, `/api/log`, `/api/log/stream`, and `orbit log tail` merge the two active feeds; per-run captures remain available separately. `runtime.log_retention_days` applies to both feeds. Size limits still take precedence over age when either feed's own budget is exhausted; seven days of operational history requires its non-relay volume to fit the operational budget.

Log snapshots retain the numeric operational `offset` and add `agent_offset`. Resume SSE with `from` and `agent_from`; split-feed event IDs carry both byte cursors as `operational:agent`. Custom log paths with a filename other than `orbit.jsonl` read that file alone.

Notes:

- Pool entries are `name` or `name:weight`, all bare or all weighted. An empty
  pool sends that tier to `default_crew`.
- Each resource-throttle percentage is 1–100, and each resume mark must be
  lower than its high-water mark.
- `orbit run ship --strict-worker-containment` and
  `orbit run auto --strict-worker-containment` turn on strict containment for
  one run, whatever `machine.worker_containment` says.
- After-landing review is not a config key. Turn it on with
  `orbit auto-task toggle delivery-code-review on`; it reviews landed
  deliveries in batches, observing `origin/<branch>` when that remote exists.
  `orbit config show --all` and `orbit doctor` report both review switches, and
  doctor is not ok while the observed commit trails the remote-tracking head
  past the batch's maximum wait.
- `operation.review_policy` and `operation.review_minutes` are deprecated and
  still load with a warning: `before-pr` becomes `review.timing = "before_pr"`,
  `after-landing` turns on the `delivery-code-review` auto-task until you
  toggle it yourself, and `none` turns neither on. A later release rejects
  them.
- `operation.review_reviewer_starts` and `operation.review_repair_cycles` are
  retired and ignored with a warning.
- The keys these consolidate still load with a deprecation warning:
  `operation.review_crew`, `workflow.final_recovery_crews`, the
  `ci_failure_sweep`, `dependabot` and `merged_head_repair` purpose pools,
  `review.before_pr`, `review.before_landing`, a boolean
  `machine.worker_containment`, `machine.worker_containment_strict`, and
  `workflow.validation_env.login_shell` and `interactive`. A file that also
  sets the replacement keeps the replacement's value. `orbit doctor
  --fix-retired-config-keys` writes each value under its replacement and then
  deletes the old key. A later release rejects them.

Crew fields are settable as `crews.<name>.<field>`, where the field is
`model`, `provider`, `enabled`, `effort`, `description`, `tags`, or `env`
(`env` requires `--global`); for
example, `orbit config set crews.sol.effort high`. `orbit config set` cannot
create a crew: first add a `[crews.<name>]` table with `model` and `provider`.
An installed plugin's declared settings are settable as `plugins.<ns>.<key>`.

The scheduled worktree GC also reclaims declared output in every kept terminal
worktree. Candidate commits, branches, tracked files and unmatched `.orbit/tmp`
evidence remain; resumed runs rebuild reclaimed output. Collection never runs a
cleanup command: checkout-authored Cargo configuration can redirect `cargo clean`
outside the worktree, and Make or npm scripts can execute code with host authority.
Declare only rebuildable paths; evidence matching a declared pattern is reclaimable.

Cost, task-store Codebase rankings, `orbit task flow` and friction stats keep working
across audit, run and journal pruning. Upgrade backfills retained denial and commit
facts once before pruning; history deleted before upgrade cannot be recovered.

## Delivery templates

The `[pr]`, `[commit]` and `[branch]` templates shape what a single-task
delivery publishes. With none set, Orbit's own titles, bodies, commit messages
and `orbit/<task>-<hash>` branches are unchanged. A delivery that bundles
several tasks keeps Orbit's own shapes.

```toml
[pr]
title_template = "{{type}}: {{title}} ({{ext.jira}})"
body_template = "repo"
agent_sections = ["Risk"]
required_sections = ["Rollback"]
missing_required_section = "draft"
labels = ["orbit"]
reviewers = ["my-org/reviewers"]

[commit]
subject_template = "{{type}}({{ext.jira}}): {{title}} [{{task_id}}]"

[branch]
name_template = "feature/{{ext.jira}}-{{task_id}}-{{hash}}"
```

Placeholders are written `{{name}}`; spaces inside the braces are ignored.
A template naming an unknown placeholder fails the configuration load with
the key and the placeholder.

| Placeholder | Value | Where |
|---|---|---|
| `{{task_id}}` | The task's ID | Everywhere |
| `{{title}}` | The task title | PR title and body, commit |
| `{{type}}` | `feat`, `fix`, `refactor` or `chore`, from the task type | PR title and body, commit |
| `{{description}}` | The task description | PR title and body, commit |
| `{{acceptance_criteria}}` | The acceptance criteria as a checklist | PR title and body, commit |
| `{{execution_summary}}` | The implementer's execution summary | PR title and body, commit |
| `{{validation}}` | The validation section | PR title and body, commit |
| `{{freshness}}` | The branch's base and head and how far it is behind and ahead; empty in commits or with `pr.freshness_section = false` | PR title and body, commit |
| `{{task_url}}` | The task link from `pr.task_url_template`; empty when unset | PR title and body, commit |
| `{{ext.<system>}}` | The ID of the task's external ref for `<system>`, such as `{{ext.jira}}`; empty when the task has none | Everywhere |
| `{{prefix}}` | The delivery's branch prefix: `orbit` unless a job sets another | Branch only |
| `{{hash}}` | Eight hex characters derived from the run, so a retry reuses the branch | Branch only |

`pr.body_template = "repo"` reads the repository's pull request template
at the delivery's base revision, never from the candidate branch. Orbit looks
for `pull_request_template.md` (any letter case) in `.github/`, the
repository root and `docs/`, in that order, and uses its own layout when none
exists. A repository-relative path must exist at the base revision. When a
template is used, Orbit appends an HTML comment naming the task
(`<!-- orbit-delivery-task: <TASK-ID> -->`). A repository template does not
have to contain `{{task_id}}`. Delivery still recognizes the pull request
from its branch, an Orbit author, and that task id as a whole token in the
body; the comment supplies the token. The template's own text is left as
rendered.

The implementer fills agent and required sections by attaching a
`pr-sections.json` task artifact shaped
`{"sections": {"Risk": "Low: one file."}}`. Each heading replaces the body
under the matching Markdown heading of the template, matched without regard
to letter case. With Orbit's own layout, filled sections are appended as
`## <heading>`. An unfilled optional section keeps the template's text. An
unfilled required section is never published as an empty heading: with
`missing_required_section = "draft"` the pull request opens as a draft with a
note under the heading, and with `"refuse"` no pull request opens. Both are
recorded in the task's history. Orbit never merges a draft automatically.

With `pr.screenshot_section` on (`auto` turns it on for tasks tagged `ui`),
Orbit renders a `## Screenshots` table of Before and After images from the
task's artifacts named
`screenshots/<view>-<theme>-<width>[-before|-after].<ext>`; a name without a
suffix is an After shot. Only PNG, JPEG, GIF and WebP images up to 1 MiB are
published. Orbit commits them to `refs/orbit/evidence/<task-id>` on `origin`,
a ref that is never merged or deleted, and links each image by that commit's
sha, so a merged pull request keeps its images. A template's `## Screenshots`
heading is filled in place; otherwise the section is added. With no
screenshot, the section carries the missing-section note and
`missing_required_section` applies.

Commit subjects keep a 72-character budget: the title is shortened first, with
an ellipsis, and kept whole in the commit body. A subject template must carry
the `[{{task_id}}]` marker Orbit uses to recognise delivered work. A branch
template must contain `{{task_id}}` and `{{hash}}`; a branch name that would
contain `..`, start with `-`, or break Git's ref-name rules is refused.

## Workspace build output

Configure the output locator your workspace's build commands consume:

```toml
[workflow.validation_env]
build_output_env = ["CARGO_TARGET_DIR"] # Rust workspace
```

A non-Rust workspace can use `build_output_env = ["DIST_DIR"]` and have its
validation command invoke `cmake -S . -B "$DIST_DIR"` followed by
`cmake --build "$DIST_DIR"`. Orbit has no toolchain-specific output default.

The built-in list is empty. A workspace list replaces the global list, including
an explicit `[]`. Empty configuration allocates no managed output and leaves
inherited variables unchanged. Names are trimmed, deduplicated and sorted;
they must be shell environment identifiers. Execution identity variables
(`PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `TMPDIR`, `TMP`, `TEMP`, `PWD`,
`OLDPWD`) and `ORBIT_` names are reserved, ignoring case.

After environment resolution, every configured variable points to the same
absolute host-owned directory, outside worker-writable build areas:
`<primary-repository>/.orbit/state/host-validation-targets/<sha256(canonical-source-checkout)>`.
Different source checkouts have separate output; repeated host checks for one
source can reuse it. Commands must honor the locator to preserve that isolation.
Bindings apply to required validation, host replay and detached evidence,
including baseline and recovery checks, on every platform. They do not apply
to agent sessions or ordinary `local_shell` steps. When a binding is in
effect, those commands also receive `CARGO_INCREMENTAL=0` unless
`workflow.validation_env.cargo_incremental` is `true`. Logs record the resolved
`build_output` environment, directory, source and host context.

Managed cleanup retains active, leased or ambiguous output and can reclaim
eligible inactive output even after its checkout is gone. Existing
`host-validation-targets` remain attributable and reclaimable when bindings
are disabled. Enable the workspace's output locator before rolling out a
runtime that removes implicit Cargo routing; keep active clients' output
contexts intact. Configuration examples describe an operator rollout,
not an automatic change to the owning workspace's file.

## Root override

Most commands accept the global `--root` option, which overrides the Orbit root
directory:

```bash
orbit --root /path/to/orbit-root task list
```

## Retired backend selection

Agent activities always run through the provider's CLI. The `--backend` flag,
`ORBIT_BACKEND`, `[runtime] backend`, and `[crews.<name>] backend` were
removed, since there is no backend left to select.

Orbit still recognizes old declarations, so nothing is silently reinterpreted:
`cli` is accepted and ignored, while `http` and `auto` are rejected with a
migration message. Remove the setting.

## Workspace state

Workspace state lives in the repository's `.orbit/` directory, including
routine definitions in `.orbit/routines/` and the auto-task definitions that
`orbit auto-task` manages. Global state lives under `~/.orbit/`, created by
`orbit init`. See [Scoping Rules](../scoping/).
