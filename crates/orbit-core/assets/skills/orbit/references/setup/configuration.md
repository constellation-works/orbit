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
orbit config keys                  # every settable key
orbit config path
```

`orbit config show` annotates every setting as `workspace`, `global`,
`built-in`, or `environment`, with the file it came from. Reach for it before
assuming a value.

## Common settings

| Key | Purpose |
|---|---|
| `workflow.base_branch` | Default base branch for ship workflows. Set this first — it decides where PRs land. |
| `workflow.default_crew` | Crew for any task that doesn't declare one. |
| `workflow.system_crew` | Crew for Orbit's own bounded activities (failure recovery, triage). |
| `workflow.auto_ship` | Opt-in for unattended ship dispatch via the scheduler. |
| `tasks.id_start` | Floor for this machine's task-id allocator; forward-only. → [multi-host.md](multi-host.md) |
| `execution.env.pass` | Environment variable names allow-listed into agent subprocesses. |
| `execution.codex.sandbox` | `read-only`, `workspace-write`, or `danger-full-access`. |
| `execution.codex.approval_policy` | `untrusted`, `on-request`, or `never`. |
| `runtime.log_retention_days` | Delete JSONL archives older than N days. |
| `runtime.log_max_total_mb` | Total archive budget; oldest pruned first. |
| `runtime.log_max_file_mb` | Roll the active log past N MiB. Must not exceed the total budget. |
| `scoring.enabled` | Record scoreboard metrics for task runs. |
| `pr.task_url_template` | URL template linking a task ID in PR descriptions. |

Use `orbit config keys` to distinguish fixed keys supported by `config set`
from settings authored as TOML. Named crew fields are also settable as
`crews.<name>.<field>` (for example `orbit config set crews.sol.effort high`)
even though those keys are not listed by `orbit config keys`. Creating a crew
still requires a `[crews.<name>]` table with `model` and `provider`. Set
workspace ship mode with
`orbit workspace init --ship-mode pr|local`; verify the registered workspace
with `orbit workspace show`. Base branch and ship mode govern source delivery,
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

`orbit init` seeds crews for the agent CLIs it detects and uses `system` for
newly initialized system work. A user-authored legacy `qa` crew remains
loadable, but init never creates it. Available providers:

```bash
orbit executor list
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
orbit policy list
orbit policy show <name>
orbit policy check <profile> <path>        # dry-run a path against the active profile rules
```

Shipped profiles: `reviewer` and `pure_compute` (read-only), `docs_writer`
(scoped writes), `implementer` (workspace writes), `unrestricted`.

The trap worth knowing: an activity that omits `fsProfile` falls back to
the `unrestricted` profile. A read-only activity must declare its profile
explicitly — silence is not a safe default here.

The operating-system boundary matters: macOS uses `sandbox-exec`; Linux uses
Bubblewrap to enforce allowed writes while leaving host reads and network
available. Unsupported platforms have only in-process filesystem guards. A
read-only profile is not a claim of network isolation or private host reads.
See [first-run.md](first-run.md) for the Linux prerequisite.

## Docs roots

```toml
[docs]
roots = ["docs/"]
```

Add more with `orbit docs add <path>` rather than hand-editing.
→ [docs-corpus.md](../docs-corpus.md)

## Crew selection and actual execution

For ship dispatch, an explicit run crew overrides `task.crew`, which overrides
`workflow.default_crew`; environment/system fallbacks apply only when no higher
selection exists. An override on one run does not rewrite the task's persisted
crew. An empty crew string on task update clears that override. Discover actual
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
