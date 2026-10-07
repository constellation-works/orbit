---
type: context
summary: Orbit Configuration
last_validated: 2026-10-05
---

# Orbit Configuration

Operator reference for Orbit's `config.toml`: every fixed key, its default, and what it does. `orbit config keys` prints the same key list with descriptions. The annotated template `orbit init` writes is [`crates/orbit-config/assets/default-config.toml`](../crates/orbit-config/assets/default-config.toml); treat it as a reference, not a file to copy wholesale. A shorter overview lives on the website under [Reference › Configuration](https://orbit-cli.com/reference/config/).

To add a new execution lane rather than configure a shipped one, see the [executor onboarding runbook](runbooks/executor-onboarding.md).

## Where config lives

| Path | Scope | Created by |
|---|---|---|
| `<workspace>/.orbit/config.toml` | Workspace-local (per user, gitignored) | Hand-authored, or `orbit config set --fresh` / `--seed-from-global` |
| `~/.orbit/config.toml` | Global | `orbit init` (only when absent, or under `--force`) |

Ordinary settings inherit per key: workspace values override global values, global values fill omissions, and built-in defaults fill remaining gaps.

- **Tables** layer down to individual settings. **Scalars and arrays** replace the matching global value.
- **Named crews** layer by crew name and field, so `[crews.sol]` with only `model = "gpt-5.6-terra"` in the workspace file overrides one field of the global `sol` crew.
- **Security exceptions.** `execution.codex.sandbox`, `execution.codex.approval_policy` and `execution.env.pass` never inherit from global once a workspace file exists. If the workspace file omits one, its built-in default applies. A workspace file holding only `[plugin_enablement]` does not count: those keys keep inheriting.
- **Global-only.** A `[machine]` table in a workspace file is refused at load, naming the file.
- **Workspace-only.** A `[plugin_enablement]` table in the global file is refused at load, naming the file.

The workspace identity file `.orbit/config.yaml` (it stores `workspace_id`) is a separate artifact and not runtime config.

## Inspecting and editing

| Command | What it does |
|---|---|
| `orbit config show` | Effective merged view, grouped as Machine, Delivery (`workflow.*`), Crews, Execution, Review (`operation.*`), Housekeeping and Paths. `--all` expands sections whose keys are all unset. `--json` adds a `provenance` object. |
| `orbit config get <key>` | One value. |
| `orbit config set <key> <value>` | Write the workspace file (`--global` for the global one). The value is parsed as a TOML literal, falling back to a string. If the workspace file does not exist, pass `--fresh` (start empty) or `--seed-from-global`. |
| `orbit config keys` | Every fixed settable key with type, section, and description. |
| `orbit config path` | The resolved `config.toml` path. |

Each value in `show` reports one state: `workspace`, `global` or `environment` (the layer that set it), `default` (built-in value in force), or `unset` (no value and no default). When a lower layer also defines the key, the row says so: `(overrides global: main)`, or `(global sets danger-full-access — not inherited)` for a security key the workspace file did not restate. A registered checkout also gets a `Workspace` line with the registry's base branch and ship mode, which is what delivery uses.

In `--json`, each key's provenance carries `scope`, `path`, `section`, `description`, `state` (`set`/`default`/`unset`) and `shadowed_by` (`[{layer, value, reason}]`, reason `overridden`, `not-inherited` or `preset-reset`). A top-level `workspace_binding` reports the registered base branch and ship mode, or `null`.

`--scope global` or `--scope workspace` displays one file's values, still filling built-in defaults for omitted keys. Workspace `config show` and the dashboard's Workspace file view validate crew references against global and workspace crew definitions, including partial workspace crew overrides. They do not inherit other global settings into the displayed values. A crew absent from both layers is still reported by name and key. A dashboard file validation error includes the file path and asks you to correct the configuration before reloading.

In scoped `config get --json`, `exists` says whether the key is present in that file. In scoped `config show --json`, `source.exists` says whether the file exists.

---

## `[machine]` — who this machine is

Global-only. `orbit init` writes the identity keys once.

```toml
[machine]
id          = "hm_9ca6004473492f06"
name        = "dk-server-1"
task_prefix = "ORB"
```

| Key | Default | Settable | What it is |
|---|---|---|---|
| `machine.id` | written by init | No | Opaque `hm_…` identity. It names run ownership, workspace ownership and federated routing. |
| `machine.name` | written by init | Yes (`--global`) | Display label. |
| `machine.task_prefix` | written by init | No | 2–5 uppercase ASCII letters used for task IDs minted here. Fixed for the life of the local task store. |
| `machine.worker_containment` | `true` | Yes | Run each detached pipeline worker in its own transient systemd user scope (`orbit-worker-<run_id>-<nonce>.scope`), so a runaway run is throttled or OOM-killed inside it. Without a user manager (containers) or with `false`, workers run in the caller's cgroup and each Linux Orbit process logs one warning by default; macOS has no systemd, so it runs uncontained without a warning. Must be `true` when strict mode is enabled. |
| `machine.worker_containment_strict` | `false` | Yes | Refuse a worker launch if a systemd user scope is unavailable. The run reports the reason and how to enable a user manager or disable strict mode; no uncontained worker starts. `true` with `machine.worker_containment=false` fails config load. `orbit run ship --strict-worker-containment` and `orbit run auto --strict-worker-containment` turn it on for one invocation even when config says `false`. The auto coordinator passes this policy to its leaf workers through `ORBIT_WORKER_CONTAINMENT_STRICT` in their inherited environment. |
| `machine.worker_memory_high` | `40%` | Yes | Scope `MemoryHigh=` (throttle point). Bytes with optional `K`/`M`/`G`/`T`, a percentage of physical RAM, or `infinity`. |
| `machine.worker_memory_max` | `50%` | Yes | Scope `MemoryMax=` (OOM point). Same grammar. |
| `machine.worker_tasks_max` | `4096` | Yes | Scope `TasksMax=` (processes plus threads), at least 1. |

- `orbit config set` refuses `machine.id` and `machine.task_prefix`: changing either would orphan or renumber records minted under it.
- `orbit init` refuses to create an identity while the task store has already minted ids under another prefix (tasks created before `orbit init` mint under the historical `ORB` default). It fails before writing anything, so the machine keeps working; run `orbit init` before creating tasks.
- Hand edits fail closed. A `[machine]` table missing any identity key is an error. A `task_prefix` that contradicts the local task allocator, or an `id` that contradicts a workspace record naming this machine as owner, is refused. Nothing falls back to the hostname.
- A legacy `~/.orbit/host.toml` is folded into `[machine]` on first load and removed. If both exist and disagree, Orbit refuses to start and names both paths. Delete the stale one.
- A run that fails after hitting a worker limit carries error code `worker_resource_limit` in `orbit run show`. Inspecting scopes: [operational logs › Worker Resource Containment](../crates/orbit-core/assets/skills/orbit-setup/references/operational-logs.md#worker-resource-containment).
- A strict launch refused before a worker starts carries error code `worker_containment_unavailable` on the run and in CLI JSON errors.

---

## `[workflow]` — branch and crew defaults

```toml
[workflow]
base_branch = "main"
default_crew = "opus"
system_crew = "luna"
low_complexity_crews = []
medium_complexity_crews = []
hard_complexity_crews = []
xhard_complexity_crews = []
final_recovery_crews = ["sol:100", "opus:20"]
```

| Key | Default | What it does |
|---|---|---|
| `workflow.base_branch` | `main` | Fallback base branch for ship, auto and pilot when the workspace registry has none. The registry value (`orbit workspace show`) wins, and `--base <branch>` overrides both. For a two-branch repo, register with `--base-branch agent-main`. |
| `workflow.default_crew` | see [resolution](#resolution-precedence) | Crew for a task with no `crew`. Must name a defined crew. |
| `workflow.system_crew` | `system` | Crew for runtime-synthesized system work such as step-failure recovery. |
| `workflow.low_complexity_crews`, `medium_…`, `hard_…`, `xhard_…` | `[]` | Crew pools a crew-less task draws from at creation, by complexity. Empty means "use `default_crew`". See [pools](#automatic-crew-pools-by-complexity). |
| `workflow.final_recovery_crews` | `["sol:100", "opus:20"]` | Weighted crew pool the final-recovery activity draws once per run after step recovery is exhausted; entries are `name` or `name:weight` (all bare or all weighted). Unset defaults to `["sol:100", "opus:20"]`, keeping only the members the crew registry defines; `[]` disables final recovery. See [final recovery pool](#final-recovery-pool). |
| `workflow.auto_ship` | `false` | Opt this workspace in to `orbit run ship-sweep`, the cross-workspace unattended ship command. While `false`, that command skips the workspace with `auto_ship_disabled`. The seeded `ship-sweep` routine does not read this key; its own `enabled:` flag is its only switch. Neither path grants `--complete` or `--approve-proposed`; a task tagged `no-auto-approve` is never approved automatically by either flag's drain or the CI sweep. |
| `workflow.required_validation_commands` | `[]` | Commands every delivered candidate must pass. `task_pr_pipeline` and `task_local_pipeline` run them on the exact candidate before push or merge and attach each log to the task; a failure goes to step recovery, except a [missing tool](#workflowvalidation_env--the-toolchain-required-validation-runs-with) or a failure the base shares, which holds the task until the command passes on a new base tip. Network-inconclusive failures are rerun first. Distributed handoffs must carry exact-candidate validation matching the owner's required list. Before-PR claims also freeze that list in the owner's review contract at admission; a later owner command-list change refuses their handoff rather than replacing the snapshot. A re-run of a task whose failed run preserved a candidate from `commit` or later also runs them on that candidate applied to the new base, to decide whether the implementation step runs at all. A candidate preserved from the implementation step, or from any step before `commit`, always returns to the implementer and these commands are not what decides that ([re-running a task](../crates/orbit-core/assets/skills/orbit-orchestrate/references/workflows.md#re-running-a-task-with-a-preserved-candidate)). An explicit empty list runs no required check, including on claimed handoffs; the other handoff guards still apply. |
| `workflow.distributed_completion` | `review` | How far this owner takes an accepted distributed-drain handoff. `review` waits for an operator's **Approve handoff**; `done` has the owner authorize it on acceptance and land it through `task_landing_pipeline`, rechecking this key before the merge. |

### `[workflow.resource_throttle]` — host pressure

| Key | Default | What it does |
|---|---|---|
| `workflow.resource_throttle.enabled` | `true` | Enable the host pressure throttle verdict. Disabling it retains resource telemetry and severity. |
| `workflow.resource_throttle.cpu_high_percent` | `90` | CPU high-water mark. |
| `workflow.resource_throttle.cpu_resume_percent` | `85` | Resume below this CPU percentage. |
| `workflow.resource_throttle.memory_high_percent` | `90` | Memory high-water mark. |
| `workflow.resource_throttle.memory_resume_percent` | `85` | Resume below this memory percentage. |
| `workflow.resource_throttle.disk_high_percent` | `90` | High-water mark for each observed filesystem. |
| `workflow.resource_throttle.disk_resume_percent` | `85` | Resume below this filesystem usage percentage. |

Percentages are integers in `1..=100`; every resume mark must be strictly less than its high-water mark. These settings inherit per key and appear in `orbit config show`. Workspace runtimes use their resolved settings. The host dashboard uses the serving machine's global settings, sampled when its monitor first opens; restart the dashboard after changing those settings.

The probe caches native reads for two seconds. Linux CPU is one-minute load divided by all online host CPUs, multiplied by 100; it can exceed 100% and includes tasks waiting for I/O. Linux memory usage is `(MemTotal - MemAvailable) / MemTotal`. macOS CPU is the busy fraction of aggregate Mach CPU tick deltas between samples (the first observation is unknown); memory subtracts free and reclaimable inactive pages from physical memory, including compressed and wired memory in usage. Speculative pages are already included in free pages; purgeable pages overlap other categories. See Apple's [VM statistics](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/mach/vm_statistics.h) and [host CPU statistics](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/mach/host_info.h). These are usage estimates rather than OS memory-pressure classifications. Collectors use procfs, Mach/sysctl and statvfs through fs2, without launching subprocesses.

Disk usage uses space available to the current user: `(total - available) / total`. Observations cover checkout and `.orbit/state/worktrees` filesystems plus the global Orbit root (`~/.orbit`, or the serving root override). A not-yet-created worktrees directory uses its nearest existing ancestor. All watched paths still participate in sampling and admission. The host API exposes a single `disk` object with the highest known percentage, its severity and path (`null` when no disk reading is known), rather than a per-path array. Held disk pressure reports only the highest held path in reasons and CLI/MCP warnings. The host-scoped `GET /api/host/resources` observes all active registered local checkout paths, independent of `?workspace=`, and remains usable with no selected workspace. It does not query remote hosts.

A value below resume is `ok`, between resume and high is `elevated`, and at or above high is `critical`. The evaluator requires high readings spanning at least ten seconds before setting `throttle=true`; it keeps each hold until that resource falls below resume. Repeated calls using the same cached timestamp do not establish sustained pressure. Gaps longer than fifteen seconds restart the observation window. Unavailable, invalid, future-dated or older-than-fifteen-second samples are explicitly `unknown` and release that resource's hold (fail open); another known resource can still hold the verdict. The API includes severity, sample time/age, thresholds, verdict and reason. The dashboard shows exactly three live CPU, Memory and Disk chips after the topbar's windowed metrics and window label. Elevated and critical chips are tinted; held resources carry a `throttled` marker. Tooltips include sample age, verdict and reason, plus the worst-known path for Disk. It polls every five seconds and checks staleness every second, marking aging data unknown even while a request is pending. While the verdict throttles, local drains, pull drains and ship discovery start no new task, and readiness, `orbit run show`, `orbit run auto`, MCP and the dashboard Drain card name the held resource, value, threshold and since-when; running work is never cancelled. See [Host resource pressure](runbooks/distributed-drain.md#host-resource-pressure). Disabling the throttle restores unthrottled admission.

The dashboard's **Settings › System** view shows these seven keys as one panel: per resource the throttle-at and resume-below marks, each with its source (`default`, `global` or `workspace`), the live reading and severity, and the current verdict with each held resource and since-when. Edits use the same `PUT /api/config/keys/{key}` write path as the Effective view, so a resume mark at or above its high mark is refused with the same inline error, but they write the **global** file by default because the topbar chips and admission on this host read the serving machine's global settings. When the workspace file also sets a key, the view marks the override; that value wins for the workspace's runtimes.

### `[workflow.task_pilot_freshness]` — when a task is piloted again

```toml
[workflow.task_pilot_freshness]
material_fields = ["title", "description", "criteria", "plan", "selectors"]
source_sensitivity = "ignore"
```

| Key | Default | What it does |
|---|---|---|
| `workflow.task_pilot_freshness.material_fields` | `["title", "description", "criteria", "plan", "selectors"]` | The task inputs whose edit makes an accepted task-pilot assessment stale, so the state-triggered task pilot assesses the task again. Choose from `title`, `description`, `criteria`, `plan`, `selectors` (`context_files`), `tags`, `crew` (the stored crew and the model/provider it resolves to), `tools`, `type`, `complexity`, `relations`, `dependencies` (each dependency's status and meaning) and `instructions` (repository `AGENTS.md`/`CLAUDE.md` at the pinned revision). Must name at least one field. |
| `workflow.task_pilot_freshness.source_sensitivity` | `ignore` | Whether moving the observed branch head makes an assessment stale. `ignore`: never; the pinned revision is still recorded as evidence. `context_files`: only when the head changed a path one of the task's selectors names (`file:` and `dir:` by prefix, `symbol:` through its file). `any`: every head move. |

Eligibility is separate: a routine's `eligibility` block still decides which tasks are piloted at all, so a task that becomes eligible with no fresh assessment is still piloted, while retagging an already-assessed, still-eligible task is not. A task-pilot routine's `trigger.state.freshness` block overrides either key for that routine (routine, then this table, then the defaults). A value that differs from the default is itself material, so changing it re-pilots the tasks assessed under the old value once. Assessments accepted before this setting existed stay fresh while their task is unchanged. See [automation triggers](design/automation-triggers/5_operations.md).

### `[workflow.validation_env]` — the toolchain required validation runs with

```toml
[workflow.validation_env]
login_shell = true
interactive = true
path = []
path_mode = "prepend"
```

| Key | Default | What it does |
|---|---|---|
| `workflow.validation_env.login_shell` | `true` | Resolve PATH and toolchain locators (`CARGO_HOME`, `RUSTUP_HOME`, `GOPATH`, `GOROOT`, `GOBIN`, `JAVA_HOME`, `PYENV_ROOT`, `NVM_DIR`, `VOLTA_HOME`, `PNPM_HOME`, `BUN_INSTALL`, `HOMEBREW_*`) from the owner's shell: the account's shell from the user database, else `$SHELL`, else `/bin/sh`. By default run `<shell> -i -l -c …`, reading interactive rc files (`~/.zshrc`, or `~/.bashrc` when sourced by the bash login profile) as well as login profiles. Fall back to `<shell> -l -c …` on startup failure, nonzero exit, timeout or a missing environment marker. Each attempt has a 10-second limit; the complete outcome, including fallback, is cached for two minutes. Nothing outside the toolchain allowlist is taken. `false` never starts the shell. |
| `workflow.validation_env.interactive` | `true` | Try interactive login startup (`-i -l -c`) first. `false` uses only the previous login-only probe (`-l -c`). Ignored when `login_shell` is false. Stdin is null, banners and rc output before the marker are ignored, and stderr noise on success (such as bash's job-control warning) is ignored. |
| `workflow.validation_env.path` | `[]` | PATH entries to add. A leading `~/` expands to `HOME`. |
| `workflow.validation_env.path_mode` | `prepend` | `prepend` puts `path` before the resolved PATH; `replace` makes it the whole PATH. |

The owner-side commands that run repository tooling are `workflow.required_validation_commands` and `local_shell` steps. They use this environment, layered over the [allowlisted agent environment](#executionenv--the-agent-subprocess-environment), so a drain launched from a minimal PATH (a service manager, `env -i`, an SSH command) still finds the user's toolchain. Each validation log and step output records `validation_env`, with these fields:

- `source`, which decided PATH:
  - `config` when `path` has entries;
  - otherwise `login_shell` when the probe returned a PATH;
  - otherwise `launcher_fallback`, the launching process's PATH.
- the PATH itself;
- the probed shell and any probe error;
- `probe_mode`: `interactive_login` or `login` for a successful probe, otherwise null;
- `fallback_reason`: why interactive startup failed when the login-only probe succeeded, otherwise null. When both probes fail, `login_shell_error` includes both failures.

**Missing tools are environment failures.** A required command can fail because a tool is missing. Orbit detects this from:

- exit status 127;
- a shell `command not found` / `not found` line;
- `make`'s `Error 127`;
- `No such file or directory` for the program;
- a missing cargo subcommand;
- a guardrail's "`<tool>` is required … install" line, for a tool that is absent from PATH.

Such a failure says nothing about the candidate. The step fails with the `[validation_environment]` marker and error code `validation_environment`, naming the tool, PATH and source; the log records `failure_kind: "environment"` and `missing_tool`. Step recovery, final recovery and blocked-task recovery do not run. No review, rework or recovery budget is spent. The failure handoff opens no `[BLOCKED]` PR and pushes nothing. It blocks the task under `validation_environment_blocked`, which keeps the validated candidate in its worktree. Fix the environment, check `orbit doctor`, then `orbit job resume <run>`. A claimed leaf on a follower skips repair the same way. Its claim settles as a failure carrying the diagnostic. It is not released for another follower to pull and implement again.

**Network flakes are rerun.** A failing command whose output looks network-inconclusive is rerun up to twice, after 2 s and then 6 s. Orbit treats output as network-inconclusive when it shows any of these:

- an HTTP status of `000`;
- a curl connect, resolve, timeout, TLS or receive error (`curl: (6)`, `(7)`, `(28)`, `(35)`, `(56)` and similar);
- a DNS resolution failure;
- a TLS handshake timeout.

The log records `network_retries`. Only the last attempt counts.

**A red base is held, not blocked.** The integration branch has no merge gates of its own, so a required command can already be failing on the base a candidate starts from. When a command still fails after its reruns, Orbit reruns it on the base commit before failing the step. The base run uses a clean detached worktree under `<git-common-dir>/orbit-baseline/`. Its result is cached per base commit and command, and the cache sits behind a file lock, so concurrent candidates on one host share one base run. Both runs are logged: `validation/<run>/<n>.json` gets a `baseline` record, and `validation/<run>/<n>.baseline.json` holds the base log.

- **The base fails the same way** (same exit status and timeout outcome). The step fails with the `[baseline_red]` marker and error code `baseline_red`. Step recovery and final recovery do not run, and no review, rework or recovery budget is spent. The failure handoff pushes nothing and opens no `[BLOCKED]` PR. It moves the task to `backlog` under a `baseline_red_hold` history event naming the base ref, base commit and command. Run finalization does the same for a local pipeline. The candidate stays in its worktree. Admission withholds the task; `orbit run readiness --json` reports it with `reason: "baseline_red_hold"`. When the base ref advances, the owner runs the held command on the new tip and releases the task only after it passes; a failing or inconclusive check keeps the hold. The next run then resumes the preserved candidate.
- **The base passes.** The candidate caused the failure. The step fails as before, and the error says the base passed, so step recovery can repair the candidate.
- **The base run is inconclusive** (a missing tool or a network failure). The step fails as a candidate failure, and nothing is cached.

A claimed leaf on a follower validates before it pushes or opens a PR. A red base releases the claim, and the owner records the same `baseline_red_hold` in the task's history. The owner's pull admission defers the task until the hold lifts.

The before-PR reviewer can hit the same red base with a check the workspace requires beyond these commands. It then records a `baseline` claim on the failed record: the base commit, the outcome there, and the shared failures. Settlement reruns the check on the final candidate and on the pinned base, using the same base cache, and believes the claim only when the base fails the same way and the candidate adds no failing test or lint location. Settlement runs commands on the host, outside the reviewer's sandbox, so it reruns only a `workflow.required_validation_commands` or `review.baseline_commands` entry, never the reviewer's own command text. A confirmed claim with nothing else open holds the task under `baseline_red_hold` as above; the certificate records the hold. When the base passes, the next run resumes the candidate, validates it again and admits a fresh review. A candidate that fails beyond the base still rejects. A claim the host contradicts or cannot check settles the review `incomplete`.

**An implementer-declared blocker stops the run.** When `implement_one` (or a resolved `agent_implement` target) returns `blocker: {kind, evidence}`, the step fails with the `[task_blocked_by_agent]` marker and error code `task_blocked_by_agent`. `kind` is a short token. `evidence` is the reason. Step retry, step recovery, and final recovery do not run. On the PR pipeline the failure handoff does not commit, push, or open a `[BLOCKED]` PR. It blocks the task under `task_blocked_by_agent` and records the kind. The worktree stays as the implementer left it, including uncommitted files. `orbit job resume` does not move that task back; an operator does, once the blocker is gone. An implementation activity cannot set status `blocked` directly. Operators and other activities still can. A claimed leaf stops before the next step and before final recovery. Blocked-task recovery skips a marked failure note, and for a claimed-leaf failure it skips when the marker is in the task's execution summary.

**Step recovery can declare the same blocker.** `step_failure_recovery` writes an `external_blocker` decision with `blocker: {kind, evidence}` when the cause is outside the run and a human must act. The post-recovery attempt and final recovery do not run. The step fails with the same marker and kind, and the failure handoff blocks the task exactly as above. A recovery that writes no decision gets its one post-recovery attempt only when Orbit sees that the worktree, the tip of the run's base ref, or the validation environment changed while it ran. Otherwise the original failure stands.

`orbit doctor` reports the resolution as `validation-env`, with the probe mode, fallback reason, resolved PATH and executable locations for `python3`, `git` and `make`. It warns when interactive startup falls back or an earlier PATH entry shadows a different executable in a later entry (for example `/usr/bin/python3` before `/opt/homebrew/bin/python3`); duplicate entries and symlink aliases of the same executable are reported once. These diagnostics are advisory. `orbit run auto`, `orbit run ship`, MCP `orbit.workflow.auto` (`status` and `start`) and `orbit.workflow.ship` warn in two cases while required commands are configured. The first is when the login shell cannot be probed. The second is when resolution is disabled and `path` is empty. They also warn when the resolved PATH drops login-shell entries, which can happen under `replace`.

**The `system` name.** Shipped job steps such as `task_pilot_pipeline` name `crew: system` directly. At load that name is aliased onto the crew `workflow.system_crew` names, so `system_crew = "luna"` runs the task pilot on Luna. A user-authored `[crews.system]` table wins over the alias. Older configs without `system_crew` fall back to an existing `[crews.qa]`, then to the default crew. An unknown custom name is not substituted and fails at dispatch. A missing or unusable system crew leaves the original failed step failed, with a diagnostic naming `workflow.system_crew`.

**What `orbit init` seeds.** Only the global file, and only when it is absent (or under `--force`):

- every [built-in crew](#crewsname--which-provider-model-runs-the-task), with `enabled = true` on the crews of each detected provider CLI and `enabled = false` on the rest,
- `default_crew` set to the default crew of the first detected family in preference order,
- `system_crew` set to the first detected of `luna`, `sonnet`, `grok`, `antigravity`, `gemini`, `copilot`, `cursor`, `pi`, `opencode` (cheapest tier first),
- all four pools as `[]`.

Interactive init asks for both crews by name, offering only enabled crews, and skips the question when there is only one candidate. With no supported CLI detected, every crew is written with `enabled = false` and both keys stay unset: `default_crew` then resolves to the disabled `opus`, so dispatch refuses until you enable a crew rather than silently running one. Turning a provider on later is one command, `orbit config set crews.<name>.enabled true`. Init never writes `[crews.system]`, `[crews.custom]` or `[crews.qa]`.

---

## `[crews.<name>]` — which provider-model runs the task

A crew is one provider-model assignment. An activity uses the crew named in its rendered input, otherwise the run's resolved crew.

| Field | Required | Values |
|---|---|---|
| `provider` | Yes | `claude`, `codex`, `antigravity`, `gemini`, `grok`, `copilot`, `cursor`, `pi`, `opencode`. See [provider identity](#provider-identity-and-resolution). |
| `model` | Yes | Model ID passed to the provider CLI. |
| `enabled` | No | Boolean, default `true`. `false` keeps the crew defined and listed but refuses to run it; see [disabled crews](#disabled-crews). |
| `effort` | No | Reasoning effort; see the table below. Omitted leaves the provider's default. |
| `description` | No | Summary. Trimmed; blank becomes absent. |
| `tags` | No | Discovery labels. Trimmed, blanks dropped, sorted and deduplicated. |

```toml
[crews.sol]
model = "gpt-6.1-sol"
provider = "codex"
effort = "high"
description = "Systems implementation"
tags = ["implementation", "review"]

[crews.gemini]
enabled = false
model = "gemini-3.8-flash"
provider = "gemini"
```

**Built-in crews.** `orbit init` seeds every family's crews and enables a family's crews when it detects that family's binary. A config with no `[crews]` table at all uses the full set, plus a built-in `system` crew (`claude`, `sonnet`). Families are listed in init's preference order. Existing explicit model pins are kept as written.

| Family | Binary | Crews (model) | Default crew |
|---|---|---|---|
| `claude` | `claude` | `opus` (`opus`), `sonnet` (`sonnet`), `fable` (`fable`) | `opus` |
| `codex` | `codex` | `astra` (`gpt-6-astra`), `sol` (`gpt-6.1-sol`), `terra` (`gpt-5.6-terra`), `luna` (`gpt-6-luna`) | `astra` |
| `antigravity` | `agy` | `antigravity` (`gemini-3.8-flash-high`) | `antigravity` |
| `gemini` | `gemini` | `gemini` (`gemini-3.8-flash`) | `gemini` |
| `grok` | `grok` | `grok` (`grok-4.7`) | `grok` |
| `copilot` | `copilot` | `copilot` (`claude-sonnet-5`) | `copilot` |
| `cursor` | `cursor-agent` | `cursor` (`gpt-5`) | `cursor` |
| `pi` | `pi` | `pi` (`sonnet`) | `pi` |
| `opencode` | `opencode` | `opencode` (`anthropic/claude-sonnet-4-5`) | `opencode` |

**Reasoning effort.** Choose capability with the model first, then tune `effort` inside it.

| Provider | Accepted `effort` | Rendered as |
|---|---|---|
| `claude`, `codex`, `pi` | `low`, `medium`, `high`, `xhigh`, `max` | `--effort` · `model_reasoning_effort` · `--thinking` |
| `antigravity` | `low`, `medium`, `high` | `--effort` |
| `opencode` | `high`, `max` | `--variant` |
| `grok` (`grok-4.7`, `grok-4.6`) | `low`, `medium`, `high`, `xhigh` | `--reasoning-effort` |
| `grok` (`grok-4.5`) | `low`, `medium`, `high` | `--reasoning-effort` |
| others | none | — |

An invalid or unsupported `effort` (for example `max` on Grok, or `effort = "hard"`) is ignored for that crew with a warning naming the file, crew, value and accepted values. It is never remapped. `orbit doctor` lists ignored properties, and `orbit config set` refuses to write a value load would drop. A selected crew's provider, model and effort all override an activity's inline baseline.

**Editing crews.** Crew fields are addressable as `crews.<name>.<field>`:

```bash
orbit config set crews.sol.effort high
orbit config get crews.sol.effort
orbit config set crews.gemini.enabled true
```

`config set` refuses invalid values, unsupported provider/model combinations and misspelled fields before writing. It cannot create a crew: add a `[crews.<name>]` table with `model` and `provider` first. `orbit.workspace.list` with `include: ["crews"]` returns, on each workspace row, the normalized crews of that checkout's effective config, each with its `enabled` state (schema version 3), or `crews_error` when that configuration cannot be read.

### Disabled crews

`enabled = false` switches a crew off without deleting its definition. A table without the key is enabled, so configs written before the flag existed resolve exactly as they did.

- **Listings show it.** `orbit config show` has an `ENABLED` column and counts disabled crews in the heading, `orbit config get crews.<name>.enabled` answers `true` or `false`, `orbit.workspace.list` with `include: ["crews"]` carries `enabled` per crew, and the dashboard's Config tab marks the row disabled and offers an enabled toggle.
- **Pools skip it.** A disabled crew is never drawn from a complexity pool. A pool whose members are all disabled behaves like an empty pool: the task falls through to `default_crew`. A disabled crew may still be listed in a pool, so re-enabling it restores its share without editing the pool.
- **Explicit references refuse.** Dispatch never substitutes another crew. A task's `crew`, an explicit run or activity crew, `workflow.default_crew`, `workflow.system_crew`, or a shipped job step's `crew: system` that resolves to a disabled crew fails with the crew's name and `orbit config set crews.<name>.enabled true`. When `system` mirrors `workflow.system_crew`, the message names the mirrored crew's table.
- **Load still succeeds.** A lane key pointing at a disabled crew does not stop unrelated commands. `orbit doctor` reports it as a `config` warning with the enabling command.
- **Task creation does not pin it.** A crew-less task created while `default_crew` is disabled keeps `crew` unset, so dispatch resolves and refuses the default by name.
- **Reads still resolve.** `orbit task show` and the dashboard still report a task's configured crew when it is disabled.

**Validation.**

- `model` and `provider` must be non-empty.
- `default_crew` must name a defined crew. If you define crews but leave `default_crew` unset, Orbit uses `opus` (or a legacy `claude` crew) when defined, and otherwise refuses to load.
- A crew name may not contain `:`, because the pool grammar reserves it. See [repair](#repairing-a-config-that-already-names-a-crew-with-a-colon).
- An Antigravity crew must use an `agy models` slug. A bare Gemini CLI ID such as `gemini-3.8-flash` fails with migration guidance.

**Retired crew shapes.** These fail load with migration guidance:

- `planner` / `implementer` / `reviewer` sub-tables: rewrite as flat `model` and `provider`.
- `backend`: `cli` is accepted and ignored, while `http` and `auto` are refused. The same applies to `ORBIT_BACKEND` and `[runtime] backend`. Remove the key.
- `[agent.<role>]` tables: migrate to `[crews.<name>]` plus `workflow.default_crew`.

### Repairing a config that already names a crew with a colon

A colon-named crew makes every command refuse to load, including `orbit config set`, so fix the file by hand. The error names the file and table:

```
error: invalid input: [crews]: crew name 'gpt-5:codex' must not contain ':'; ...
Edit '~/.orbit/config.toml' and rename or remove the [crews."gpt-5:codex"] table, then rerun the command
```

Rename the table to a colon-free name (or delete it), update anything that referenced the old name (`default_crew`, `system_crew`, pool entries, a task's `crew`), and rerun. Each layer is checked before merging, so the message names the file that actually defines the crew.

---

## Provider identity and resolution

Every `provider` string, whether in a crew, an activity's inline `provider`, or setup detection, goes through one canonical parser.

### Canonical providers

| ID | CLI binary | Notes |
|---|---|---|
| `claude` | `claude` | |
| `codex` | `codex` | |
| `gemini` | `gemini` | Legacy Gemini CLI (enterprise / API-key deployments). |
| `grok` | `grok` | |
| `copilot` | `copilot` | [GitHub Copilot CLI](#github-copilot-cli) |
| `cursor` | `cursor-agent` | [Cursor Agent CLI](#cursor-agent-cli) |
| `pi` | `pi` | [Pi CLI](#pi-cli) |
| `antigravity` | `agy` | [Antigravity CLI](#antigravity-cli) |
| `opencode` | `opencode` | [OpenCode CLI](#opencode-cli) |
| `ollama` | — | Recognized but unsupported at the Orbit CLI entry point. Selecting it fails with `provider.unsupported`. |
| `openai_compat` (`openai-compat`) | — | HTTP-only, with no CLI runtime. Selecting it fails. |

- Parsing is case- and whitespace-insensitive.
- Deprecated aliases resolve with an `orbit.config.crew` warning: `anthropic` → `claude`, `openai`/`chatgpt` → `codex`, `google` → `gemini`, `xai` → `grok`. Update the config.
- `copilot`, `cursor`, `pi`, `antigravity` and `opencode` have no aliases. The model vendor a lane runs (a Claude model through Copilot, say) never changes the provider identity.
- The cross-repo Worker executor runs only `claude`, `codex`, `gemini` and `grok`. A Worker-routed step naming another lane is refused rather than re-pointed.

Process audit attribution resolves `ORBIT_AGENT_NAME` and `ORBIT_AGENT_MODEL`
to a canonical agent family. If that pair is invalid (for example, `copilot`
with a `claude-*` model), or an agent envelope has no identity, Orbit records
`unknown` and emits a warning. Agent envelopes include non-empty name/model
values, a truthy `ORBIT_MANAGED_RUN_CONTEXT`, and
`ORBIT_TASK_ACTOR_KIND=agent`. These signals take precedence over
`ORBIT_ACTOR`, the operator override, and the OS username for attribution.
This changes the recorded identity only; command authorization still uses its
existing agent-envelope rules.

### Resolution precedence

**Which crew a task dispatches.** The first tier that is set wins:

1. **explicit**: `--crew` or run-input `crew`.
2. **task_config**: an explicit `task.crew` or a pool assignment valid for the current tier. Tasks normally get this at creation; admission redraws stale pool assignments before resolving the run (see [pools](#automatic-crew-pools-by-complexity)).
3. **workspace_default**: `workflow.default_crew`.
4. **environment_default**: `CONSTELLATION_DEFAULT_PROVIDER`, a provider ID or alias. `claude` selects `opus` and `codex` selects `sol` when defined, and any other ID selects the same-named crew. It never overrides a configured `default_crew`.
5. **system_default**: the `opus` crew, or a legacy `claude` crew.

**Which crew an activity uses.** A non-empty `crew` in the activity's rendered input, otherwise the run's crew. Activity and job assets that declare `role` are rejected.

**Crew over inline baseline.** For each of `provider`, `model` and `effort`, the selected crew's value wins when present, and otherwise the activity's inline `agent_loop` value stands. An unrecognized crew `provider` is logged and falls back to the inline provider, so a typo never moves dispatch onto a different runtime. A provider already recorded on a run is reused verbatim on reconciliation.

### No silent fallback

Explicit selections that cannot run fail with a stable diagnostic and never fall back to another runtime:

- `provider openai_compat is unsupported by the Orbit CLI entry point (HTTP-only)`
- `provider ollama is unsupported by the Orbit CLI entry point`
- `unknown provider '<x>'; expected one of claude, codex, gemini, grok, copilot, ollama, openai_compat, cursor, pi, antigravity, opencode`
- A selected lane whose binary is missing fails with a permanent diagnostic naming the binary.

---

## Provider CLI notes

Common to every lane below: Orbit passes `--model` from the crew, sends the prompt on **stdin** (never argv, which is visible in process listings and audit), and treats the Orbit OS sandbox as the filesystem boundary. Credentials reach the agent only through [`[execution.env].pass`](#executionenv--the-agent-subprocess-environment), and Orbit never puts a key on argv. Provider-specific write grants apply only while that provider is running. Lanes without native MCP (or without Orbit-managed MCP config) reach Orbit tools with `orbit tool run <tool> --input '<json>'` through their shell tool, under the same grants.

For `orbit tool run`, an explicit `workspace` in the JSON input or the global `--workspace` flag selects a registered workspace from any current directory. The selector may be its name, `ws_*` ID, absolute local checkout path, or a local `hm_*/ws_*` selector copied from federated workspace listing. A selector naming another host fails locally and names that host; run the command on its owner host instead. JSON that does not parse, and an `--input-file` that cannot be read, are reported as that input error before workspace selection, runtime open, or migration. A typo in the input does not migrate the current checkout.

## GitHub Copilot CLI

| | |
|---|---|
| Install | `npm install -g @github/copilot`, then `copilot --version`. The retired `gh copilot` extension is not supported. |
| Auth | `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, else the `copilot /login` session (macOS keychain item `github-copilot-app`). Orbit forwards none of the token variables unless you list one in `pass`. The recommended Mac setup is `/login` once. |
| Model | `--model` is always passed, so `COPILOT_MODEL` and the CLI's saved `/model` are ignored. List IDs with `/model` inside `copilot`, and recheck pins after upgrading the CLI. |
| Flags | `--allow-all-tools --no-ask-user --output-format json`. Never `--allow-all`/`--yolo`, which would widen paths and URLs. |
| Sandbox | Write: `COPILOT_HOME` (default `~/.copilot`) and `$XDG_CACHE_HOME/copilot`. macOS: read `~/Library/Keychains`. No access to `~/.config/gh`. |

An account without Copilot entitlement fails with `Error: Authentication failed`. Third-party MCP servers disabled by org policy produce a `session.warning` (`warningType: "policy"`). Both are fixed by the GitHub org admin, not in Orbit. A run with no assistant message fails the step.

## Cursor Agent CLI

| | |
|---|---|
| Install | `curl https://cursor.com/install -fsS \| bash`, then `cursor-agent --version`. Put `~/.local/bin` on `PATH`. Cloud agents are not used. |
| Auth | `cursor-agent login` (check with `cursor-agent status`), stored in the macOS login keychain by default. A file-store login (`AGENT_CLI_CREDENTIAL_STORE=file` at login) also needs `pass = ["AGENT_CLI_CREDENTIAL_STORE"]`. Alternatively `pass = ["CURSOR_API_KEY"]`. |
| Model | `cursor-agent models` (or `--list-models`). |
| Flags | `--print --force --output-format json`. The terminal `{"type":"result","subtype":"success","is_error":false,"result":…}` object is validated before the envelope is read. |
| Sandbox | Write: `~/.cursor`. macOS: read `~/Library/Keychains`. |

## Pi CLI

| | |
|---|---|
| Install | `npm install -g @earendil-works/pi-coding-agent`, then `pi --version`. |
| Auth | `/login` in an interactive `pi` session (stored under `$PI_CODING_AGENT_DIR`, default `~/.pi/agent`), or a vendor key via `pass`. Orbit never renders `--api-key`. |
| Model | `--model` takes a pattern that may carry a vendor prefix (`openai/gpt-4o`). List with `pi --list-models`. `effort` renders as `--thinking <level>`. |
| Flags | `--mode json --no-session --no-approve --offline`: ephemeral sessions, no inherited project trust (so project-local `.pi` extensions don't run, while `AGENTS.md`/`CLAUDE.md` still load), and no startup network calls. |
| Sandbox | Write: `$PI_CODING_AGENT_DIR`, else `~/.pi`. |

Pi has no MCP client, and `orbit mcp init` offers none. Keep Pi's `bash` tool available, because Orbit tools go through it. Orbit reads completion only from the final assistant `message_end` frame, and reports no provider token usage for Pi runs. Raw output is still kept in the audit blob store.

## Antigravity CLI

`antigravity` launches `agy`, Google's current terminal agent. It is a separate lane from `gemini` (the legacy Gemini CLI), with different flags, MCP config and model slugs. `gemini-*` models still attribute to the Gemini model family.

| | |
|---|---|
| Install | See the [Antigravity CLI docs](https://www.antigravity.google/docs/cli/headless/), then `agy --version` and `agy models`. Init prefers it over `gemini` when both are present. |
| Auth | One interactive `agy` login, cached under `~/.gemini/antigravity-cli/`. An unauthenticated headless run exits with `authentication required`. |
| Model | A slug from `agy models`. `effort` accepts `low`/`medium`/`high` only. To migrate a `provider = "gemini"` crew, change the provider and switch to an `agy models` slug. |
| Flags | `--input-format stream-json --output-format stream-json --dangerously-skip-permissions`, plus `--print-timeout` set to the activity deadline minus 30 s (a shorter custom value is kept). Don't add `agy --sandbox` or Gemini CLI flags. |
| Sandbox | Write: `~/.gemini` (shared with the Gemini CLI). macOS: read `~/Library/Keychains`. |
| MCP | `~/.gemini/config/mcp_config.json` or `.agents/mcp_config.json`, not `.gemini/settings.json`. |

A terminal `result` with `status: "SUCCESS"` completes the step. On a non-zero exit with a terminal `ERROR`, Orbit surfaces the bounded, redacted `error` string.

## MCP client registration

`orbit workspace init --mcp` registers Grok through the shared project
`.mcp.json` used by Claude Code. `orbit mcp init --scope home --grok` uses
`~/.claude.json`; Grok's [MCP compatibility documentation](https://docs.x.ai/build/features/mcp-servers)
lists both locations. Re-running init migrates an older Orbit-generated Grok
entry out of `.grok/config.toml` while preserving other Grok settings and
servers. When `[claude_compat] imported = true` in `~/.grok/config.toml`,
Grok ignores project `.mcp.json` and init retains the native `.grok/config.toml`
target. A disabled `[compat.claude] mcps` setting or
`GROK_CLAUDE_MCPS_ENABLED=0` likewise keeps the native home target. Inspect
the loaded source with `grok inspect`. Removing a shared `orbit` registration
through either Claude or Grok removes that one entry for both clients.
Gemini CLI continues to use `.gemini/settings.json` (or
`~/.gemini/settings.json` for home scope): its [configuration reference](https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/configuration.md)
does not document the shared MCP locations as a settings source.

## OpenCode CLI

| | |
|---|---|
| Install | `opencode --version`, then `opencode models`. Served modes (`serve`, `--attach`, `web`) are not used. |
| Auth | `opencode auth login` (stored in `auth.json` under `$XDG_DATA_HOME/opencode`), or a vendor key via `pass`. |
| Model | Must be a fully qualified `<vendor>/<model>`, for example `anthropic/claude-sonnet-4-5`. `effort` renders as `--variant` and accepts `high`/`max` only. |
| Flags | `run --format json --auto`. `--auto` is required unattended (without it every permission request is auto-rejected) and is not a security boundary. `--continue`, `--session` and `--share` are never passed. |
| Sandbox | Write: `$XDG_DATA_HOME/opencode`, the config root (`$OPENCODE_CONFIG_DIR`, else `$XDG_CONFIG_HOME/opencode`), `$XDG_STATE_HOME/opencode` and `$XDG_CACHE_HOME/opencode`. |

Orbit does not write OpenCode's `opencode.json` MCP config, and `orbit mcp init` has no OpenCode target. Orbit tools go through OpenCode's shell tool. Only assistant `text` parts are read, and a terminal `error` event or a non-zero exit fails the step. No provider token usage is reported.

---

## Per-task crew override

`workflow.default_crew` is only the fallback. Every task has an optional `crew` field, and `orbit run ship` resolves each task's crew by the [resolution precedence](#resolution-precedence), so one ship can mix crews. Each child run records its crew (`orbit run show` → `resolved_crew`). A single child pipeline whose tasks name different crews, or mix set and unset crews, fails rather than falling back to the default.

### Automatic crew pools by complexity

```toml
[workflow]
low_complexity_crews = ["luna"]
medium_complexity_crews = ["grok", "terra"]
hard_complexity_crews = ["astra"]
xhard_complexity_crews = ["fable", "astra"]
```

- **Crew is selected when the task is created.** A task created without `crew` (via `orbit task add`, `orbit.task.add`, an auto-task mint with no template crew, or an import) draws from the pool for its complexity, falling back to `default_crew`, and stores the result in `task.crew` and the provenance in `task.crew_source`. A `crew_assigned` history entry also records the source: `explicit`, `pool:<complexity>` or `default`. A workspace with no crews configured leaves the field unset.
- **Complexity changes redraw pool assignments.** When the pilot or `task update --complexity` changes the tier, a crew sourced from another complexity pool is redrawn for the new tier in the same mutation. A `crew_redrawn` history entry names the previous and new tiers and crews. Explicit crews stay pinned, including when an update explicitly names the crew already selected by a pool. Status transitions alone preserve the selection. An explicit `task update --crew <name>` pins the selection; [`task update --crew ""`](#setting-taskcrew) draws again. Every stored crew change records the actor, prior and new crew, and whether it came from an explicit name or a pool draw.
- **Tiers:** `low`, `medium`, `hard`, `xhard`. Unset or `unassessed` complexity uses the default chain. The task pilot never demotes a task out of `xhard`.
- **Empty pool** (`[]`, the init scaffold) means no pool, so the task gets `default_crew`. A pool whose members are all [disabled](#disabled-crews) is treated the same way; disabled members of a mixed pool are skipped. Blank entries and unknown crew names fail before dispatch.
- **Pools are preferences, not allowlists.** An explicit `task.crew`, an explicit run crew, and system, review and preparation jobs keep the crew they name. The one exception is a standing [provider failure hold](#provider-failure-holds), which redirects a task-crew draw away from the crews it excludes.
- **Admission and legacy tasks.** At admission (drain or ship), a pool-sourced crew is checked against the current tier and its enabled, positive-weight pool members. A stale assignment draws from the current pool, falling back to the default chain. Legacy assignment history recovers provenance when `crew_source` is absent; a crew with no assignment evidence is treated as explicit. Tasks without a crew also use the current pool. Admission does not write back to the task.
- **Run overrides.** `orbit run auto --low-complexity-crews …` (and `--medium-…`, `--hard-…`, `--xhard-…`) replaces that one pool for one drain, and the flag with no names disables it. `orbit run ship` has no override flags.

#### Weighting a pool

```toml
[workflow]
low_complexity_crews    = ["luna:50", "sonnet:50"]
medium_complexity_crews = ["grok:70", "opus:10", "sol:20"]
hard_complexity_crews   = ["opus", "sol"]          # bare = uniform
```

- Weights are relative non-negative integers, so `["grok:7", "sol:3"]` gives the same odds as `["grok:70", "sol:30"]`.
- A pool is either all bare or all weighted. Mixing them, or a suffix like `grok:-1` or `grok:2.5`, is an error. The same checks run at load, in `orbit config set`, and on the CLI flags.
- Bare duplicates collapse to one ticket. A weighted pool names each crew once.
- Weight `0` parks a crew, which is never drawn. At least one entry must weigh more than 0.
- The draw walks the pool in canonical name order over `[0, total_weight)`.

**Allowlists.** With `--allow-crew`, the draw renormalizes over the permitted members, preserving their ratios. A pool with no permitted positive-weight member is disjoint, and `orbit run readiness --allow-crew <crew>` reports `crew_not_allowed`. The allowlist is checked against `task.crew`, and still applies to system and review activities at dispatch.

**Frozen per run.** The admitting run captures the effective pools in run input `auto_crew_pools`, and descendants inherit that copy. Each admitted leaf records `crew` and `crew_selection` (task ID, complexity, source, and the eligible `[{name, weight}]` after renormalization), shown as `Crew Selection:` in `orbit run show`. Retries and resumes keep the admitted selection.

### Provider failure holds

Sometimes a local run fails because of its provider rather than the work. The step's error text then carries one of three typed markers, each followed by `provider=<name>`:

- `[provider_capacity]`: the selected model was at capacity.
- `[provider_unavailable]`: the provider could not be used on this host, for example because authentication failed.
- `[provider_refusal]`: the provider's content policy refused the task. This covers a Codex content-filter `error` or `turn.failed` frame (`This content was flagged for possible …`) and a Claude `result` with `stop_reason: "refusal"`. Only text the failing provider CLI wrote counts. The agent's transcript, tool output and Orbit envelopes never set the marker.

Step recovery and final recovery skip these failures. On the PR pipeline, the failure handoff returns `held_provider_failure`. It commits the candidate on the run's branch, but pushes nothing and opens no `[BLOCKED]` PR.

Run finalization then moves the task to `backlog` under a `provider_failure_hold` history event, where a non-provider failure would block it. The event's note carries the hold: the failure class, the provider, the excluded crews, a `not_before` time and the run.

**Which crews are excluded:**

- A capacity or unavailability failure excludes the crew the run used. If the failing step ran on another provider (a reviewer, say), it excludes every crew of that provider instead.
- A refusal excludes every crew of the refusing provider, because the same content would be refused again.
- If an earlier hold still stands, its excluded crews stay excluded.

**How long the hold lasts:** the base backoff is 15 minutes for capacity, 30 minutes for unavailability and 24 hours for a refusal. It doubles for each other hold the task got in the last 24 hours, up to 24 hours.

**Admission during the hold:** until `not_before`, the task's crew is drawn from the crews the hold does not exclude. The draw tries, in order:

1. the task's own crew or pool;
2. its complexity pool;
3. `default_crew`.

`crew_selection.source` names the hold. When every one of those crews is excluded, the local drain defers the task, and `orbit run readiness --json` reports it with `reason: "provider_backoff"` and the release time. An explicit run-input `crew` ignores the hold.

Once `not_before` passes, or any later status change happens, the hold no longer applies. The next run resumes the committed candidate.

Pull drains and claimed leaves keep their own handling. An authentication failure there releases the claim and excludes every crew of that provider for the drain window. Provider labels are parsed first, so a crew configured as `anthropic` is the same provider as `claude`. A capacity failure excludes only the crew the leaf ran: another model on that provider may still have room.

### Final recovery pool

`workflow.final_recovery_crews` sets the weighted crew pool the `final_recovery` activity draws from once per run when a task fails after step-failure recovery is exhausted.

```toml
[workflow]
final_recovery_crews = ["sol:100", "opus:20"]
```

- **Draw:** One crew is drawn per run from this pool to evaluate the task failure and attempt a final typed decision and recovery fix.
- **Built-in default:** Unset defaults to `["sol:100", "opus:20"]` (the strongest reasoning crews).
- **Graceful filtering:** To tolerate custom configurations that may not define `sol` or `opus`, an unset key filters the default pool to retain only the crews defined in `[crews.*]`. If neither crew is defined, the admitted pool is empty and final recovery is quietly disabled rather than failing config validation.
- **Explicit entries fail loud:** If you explicitly define `final_recovery_crews`, entries must follow the standard pool grammar (`name` or `name:weight`), and every named crew must exist in `[crews.*]`.
- **Disabling:** Set `final_recovery_crews = []` to disable final recovery entirely.

### Setting `task.crew`

| Surface | How |
|---|---|
| Dashboard | The crew dropdown on each task card. The label `default: <crew>` means the task has no `crew` and inherits `default_crew`. |
| CLI | `orbit task add --crew <name>`, or `orbit task update <id> --crew <name>`. Passing `--crew ""` to `update` re-draws for the current complexity. |
| MCP | The `crew` parameter on `orbit.task.add` / `orbit.task.update`. An empty string on update re-draws; `null` is rejected. Omitting `crew` preserves an explicit pin; changing complexity can redraw a pool assignment. |

### What "ran" vs what "was selected"

`orbit.task.show` returns `crew` (the stored selection) and `crew_source` (its provenance, absent on legacy records), and separately annotates it with `resolved_crew` and `crew_model` when this host can resolve it. An associated run's persisted resolution takes precedence over current configuration. A one-field `fields: ["crew"]` projection returns the same stored selection. `task.crew` is validated on write. If you later delete an explicitly pinned crew, `orbit run ship` fails at run start, before any agent dispatches. Pool assignments are revalidated at admission; keep the configured pools valid when deleting their members.

---

## Sandbox write grants for the shared Cargo caches

Both OS sandboxes (macOS `sandbox-exec`, Linux Bubblewrap) let workers share one Cargo cache:

| Path | Inside a worker |
|---|---|
| `$CARGO_HOME/registry`, `$CARGO_HOME/git` | read-write |
| `$CARGO_HOME/.package-cache`, `.package-cache-mutate` | read-write (cargo's download locks, so concurrent workers stay serialized) |
| `$CARGO_HOME/bin` | read and execute, never write |
| `$CARGO_HOME/credentials.toml`, `$CARGO_HOME/credentials` | read-denied |
| `$CARGO_HOME`, `$CARGO_HOME/.global-cache` | read-only |

- `$CARGO_HOME` is the child's variable if you pass it through `[execution.env].pass`, else `~/.cargo`.
- The grant applies only to profiles that already grant some write (`implementer`, `unrestricted`, `docs_writer`, …). Read-only profiles such as `reviewer` and `pure_compute` get none.
- On Linux, a cache path that doesn't exist on the host is skipped rather than created.

## Sandbox pseudo-tty allocation (macOS)

The macOS profile allows `pseudo-tty`, `/dev/ptmx` (read, write, ioctl), and read, write and ioctl on `/dev/ttys[0-9]+`, so `openpty`/`posix_openpt` work inside a worker (for example, a test that drives a CLI through a real terminal). Access to other PTYs remains subject to normal OS checks. Linux needs no equivalent rule.

---

## `[execution.env]` — the agent subprocess environment

Every agent subprocess (bare, Bubblewrap or `sandbox-exec`) starts from a **cleared** environment and receives only:

| Group | Contents |
|---|---|
| Baseline | `HOME`, `LANG`, `LC_ALL`, `LOGNAME`, `PATH`, `SHELL`, `TERM`, `TMPDIR`, `TZ`, `USER` |
| `pass` | Names listed in `execution.env.pass`. Default: `HOME`, `PATH`, `CODEX_HOME`, `TMPDIR`, `USER`, plus `__CF_USER_TEXT_ENCODING` on macOS. |
| Provider extras | Variables the selected provider runtime declares it needs. |
| Orbit envelope | Named `ORBIT_*` execution variables: run, task and session identity (`ORBIT_RUN_ID`, `ORBIT_TASK_ID`, `ORBIT_SESSION_ID`, …), locators (`ORBIT_WORKSPACE`, `ORBIT_WORKTREE_ROOT`, `ORBIT_BIN`, `ORBIT_REGISTRY_ROOT`, …) and activity bindings (`ORBIT_ACTIVITY_*`, `ORBIT_STEP_INDEX`, …). Privilege-bearing names (`ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN`) are not admitted, and an inherited `ORBIT_ROOT` is removed. |

This is an allowlist, not a secret filter. A benignly named credential such as `DATABASE_URL` never reaches an agent unless you name it. Agents keep network access, so this is the boundary against accidental exfiltration.

```toml
[execution.env]
pass = ["HOME", "PATH", "CODEX_HOME", "TMPDIR", "USER", "GITHUB_TOKEN"]
```

- `pass` replaces the default instead of extending it, so restate the baseline names you want.
- A listed name that is unset is absent, not empty. Names must be valid identifiers.
- `pass` is a security key: a workspace file that omits it gets the built-in default, not the global value.
- Inheriting the full environment is not configurable. A stale `execution.env.inherit` key is ignored.

---

## `[security_alert_sweep]` — security finding severity

`security_alert_sweep.min_severity` is a string: `low`, `moderate`, `high` or
`critical`. The built-in default is `moderate`. It governs Dependabot and Code
scanning findings filed by `dependabot_alert_sweep_pipeline`; secret scanning
findings are always filed regardless of this floor.

```toml
[security_alert_sweep]
min_severity = "high"
```

Set it in the workspace `.orbit/config.toml` or global `~/.orbit/config.toml`.
Precedence is explicit run/job `min_severity` input > workspace config > global
config > built-in `moderate`. For example,
`orbit run job dependabot_alert_sweep_pipeline --input min_severity=critical`
overrides the floor for that run without changing config. Scheduled routines
use the configured floor without shadowing the job. Invalid config values fail
validation with `security_alert_sweep.min_severity` named in the error.

The file step records the effective `min_severity` and `min_severity_source`
(`input`, `workspace`, `global` or `built-in`), plus
`excluded_below_min_severity`. `orbit run show <RUN_ID>` summarizes the filed
count, floor with its source, and excluded alert count and numbers even when
the sweep succeeded.

## Other sections

| Key | Default | What it does |
|---|---|---|
| `execution.codex.sandbox` | `workspace-write` | Codex sandbox mode: `read-only`, `workspace-write` or `danger-full-access`. Under `workspace-write`, Codex's extra writable roots are Orbit's runtime stores, never the whole workspace `.orbit` or `~/.orbit`. The file `orbit init` seeds sets `danger-full-access` globally. Security key, not inherited by a workspace file. |
| `execution.codex.approval_policy` | unset | `untrusted`, `on-request` or `never`. Security key. |
| `execution.proc_spawn_max_timeout_minutes` | `45` | Longest timeout one `proc.spawn` call may run with inside a managed activity (1–1440). A call is also capped at the activity's remaining wall-clock budget, which the CLI runner passes to the agent as `ORBIT_ACTIVITY_DEADLINE_UNIX_MS`. Outside an activity the ceiling stays 60 seconds. The tool result reports the effective `timeout_ceiling_ms` and its source. |
| `review.before_pr` | `false` | Before-PR review: hold PR creation for a fresh reviewer that fixes what it finds. PR route only: refused for local-only delivery. A delivery run or drain captures the value at submission, so a run in flight keeps it. Distributed PR claims use the owner's captured review contract, executed by compatible followers (details below). See [review-gate design](design/review-gate/2_design.md). |
| `review.minutes` | `30` | Wall-clock limit for one candidate's before-PR review, its fix commit and final validation included. Each candidate gets one review: a retry or resume continues it within the same minutes, the running reviewer is stopped when they run out, and a spent review is not restarted. A changed candidate, such as a completion rebase, is a new review (1..=1440). |
| `review.baseline_commands` | `[]` | Commands before-PR settlement may rerun on the host to confirm a reviewer's claim that a failed required check also fails on the pinned base (`workflow.required_validation_commands` always count). A confirmed claim holds the task for the red base instead of blocking it; see [red base](#workflowvalidation_env--the-toolchain-required-validation-runs-with). |
| `[[review.host_evidence]]` | `[]` | Checks a claimed leaf owes for the paths its candidate changed, derived by Orbit rather than read from the reviewer's report. Each rule has `kind` (`codeql` or `host_sandbox_test`), `name`, `paths` (workspace-relative globs), `os` (`linux` or `macos`), the exact `command`, and the result `artifact` (a relative `.json` path, unique across rules). A `codeql` rule is owed on a host of another OS and fulfilled by the owner on `os`. A `host_sandbox_test` rule is owed on a host of `os`, which runs it outside the agent sandbox at settlement, so its command must be one that settlement admits. The owner captures the rules on each claim. The reviewer records an owed check `not_run` and never attempts it, and a verdict whose only gaps are owed checks holds for them instead of blocking. A workspace list replaces the global one. Example: `kind = "codeql"`, `paths = ["**/*.rs"]`, `os = "linux"`, `command = "scripts/codeql-rust-local.sh …"`, `artifact = "evidence/codeql-rust-linux.json"`. See [owed evidence](design/review-gate/2_design.md#4-what-the-validation-records-establish-orb-11528-orb-11545). |
| `operation.review_crew` | unset | Crew for automatic review: the before-PR reviewer, and the crew of every review task the after-landing `delivery-code-review` consumer mints (unset, that definition's template crew). |
| `tasks.id_start` | unset | Forward-only floor for this machine's task-ID allocator, raised on every runtime build and never lowered, so machines can hold disjoint ranges. For the first seed prefer `orbit workspace init --task-id-start N`. See [task migration](design/task-migration/1_overview.md). |
| `automation.stall_window_minutes` | `60` | How long a delivery-automation consumer may sit on a stuck deferral (`history_diverged`, `repository_changed`, `provider_identity_missing`, `state_missing`) before a warning and one deduped friction (1–1440). Transient backpressure never escalates. See [auto-tasks](../plugin/skills/orbit-setup/references/auto-tasks.md). |
| `scoring.enabled` | `true` | Record per-agent scoreboard metrics for task runs. |
| `pr.task_url_template` | unset | URL template linking a task ID in PR descriptions. |
| `pr.close_on_terminal` | `true` | When a task lands (`done`), is rejected or is archived, close its open Orbit-authored PRs: delivery PRs and `[BLOCKED]` preservation PRs. The closing comment names the landing (the done transition's note, such as `delivered by pull request #N merged as <sha>` or `already landed as <sha>`) or the state and its reason. On `done` the landing PR stays open: any `#N` the note names, or, when the note names no landing and the task leaves `review`, the PR it was reviewed through. A PR qualifies only if its head branch is `orbit/<TASK-ID>-…`, its body names the task, and it was opened by a `pr.delivery_authors` login or is the delivery PR of a follower handoff this owner accepted. Human PRs and other tasks' PRs are never touched, and branches are never deleted. A new run, block or requeue closes nothing, so a re-run can resume from its `[BLOCKED]` PR. Tasks with no recorded PR make no forge call. A forge error is logged as a warning and never fails the transition. `false` disables closing. |
| `pr.delivery_authors` | `[]` | Forge logins (case-insensitive) whose PRs count as Orbit-authored for `pr.close_on_terminal`. Empty uses the login `gh` is authenticated as on this machine. List a follower's login here if it opens PRs this owner has not accepted as a handoff. |
| `runtime.log_retention_days` | `7` | Delete archives of both `orbit.jsonl` and `orbit-agent.jsonl` older than N days (≥ 1). |
| `runtime.log_max_total_mb` | `500` | Operational `orbit.jsonl` archive budget in MiB, pruned oldest first (≥ 1). |
| `runtime.log_max_file_mb` | `100` | Roll the active operational log past N MiB (≥ 1, ≤ `log_max_total_mb`). |
| `plugin.legacy_callback_identity` | `false` | Deprecated. Also accept the environment token and process ancestry as a plugin callback credential. Removed next release. |

Agent relay output lives beside the operational feed in `orbit-agent.jsonl`, with an independent 200 MiB archive budget and 50 MiB active-file limit. The dashboard and `orbit log tail` merge both active feeds. Agent volume cannot consume the operational size budget; each feed is still pruned when its own size or age budget is exceeded.

Full log rotation runs in long-lived processes (`orbit mcp serve`, `orbit clock tick`/`orbit sweep`, `orbit web serve`). Short-lived commands roll only an oversized active file. `[review]` and `[operation]` keys resolve built-in → global → workspace, and unknown keys in either table fail load.

For distributed PR delivery, the owner captures its `review.before_pr` policy and, when enabled, the review contract version, `operation.review_crew`, `review.minutes` budget, `[[review.host_evidence]]` rules, and `workflow.required_validation_commands` list when it admits the claim. A follower with a matching binary/protocol, a before-PR gate, and the ability to resolve that captured reviewer crew executes the contract before opening the PR. An unset or unresolvable reviewer is refused as `before_pr_reviewer_unavailable`; the implementer's crew is never substituted. The follower's own `before_pr` value is declared for diagnostics and neither adds nor removes the owner's gate. Its local command list cannot replace the owner-admitted review requirements. The handoff carries typed review evidence binding the reviewed head and base, reviewer identity, and an owner-accessible certificate artifact; the owner verifies it against the captured contract and candidate before accepting. Every captured command needs a required passing review record; review settlement uses the frozen list. Handoff acceptance also requires the owner's current command list and the certificate's list to match that snapshot, so owner-policy drift fails closed with fresh-claim guidance. Protocol revision 8 carries the list: explicit `[]` means no required checks, while a missing legacy field cannot establish the contract and is refused. After-landing review remains owner-side and does not refuse a pull.

For a workspace registered for local-only delivery with effective `review.before_pr = true`, `orbit doctor` reports the incompatibility in its `review` check, naming the deciding config layer as `review.before_pr (global)` or `review.before_pr (workspace)`. Its remedy is to ship through the PR route or turn `review.before_pr` off for local delivery; after-landing review is the `delivery-code-review` auto-task. `orbit run readiness --json` reports otherwise eligible local tasks with `eligible: false`, `reason: "local_route_before_pr"`, and a `detail` carrying the same explanation and config layer. The local drain withholds those tasks before spawning delivery; earlier per-task exclusions retain their own reasons. PR-route tasks remain eligible subject to the normal gates. A workspace override of `before_pr = false` clears the local hold, while direct `task_local_pipeline` admission still fails closed whenever its captured policy enables before-PR review.

## Plugins — `.orbit/plugins.yaml` and `[plugins.<ns>]`

Plugins install once per machine (`~/.orbit/plugins/<ns>/<version>/`). Enable state and grants are host-local. A checkout keeps only its pin file, which git ignores with the rest of `.orbit/`, and `orbit plugin sync` installs what the pins name. The full operator guide is the orbit-setup [plugins reference](../crates/orbit-core/assets/skills/orbit-setup/references/plugins.md), and the manifest spec is the [plugin standard](design/plugins/1_scope.md).

Linked Git worktrees share the main checkout's `.orbit/plugins.yaml`. Runtime plugin loading, `orbit plugin sync` and `orbit plugin doctor` all read that shared file; a worktree-local `.orbit/plugins.yaml` is ignored.

```yaml
# .orbit/plugins.yaml — per-checkout, gitignored
schemaVersion: 1
plugins:
  - name: graph
    version: "^0.4.1"                # optional version or semver range
    source: git+https://github.com/constellation-works/orbit-graph#v0.4.1
    enabled: true
  - name: chart
    source: https://github.com/constellation-works/orbit-chart/releases/download/v1.2.0/orbit-chart-1.2.0.tar.gz
    digest: sha256:0a1b2c3d…         # required for, and only allowed on, https:// archives
    enabled: true
```

- **Sources:** a directory outside the current repo, `git+<url>#<ref>`, a local `.tar.gz`/`.tgz`/`.tar`/`.zip`, or an `https://` archive pinned by `sha256` digest (no trust-on-first-use). Sources containing symlinks are refused.
- **Permissions are requested, never implied.** `spec.permissions` in the manifest is a request. Only `--grant` on `plugin add --enable`, `enable`, `upgrade` or `sync` grants it, and an ungranted plugin's tools register inactive. An upgrade that widens the request disables the plugin until you re-grant it.
- **Source builds and consent (`spec.build`).** A `git+<url>#<full commit id>` source whose manifest declares `spec.build` compiles its backend on the installing host under an isolated build sandbox (Bubblewrap on Linux, `sandbox-exec` on macOS). Installing or upgrading a source-built plugin requires explicit operator consent via `--allow-build` on `orbit plugin add` or `orbit plugin upgrade`; without the flag, the command refuses with `build_consent_required` and displays the build plan. Consent covers only that single invocation — it is never stored in pins, configuration (`config.toml`), environment variables, or granted via MCP tools. Managed agent runs and automated routines cannot consent to builds (`build_consent_unavailable`).
- **`[plugins.<ns>]`** holds the plugin's own settings, validated against its `spec.config.schema` with its defaults underneath. `orbit config get`/`set plugins.<ns>.<key>` accepts only declared keys, and values layer workspace over global. An invalid value disables only that plugin. A section for a plugin this machine hasn't installed is warned about and ignored.
- **`[plugin_enablement]`** (workspace file only) switches a host-enabled plugin off in this workspace: `<ns> = false`. Write it with `orbit plugin disable|enable <ns> --scope workspace` rather than by hand. An unset entry inherits the host state, and `true` never switches on a plugin the host has disabled — `enable --scope workspace` refuses instead. The pin's `enabled: false` is applied here by `orbit plugin sync`, never to the host row. Values must be booleans keyed by plugin namespace, and the table is refused in the global file.

```toml
# .orbit/config.toml — this workspace only
[plugin_enablement]
graph = false
```

---

## Validation and errors

Config is parsed at startup, and invalid entries fail loud. Common errors:

| Message | Fix |
|---|---|
| `crew '<x>' is not defined in [crews.*]` | Name a crew that exists. |
| `[workflow].default_crew must be set when defining [crews.*]` | Set `default_crew`, or define an `opus` crew. |
| `[crews.<name>].<field> must not be empty` | Give the crew a `model` and `provider`. |
| ``crew `<x>`, which is disabled ([crews.<x>] enabled = false)`` | `orbit config set crews.<x>.enabled true`, or select an enabled crew. |
| `invalid type: string "…", expected a boolean` for `enabled` | Write `enabled = true` or `enabled = false`, unquoted. |
| `config schema no longer supports [agent.<role>] tables` | Migrate to `[crews.<name>]`. |
| `execution.codex.sandbox has invalid value '<x>'` | Use `read-only`, `workspace-write` or `danger-full-access`. |
| `[operation] has unknown key '<x>'`, `[review] has unknown key '<x>'`, `operation.review_policy has invalid value '<x>'` | The review keys are a closed set. |
| `[task] artifact_store is no longer supported` | Remove the key. |

**Retired keys that warn and are ignored.** Delete them. `orbit config get`/`set` reject them with migration notes.

| Retired | Note |
|---|---|
| `operation.preset`, `completion`, `preparation`, `preparation_due_seconds`, `promotion`, `leaf_ceiling`, `recovery`, `recovery_episodes_per_task`, `recovery_minutes_per_task`, `delivery_cap` | Operation mode was removed. `[operation]` keeps only `review_crew`. |
| `operation.review_repair_cycles`, `operation.review_reviewer_starts` | Retired: each candidate gets one review whose reviewer fixes its findings in one commit, so neither repair cycles nor reviewer starts are counted. `review.minutes` bounds that review. |
| `[docs]` | The docs corpus was removed. |
| `[semantic]`, `search.model` | Search is lexical (SQLite FTS5) and needs no model. The legacy `semantic.db` path remains the lexical search database. |
| `workflow.pilot_max_complexity` | Route a tier with `workflow.<tier>_complexity_crews`, or pin `crew` on the task. |
| `[duel]`, `[duel.models]` | Retired. |
| `[routines]` (`role = "source"`) | Every registered owner checkout is a routine source. |
| `knowledge.task_id_pattern` | Deprecated. |
| `execution.env.inherit` | Inheritance is fixed off. |

**Deprecated keys that warn and are translated.** These are still honoured, warned on every load, and refused by `orbit config get`/`set`. A later release makes them errors, so move them now.

| Deprecated | Translation |
|---|---|
| `operation.review_policy` | `before-pr` sets `review.before_pr = true`. `after-landing` enables the `delivery-code-review` auto-task while no operator has configured it: once its `enabled` flag is set by `orbit auto-task toggle` or any other edit, that flag decides. `none` turns neither on. A `[review]` table in the same file wins. |
| `operation.review_minutes` | Becomes `review.minutes`, now the limit for one candidate's review rather than a lineage total. |

After-landing review is not a config key: it is the `delivery-code-review` auto-task's own `enabled` flag (`orbit auto-task toggle delivery-code-review on|off`). `orbit config show`, `orbit doctor` (the `review` check), the dashboard Config tab and `orbit.drain.probe` all report both switches with their sources: before-PR on/off and minutes, and after-landing enabled with the next batch due. The review check also names the observed commit against `origin/<branch>` and is not ok when the oldest unobserved first-parent commit has waited at least the batch's `max_wait_minutes`, even if the remote-tracking tip is recent.

Doctor and auto-task inspection verify active coverage against the existing `origin/<branch>` tracking ref without fetching, including when reporting a wedged consumer or an adoption refusal. With no origin, they use the local branch. A missing tracking ref leaves evidence verification unknown; inspection does not fall back to the local branch. Delivery evaluation still fetches origin and defers on fetch failure without spending a coverage retry.

Start with a minimal workspace file that holds only genuine overrides.
