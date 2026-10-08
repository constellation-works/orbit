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
workspace file exists — `execution.codex.sandbox`,
`execution.codex.approval_policy`, and `execution.env.pass`. If the workspace
file omits one, its built-in default applies rather than the user's global
value. This keeps repository sandboxing deterministic instead of dependent on
whoever's machine is running.

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
| `workflow.system_crew` | Crew for Orbit's own bounded activities (failure recovery, task pilot). Shipped `crew: system` steps resolve onto it. |
| `workflow.<tier>_complexity_crews` | Automatic crew pool per task complexity (`low`, `medium`, `hard`, `xhard`); entries `name` or `name:weight`. Empty (`[]`) routes that tier to `default_crew`. |
| `workflow.auto_ship` | Opt-in for `orbit run ship-sweep` unattended ship dispatch. The seeded `ship-sweep` routine does not read it. |
| `review.before_pr` | Before-PR review: hold PR creation for a fresh reviewer that fixes what it finds (default `false`). A run captures it at submission. After-landing review is not a key: toggle the `delivery-code-review` auto-task (`orbit auto-task toggle delivery-code-review on`). |
| `review.minutes` | Wall-clock limit for one candidate's before-PR review (1..=1440, default 30). Each candidate gets one review; a changed candidate, such as a completion rebase, is a new one. |
| `operation.review_crew` | Crew for automatic review: the before-PR reviewer and, when set, every review task the after-landing auto-task mints. |
| `tasks.id_start` | Floor for this machine's task-id allocator; forward-only. → [multi-host.md](multi-host.md) |
| `execution.env.pass` | Environment variable names allow-listed into agent subprocesses. |
| `execution.codex.sandbox` | `read-only`, `workspace-write`, or `danger-full-access`. |
| `execution.codex.approval_policy` | `untrusted`, `on-request`, or `never`. |
| `runtime.log_retention_days` | Delete JSONL archives older than N days. |
| `runtime.log_max_total_mb` | Total archive budget; oldest pruned first. |
| `runtime.log_max_file_mb` | Roll the active log past N MiB. Must not exceed the total budget. |
| `scoring.enabled` | Record scoreboard metrics for task runs. |
| `pr.task_url_template` | URL template linking a task ID in PR descriptions. |
| `pr.close_on_terminal` | Close a task's open Orbit-authored delivery and `[BLOCKED]` PRs when it lands, is rejected or is archived (default `true`). Branches are kept; a forge error is only a warning. |
| `pr.delivery_authors` | Forge logins whose PRs count as Orbit-authored for that closure. Empty uses the `gh` login on this machine. |

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
globally defined crew. A `default_crew` or `system_crew` naming an undefined crew
fails config load — deliberately, since the alternative is silently dispatching
to the wrong model.

`enabled = false` on a crew keeps it defined and listed (`orbit config show`
has an `ENABLED` column; `orbit.workspace.list` with `include: ["crews"]`
returns `enabled` per crew) but takes it out
of execution. Omitting `enabled` means enabled, so older configs are unchanged.
A disabled crew is never drawn from a complexity pool; a pool whose members are
all disabled routes the task to `default_crew`, like an empty pool. A task
`crew`, explicit crew, `default_crew`, `system_crew` or `crew: system` step that
lands on a disabled crew refuses dispatch with an error naming the crew — no
substitution. Config still loads; `orbit doctor` warns when a lane key names a
disabled crew. Enable one with:

```bash
orbit config set crews.gemini.enabled true
```

`orbit init` writes every built-in crew (Claude: `opus`, `sonnet`, `haiku`, `fable`;
Codex: `astra`, `sol`, `luna`; one crew each for Antigravity, Gemini,
Grok, Copilot, Cursor, Pi, OpenCode), with `enabled = true` on the crews whose
agent CLI it detects and `enabled = false` on the rest, and points the two lane
keys at enabled crews:
`workflow.default_crew` is the preferred family's default (`opus` on a Claude
host) and `workflow.system_crew` is the cheapest tier of the preferred family
(`luna` when Codex is present, else `haiku`, `grok`, …). Interactive init
offers those enabled crews by name; `--non-interactive` writes the
recommendations. Init does not write a `custom` or `system` crew table: the
`system` name shipped job steps use resolves onto `system_crew` at load, and an
explicit user-authored `[crews.system]` table wins if one exists. The four
`workflow.*_complexity_crews` pools are seeded from the detected families
(Claude only: `haiku` / `sonnet` / `opus` / `opus`; Codex only: `luna` / `sol` /
`sol` / `astra`; both: `haiku, luna` / `sol, sonnet` / `opus` / `opus, astra`;
grok joins `medium`, `antigravity` or else `gemini` joins `low`; other families
leave them `[]`). A user-authored
legacy `qa` crew remains loadable, but init never creates it. To move system
work, run `orbit config set workflow.system_crew <crew>`. With no supported
agent CLI, every crew is written disabled and no lane key is set, so nothing
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

For ship dispatch, an explicit run crew overrides the task's explicit pin or
validated pool assignment. Otherwise the complexity pool and default chain
apply as described below. A creation without a crew draws from its complexity pool,
falling back to
`default_crew`, and stores `crew_source` as `explicit`, `pool:<complexity>`,
or `default`, also recorded in assignment history. A complexity re-rate
redraws a pool-sourced crew from another tier and records both tiers and crews
in `crew_redrawn` history; explicit crews stay pinned. Status transitions alone
preserve the choice. Admission revalidates pool assignments against the current
tier and enabled pool members, recovering legacy provenance from assignment
history when needed, without writing to the task. An empty crew string on task
update re-draws for the task's current complexity rather than leaving the field
empty. Discover actual
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
