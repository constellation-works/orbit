---
title: Configuration
description: "Where Orbit reads configuration, how to define crews and crew pools, and every settable key."
---

Orbit reads a global `config.toml` and one per workspace. Use this page to
look up a key's type and default, define crews and their pools, and give
agents the credentials they need.

## File locations

| Path | Scope |
|---|---|
| `~/.orbit/config.toml` | Global |
| `~/.orbit/clock.env` | Global, machine only. Credentials for clock-started runs; see [Scheduled credentials: clock.env](#scheduled-credentials-clockenv) |
| `~/.orbit/hosts.toml` | Global, machine only. Remote hosts; see [Host registry](#host-registry) |
| `.orbit/config.toml` | Workspace, per user (gitignored) |

Each key resolves on its own: the workspace value wins, then the global value,
then the built-in default. Tables merge setting by setting; scalars and arrays
replace the global value whole. Crews merge by name and field, so a workspace
can override one crew's `model` without restating the crew.

Three security keys never inherit from global once a workspace file exists:
`execution.codex.sandbox`, `execution.codex.approval_policy` and
`execution.env.pass`. A workspace file that omits one gets the built-in
default.

```bash
orbit config path              # the config.toml in effect
orbit config show              # values with their source
orbit config show --all        # every key, plus summaries
orbit config get workflow.default_crew
orbit config set workflow.default_crew opus
orbit config keys              # every settable key, with type, allowed values and description
```

- `orbit config show` hides **Advanced** keys that no file sets; `--all`
  lists them. `--scope global|workspace` resolves one file alone. `--json`
  always includes every key.
- `orbit config set` writes the workspace file, or the global one with
  `--global`. When the workspace file does not exist yet, add `--fresh` to
  start it empty or `--seed-from-global` to start from a copy of the global
  file.
- `orbit init` writes the global file when it is absent and never rewrites an
  existing one. `orbit init --force` resets the global root, including the
  machine identity; a non-interactive reset requires `--machine-name` and
  `--task-prefix`.

Workspace state lives in the repository's `.orbit/` directory and global state
under `~/.orbit/`; see [Scoping](/reference/scoping/).

### What orbit init seeds

A new global `config.toml` sets these values explicitly. Several differ from
the built-in defaults listed under [Settable keys](#settable-keys).

- **Crews.** Every built-in crew, enabled when its provider CLI is detected
  and disabled otherwise, plus a [human crew](#human-crews). `default_crew`
  is the default crew of the first detected provider family.
- **Complexity pools.** From the detected families:

  | Detected | `low` | `medium` | `hard` | `xhard` |
  |---|---|---|---|---|
  | Codex and Claude | `haiku, luna` | `terra, sonnet` | `sol, opus` | `fable, astra` |
  | Codex only | `luna` | `terra` | `sol` | `astra` |
  | Claude only | `haiku` | `sonnet` | `opus` | `fable` |

  Other providers add to these pools. Grok appends `grok` to `medium`.
  Antigravity (`agy`) appends `gemini-flash` to `low` and `argon` to `hard`.
  Muse appends `muse-spark` to `low`, `muse-wonder` to `medium` and
  `muse-wonder-full` to `hard`. Copilot adds nothing.
- **Purpose pools.** Init keeps only the members whose crew it seeds enabled
  and omits a pool left empty, except `final_recovery`, which it writes as
  `[]` (final recovery off). It does not write `sweep_tasks` or
  `pilot_warning_triage`.

  | Purpose | Members |
  |---|---|
  | `review` | `sol`, `sonnet`, `muse-wonder`, `grok`, `opus@medium` |
  | `task_pilot` | `luna@high`, `haiku@high`, `muse-spark@high` |
  | `failure_recovery` | `luna`, `haiku`, `muse-spark`, `grok` |
  | `auto_fix` | `luna`, `haiku`, `muse-spark` |
  | `final_recovery` | `sol`, `muse-wonder`, `opus@medium` |
  | `baseline_red_repair` | `terra`, `sonnet`, `muse-wonder`, `grok` |
  | `pre_landing_review` | `luna`, `haiku`, `muse-spark` |

- **Other keys.**

  | Key | Seeded value | Built-in default |
  |---|---|---|
  | `experiments.enabled` | `true` | `false` |
  | `workflow.mid_run_steering.enabled` | `true` | `false` |
  | `workflow.unlocked_context_paths` | `["**/*.json", "**/*.txt", "**/*.md", "**/mod.rs", "**/*.toml"]` | `["**/*.md"]` |
  | `workflow.task_pilot_freshness.material_fields` | `["description", "criteria"]` | also `title`, `plan`, `selectors` |
  | `execution.codex.sandbox` | `"danger-full-access"` | `workspace-write` |
  | `machine.auto_concurrency.mode` | `"enforce"`, when init creates this machine's identity | `off` |

- **Review-skip profiles** for the shipped auto-tasks' `auto-task:<name>`
  tags, limited to documentation and CI paths. See
  [Review-skip tags](#review-skip-tags).

To use a seeded value in a config that already exists, set the key yourself:

```bash
orbit config set --global experiments.enabled true
```

## Provider credentials

No provider credential reaches an agent by default: neither the seeded config
nor the built-in `execution.env.pass` includes one. Configure the one
authentication route you intend to use.

| Authentication route | Agent variable |
|---|---|
| Claude dedicated worker token (`claude setup-token`) | `CLAUDE_CODE_OAUTH_TOKEN` |
| Claude API key | `ANTHROPIC_API_KEY` |
| Codex noninteractive API-key override (`codex exec`) | `CODEX_API_KEY` |

`CODEX_API_KEY` is Codex's per-run API-key override; `OPENAI_API_KEY` alone is
not. See
[OpenAI's noninteractive authentication guidance](https://developers.openai.com/codex/noninteractive#use-api-key-auth).

Give the credential to every crew or to one:

- **Shared.** Add the name to the workspace's `execution.env.pass`, keeping
  the existing entries. Your list replaces the default, and a workspace file
  that omits this key gets the built-in default, not the global list.
- **Per crew.** Map a source in the crew's `env`, in the global config:

  ```toml
  [crews.sol]
  env = ["CODEX_API_KEY=CODEX_CREDITS_API_KEY_1"]
  ```

  The right-hand side names a variable in the launching environment, never a
  literal key. Only that crew's agents receive `CODEX_API_KEY`, and neither
  name needs adding to `pass`. `NAME=file:PATH` (absolute, or `~/…`) reads the
  value from an owner-only file (mode `600`, not a symlink) instead. When a
  crew entry sets one Claude credential (`ANTHROPIC_API_KEY` or
  `CLAUDE_CODE_OAUTH_TOKEN`), the other is removed from that crew's agents.

Check the execution host, using your crew's name in place of `sol`. Orbit
reports credentials by name and never prints a value.

```bash
orbit config get execution.env.pass
orbit config get crews.sol.env
orbit doctor --check env-pass --json
```

**If a variable is missing.** A `pass` name the launching shell does not set
reaches no agent, unless a timer-started run reads it from
[clock.env](#scheduled-credentials-clockenv). A manual start whose launching
environment lacks the name still proceeds with a warning, and the provider
may fall back to a saved login.
A missing per-crew source, or an unusable `file:` source, excludes that crew
from drains and refuses its activities.

**Where the value comes from.** Terminal runs read your shell, so start drains
from a login shell. Dashboard runs inherit the web service's environment:
restart the service after you change its exports. Timer-started runs read
[clock.env](#scheduled-credentials-clockenv).

:::caution
On macOS, a Claude activity with neither `CLAUDE_CODE_OAUTH_TOKEN` nor
`ANTHROPIC_API_KEY` in its environment is refused before `claude` starts,
because the Claude Desktop login it would fall back to is revoked mid-run.
Create a token with `claude setup-token` and admit `CLAUDE_CODE_OAUTH_TOKEN`
through `pass` or a crew's `env`.
:::

## Scheduled credentials: clock.env

`~/.orbit/clock.env` supplies credentials to the runs the clock starts.
launchd (macOS) or a systemd user timer (Linux) starts `orbit clock tick` with
no login environment, so the tick reads this file and passes its values to the
runs it starts. For setup, see
[Schedule recurring work](/how-to/recurring-work/#scheduled-credentials).

`orbit routine init --install-clock` creates the file, comment-only and
owner-only, when it is absent.

```ini
# ~/.orbit/clock.env — placeholders; put real values only in the file
CLAUDE_CODE_OAUTH_TOKEN=<token-from-claude-setup-token>
ANTHROPIC_CREDITS_API_KEY_1=<api-key-for-the-credits-crew>
GITHUB_TOKEN=<github-token>
```

- **Format.** One `NAME=value` per line. Blank lines, `#` comments, a leading
  `export ` and one pair of surrounding quotes are accepted. Nothing is
  expanded.
- **Admission.** A name loads only when a workspace's effective
  `execution.env.pass` lists it, or a crew's `env` entry reads it as its
  source. Each workspace's runs receive only the names its own `pass` admits.
  Empty values and names starting with `ORBIT_` are dropped.
- **Precedence.** A non-empty value already in the tick's environment wins.
- **Permissions.** The file must be a regular file, not a symlink, owned by
  the user running the tick, with mode `600`. Otherwise the tick still runs
  but loads nothing and reports a load error naming the fix:

  ```bash
  chmod 600 ~/.orbit/clock.env
  ```

- **Scope.** There is no workspace `clock.env`, the web service does not
  read the file, and `orbit clock tick --dry-run` loads nothing.

To confirm what loaded, `orbit clock tick --json` lists the names as
`clock_env_loaded`. The `env-pass` row of `orbit doctor` counts a `pass` name as
held when its own environment or a non-empty `clock.env` value supplies it, so
it reports `ok` for a name only `clock.env` holds. A manual drain, ship or job
start judges only its launching shell and still warns for that name. Crew `env`
sources have their own checks: the shell check reads only doctor's environment,
and the managed-service check counts `clock.env` for the sweep service alone.
On macOS, the `claude-worker-token` row reports a missing Claude credential or
crew source.

## Unknown settings

An unknown key or table loads with a warning that names its file and path and,
when one is close, a known key. `orbit doctor` lists unknown settings beside
retired ones.

### Retired settings

A retired key still loads, with a deprecation warning that names its
replacement or the value now fixed. `orbit config get` and `set` refuse it.
Delete it, or run `orbit doctor --fix-retired-config-keys`, which backs up
each affected file, writes renamed values under their replacement and removes
the rest. Most ignored keys stop being recognised on 2026-12-07 or
2026-12-08, as their warning says, and then count as unknown settings.

| Retired | Now |
|---|---|
| `operation.review_policy`, `review.before_pr`, `review.before_landing` | `review.timing`. A legacy `after-landing` policy turns on the `delivery-code-review` auto-task until you toggle it yourself. |
| `operation.review_minutes`, `operation.review_crew` | `review.minutes`, `workflow.purpose_crews.review` |
| `workflow.final_recovery_crews` | `workflow.purpose_crews.final_recovery` |
| The `ci_failure_sweep`, `dependabot` and `merged_head_repair` purpose pools | `workflow.purpose_crews.sweep_tasks`. A file whose old pools differ fails to load until you merge them. |
| `machine.worker_containment_strict`, a boolean `machine.worker_containment` | `machine.worker_containment` set to `off`, `warn` or `strict` |
| `workflow.crew_performance_types` | `workflow.task_types` |
| `workflow.system_crew`, `crew: system` in a template or job | Ignored. System work routes through the [pools](#system-work). |
| `[visual_review]`, `[ux_rules]`, `[lease]`, `[assistance]`, `[workflow.crew_ladder]`, `[workflow.regression_risk]`, `[workflow.explore_brief]`, `[review.selective_before_landing]`, `[machine.web_terminal]`, `[machine.web_session]`, `workflow.explore.*`, `workflow.crew_performance*`, `workflow.auto_ship`, `review.verify_findings`, `publication.routine_*`, the `explorer` and `visual_review` purpose pools | Ignored. The feature was removed. |
| `[attention]`, `[notice]`, `[resident.evolution]`, the `[runtime]` log keys, and any other key under `pr`, `commit`, `workflow`, `machine`, `execution`, `experiments`, `retention`, `automation`, `ci_failure`, `jobs`, `security_alert_sweep` or `worktree` that warns as deprecated | Ignored. The value is fixed, and the warning states it. |

## Host registry

`~/.orbit/hosts.toml` lists the remote Orbit installations this machine
reaches over SSH. Manage it with `orbit host add`, `rename` and `remove`, or
the dashboard's [Observe › Fleet](/how-to/dashboard/#fleet) page. For setup and
prefix routing, see [Run Orbit across hosts](/how-to/multi-host/).

`orbit host add` reads the remote's identity and writes an entry like this
one:

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
| `hosts` | Array of remote entries (`[[hosts]]`). Omitted means no remote hosts. |
| `name` | Required. Defaults to the remote's `machine.name`. At most 128 bytes, no surrounding whitespace, control characters or path separators. Unique ignoring case, including against this machine's name. |
| `machine_id` | Required. `hm_` followed by ASCII letters, digits, `_` or `-`, at most 128 bytes. Unique, and different from this machine's ID. |
| `ssh` | Required SSH alias or `user@host`. No leading `-`, whitespace, control characters or shell metacharacters. |
| `task_prefix` | Required task namespace: two to five uppercase ASCII letters, unique across registered hosts and this machine. |

Workspace configuration does not override this file, and it stores no SSH
credentials. An unknown key, a malformed entry, an unsupported schema
version or a duplicate identity or prefix fails the load and leaves the file
untouched.

### Legacy migration

`~/.orbit/mcp-destinations.toml` is no longer read. While it exists, every
host-registry load fails with `host_file_conflict` and `orbit doctor` reports
an error. Move it aside, run `orbit host add <ssh-target>` for each of its
`ssh` rows, then delete it.

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

[crews.argon]
enabled = false
provider = "antigravity"
model = "gemini-4-argon-high"

[workflow]
default_crew = "sol"
```

| Field | Purpose |
|---|---|
| `provider` | Required. Agent CLI family: `claude`, `codex`, `antigravity`, `gemini`, `grok`, `copilot` or `muse`. |
| `model` | Required. Model ID passed to the provider CLI. |
| `enabled` | Default `true`. `false` keeps the crew listed but refuses to run it. See [Disabled crews](#disabled-crews). |
| `effort` | Reasoning effort. See [Reasoning effort](#reasoning-effort). |
| `vision` | Default `true`: the crew may be given images when its executor declares `image_input`. `false` opts a text-only model out. |
| `description` | Short summary for people. |
| `tags` | Discovery labels. |
| `env` | Variables this crew's agents receive on top of `execution.env.pass`, as `NAME` or `NAME=SOURCE`. Global config only. See [Provider credentials](#provider-credentials). |
| `kind` | `agent` (default) or `human`. See [Human crews](#human-crews). |

Set a field with `orbit config set crews.<name>.<field> <value>`, for example
`orbit config set crews.sol.effort high`; `env` requires `--global`. The
command cannot create a crew: add a `[crews.<name>]` table with `provider` and
`model` first. Crew names cannot contain `@` or `:`.

At dispatch the first crew that is set wins: an explicit crew on the run or
activity input, then the task's `crew`, then `default_crew`. `default_crew`
must name a defined crew. If you define crews and leave it unset, Orbit uses
an `opus` (or legacy `claude`) crew, and otherwise refuses to load.

`gemini` is the legacy Google CLI lane. `ollama` and `openai_compat` are
recognized provider IDs with no shipped executor, so a crew naming one loads
and then fails at dispatch. See [Providers](/concepts/agents/#providers) and
[Set up an executor](/concepts/agents/#set-up-an-executor).

### Disabled crews

`orbit init` writes every built-in crew and disables those whose provider CLI
it does not detect. Turn one on later:

```bash
orbit config set crews.argon.enabled true
```

A disabled crew:

- stays listed in `orbit config show`, `orbit.workspace.list` and the
  dashboard;
- is never drawn from a pool. A pool whose members are all disabled acts as an
  empty pool, so the task gets `default_crew`;
- is refused, never substituted, when a task's `crew`, an explicit crew or
  `default_crew` selects it. The error names the command that enables it.

With no supported agent CLI detected, every crew is seeded disabled and
`default_crew` stays unset, so nothing dispatches until you enable a crew.

### Human crews

A human crew holds work you do yourself, such as renewing a domain. You track
it in Orbit beside agent work, and nothing dispatches it.

```toml
[crews.dk]
kind = "human"
description = "My own work"
```

A human crew takes only `description` and `tags`; any agent field fails the
load. `orbit init` adds one named after your OS user when the global config
has none, or after `--human-name <name>`.

- **Never run by agents.** Drains, pull followers and the task pilot skip its
  tasks, even under `--allow-crew`, and `orbit run readiness` reports each
  with reason `human_crew`.
- **Never drawn.** `default_crew`, crew pools, purpose pools and experiment
  arms may not name it.
- **Closed only by you.** An agent's `orbit.task.update` cannot move its task
  to `done`; agents otherwise keep the normal lifecycle on it.
- **Left out** of crew analytics, performance rankings and provider usage.

You can move a human-crew task from any status to any other with
`orbit task update <id> --status <status>` or the dashboard's status control.
No plan, review or completion evidence is needed. Leaving `done` or `archived`
still needs `--force`, and a run that still owns the task refuses the move.
The dashboard tags these tasks `for <name>` and lists your open ones under
the **My work** chip on the Tasks page.

Write the task body as a short card, because the person doing it copies each
step: a one- or two-line summary, numbered steps, each command in its own
fenced shell block, and a `Verify:` line followed by one fenced command.

````markdown
Renew the domain before it lapses.

1. Open the registrar's renewal page and pay the fee.
2. Run the check below.

Verify:

```sh
whois example.com | grep -i "registry expiry"
```
````

Filing warns, and does not refuse, when the body lacks one of those parts or
runs past 2000 characters.

### Reasoning effort

`effort` is unset by default, which keeps the provider's own default.

| Provider | Accepted values | Rendered as |
|---|---|---|
| `claude` | `low`, `medium`, `high`, `xhigh`, `max` | `--effort` |
| `codex` | `low`, `medium`, `high`, `xhigh`, `max` | `--config model_reasoning_effort` |
| `muse` | `low`, `medium`, `high`, `xhigh`, `max` | `--reasoning-effort` |
| `antigravity` | `low`, `medium`, `high` | `--effort` |
| `grok` with `grok-4.6` or `grok-4.7` | `low`, `medium`, `high`, `xhigh` | `--reasoning-effort` |
| `grok` with `grok-4.5` | `low`, `medium`, `high` | `--reasoning-effort` |
| `grok` with another model, and other providers | None | — |

A crew table's `effort` that its provider does not accept is ignored for that
crew, as if unset. Loading warns with the accepted values, `orbit doctor`
lists it with the fix, and `orbit config set` refuses to write it. The
provider can still reject a value for a particular model.

#### Effort per reference

Anywhere you name a crew, you can pick its effort with `<crew>@<effort>`:
pools, `workflow.default_crew`, a task's `crew` and `--crew`. In a weighted
pool the weight follows: `["opus@medium:100", "opus:100"]`.

- The reference effort wins over the crew's own `effort`.
- `opus@medium` and `opus` are separate pool members, recorded separately in
  runs, scorecards and experiments. They share the crew's account, usage
  limits and holds.
- An effort the provider or model does not accept, such as `grok@max`, fails
  the config load and is refused by `orbit config set`.

#### Effort chosen per draw (`@auto`)

A pool entry can leave the effort to each draw: `opus@auto`, or
`opus@auto:100` in a weighted pool. Complexity pools, type pools, the
task-pilot pool and the two review pools accept it, for a crew that has bands.
Anywhere else it is refused.

```toml
[workflow.auto_effort]
bands.opus = [
  { complexity = ["hard", "xhard"], effort = "xhigh" },
  { below_files = 4, effort = "medium" },
  { below_lines = 400, effort = "medium" },
]
```

A band sets an `effort` and at least one of `complexity`, `below_files` and
`below_lines`. The first band whose every condition matches wins; with no
match, the crew's own `effort` runs. Each role matches on what is known when
it is admitted:

| Role | Signal |
|---|---|
| Implementer | The task's complexity and the number of files its `context_files` declare. `below_lines` never matches. |
| Task pilot | The filed complexity. |
| Reviewer | The candidate's reviewable lines, its changed files and the reviewed tasks' highest complexity. |

Runs, analytics and experiment readouts record the resolved `<crew>@<effort>`,
never `@auto`. A fixed 10% of draws goes to another effort the crew accepts,
recorded with `explored: true`. When an `@auto` pilot raises the task into a
higher-effort band, or reports `medium` or `low` confidence, Orbit re-pilots
the task once at the higher effort.

### System work

Orbit's own work draws a crew from the complexity pools, like a task with no
crew, until you give its purpose a [pool of its own](#purpose-pools). Shipped
auto-task templates name no crew, so each task they mint draws from the pool
for its complexity; a crew set on a template still wins.

A run draws once per pool, by weight, skipping disabled and usage-limited
crews. An empty pool falls back to `default_crew`. Recovery never inherits a
failed task's crew, and a repair whose pool the run cannot use stops instead
of escalating.

### Purpose pools

Give a purpose of work its own weighted pool under `[workflow.purpose_crews]`:

```toml
[workflow.purpose_crews]
task_pilot = ["luna:3", "sonnet:1"]
failure_recovery = ["sol"]
baseline_red_repair = ["opus"]
```

| Purpose | Covers | While unset, draws from |
|---|---|---|
| `task_pilot` | The task pilot | `low` |
| `failure_recovery` | Step-failure and PR conflict recovery | `medium` |
| `auto_fix` | Repairs under `orbit run auto --auto-fix` | `low` |
| `baseline_red_repair` | Tasks the red-base re-check files | A matching [type pool](#type-pools), then the complexity pool |
| `sweep_tasks` | Tasks filed by the CI failure, Dependabot, code-scanning and merged-head repair sweeps | A matching type pool, then the complexity pool |
| `pilot_warning_triage` | The triager of each held task-pilot assessment | `hard` |
| `final_recovery` | Final recovery | Its default pool; `[]` turns it off |
| `review`, `pre_landing_review` | Automatic review | See each key under [Settable keys](#settable-keys) |

For the first five, Orbit picks a crew in this order:

1. An explicit crew on the task, auto-task template or activity input.
2. The purpose's pool, when it has an enabled member. A usage limit or
   failure hold on the drawn member redraws from the pool's other members.
3. The pool in the last column, then `default_crew`.

A task's purpose comes from the tag it was filed with, such as
`baseline-red-repair` or `ci-failure-sweep`, or from a
`crew-purpose:<purpose>` tag an auto-task template adds.
`orbit config show --all` lists each purpose's resolved pool, and
`orbit run show` names the rule behind each crew a run drew.

### Type pools

A task type from `workflow.task_types` can give a complexity tier its own
pool:

```toml
[workflow.type_complexity_crews.ui]
hard = ["opus"]

[workflow.type_complexity_crews.security]
hard = ["fable:3", "opus:1"]
```

A task's type is the first `task_types` entry its tags or context files
match; `other` never has a pool. A typed task with no crew draws from its
type's pool for its tier and records the source `type:<type>/<tier>`. The
full order is an explicit crew, a purpose pool, the type pool, the complexity
pool, then `default_crew`.

Orbit draws at creation, and again when the crew is cleared, the complexity
changes, or a tag or context-file edit moves the task to another type. A
provider hold or usage limit redraws from the pool's other members, then the
complexity pool, then `default_crew`.

The load refuses an undefined type, an unknown tier, an empty pool, an unknown
or disabled crew and a zero weight. A workspace pool replaces only the same
type and tier from global.

## Settable keys

`orbit config set` accepts every key below except the read-only `machine.id`
and `machine.task_prefix`. `orbit config keys` prints the same list.
**Advanced** keys are hidden from `orbit config show` until a file sets them.

| Key | Type, default and purpose |
|---|---|
| `workflow.base_branch` | **Advanced.** string · **Default:** `main`<br>Base branch for ship, auto and the task pilot when the registered workspace has none. |
| `workflow.default_crew` | string · **Default:** See [Crews](#crews)<br>Crew for a task that names none and gets no override. |
| `workflow.low_complexity_crews` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for low-complexity tasks with no crew. See [System work](#system-work) for what else draws from it. |
| `workflow.medium_complexity_crews` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for medium-complexity tasks with no crew. |
| `workflow.hard_complexity_crews` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for hard-complexity tasks with no crew. |
| `workflow.xhard_complexity_crews` | array&lt;string&gt; · **Default:** `[]`<br>Crew pool for xhard-complexity tasks with no crew. |
| `workflow.type_complexity_crews` | table&lt;string,table&lt;string,array&lt;string&gt;&gt;&gt; · **Default:** `{}`<br>Per task type and tier, a weighted pool that replaces the tier's complexity pool. See [Type pools](#type-pools). |
| `workflow.purpose_crews.auto_fix` | array&lt;string&gt; · **Default:** `[]`<br>Pool for repairs under `orbit run auto --auto-fix`. Unset, they draw from `workflow.low_complexity_crews`. See [Purpose pools](#purpose-pools). |
| `workflow.purpose_crews.baseline_red_repair` | array&lt;string&gt; · **Default:** `[]`<br>Pool for the tasks the red-base re-check files. Unset, they draw from their type pool, then their complexity pool. |
| `workflow.purpose_crews.failure_recovery` | array&lt;string&gt; · **Default:** `[]`<br>Pool for step-failure and PR conflict recovery. Unset, they draw from `workflow.medium_complexity_crews`. |
| `workflow.purpose_crews.final_recovery` | array&lt;string&gt; · **Default:** `["sol:100", "opus:20"]`<br>Pool for final recovery, drawn once per run after step recovery is exhausted. The default keeps only members you define. `[]` disables final recovery. |
| `workflow.purpose_crews.pilot_warning_triage` | array&lt;string&gt; · **Default:** `[]`<br>Pool for the triager of each held task-pilot assessment. Only crews whose executor declares `inspection_args` are drawn; with none, it draws from `workflow.hard_complexity_crews`. |
| `workflow.purpose_crews.pre_landing_review` | string or array · **Default:** Unset<br>Crew or pool for before-PR and before-landing review, written and drawn like `workflow.purpose_crews.review`. Unset, that key supplies the reviewer. A review layer that resolves no crew from either key refuses to start. |
| `workflow.purpose_crews.review` | string or array · **Default:** Unset<br>Crew or pool for after-landing review tasks, and for review before landing while `pre_landing_review` is unset. Each review draws one crew by weight, skips disabled and usage-limited crews, and prefers one that did not implement the work. Unset, after-landing review uses its auto-task's template crew. |
| `workflow.purpose_crews.sweep_tasks` | array&lt;string&gt; · **Default:** `[]`<br>Pool for every task a sweep files: CI failure, Dependabot, code-scanning and merged-head repairs. Unset, they draw from their type pool, then their complexity pool. |
| `workflow.purpose_crews.task_pilot` | array&lt;string&gt; · **Default:** `[]`<br>Pool for the task pilot. Unset, it draws from `workflow.low_complexity_crews`. |
| `workflow.provider_limit_overrides` | array&lt;string&gt; · **Default:** `[]`<br>Admission skips a crew while a live usage window of its provider or model is exhausted or at least 90% used. Each entry replaces that threshold for one provider, or for one crew that sets `env`: `name:percent`, or `name:off` to record readings without gating, such as `["claude:80", "codex:off"]`. `100` skips only on exhaustion. |
| `workflow.provider_limit_budgets` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Rolling budgets for providers that report no usage, each `provider:<amount><usd\|tokens>/<n><h\|d>`, such as `["grok:30usd/5h"]`. Counts this host's Orbit runs only. |
| `workflow.provider_limit_explicit_crews` | **Advanced.** string · **Default:** `wait`<br>A task whose explicit crew is usage-limited waits in the backlog (`wait`) or redraws from the unlimited members of its complexity pool (`pool`). |
| `workflow.crew_selection` | **Advanced.** string · **Default:** `static`<br>How pool draws weigh members. `static` uses the configured weights. `usage_pressure` scales each weight by the crew's weekly usage pressure and redraws system-chosen crews at admission. Explicit crews are never redrawn. |
| `workflow.task_types` | **Advanced.** array&lt;table&gt; · **Default:** `[]`<br>Ordered task types for [type pools](#type-pools) and `orbit agents scorecard --role implementers --by-type`. Each `[[workflow.task_types]]` entry has a unique `name` (never `other`) and at least one of `tags` or `paths`. A task takes the first type whose tags it carries or whose globs match its context files, else `other`. |
| `workflow.tag_requires_labels` | **Advanced.** table&lt;string,array&lt;string&gt;&gt; · **Default:** `{}`<br>Additional host capability labels required by each task tag. Every matching tag adds labels to the task's `requires_labels`; tags match after trimming and lowercasing. The owner applies its mapping to pull admission. Workspace entries replace global entries with the same key spelling; an empty array disables that inherited entry. |
| `workflow.activity_timeouts` | **Advanced.** table&lt;string,integer&gt; · **Default:** `{}`<br>Wall-clock seconds per CLI agent activity name (1–43200). For example, `agent_implement = 28800` overrides the shipped 6-hour budget. A workspace table replaces the global table. |
| `workflow.auto_effort.bands` | **Advanced.** table · **Default:** `{}`<br>Per crew, the ordered bands a `<crew>@auto` pool entry resolves through. See [Effort chosen per draw](#effort-chosen-per-draw-auto). |
| `workflow.required_validation_commands` | array&lt;string&gt; · **Default:** `[]`<br>Commands every delivered candidate must pass before push, merge or acceptance of a distributed handoff. Empty runs no required check. |
| `workflow.distributed_completion` | string · **Default:** `review`<br>How far this owner takes an accepted distributed handoff: `review` waits for an operator's Approve handoff; `done` lands it, as `orbit run auto --complete` does. |
| `workflow.unlocked_context_paths` | array&lt;string&gt; · **Default:** `["**/*.md"]`<br>Path globs whose `context_files` entries take no context lock. The agent still receives them as context. A task whose every entry matches still locks them all. `[]` locks every entry. |
| `workflow.lock_modes` | **Advanced.** array&lt;table&gt; · **Default:** `[]`<br>Ordered `[[workflow.lock_modes]]` entries: a `pattern` (a Rust regex searched for in the workspace-relative path) and a `mode`, `exclusive` (default) or `shared`. The first match wins. Tasks that overlap only at shared paths run together; a conflict is resolved when the second lands. |
| `workflow.unlocked_path_validation_commands` | array&lt;string&gt; · **Default:** `[]`<br>Commands a repaired candidate runs instead of the full required list when its changes since the last full pass touch only `workflow.unlocked_context_paths`, such as `["make ci-fast"]`. Empty always runs the full list. |
| `workflow.generated_paths` | table&lt;string,string&gt; · **Default:** `{}`<br>Generated files, as path glob = regenerate command, such as `"docs/INDEX.md" = "make docs-index"`. When a rebase conflicts on a match, conflict recovery runs the command and stages its output instead of merging. |
| `workflow.light_delivery_paths` | array&lt;string&gt; · **Default:** `["docs/**", "**/*.md", "!crates/**", "!plugin/**", ".github/**", "scripts/ci-*", "scripts/test-*-workflow*.py"]`<br>Path globs (a leading `!` excludes) a candidate must stay within for a `skip-review` or `skip-validation` tag to apply. `[]` disables both tags. |
| `workflow.test_paths` | **Advanced.** array&lt;string&gt; · **Default:** `["**/test/**", "**/tests/**", "**/__tests__/**", "**/spec/**", "**/testdata/**", "**/*_test.*", "**/*.test.*", "**/test_*.*", "**/*_spec.*", "**/*.spec.*"]`<br>Path globs whose changed lines count as tests in landed-delivery diff stats. Checked before `workflow.non_code_paths`. |
| `workflow.non_code_paths` | **Advanced.** array&lt;string&gt; · **Default:** `["docs/**", "**/*.md", "**/*.mdx", "**/*.rst", "**/*.txt", "**/*.json", "**/*.yaml", "**/*.yml", "**/*.toml", "**/*.lock", "**/*.csv", "**/*.golden", "**/*.png", "**/*.jpg", "**/*.jpeg", "**/*.gif", "**/*.webp", "**/*.svg", "**/*.ico", "**/LICENSE*"]`<br>Path globs whose changed lines count as other (docs, config, data, assets). A path matching neither list counts as code. |
| `workflow.regression_layers` | **Advanced.** array&lt;table&gt; · **Default:** a `ci_tests` row (`.github/`; `tests`, `testdata` and `fixtures` directories) and a `docs_skills` row (`docs/`; `.md` files)<br>Ordered layers that `orbit codebase analytics` and `show` classify a regression fix into, by path. Each `[[workflow.regression_layers]]` entry has a unique `name` and at least one of `prefixes`, `dirs` or `suffixes`. Setting the list replaces the defaults. |
| `workflow.non_reviewable_paths` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Path globs left out of the reviewable-lines denominator (`cost_per_line`, `lead_time_per_line`). List lockfiles, goldens, snapshots and generated files. |
| `workflow.task_pilot_freshness.material_fields` | **Advanced.** array&lt;string&gt; · **Default:** `title`, `description`, `criteria`, `plan`, `selectors`<br>Task fields checked for drift between task-pilot preparation and apply. Also accepts `tags`, `crew`, `tools`, `type`, `complexity`, `relations`, `dependencies` and `instructions`. |
| `workflow.task_pilot_freshness.source_sensitivity` | **Advanced.** string · **Default:** `ignore`<br>Whether a branch-head change counts as that drift: `ignore`, `context_files` (only when a path the task's selectors name changed) or `any`. |
| `workflow.mid_run_steering.enabled` | bool · **Default:** `false`<br>Lead a run's later prompts with the operator comments written on its task since the run last read them. The shipped pipelines do not commit while one is undelivered. |
| `workflow.mid_run_steering.max_rounds` | **Advanced.** integer · **Default:** `2`<br>Steering rounds a run may spend at the promotion gate before it holds as `steering_unconsumed` (1–10). |
| `workflow.ci_infrastructure_signatures` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Log-line prefixes a failed CI job prints when infrastructure, not the change, failed. A match reruns the failed jobs under the retry keys below, with no repair agent and no CI sweep task. |
| `workflow.ci_infrastructure_retry.max_attempts` | **Advanced.** integer · **Default:** `3`<br>Total failed-job reruns on one PR head for infrastructure failures (0–10; `0` disables). Exhaustion records `ci_infrastructure_retry_exhausted` in task history. |
| `workflow.ci_infrastructure_retry.backoff_seconds` | **Advanced.** integer · **Default:** `60`<br>Initial delay between infrastructure reruns (1–3600), doubled after each. The first rerun is immediate. |
| `workflow.ci_infrastructure_retry.handoff_steps` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Names of CI steps that await or download another job's output. Each pattern matches the whole step name, case-sensitively; `*` matches any characters. When every failed job failed only in such a step, the failure is retried as infrastructure without reading logs. |
| `workflow.resource_throttle.enabled` | **Advanced.** bool · **Default:** `true`<br>Hold new task admission while host CPU, memory, disk space or, on Linux, disk I/O pressure is at or above its high mark. |
| `workflow.resource_throttle.cpu_source` | string · **Default:** `"load"`<br>CPU signal: `load` or `utilization`. macOS measures utilization for either. Readiness and doctor show the active source. |
| `workflow.resource_throttle.cpu_high_percent` | integer · **Default:** `90`<br>CPU high-water mark. With `load` on Linux this is load average divided by online CPUs, which can exceed 100%. |
| `workflow.resource_throttle.cpu_resume_percent` | integer · **Default:** `85`<br>Resume below this CPU percentage. |
| `workflow.resource_throttle.memory_high_percent` | integer · **Default:** `90`<br>Memory high-water mark. Must stay above 75, auto-concurrency's fixed memory limit. |
| `workflow.resource_throttle.memory_resume_percent` | integer · **Default:** `85`<br>Resume below this memory percentage. |
| `workflow.resource_throttle.disk_high_percent` | integer · **Default:** `90`<br>Disk high-water mark, per observed filesystem. |
| `workflow.resource_throttle.disk_resume_percent` | integer · **Default:** `85`<br>Resume below this disk percentage. |
| `workflow.validation_env.path` | array&lt;string&gt; · **Default:** `[]`<br>`PATH` entries put ahead of the resolved `PATH` for required validation and `local_shell` steps. A leading `~/` expands to `HOME`. |
| `workflow.validation_env.build_output_env` | array&lt;string&gt; · **Default:** `[]`<br>Environment variable names bound to a managed host output directory for required validation. See [Workspace build output](#workspace-build-output). |
| `machine.name` | string · **Default:** Set by `orbit init`<br>Global only. This machine's display name. |
| `machine.labels` | array of strings · **Default:** `[]`<br>Global only. Capability labels, such as `["gpu", "xcode"]`. A host starts a task only when it carries every label in the task's `requires_labels`. Its OS (`linux`, `macos` or `windows`) is an implicit label. |
| `machine.id` | string · **Default:** Set by `orbit init`<br>Global only, read-only. This machine's generated identity (`hm_…`), written once by `orbit init`. |
| `machine.task_prefix` | string · **Default:** Set by `orbit init`<br>Global only, read-only. Namespace for task IDs minted on this machine (2–5 uppercase ASCII letters), chosen once at `orbit init`. |
| `machine.worker_containment` | string · **Default:** `warn`<br>Global only. How detached pipeline workers are bounded. `warn`: on Linux each runs in a systemd user scope under the `machine.worker_*` limits (uncontained, with a warning, without a user manager); on macOS a watchdog stops a run past `worker_memory_max` or `worker_tasks_max`. `strict` refuses a launch without kernel containment, including every macOS launch. `off` uses neither. |
| `machine.worker_memory_high` | **Advanced.** string · **Default:** `40%`<br>Global only. Linux worker scope `MemoryHigh=` (throttle point): a size such as `6G`, a percentage of RAM, or `infinity`. Not applied on macOS. |
| `machine.worker_memory_max` | **Advanced.** string · **Default:** `50%`<br>Global only. Worker scope `MemoryMax=` (OOM point), same format. The macOS watchdog uses it as its memory ceiling. |
| `machine.worker_tasks_max` | **Advanced.** integer · **Default:** `4096`<br>Global only. Worker scope `TasksMax=` (processes plus threads). The macOS watchdog counts processes only. |
| `machine.mcp.toolsets` | array&lt;string&gt; · **Default:** `[]`<br>Global only. MCP toolsets `orbit mcp serve` and `orbit mcp listen` advertise beside the role or the default surface. A server's `--toolset` replaces this list. See [MCP toolsets](/how-to/mcp-integration/#toolsets). |
| `machine.mcp.role` | string · **Default:** unset<br>Global only. MCP role `orbit mcp serve` and `orbit mcp listen` serve: `operator`, `manager`, `resident`, `agent` or `reader`. Unset serves the default surface. A server's `--role` replaces it. See [MCP roles](/how-to/mcp-integration/#roles). |
| `machine.worker_cpu_quota` | **Advanced.** integer · **Default:** unset<br>Global only, Linux only. Worker scope `CPUQuota=` as a percentage of one core (`400` is four cores). `0` or unset sets no limit. |
| `machine.auto_concurrency.mode` | string · **Default:** `off`<br>Global only. Forecast admission: `off`; `shadow` records what it would admit and admits as `off`; `enforce` admits the candidates whose profile fits under the forecast. |
| `machine.auto_concurrency.admit_cpu_percent` | integer · **Default:** `90`<br>Global only. Maximum forecast CPU utilization as a percentage of host cores (1–100). Distinct from the throttle's `cpu_high_percent`. |
| `machine.auto_concurrency.min_profile_deliveries` | integer · **Default:** `10`<br>Global only. Completed deliveries needed before a profile is used without fallback. |
| `tasks.id_start` | integer · **Default:** Unset<br>Floor for this machine's task-ID allocator. Moves only forward, so machines can hold disjoint ID ranges. |
| `decision_channel.enabled` | bool · **Default:** `false`<br>Raise decision requests for work held on a human decision. |
| `experiments.enabled` | bool · **Default:** `false`<br>Allow experiments to start in this workspace. While false, `orbit experiment start` is refused; drafts can still be defined and edited. |
| `crew_memory.enabled` | bool · **Default:** `false`<br>Experimental. Allow crew memory in this workspace. While false, `orbit memory write`, `reset` and `copy` are refused and memory arms deliver nothing. See [Give models a memory](/how-to/crew-memory/). |
| `seeding.routines` | string or array&lt;string&gt; · **Default:** `all`<br>Which shipped default routines this workspace seeds: `"all"`, `"none"` or a list of file stems, such as `["worktree_gc", "store_gc"]`. Narrowing it retires a left-out default you never edited; an edited copy moves to `.orbit/.retired-managed/`. |
| `seeding.auto_tasks` | string or array&lt;string&gt; · **Default:** `all`<br>The same policy for shipped default auto-tasks, by name, such as `["doc-duties", "qa-sweep"]`. |
| `experiments.config_keys` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Config keys an experiment arm's `config` overlay may set. Starting an experiment whose arm sets any other key is refused. |
| `pr.delivery_authors` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Forge logins whose pull requests count as Orbit-authored. Empty means the login the forge CLI is authenticated as on this machine. |
| `pr.title_template` | **Advanced.** string · **Default:** Unset<br>Template for a single-task pull request title. Unset uses the task title. See [Delivery templates](#delivery-templates). |
| `pr.body_template` | **Advanced.** string · **Default:** Unset<br>Pull request body template: `"repo"` (the repository's `pull_request_template.md`) or a repository-relative path. Unset uses Orbit's layout. |
| `pr.agent_sections` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Body headings the implementer may fill through the `pr-sections.json` task artifact. |
| `pr.required_sections` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Body headings the implementer must fill. |
| `pr.missing_required_section` | **Advanced.** string · **Default:** `draft`<br>When a required section is unfilled: `draft` opens the pull request as a draft; `refuse` opens none. |
| `pr.labels` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Labels applied to each pull request Orbit opens. |
| `pr.task_tag_labels` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Exact, case-sensitive allowlist of task tags mirrored as pull request labels. |
| `pr.task_type_labels` | **Advanced.** table&lt;string,string&gt; · **Default:** `{}`<br>Map `feature`, `bug`, `refactor` or `chore` to a label name, such as `{ bug = "bug", feature = "enhancement" }`. Applied best-effort after the pull request is created or reused; missing labels are created. |
| `pr.reviewers` | **Advanced.** array&lt;string&gt; · **Default:** `[]`<br>Reviewers (logins or `org/team`) requested on each pull request Orbit opens. |
| `commit.subject_template` | **Advanced.** string · **Default:** Unset<br>Template for single-task commit subjects; must contain `[{{task_id}}]`. |
| `commit.body_template` | **Advanced.** string · **Default:** Unset<br>Template for single-task commit bodies, before the trailers. |
| `branch.name_template` | **Advanced.** string · **Default:** `{{prefix}}/{{task_id}}-{{hash}}`<br>Template for single-task delivery branch names; must contain `{{task_id}}` and `{{hash}}`. |
| `pr.task_url_template` | **Advanced.** string · **Default:** Unset<br>URL template that links a task ID in pull request descriptions. |
| `execution.env.pass` | array&lt;string&gt; · **Default:** `HOME`, `PATH`, `CODEX_HOME`, `TMPDIR`, `USER`<br>Environment variables passed to agent subprocesses. Your list replaces the default. macOS also passes `__CF_USER_TEXT_ENCODING` by default. See [Provider credentials](#provider-credentials). |
| `execution.codex.sandbox` | string · **Default:** `workspace-write`<br>Codex sandbox mode: `read-only`, `workspace-write` or `danger-full-access`. |
| `execution.codex.approval_policy` | **Advanced.** string · **Default:** Unset<br>Codex approval policy: `untrusted`, `on-request` or `never`. |
| `completion.hold` | array&lt;table&gt; · **Default:** `[]`<br>Ordered `[[completion.hold]]` rules that keep a `--complete` run's task in `review`, with its pull request open. A rule sets at least one of `paths` (globs on the changed files), `complexity_at_least`, `types` and `tags`, and holds when any field matches. Approve a held task with `orbit task update <id> --approve` once its pull request is merged. |
| `retention.audit_days` | integer · **Default:** `60`<br>Days `orbit gc audit` keeps audit rows (1–36500). Nothing is deleted until you run `orbit gc audit --confirm` or enable the `store-gc` routine. |
| `retention.runs_days` | integer · **Default:** `60`<br>Days after a run finishes before `orbit gc runs` may drop its pipeline state (30–36500). Run rows, steps, summaries and commit outcomes stay. |
| `worktree.reclaim` | **Advanced.** array&lt;string&gt; · **Default:** `["target", ".orbit/tmp/*-target", ".orbit/tmp/base-wt"]`<br>Relative globs of rebuildable paths reclaimed in kept terminal run worktrees. Only ignored or untracked paths are deleted. `[]` disables reclamation. Inspect with `orbit gc worktrees --reclaim`; apply with `--confirm`. |
| `worktree.reclaim_below_free_mib` | **Advanced.** integer · **Default:** Unset<br>Reclaim during admission when free MiB under the state directory falls below this, oldest terminal worktrees first. |
| `review.timing` | string · **Default:** `off`<br>When a fresh reviewer, who fixes what it finds, runs before landing. `off`: never. `before_pr`: holds PR creation for the review. `before_landing`: opens the PR, reviews it while hosted CI runs, and merges only the reviewed head. Refused for local-only delivery. |
| `review.before_landing_hosts` | array&lt;string&gt; · **Default:** `[]`<br>Machine IDs (`hm_…`) whose claimed leaves run before-landing review while this owner's own deliveries do not. Config fails to load when this is non-empty and `review.timing` is `before_pr`. This is policy, not a security boundary. |
| `review.minutes` | integer · **Default:** `30`<br>Time limit for one candidate's before-PR or before-landing review (1–1440). |
| `review.baseline_commands` | array&lt;string&gt; · **Default:** `[]`<br>Commands before-PR review may rerun to confirm that a failed check also fails on the base. `workflow.required_validation_commands` always count. A confirmed claim holds the task in the backlog until the base passes. |
| `review.host_evidence` | array&lt;table&gt; · **Default:** `[]`<br>Ordered `[[review.host_evidence]]` rules naming checks a claimed leaf's host owes for the paths it changed or the tags its task carries. A workspace list replaces the global one. See [Host evidence](#host-evidence). |

Notes:

- Pool entries are `crew[@effort]` or `crew[@effort]:weight`, all bare or all
  weighted. An empty pool sends that tier to `default_crew`. An explicit crew
  wins over any pool, and `--allow-crew` narrows every draw.
- Path globs use `*` within a path component and `**` across components.
- Each resource-throttle percentage is 1–100, and each resume mark must be
  lower than its high-water mark.
- `orbit run ship --strict-worker-containment` and
  `orbit run auto --strict-worker-containment` turn on strict containment for
  one run, whatever `machine.worker_containment` says.
- After-landing review is not a config key. Turn it on with
  `orbit auto-task toggle delivery-code-review on`.
- An installed plugin's declared settings are settable as
  `plugins.<ns>.<key>`, and crew fields as `crews.<name>.<field>`.

## Host evidence

A claimed leaf runs on a follower host whose OS can differ from the owner's.
Some checks can only run on one OS: a Linux CodeQL run needs Linux
extraction, and a sandbox test needs its own host's sandbox. Add
`[[review.host_evidence]]` tables to `config.toml` to declare those checks,
so a leaf on the wrong host owes them instead of failing, and the owner's
host fulfils them. A layer that states the list replaces the earlier layer's
list whole.

Each rule has a `kind` (`codeql` or `host_sandbox_test`), a `name`, one or
more workspace-relative `paths` globs, an `os` (`linux` or `macos`), the
exact `command`, and a result `artifact` (a relative `.json` path, unique
across rules). Two fields are optional: `tags`, a list of task tags added in
Orbit 0.51.0, and `hosted_check`, which only a Linux `codeql` rule may set.

```toml
[[review.host_evidence]]
kind = "codeql"
name = "Linux CodeQL (rust)"
paths = ["**/*.rs"]
tags = ["security", "security-review"]
os = "linux"
command = "scripts/codeql-rust-local.sh --ram 16384 codeql/rust-queries:codeql-suites/rust-security-extended.qls"
artifact = "evidence/codeql-rust-linux.json"
```

- **Paths.** `*` stays within one path component and `**` spans components.
  A rule needs at least one glob, and a glob that does not compile fails the
  config load.
- **Tags.** A rule with `tags` is also owed when the claimed task carries any
  listed tag, whatever paths the candidate changed. Tags compare
  case-insensitively after trimming, so `Security` matches `security`. A
  rule without `tags` matches paths only. Because tags change the
  distributed-drain protocol fingerprint, a follower on an older build
  refuses a tagged rule with `protocol_skew` before it can claim: keep rules
  path-only or upgrade every reader to 0.51.0 or later.
- **Which host owes.** A `codeql` rule is owed on a host of another OS than
  the rule's `os`, and fulfilled by the owner on `os`. A `host_sandbox_test`
  rule is owed on a host of its own `os`, which runs it outside the agent
  sandbox at settlement. So the example above is owed by a macOS follower
  that changes a `.rs` file, or by a macOS follower on a `security`-tagged
  task even when the change touches no Rust file; a Linux follower owes
  nothing for it.
- **Commands and artifacts.** `name` and `command` must both be set.
  `artifact` must be a relative `.json` path (never `./`-prefixed) and must
  not repeat another rule's artifact. A `host_sandbox_test` command must be
  one settlement admits — `cargo test -p <crate> --test <target> [<filter>]`,
  `cargo test -p <crate> --lib [<filter>]`, or an owner required-validation
  command — or the config fails to load. A rule Orbit could never fulfil
  fails load the same way, instead of holding every matching review forever.
- **`hosted_check`.** A Linux `codeql` rule may name the pull request check
  that runs exactly its `command` on every candidate, as the forge reports
  it (`hosted_check = "CodeQL / Analyze (rust)"`). Any other kind or OS, or
  an empty value, fails load. Declare it only for a check that really runs
  the rule's command: Orbit compares the name, not the queries the job runs.
  Without it — a custom query, or a workspace with no equivalent hosted
  check — the requirement holds the review for the owner's run.

When a rule is owed, each side records that instead of running what its host
cannot:

1. The implementer records the command `not_run` with the reason (for
   example the platform refusal) and hands off; the missing run is not a
   failure.
2. The reviewer names the requirement in its owed evidence, records it
   `not_run`, and never attempts it. A verdict whose only gaps are owed
   checks settles into a hold for that evidence instead of blocking.
3. The owner captured the rules when the claim was admitted, and the
   follower runs that captured contract, never its own settings. The owner's
   host then fulfils the hold: it runs the admitted command fresh — CodeQL
   through Bubblewrap namespaces in a standalone checkout of the held
   commit, sandbox tests outside the agent sandbox in a fresh detached
   worktree — and attaches the result at the rule's `artifact` path plus a
   log. Receipt moves the task back for a fresh review, which verifies the
   evidence. A complete CodeQL run whose SARIF has findings is a verdict on
   the candidate, not evidence, and does not wait under the hold.

The one exception is a before-landing review on another-OS host for a Linux
`codeql` rule with `hosted_check`: the requirement is then owed to the owner
and that check (`owner_owed_evidence`) instead of holding the review, and the
owner's landing merges only after the named check reports an explicit
success on the reviewed head. It waits while the check is absent, still
running, or completed without success (`SKIPPED` or `NEUTRAL`).

## Tag instructions

Use `[workflow.tag_instructions.<tag>]` to give every task carrying a tag
extra implementation, review or recovery guidance from a file in your
repository. Edit `config.toml` directly: `orbit config set` does not write
these tables.

```toml
[workflow.tag_instructions.data-migration]
implement = "agent-guidance/migration-implement.md"
review = "agent-guidance/migration-review.md"
recovery = "agent-guidance/migration-recovery.md"

[workflow.tag_instructions.ui]
review = false # Disable the shipped reviewer instruction for this tag.
```

| Role | Activities that receive the file |
|---|---|
| `implement` | `agent_implement`, including QA sweeps tagged `qa-sweep` |
| `review` | `agent_review_repair` and `review_reconciliation_review` |
| `recovery` | `step_failure_recovery`, `final_recovery` and `pr_conflict_recovery` |

Commit the mapped files to the base branch before you use the tag. Orbit
reads each file from the delivery's base revision, never the working copy, so
a delivery cannot change the instructions it runs under.

- **Values.** A repository-relative path with `/` separators and no `..`, or
  `false` to supply nothing for that tag and role, shipped default included.
- **Layering.** Mappings layer per tag and role. A workspace `review` entry
  replaces the global one for that tag; its other roles and other tags stay
  inherited. An omitted role falls back to Orbit's shipped default, if any.
- **Matching.** Tags match trimmed and lowercased, so `DATA-MIGRATION`
  selects `data-migration`. You can map at most 32 tags.
- **Limits.** Instructions only add prompt text. Built-in safety, sandbox,
  gate and handoff rules still win, and a mapping grants no skip; see
  [Review-skip tags](#review-skip-tags).

Orbit ships these defaults:

| Tag | Role | Instruction |
|---|---|---|
| `no-diff-expected` | `implement` | Read-only handoff, with no workspace gates or `no-diff.json`. |
| `skip-validation` | `implement` | No local validation while every changed path matches `workflow.light_delivery_paths`. |
| `ui` | `implement`, `review` | Route implementers to a host with a verified browser kit and select the optional PR screenshots section. |

### If an instruction is refused

A refused mapping fails the activity before its provider starts, and only for
tasks carrying that tag.

| Error | Cause |
|---|---|
| `tag_instruction_unreadable` | The file is missing at the base revision, is not a file, is not UTF-8, or is empty. |
| `tag_instruction_too_large` | One file exceeds 8 KiB. |
| `tag_instructions_too_large` | The combined instructions for one prompt exceed 16 KiB. |

For a distributed claim, the owner captures the instructions and any refusal
in the claim, and the follower uses that capture instead of its own files.

### Inspect applied instructions

```bash
orbit run show "$RUN_ID"
orbit run show "$RUN_ID" --json
```

The text view prints a `Tag instructions:` line per dispatch that received
any. JSON exposes `tag_instructions` with state `observed`, `none` or
`unavailable` (no audit evidence).

## Agent corpus

Use `[workflow.agent_corpus]` to send an activity workspace knowledge with its
prompt. Map exact activity names to one repository-relative corpus index:

```toml
[workflow.agent_corpus]
agent_implement = "agent-guidance/corpus/INDEX.md"
final_recovery = "agent-guidance/corpus/INDEX.md"
```

Unset by default, so no activity receives a corpus. You can map at most 32
activities, and every entry must name the same index. Absolute paths and `..`
are refused.

Orbit reads the index at the run's base revision, keeps the rows whose
`applies_to` matches the activity, omits resolved issues, and injects at most
8 KiB, with a note when rows are left out. A claimed run uses the rows its
owner captured.

## Review-skip tags

Use `workflow.review_skip_tags` to let a tag skip agent review for a bounded
class of low-risk changes, including paths outside
`workflow.light_delivery_paths`. For example, a comment-only maintenance task
can touch source files when a guard script confirms that nothing but comments
changed:

```toml
[workflow.review_skip_tags.comment-hygiene]
paths = ["src/**", "!src/generated/**"]
guard = "scripts/check-comment-only-diff.sh"
```

Review is skipped only when all of these hold:

- Every task the candidate delivers carries the tag.
- Every changed path matches `paths`. Deletions count, a rename checks both
  paths, and an empty diff does not qualify.
- The optional `guard` exits 0. A guard missing at the base revision, a
  nonzero exit or a timeout keeps normal review.

A profile skips before-PR review, before-landing review and after-landing
`delivery-code-review` sampling. Required validation, workspace gates and
hosted CI still run.

| Field | Rules |
|---|---|
| `paths` | Required, non-empty array of globs. `*` stays within a path component, `**` spans components and a leading `!` excludes. `["**"]` allows every path. |
| `guard` | Optional repository-relative executable, not a shell command. It receives the candidate's base and head commits as its first two arguments and as `ORBIT_REVIEW_SKIP_BASE` and `ORBIT_REVIEW_SKIP_HEAD`, and the tag as `ORBIT_REVIEW_SKIP_TAG`. |

Orbit runs the base revision's copy of the guard in a throwaway checkout of
the candidate's head, under the same rules as required validation. Commit the
guard before you rely on the profile; a candidate cannot weaken it.

- **Who can add the tag.** Task filing, including an auto-task definition, or
  an operator update. An agent update that adds a mapped tag is refused;
  removing one is always allowed.
- **Layering.** Profiles layer per tag: a workspace value overrides the
  global value for the same tag, and a workspace `paths` array replaces the
  global one whole. You can map at most 32 tags, matched case-insensitively.
- **Seeded profiles.** A new global config maps the shipped auto-tasks'
  `auto-task:<name>` tags to documentation and CI paths. Keep finding tags
  such as `code-review` and `security-review` unmapped, so the fixes those
  reviews file still get reviewed.
- **Built-in tags.** `skip-review` and `skip-validation` cannot be remapped.
  They apply when every delivered task carries the tag and every changed path
  matches `workflow.light_delivery_paths`. Neither skips hosted CI.

`orbit config show` lists the effective profiles under **Review skip tags**
(`review_skip_tags` in `--json`). To see why a candidate did or did not skip,
read its `light_delivery_applied` or `light_delivery_ignored` event:

```bash
orbit task show "$TASK_ID" --fields history
```

For a distributed claim, the follower uses the profiles and guard its owner
captured, and the owner evaluates the handoff again on acceptance.

## Delivery templates

The `[pr]`, `[commit]` and `[branch]` templates shape what a single-task
delivery publishes. With none set, and for a delivery that bundles several
tasks, Orbit uses its own titles, bodies, commit messages and
`orbit/<task>-<hash>` branches.

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

A template naming an unknown placeholder fails the configuration load.
`{{task_id}}` and `{{ext.<system>}}` work in every template, `{{prefix}}` and
`{{hash}}` only in branch names, and the rest in PR titles, PR bodies and
commits.

| Placeholder | Value |
|---|---|
| `{{task_id}}` | The task's ID |
| `{{title}}` | The task title |
| `{{type}}` | `feat`, `fix`, `refactor` or `chore`, from the task type |
| `{{description}}` | The task description |
| `{{acceptance_criteria}}` | The acceptance criteria as a checklist |
| `{{execution_summary}}` | The implementer's execution summary |
| `{{validation}}` | The validation section |
| `{{freshness}}` | The branch's base and head and how far it is behind and ahead; empty in commits |
| `{{task_url}}` | The task link from `pr.task_url_template`; empty when unset |
| `{{ext.<system>}}` | The ID of the task's external ref for `<system>`, such as `{{ext.jira}}`; empty when the task has none |
| `{{prefix}}` | The delivery's branch prefix: `orbit` unless a job sets another |
| `{{hash}}` | Eight hex characters derived from the run, so a retry reuses the branch |

- **Body template.** `pr.body_template = "repo"` reads
  `pull_request_template.md` (any letter case) from `.github/`, the repository
  root or `docs/`, in that order, at the delivery's base revision. With none,
  Orbit uses its own layout. A repository-relative path must exist at the base
  revision. The template need not contain `{{task_id}}`: Orbit appends
  `<!-- orbit-delivery-task: <TASK-ID> -->`.
- **Sections.** The implementer fills agent and required sections by
  attaching a `pr-sections.json` task artifact shaped
  `{"sections": {"Risk": "Low: one file."}}`. Each entry replaces the body
  under the matching template heading, ignoring letter case; with Orbit's own
  layout it is appended as `## <heading>`. An unfilled required section
  follows `pr.missing_required_section` and is recorded in task history. Orbit
  never merges a draft automatically.
- **Screenshots.** For a task tagged `ui`, Orbit adds a `## Screenshots` table
  of Before and After images from task artifacts named
  `screenshots/<view>-<theme>-<width>[-before|-after].<ext>`; no suffix means
  After. It publishes PNG, JPEG, GIF and WebP images up to 1 MiB. With none,
  the section shows `No screenshots attached.` and the pull request still
  opens.
- **Commits and branches.** Subjects keep to 72 characters: the title is
  shortened first and kept whole in the body. A subject template must contain
  `[{{task_id}}]`. A branch template must contain `{{task_id}}` and
  `{{hash}}`; a name that contains `..`, starts with `-` or breaks Git's
  ref-name rules is refused.

## Workspace presets

`orbit workspace init --preset <file>` applies a preset, a TOML file a
distribution ships to set a workspace up in one step. Add `--force` to apply
one to a registered workspace.

```toml
name = "graph-distro"            # required: 1-64 of A-Z a-z 0-9 . _ -

[seeding]                        # seeding.routines / seeding.auto_tasks
routines = ["worktree_gc"]
auto_tasks = "none"

[enable]                         # plugin-seeded definitions to switch on
routines = ["graph-refresh"]
auto_tasks = ["graph-reindex"]

[config]                         # workspace keys `orbit config set` accepts
"workflow.crew_selection" = "static"
```

Orbit checks the whole file before it writes anything. It refuses:

- an unknown field;
- a `[config]` key or value that `orbit config set` would refuse;
- an `[enable]` name that no enabled plugin seeded into this workspace;
- any field that grants plugin permissions or enables a plugin. Run
  `orbit plugin enable <plugin>` yourself first.

The `[seeding]` and `[config]` keys are written to `.orbit/config.toml` before
the defaults are seeded. `orbit workspace show` reports the preset's name, the
file's `sha256:` digest and the seeding policy.

## Workspace build output

Set `workflow.validation_env.build_output_env` to the variable your build
commands read their output directory from. Orbit then points it at a
host-owned directory, kept apart from the areas workers can write:

```toml
[workflow.validation_env]
build_output_env = ["CARGO_TARGET_DIR"] # Rust workspace
```

A non-Rust workspace can set `["DIST_DIR"]` and have its validation command
run `cmake -S . -B "$DIST_DIR"`. The default is empty, which allocates nothing
and leaves inherited variables unchanged.

- **Names** must be shell environment identifiers. `PATH`, `HOME`, `USER`,
  `LOGNAME`, `SHELL`, `TMPDIR`, `TMP`, `TEMP`, `PWD`, `OLDPWD` and `ORBIT_`
  names are reserved. A workspace list replaces the global list.
- **Directory.** Every configured variable points to the same directory under
  `<primary-repository>/.orbit/state/host-validation-targets/`, one per source
  checkout. Your commands must honor the variable to keep that isolation.
- **Applies to** required validation, host replay and detached evidence. Agent
  sessions and ordinary `local_shell` steps are not bound. Bound commands also
  receive `CARGO_INCREMENTAL=0`.

## Root override

Most commands accept the global `--root` option, which overrides the Orbit
root directory:

```bash
orbit --root /path/to/orbit-root task list
```

## Retired backend selection

Agent activities always run through the provider's CLI. The `--backend` flag,
`ORBIT_BACKEND`, `[runtime] backend` and `[crews.<name>] backend` were
removed. An old `cli` declaration is accepted and ignored; `http` and `auto`
are rejected with a migration message. Remove the setting.
