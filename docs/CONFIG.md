---
type: context
summary: Orbit Configuration
last_validated: 2026-10-10
---

# Orbit Configuration

Reference for Orbit's `config.toml`: every fixed key, its default and what it does. `orbit config keys` prints the same keys with descriptions. The annotated template `orbit init` writes is [`crates/orbit-config/assets/default-config.toml`](../crates/orbit-config/assets/default-config.toml); use it as a reference, not a file to copy. The website has a shorter overview under [Reference › Configuration](https://orbit-cli.com/reference/config/). To add an execution lane rather than configure a shipped one, see the [executor onboarding runbook](runbooks/executor-onboarding.md).

## Where config lives

| Path | Scope | Created by |
|---|---|---|
| `<workspace>/.orbit/config.toml` | Workspace-local (per user, gitignored) | Hand-authored, or `orbit config set --fresh` / `--seed-from-global` |
| `~/.orbit/config.toml` | Global | `orbit init` (only when absent, or under `--force`) |

Settings inherit per key: workspace overrides global, global fills omissions, built-in defaults fill the rest. Keep a workspace file to genuine overrides.

- **Tables** layer down to individual settings; **scalars and arrays** replace the global value.
- **Named crews** layer by crew name and field: a workspace `[crews.sol]` holding only `model = "gpt-6-astra"` overrides that one field of the global `sol` crew.
- **Security keys** (`execution.codex.sandbox`, `execution.codex.approval_policy`, `execution.env.pass`) never inherit from global once a workspace file exists; an omitted one takes its built-in default. A workspace file holding only `[plugin_enablement]` does not count, so those keys keep inheriting.
- `[machine]` is global-only and `[plugin_enablement]` workspace-only; either in the wrong file is refused at load, naming the file.

`.orbit/config.yaml` (it stores `workspace_id`) is the workspace identity file, not runtime config.

## Inspecting and editing

| Command | What it does |
|---|---|
| `orbit config show` | Effective merged view, grouped as Machine, Delivery (`workflow.*`), Crews, Execution, Review (`operation.*`), Housekeeping and Paths. `--all` expands sections whose keys are all unset; `--json` adds `provenance`. |
| `orbit config get <key>` | One value. |
| `orbit config set <key> <value>` | Write the workspace file (`--global` for the global one). The value parses as a TOML literal, else a string. Without a workspace file, pass `--fresh` (start empty) or `--seed-from-global`. |
| `orbit config keys` | Every fixed settable key with type, section and description. |
| `orbit config path` | The resolved `config.toml` path. |

Each `show` value has one state: `workspace`, `global` or `environment` (the layer that set it), `default` or `unset`. A row names a shadowed layer: `(overrides global: main)`, or `(global sets danger-full-access — not inherited)` for a security key the workspace omits. A registered checkout also gets a `Workspace` line with the registry's base branch and ship mode, which delivery uses. In `--json`, each key's provenance has `scope`, `path`, `section`, `description`, `state` (`set`/`default`/`unset`) and `shadowed_by` (`[{layer, value, reason}]`, reason `overridden`, `not-inherited` or `preset-reset`); top-level `workspace_binding` is the registered base branch and ship mode, or `null`.

`--scope global|workspace` shows one file's values plus built-in defaults, without inheriting the other layer. Crew references are still validated against both layers' crews (partial workspace overrides included), here and in the dashboard's Workspace file view; a crew in neither is reported by name and key. A dashboard file validation error names the file and asks you to fix it before reloading. Scoped JSON reports `exists` (`config get`: key in that file) and `source.exists` (`config show`: file exists).

---

## `[machine]` — who this machine is

Global-only. `orbit init` writes the identity keys once.

```toml
[machine]
id          = "hm_0123456789abcdef"
name        = "example-host"
task_prefix = "ORB"
```

| Key | Default | Settable | What it is |
|---|---|---|---|
| `machine.id` | written by init | No | Opaque `hm_…` identity for run ownership, workspace ownership and federated routing. |
| `machine.name` | written by init | Yes (`--global`) | Display label: `executed_on.machine_name` on new local and claimed runs, `run_context.machine_name` in pull admission, and dashboard execution labels (full id in the tooltip). |
| `machine.task_prefix` | written by init | No | 2–5 uppercase ASCII letters for task IDs minted here; fixed for the life of the local task store. |
| `machine.worker_containment` | `true` | Yes | Run each detached pipeline worker in a transient systemd user scope (`orbit-worker-<run_id>-<nonce>.scope`), so a runaway run is throttled or OOM-killed inside it. See [worker containment](#worker-containment). |
| `machine.worker_containment_strict` | `false` | Yes | Refuse a worker launch when no systemd user scope is available. Requires `worker_containment = true`. |
| `machine.worker_memory_high` | `40%` | Yes | Scope `MemoryHigh=` (throttle point): bytes with optional `K`/`M`/`G`/`T`, a percentage of physical RAM, or `infinity`. |
| `machine.worker_memory_max` | `50%` | Yes | Scope `MemoryMax=` (OOM point). Same grammar. |
| `machine.worker_tasks_max` | `4096` | Yes | Scope `TasksMax=` (processes plus threads), at least 1. |
| `machine.worker_cpu_quota` | `0` (unset) | Yes | Scope `CPUQuota=` as a percentage of one core (`400` = four cores); `0` sets no CPU limit. Linux only. |

- `orbit config set` refuses `machine.id` and `machine.task_prefix`: changing either would orphan or renumber records minted under it.
- `orbit init` refuses to create an identity once the task store has minted ids under another prefix (tasks created before `orbit init` use the historical `ORB`). It fails before writing anything; run `orbit init` before creating tasks.
- Hand edits fail closed: a `[machine]` table missing an identity key, a `task_prefix` that contradicts the local task allocator, or an `id` that contradicts a workspace record naming this machine as owner is refused. Nothing falls back to the hostname.
- A legacy `~/.orbit/host.toml` is folded into `[machine]` on first load and removed. If both exist and disagree, Orbit refuses to start and names both paths; delete the stale one.
- `orbit init --force` deletes the global root, machine identity and executor sandbox settings included, before seeding defaults, so it creates a new machine ID and prompts for name and prefix. Non-interactively, pass both `--machine-name` and `--task-prefix`; missing or invalid flags are rejected before anything is deleted. Plain `orbit init` keeps an existing identity.

### Worker containment

- Without a systemd user manager (containers), or with `worker_containment = false`, workers run in the caller's cgroup and each Linux Orbit process logs one warning. macOS has no systemd and runs workers uncontained without a warning.
- Strict mode starts no uncontained worker: the run reports why and how to enable a user manager or turn strict mode off, with error code `worker_containment_unavailable` (also in CLI JSON errors). Strict with `worker_containment = false` fails config load. `--strict-worker-containment` on `orbit run ship` or `orbit run auto` turns it on for one invocation; the auto coordinator passes it to leaf workers as `ORBIT_WORKER_CONTAINMENT_STRICT`.
- `machine.worker_cpu_quota` adds `CPUQuota=` to each scope, so one run cannot take every core. Only an unsigned integer loads, with no upper bound. `orbit doctor`'s `worker-containment` row reads `info` while it is unset and `ok` once it is set. systemd enforces it only where the user manager delegates the cpu controller; elsewhere the property is accepted and not enforced.
- A run that fails after hitting a worker limit carries error code `worker_resource_limit` in `orbit run show`. Inspecting scopes: [operational logs › Worker Resource Containment](../crates/orbit-core/assets/skills/orbit-setup/references/operational-logs.md#worker-resource-containment).
- Admitted names reach the contained worker through its environment, not the `systemd-run` command line. Orbit passes each name as `--setenv=NAME` with no value. systemd (211 and later, including 259) copies the value from the wrapper process, which already holds the allow-listed environment. That includes pass-listed provider credentials and clock-file defaults. A toolchain `PATH` override, an explicit removal, and the workspace allowlist are unchanged. Agent subprocesses still start from a cleared environment.

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
provider_limit_max_used_pct = 90
provider_limit_overrides = []
provider_limit_budgets = []
provider_limit_explicit_crews = "wait"
```

| Key | Default | What it does |
|---|---|---|
| `workflow.base_branch` | `main` | Fallback base branch for ship, auto and pilot. The workspace registry's value (`orbit workspace show`) wins; `--base <branch>` overrides both. For a two-branch repo, register with `--base-branch agent-main`. |
| `workflow.default_crew` | see [resolution](#resolution-precedence) | Crew for a task with no `crew`. Must name a defined crew. |
| `workflow.system_crew` | `system` | Crew for system work such as step-failure recovery and the task pilot. See [the `system` crew name](#the-system-crew-name). |
| `workflow.low_complexity_crews` | `[]` | Pool a crew-less `low` task draws from at creation; empty uses `default_crew`. See [pools](#automatic-crew-pools-by-complexity). |
| `workflow.medium_complexity_crews` | `[]` | The same, for `medium`. |
| `workflow.hard_complexity_crews` | `[]` | The same, for `hard`. |
| `workflow.xhard_complexity_crews` | `[]` | The same, for `xhard`. |
| `workflow.final_recovery_crews` | `["sol:100", "opus:20"]` | Weighted pool for the final-recovery activity; `[]` disables it. See [final recovery pool](#final-recovery-pool). |
| `workflow.provider_limit_max_used_pct` | `90` | Admission skips a crew while a live usage window of its provider or model is exhausted or used at or above this percent (`1`–`100`; `100` skips only on exhaustion). See [provider usage limits](#provider-usage-limits). |
| `workflow.provider_limit_overrides` | `[]` | Per-provider thresholds replacing the one above, each `provider:percent` (`["claude:80", "grok:100"]`): one entry per provider, percent `1`–`100`, an alias such as `anthropic` stored as its provider. |
| `workflow.provider_limit_budgets` | `[]` | Rolling budgets for providers that report no usage, each `provider:<amount><usd\|tokens>/<n><h\|d>` (`["grok:30usd/5h"]`). See [budgets](#budgets-for-providers-that-report-no-usage). |
| `workflow.provider_limit_explicit_crews` | `wait` | A task whose explicit crew is limited: `wait` keeps it in the backlog; `pool` draws from the unlimited members of its complexity pool. |
| `workflow.auto_ship` | `false` | Opt this workspace in to `orbit run ship-sweep`, the cross-workspace unattended ship command (skipped with `auto_ship_disabled` while `false`). The seeded `ship-sweep` routine ignores this key; its `enabled:` flag is its only switch. Neither grants `--complete` or `--approve-proposed`, and a `no-auto-approve` task is never auto-approved by either flag's drain or the CI sweep. |
| `workflow.required_validation_commands` | `[]` | Commands every delivered candidate must pass before push or merge. Empty runs no required check. See [required validation](#required-validation). |
| `workflow.distributed_completion` | `review` | How far this owner takes an accepted distributed-drain handoff: `review` waits for an operator's **Approve handoff**; `done` authorizes it on acceptance and lands it through `task_landing_pipeline`, rechecking this key before the merge. |

### What `orbit init` seeds

Only the global file, and only when it is absent (or under `--force`):

- every [built-in crew](#crewsname--which-provider-model-runs-the-task), enabled for each detected provider CLI and `enabled = false` otherwise;
- `default_crew`: the default crew of the first detected family in preference order;
- `system_crew`: the first detected of `luna`, `haiku`, `grok`, `antigravity`, `gemini`, `copilot`, `cursor`, `pi`, `opencode` (cheapest tier first);
- the four complexity pools, as bare lists:

  | Detected | `low` | `medium` | `hard` | `xhard` |
  |---|---|---|---|---|
  | codex + claude | `["haiku", "luna"]` | `["sol", "sonnet"]` | `["opus"]` | `["opus", "astra"]` |
  | codex only | `["luna"]` | `["sol"]` | `["sol"]` | `["astra"]` |
  | claude only | `["haiku"]` | `["sonnet"]` | `["opus"]` | `["opus"]` |

  On any row, and alone when neither codex nor claude is detected, a detected `grok` is appended to `medium`, and a Google CLI's crew to `low` (`antigravity` when `agy` is detected, otherwise `gemini`). Other families leave the pools `[]`, which route to `default_crew`. Init fails rather than write a pool entry it does not seed enabled.

Interactive init asks for both lane crews by name, offering only enabled crews, unless there is one candidate. With no supported CLI, every crew is written disabled and both keys stay unset, so `default_crew` resolves to the disabled `opus` and agent dispatch refuses until you run `orbit config set crews.<name>.enabled true`; deterministic jobs such as store retention still run. Init never writes `[crews.system]`, `[crews.custom]` or `[crews.qa]`.

### `[workflow.resource_throttle]` — host pressure

While CPU, memory or a watched filesystem stays at or above its high mark, this host starts no new task work until it falls below the resume mark; running work is never cancelled. Drain, ship and readiness behavior: [host resource pressure runbook](runbooks/distributed-drain.md#host-resource-pressure).

| Key | Default | What it does |
|---|---|---|
| `workflow.resource_throttle.enabled` | `true` | Enable the throttle verdict. `false` restores unthrottled admission; telemetry and severity are still reported. |
| `workflow.resource_throttle.cpu_high_percent` | `90` | CPU high-water mark. |
| `workflow.resource_throttle.cpu_resume_percent` | `85` | Resume below this CPU percentage. |
| `workflow.resource_throttle.cpu_light_leaves` | `2` | Leaves a local drain still starts for `no-diff-expected` auto-tasks (reviews, curation) while CPU is the only held resource; memory and disk pressure hold them too. `0..=32`; `0` holds them with everything else. |
| `workflow.resource_throttle.memory_high_percent` | `90` | Memory high-water mark. |
| `workflow.resource_throttle.memory_resume_percent` | `85` | Resume below this memory percentage. |
| `workflow.resource_throttle.disk_high_percent` | `90` | High-water mark for each watched filesystem. |
| `workflow.resource_throttle.disk_resume_percent` | `85` | Resume below this filesystem percentage. |

Percentages are integers `1..=100`; each resume mark must be below its high mark. Workspace runtimes use their resolved values. The host dashboard uses the serving machine's global values, read when its monitor first opens, so restart it after changing them.

**Verdict.** Below resume is `ok`, between resume and high `elevated`, at or above high `critical`. A resource is held once its high readings span ten seconds (repeated reads of one cached sample do not count; a gap over fifteen seconds restarts the window) and stays held until below resume. A sample that is unavailable, invalid, future-dated or older than fifteen seconds is `unknown` and releases that resource's hold (fail open). Readiness, `orbit run show`, `orbit run auto`, MCP and the dashboard Drain card name the held resource, value, threshold and since-when.

**Readings** are usage estimates, not OS memory-pressure classes, cached two seconds and read from procfs, Mach/sysctl and statvfs (via fs2), never a subprocess.

| Resource | Linux | macOS |
|---|---|---|
| CPU | One-minute load ÷ online CPUs × 100; can exceed 100% and includes I/O wait. | Busy fraction of aggregate Mach CPU tick deltas. A process's first sample takes a baseline, waits about 1.1 seconds and reads again, crossing the kernel's shared statistics cache window. |
| Memory | `(MemTotal - MemAvailable) / MemTotal` | Physical memory minus free and reclaimable inactive pages, so compressed and wired memory count as used (speculative pages are already free; purgeable pages overlap other categories). See Apple's [VM](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/mach/vm_statistics.h) and [host CPU](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/mach/host_info.h) statistics. |
| Disk | `(total - available) / total` with space available to the current user, for each checkout's filesystem, `.orbit/state/worktrees` (or its nearest existing ancestor) and the global Orbit root (`~/.orbit` or the serving root override). | Same. |

On macOS, [XNU caches host statistics after a shared query quota is reached](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/host.c). Identical CPU tick counters do not mean zero utilization. The initial sampling pair waits across the one-second cache window without retrying or increasing the query rate; a failed native read remains `unknown` with reason `unavailable`.

Every watched path takes part in admission, but `GET /api/host/resources` reports one `disk` object (highest known percentage, severity and path; `null` when none is known), and a disk hold names only the highest held path. That endpoint covers every active registered local checkout whatever `?workspace=` says (or with none), never remote hosts, and returns severity, sample time and age, thresholds, verdict and reason.

**Dashboard.** The top bar's `load` (CPU as a multiple of online cores), `mem` and `disk` chips show the selected host's readings, severity and held resources ([UI design §6](design/user-interface/2_design.md#6-top-level-navigation)); they poll every five seconds and mark aging data unknown even while a request is pending. **Settings › System** shows the enable switch and six marks with source, live reading, severity and verdict (`cpu_light_leaves` is edited in the Effective view). Its edits share the Effective view's `PUT /api/config/keys/{key}` validation but write the **global** file by default, because this host's chips and admission read global settings; a workspace override is marked and wins for that workspace's runtimes.

### `[workflow.task_pilot_freshness]` — when a task is piloted again

```toml
[workflow.task_pilot_freshness]
material_fields = ["title", "description", "criteria", "plan", "selectors"]
source_sensitivity = "ignore"
```

| Key | Default | What it does |
|---|---|---|
| `workflow.task_pilot_freshness.material_fields` | `["title", "description", "criteria", "plan", "selectors"]` | Task inputs whose edit makes an accepted task-pilot assessment stale, so the task is piloted again. At least one of `title`, `description`, `criteria`, `plan`, `selectors` (`context_files`), `tags`, `crew` (the stored crew and its resolved model/provider), `tools`, `type`, `complexity`, `relations`, `dependencies` (each one's status and meaning) and `instructions` (`AGENTS.md`/`CLAUDE.md` at the pinned revision). |
| `workflow.task_pilot_freshness.source_sensitivity` | `ignore` | Whether a move of the observed branch head makes an assessment stale. `ignore`: never (the pinned revision is still recorded). `context_files`: when the head changed a path a selector names (`file:` and `dir:` by prefix, `symbol:` through its file). `any`: every move. |

**Unprepared backlog work.** A backlog task with no `context_files` and no `no-diff-expected` tag holds no file lock. A local drain or discovery ship with more than one slot (`max_active_leaf_runs` > 1) therefore holds it: while an enabled `preparation_eligible` routine owned by this host (`trigger.state.owner_machine`) would still prepare it, it waits as `awaiting_footprint` for at most that trigger's `max_wait_minutes + deadline_minutes` from the task's last change. When no such pilot exists, the pilot assessed it and left it empty, or the wait elapsed, it starts only when no other leaf is in flight (`awaiting_exclusive_slot`) and holds the whole tree while it runs. A single-slot drain, an explicit `orbit run ship <id>` and owner pull admission admit it as before. `orbit run readiness` reports both reasons with the fix.

A routine's `eligibility` block still decides which tasks are piloted: a newly eligible task with no fresh assessment is piloted, while retagging an assessed, still-eligible task is not. A task-pilot routine's `trigger.state.freshness` overrides either key (routine, then this table, then defaults). A non-default value is itself material, so changing it re-pilots tasks assessed under the old value once. Assessments from before this setting existed stay fresh while their task is unchanged. See [automation triggers](design/automation-triggers/5_operations.md).

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
| `workflow.validation_env.login_shell` | `true` | Resolve PATH and toolchain locators from the owner's shell (below). `false` never starts the shell. |
| `workflow.validation_env.interactive` | `true` | Try `-i -l -c` first, so toolchains exported in rc files are found. `false` probes only `-l -c`. Ignored when `login_shell` is `false`. |
| `workflow.validation_env.path` | `[]` | PATH entries to add. A leading `~/` expands to `HOME`. Agent sessions get them too ([below](#agent-sessions-share-the-configured-path)). |
| `workflow.validation_env.path_mode` | `prepend` | `prepend` puts `path` first; `replace` makes it the whole PATH. |

Required validation commands and `local_shell` steps run in this environment over the [allowlisted agent environment](#executionenv--the-agent-subprocess-environment), so a drain started from a minimal PATH (a service manager, `env -i`, SSH) still finds the toolchain.

**Probe.** The shell is the account's user-database shell, else `$SHELL`, else `/bin/sh`. Orbit runs `<shell> -i -l -c …` (login profiles plus interactive rc files: `~/.zshrc`, or `~/.bashrc` when the bash login profile sources it), falling back to `<shell> -l -c …` on startup failure, nonzero exit, timeout or a missing environment marker. Stdin is null; output before the marker and stderr noise on success (bash's job-control warning) are ignored. Each attempt is limited to 10 seconds; the whole outcome, fallback included, is cached two minutes. Only `PATH` and the toolchain allowlist cross: `CARGO_HOME`, `RUSTUP_HOME`, `GOPATH`, `GOROOT`, `GOBIN`, `JAVA_HOME`, `PYENV_ROOT`, `NVM_DIR`, `VOLTA_HOME`, `PNPM_HOME`, `BUN_INSTALL`, `HOMEBREW_*`.

**Recorded.** Each validation log and step output records `validation_env`: the PATH; its `source` (`config` when `path` has entries, else `login_shell` when the probe returned a PATH, else `launcher_fallback`); the shell and any probe error; `probe_mode` (`interactive_login` or `login`, else null); and `fallback_reason` (why interactive startup failed when login-only succeeded, else null). When both probes fail, `login_shell_error` holds both.

**Diagnostics.** `orbit doctor`'s `validation-env` row shows the probe mode, fallback, PATH and the `python3`, `git` and `make` it finds, and warns (advisory) on a fallback or an earlier PATH entry shadowing a different executable (`/usr/bin/python3` before `/opt/homebrew/bin/python3`); duplicates and symlink aliases count once ([health checks](runbooks/health-checks.md#run-orbit-doctor)). While required commands are configured, `orbit run auto`, `orbit run ship`, and MCP `orbit.workflow.auto` (`status`, `start`) and `orbit.workflow.ship` warn when the shell cannot be probed, when resolution is off and `path` is empty, or when the resolved PATH drops login-shell entries (possible under `replace`).

#### Agent sessions share the configured PATH

A reviewer that resolves a different `python3` or `make` than host validation reports checks red that the host then refutes. So every agent session — the implementer's and reviewer's provider CLI, and the commands it runs through `proc.spawn` — starts with `workflow.validation_env.path` ahead of its [allowlisted](#executionenv--the-agent-subprocess-environment) PATH. The entries are prepended under either `path_mode`, since the rest of PATH still has to find the provider CLI. The login-shell probe is not applied to agents. Settlement does not rerun a disputed check in the reviewer's environment: host validation stays the reference.

Codex runs each tool command through the user's shell as a login shell (`zsh -lc`) by default, and the profiles it rereads can reorder PATH: on macOS `/etc/zprofile` runs `path_helper`, which moves inherited entries behind `/etc/paths`, so `/usr/bin/python3` (3.9) would win over Homebrew's. While `path` has an entry, Orbit starts Codex with `--config allow_login_shell=false`. Its commands then run in a non-login shell (`zsh -c`, which still reads `~/.zshenv`) with the composed PATH, and Codex refuses a model request for a login shell. No `~/.zprofile` change is needed. In exchange, Codex commands no longer see what only `~/.zprofile` or `~/.zlogin` sets, so list every toolchain directory agents need in `path`; after it Orbit appends `~/.local/bin`, `~/.orbit/bin`, `~/.cargo/bin`, `~/bin`, `/opt/homebrew/bin` and `/usr/local/bin` when absent. With `path` empty, Codex keeps its login shell. Claude Code runs commands in a non-login shell and keeps the composed PATH. Another provider whose shell tool starts a login shell rereads profiles; check it with `zsh -l -c 'command -v python3'` against `orbit doctor`'s `validation-env` row.

#### Required validation

`task_pr_pipeline` and `task_local_pipeline` run `workflow.required_validation_commands` on the exact candidate before push or merge and attach each command's log to the task.

- A failure goes to step recovery, except a [missing tool](#missing-tools-block-the-task) or a failure the base shares ([held](#a-red-base-is-held-not-blocked) until the command passes on a new base tip). [Network-inconclusive](#network-flakes-are-rerun) failures are rerun first.
- Distributed handoffs must carry exact-candidate validation matching the owner's list. A before-PR claim also freezes the list in the owner's review contract at admission; a later owner change refuses the handoff rather than replacing the snapshot.
- Re-running a task whose failed run preserved a candidate from `commit` or later runs the commands on that candidate over the new base, to decide whether implementation runs at all. A candidate preserved from implementation or earlier always returns to the implementer ([re-running a task](../crates/orbit-core/assets/skills/orbit-orchestrate/references/workflows.md#re-running-a-task-with-a-preserved-candidate)).
- An empty list runs nothing, including on claimed handoffs; the other handoff guards still apply.
- An implementer whose affected-test gate passed with only `DEFERRED: bubblewrap unavailable:` notices hands that gate off as a `deferred_sandbox_validation` record instead of failing [ORB-15287]. The gate must be a required command or a `review.baseline_commands` entry. Validation runs it after the required commands, outside the agent sandbox, on this run and on a resumed candidate: a pass that still defers holds the candidate as a `validation_environment` failure, and a run that reports no executed tests is refused ([development guide](DEVELOPMENT.md#test-process-environment)).

#### Missing tools block the task

A command that fails for lack of a tool says nothing about the candidate. Orbit detects exit status 127, a shell `command not found` / `not found` line, `make`'s `Error 127`, `No such file or directory` for the program, a missing cargo subcommand, or a guardrail's "`<tool>` is required … install" line for a tool absent from PATH.

The step fails with the `[validation_environment]` marker and error code `validation_environment`, naming the tool, PATH and source; the log records `failure_kind: "environment"` and `missing_tool`. No step, final or blocked-task recovery runs, and no review, rework or recovery budget is spent. The handoff pushes nothing, opens no `[BLOCKED]` PR, and blocks the task under `validation_environment_blocked` with the candidate kept in its worktree. Fix the environment, check `orbit doctor`, then `orbit job resume <run>`. A claimed leaf skips repair the same way: its claim settles as a failure carrying the diagnostic and is not released for another follower to implement again.

#### Network flakes are rerun

Network-inconclusive output (HTTP status `000`; a curl connect, resolve, timeout, TLS or receive error such as `curl: (6)`, `(7)`, `(28)`, `(35)`, `(56)`; a DNS resolution failure; a TLS handshake timeout) reruns the command up to twice, after 2 s and 6 s. Only the last attempt counts; the log records `network_retries`.

#### A red base is held, not blocked

The integration branch has no merge gates, so a command may already fail on the base. A command still failing after its reruns is rerun on the base commit in a clean detached worktree under `<git-common-dir>/orbit-baseline/`, cached per base commit, command and selection behind a file lock (concurrent candidates on one host share one base run). `validation/<run>/<n>.json` gets a `baseline` record; `validation/<run>/<n>.baseline.json` holds the base log.

| Base run | Outcome |
|---|---|
| Fails the same way (exit status and timeout outcome) | The task is held (below). |
| Passes | Candidate failure; the error says the base passed, so step recovery can repair it. |
| Inconclusive (missing tool or network) | Candidate failure; nothing cached. |
| Not comparable (below) | Candidate failure; the `baseline` record's `decision` is `not_comparable`. |

**Hold.** The step fails with the `[baseline_red]` marker and error code `baseline_red`, and the run ends `held`, not `failed`; the gate and auto parents waiting on it pass it as held. No step or final recovery runs and no budget is spent; the handoff pushes nothing and opens no `[BLOCKED]` PR. The task moves to `backlog` under a `baseline_red_hold` history event naming base ref, base commit and command (run finalization does this for a local pipeline), with the candidate kept in its worktree. Admission withholds it; `orbit run readiness --json` reports `reason: "baseline_red_hold"`. Operator steps: [stuck job runs](runbooks/stuck-job-runs.md#a-task-held-for-a-red-base).

**Release.** The held command must pass on a new base tip; failing or inconclusive keeps the hold. `orbit clock tick` never runs it (it holds the sweep lock): it reads the cached base result for the new tip, or dispatches one `baseline_hold_refresh_pipeline` run per workspace at a time to run it and record a `baseline_red_hold_verdict` event. An inconclusive attempt suppresses refreshes for that tip and command for 15 minutes; a new tip is checked at once, and a conclusive cached result overrides the attempt. Readiness, the backlog snapshot and pull admission only read verdicts, so a hold with none stays held. The next run resumes the preserved candidate.

**Self-selecting commands** (`make ci-test-affected`) report their selection and executed-test count through `ORBIT_VALIDATION_SUMMARY`, and the base run reruns the candidate's selection from `ORBIT_VALIDATION_SELECTION`; a base run with another selection, or a pass with no counted test, is not comparable ([validation summary](DEVELOPMENT.md#validation-summary-and-base-reruns)). A hold records the selection and lifts only when that selection passes on a moved base with at least one test executed. A hold from before selections were recorded is judged by exit status, and a still-red base re-holds the next delivery with the selection. A command that writes no summary is judged by exit status.

**Followers.** A claimed leaf validates before it pushes or opens a PR. A red base releases the claim, the owner records the same `baseline_red_hold`, and its pull admission defers the task until a verdict lifts it.

**Reviewer claims.** A before-PR reviewer can attach a `baseline` claim to a further failing check. Settlement runs on the host, outside the reviewer's sandbox, so it reruns only `workflow.required_validation_commands` and `review.baseline_commands` entries (never the reviewer's command text) on the final candidate and the pinned base, using the lists captured at admission. A confirmed claim with nothing else open holds the task as above. Outcomes for refuted or uncheckable claims: [review-gate design §4](design/review-gate/2_design.md#4-what-the-validation-records-establish-orb-11528-orb-11545).

#### Agent-declared blockers stop the run

An implementer that cannot proceed returns `blocker: {kind, evidence}` (a short token and the reason) from `implement_one` or a resolved `agent_implement` target. The step fails with the `[task_blocked_by_agent]` marker and error code `task_blocked_by_agent`; no step retry, step recovery or final recovery runs. On the PR pipeline the handoff commits, pushes and opens nothing, blocks the task under `task_blocked_by_agent` with the kind, and leaves the worktree as the implementer left it, uncommitted files included. `orbit job resume` does not move the task back; an operator does once the blocker is gone. An implementation activity cannot set status `blocked` itself (operators and other activities can). A claimed leaf stops before its next step and final recovery. Blocked-task recovery skips a marked failure note, or for a claimed leaf, a marker in the execution summary.

A blocker whose kind names an Orbit upgrade refusal (`upgrade_pending`, `upgrade_admission_refused`, `orbit.generation_switch_pending` and similar spellings) is the host's state, not the task's. It ends any agent step, reviewers included, with error code `upgrade_pending` instead. The step is not retried or recovered, the candidate is kept, and the task returns to the backlog rather than `blocked`. See [in-flight agent steps during a binary swap](runbooks/upgrades.md#in-flight-agent-steps-during-a-binary-swap).

`step_failure_recovery` declares the same blocker with an `external_blocker` decision when a human must act on a cause outside the run; the post-recovery attempt and final recovery are skipped and the task is blocked as above (or returned to the backlog, for an upgrade refusal). A recovery that writes no decision gets its one post-recovery attempt only if Orbit sees the worktree, the run's base-ref tip or the validation environment change while it ran. Mechanism: [activity-job design](design/activity-job/2_design.md#recovery-authority-is-host-only).

---

## `[crews.<name>]` — which provider-model runs the task

A crew is one provider-model assignment. An activity uses the crew named in its rendered input, otherwise the run's resolved crew.

| Field | Required | Values |
|---|---|---|
| `provider` | Yes | `claude`, `codex`, `antigravity`, `gemini`, `grok`, `copilot`, `cursor`, `pi`, `opencode`. See [provider identity](#provider-identity-and-resolution). |
| `model` | Yes | Model ID passed to the provider CLI. |
| `enabled` | No | Default `true`. `false` keeps the crew defined and listed but refuses to run it; see [disabled crews](#disabled-crews). |
| `effort` | No | Reasoning effort (below). Omitted leaves the provider's default. |
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

**Built-in crews**, in init's family preference order. A config with no `[crews]` table uses the full set plus a built-in `system` crew (`claude`, `sonnet`). Existing explicit model pins are kept as written.

| Family | Binary | Crews (model) | Default crew |
|---|---|---|---|
| `claude` | `claude` | `opus` (`opus`), `sonnet` (`sonnet`), `haiku` (`haiku`), `fable` (`fable`) | `opus` |
| `codex` | `codex` | `astra` (`gpt-6-astra`), `sol` (`gpt-6.1-sol`), `luna` (`gpt-6-luna`) | `astra` |
| `antigravity` | `agy` | `antigravity` (`gemini-3.8-flash-high`) | `antigravity` |
| `gemini` | `gemini` | `gemini` (`gemini-3.8-flash`) | `gemini` |
| `grok` | `grok` | `grok` (`grok-4.7`) | `grok` |
| `copilot` | `copilot` | `copilot` (`claude-sonnet-5`) | `copilot` |
| `cursor` | `cursor-agent` | `cursor` (`gpt-5`) | `cursor` |
| `pi` | `pi` | `pi` (`sonnet`) | `pi` |
| `opencode` | `opencode` | `opencode` (`anthropic/claude-sonnet-4-5`) | `opencode` |

**Reasoning effort.** Choose capability with the model, then tune `effort` within it.

| Provider | Accepted `effort` | Rendered as |
|---|---|---|
| `claude`, `codex`, `pi` | `low`, `medium`, `high`, `xhigh`, `max` | `--effort` · `model_reasoning_effort` · `--thinking` |
| `antigravity` | `low`, `medium`, `high` | `--effort` |
| `opencode` | `high`, `max` | `--variant` |
| `grok` (`grok-4.7`, `grok-4.6`) | `low`, `medium`, `high`, `xhigh` | `--reasoning-effort` |
| `grok` (`grok-4.5`) | `low`, `medium`, `high` | `--reasoning-effort` |
| others | none | — |

An unsupported `effort` (`max` on Grok, `effort = "hard"`) is ignored for that crew, never remapped, with a warning naming the file, crew, value and accepted values. `orbit doctor` lists ignored properties; `orbit config set` refuses a value load would drop. A selected crew's provider, model and effort each override an activity's inline baseline.

**Editing.** Crew fields are `crews.<name>.<field>`:

```bash
orbit config set crews.sol.effort high
orbit config get crews.sol.effort
orbit config set crews.gemini.enabled true
```

`config set` refuses invalid values, unsupported provider/model combinations and misspelled fields. It cannot create a crew: add a `[crews.<name>]` table with `model` and `provider` first. `orbit.workspace.list` with `include: ["crews"]` returns each checkout's effective normalized crews with `enabled` (schema version 3), or `crews_error` when its config cannot be read.

**Dashboard.** **Settings › Crews** lists provider, model, usage **Limit** ([provider usage limits](#provider-usage-limits)), effort, tags, layer and **Used by** (inline field labels at 900 px or less). **Used by** is resolved server-side from the default, system and review crews, the final-recovery and complexity pools, and enabled auto-tasks with an explicit crew whose plugin is active; if the auto-task listing fails, it omits them and logs a warning, and crew writes still work.

### The `system` crew name

Shipped job steps such as `task_pilot_pipeline` name `crew: system`. At load, `system` aliases to the crew `workflow.system_crew` names (`system_crew = "luna"` runs the task pilot on Luna); a user-authored `[crews.system]` wins. Older configs without `system_crew` fall back to an existing `[crews.qa]`, then the default crew. An unknown custom name is not substituted and fails at dispatch. A missing or unusable system crew leaves the original failed step failed, with a diagnostic naming `workflow.system_crew`.

### Disabled crews

`enabled = false` switches a crew off without deleting it; a table without the key is enabled.

- **Listed.** `orbit config show` has an `ENABLED` column and counts disabled crews; `orbit config get crews.<name>.enabled` answers `true`/`false`; `orbit.workspace.list` carries `enabled`; the dashboard Config tab marks the row and offers a toggle.
- **Skipped by pools.** A disabled member is never drawn. An all-disabled pool acts as empty, so the task falls through to `default_crew`. It may stay listed, so re-enabling restores its share.
- **Refused for agent dispatch, never substituted.** A task's `crew`, an explicit run or activity crew, `workflow.default_crew`, `workflow.system_crew` or a shipped step's `crew: system` that resolves to a disabled crew fails with its name and `orbit config set crews.<name>.enabled true` (naming the mirrored crew's table for `system`).
- **Deterministic jobs still run.** A job whose steps and hooks are all deterministic accepts a disabled crew and records no run crew, so retention and reap jobs run on hosts without a provider CLI. An unknown crew name still fails at startup.
- **Load succeeds.** A lane key naming a disabled crew does not stop unrelated commands; `orbit doctor` warns under `config` with the enabling command.
- **Not pinned at creation.** A crew-less task created while `default_crew` is disabled keeps `crew` unset, so dispatch refuses the default by name. `orbit task show` and the dashboard still report a disabled configured crew.

**Validation.**

- `model` and `provider` must be non-empty.
- `default_crew` must name a defined crew. Left unset with crews defined, Orbit uses `opus` (or a legacy `claude` crew), else refuses to load.
- A crew name may not contain `:`, which the pool grammar reserves ([repair](#repairing-a-config-that-already-names-a-crew-with-a-colon)).
- An Antigravity crew needs an `agy models` slug; a bare Gemini CLI ID such as `gemini-3.8-flash` fails with migration guidance.

**Retired crew shapes** fail load with migration guidance: `planner` / `implementer` / `reviewer` sub-tables (use flat `model` and `provider`); `backend` (`cli` is ignored, `http` and `auto` refused, likewise in `ORBIT_BACKEND` and `[runtime] backend`; remove it); `[agent.<role>]` tables (use `[crews.<name>]` plus `workflow.default_crew`).

### Repairing a config that already names a crew with a colon

Every command, `orbit config set` included, refuses to load, so edit the file by hand. Layers are checked before merging, so the error names the defining file:

```
error: invalid input: [crews]: crew name 'gpt-5:codex' must not contain ':'; ...
Edit '~/.orbit/config.toml' and rename or remove the [crews."gpt-5:codex"] table, then rerun the command
```

Rename the table (or delete it), update every reference (`default_crew`, `system_crew`, pool entries, a task's `crew`), and rerun.

---

## Provider identity and resolution

Every `provider` string (a crew's, an activity's inline `provider`, setup detection) goes through one canonical parser.

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
| `ollama` | — | Recognized but unsupported at the Orbit CLI entry point (`provider.unsupported`). |
| `openai_compat` (`openai-compat`) | — | HTTP-only, no CLI runtime; selecting it fails. |

- Parsing ignores case and surrounding whitespace.
- Deprecated aliases resolve with an `orbit.config.crew` warning: `anthropic` → `claude`, `openai`/`chatgpt` → `codex`, `google` → `gemini`, `xai` → `grok`.
- `copilot`, `cursor`, `pi`, `antigravity` and `opencode` have no aliases, and the model vendor a lane runs (a Claude model through Copilot) never changes the provider.
- The cross-repo Worker executor runs only `claude`, `codex`, `gemini` and `grok`; a Worker-routed step naming another lane is refused, not re-pointed.

**Audit attribution.** Process audit maps `ORBIT_AGENT_NAME` and `ORBIT_AGENT_MODEL` to an agent family. An invalid pair (`copilot` with a `claude-*` model) or an agent envelope with no identity records `unknown` with a warning. An agent envelope (non-empty name and model, truthy `ORBIT_MANAGED_RUN_CONTEXT`, `ORBIT_TASK_ACTOR_KIND=agent`) takes precedence over `ORBIT_ACTOR`, the operator override and the OS username. This affects recorded identity only, not command authorization.

### Resolution precedence

**Which crew a task dispatches.** The first tier that is set wins:

1. **explicit**: `--crew` or run-input `crew`.
2. **task_config**: an explicit `task.crew`, or a pool assignment valid for the current tier (normally set at creation; admission redraws stale pool assignments and default fallbacks, see [pools](#automatic-crew-pools-by-complexity)).
3. **workspace_default**: `workflow.default_crew`.
4. **environment_default**: `CONSTELLATION_DEFAULT_PROVIDER`, a provider ID or alias. `claude` selects `opus` and `codex` selects `sol` when defined; any other ID selects the same-named crew. Never overrides a configured `default_crew`.
5. **system_default**: the `opus` crew, or a legacy `claude` crew.

**Which crew an activity uses.** A non-empty `crew` in its rendered input, otherwise the run's crew. Activity and job assets that declare `role` are rejected.

**Crew over inline baseline.** For each of `provider`, `model` and `effort`, the selected crew's value wins when present, else the activity's inline `agent_loop` value. An unrecognized crew `provider` is logged and falls back to the inline provider, so a typo never moves dispatch to another runtime. A provider already recorded on a run is reused verbatim on reconciliation.

### No silent fallback

An explicit selection that cannot run fails with a stable diagnostic and never falls back to another runtime:

- `provider openai_compat is unsupported by the Orbit CLI entry point (HTTP-only)`
- `provider ollama is unsupported by the Orbit CLI entry point`
- `unknown provider '<x>'; expected one of claude, codex, gemini, grok, copilot, ollama, openai_compat, cursor, pi, antigravity, opencode`
- A selected lane whose binary is missing fails permanently, naming the binary.

---

## Provider CLI notes

For every lane: Orbit passes `--model` from the crew and sends the prompt on **stdin**, never argv (process listings and audit see argv). The Orbit OS sandbox is the filesystem boundary, and provider-specific write grants apply only while that provider runs. Credentials reach the agent only through [`[execution.env].pass`](#executionenv--the-agent-subprocess-environment); Orbit never puts a key on argv. A lane without native or Orbit-managed MCP reaches Orbit tools with `orbit tool run <tool> --input '<json>'` through its shell tool, under the same grants.

`orbit tool run` selects a workspace from any directory with `workspace` in the JSON input or the global `--workspace` flag: a name, `ws_*` ID, absolute local checkout path, or local `hm_*/ws_*` selector from the federated listing. A selector for another host fails locally, naming it; run the command on that owner host. Unparseable JSON or an unreadable `--input-file` is reported before workspace selection, runtime open or migration, so an input typo never migrates the checkout.

### GitHub Copilot CLI

| | |
|---|---|
| Install | `npm install -g @github/copilot`, then `copilot --version`. The retired `gh copilot` extension is not supported. |
| Auth | `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, else the `copilot /login` session (macOS keychain item `github-copilot-app`). Orbit forwards none of the token variables unless one is in `pass`. On a Mac, run `/login` once. |
| Model | `--model` is always passed, so `COPILOT_MODEL` and the saved `/model` are ignored. List IDs with `/model` inside `copilot`; recheck pins after CLI upgrades. |
| Flags | `--allow-all-tools --no-ask-user --output-format json`. Never `--allow-all`/`--yolo`, which widen paths and URLs. |
| Sandbox | Write: `COPILOT_HOME` (default `~/.copilot`) and `$XDG_CACHE_HOME/copilot`. macOS: read `~/Library/Keychains`. No access to `~/.config/gh`. |

An account without Copilot entitlement fails with `Error: Authentication failed`; org-policy-disabled third-party MCP servers produce a `session.warning` (`warningType: "policy"`). The GitHub org admin fixes both. A run with no assistant message fails the step.

### Cursor Agent CLI

| | |
|---|---|
| Install | `curl https://cursor.com/install -fsS \| bash`, then `cursor-agent --version`; put `~/.local/bin` on `PATH`. Cloud agents are not used. |
| Auth | `cursor-agent login` (check with `cursor-agent status`), in the macOS login keychain by default. A file-store login (`AGENT_CLI_CREDENTIAL_STORE=file` at login) also needs `pass = ["AGENT_CLI_CREDENTIAL_STORE"]`. Or `pass = ["CURSOR_API_KEY"]`. |
| Model | `cursor-agent models` (or `--list-models`). |
| Flags | `--print --force --output-format json`. The terminal `{"type":"result","subtype":"success","is_error":false,"result":…}` object is validated before the envelope is read. |
| Sandbox | Write: `~/.cursor`. macOS: read `~/Library/Keychains`. |

### Pi CLI

| | |
|---|---|
| Install | `npm install -g @earendil-works/pi-coding-agent`, then `pi --version`. |
| Auth | `/login` in an interactive `pi` session (under `$PI_CODING_AGENT_DIR`, default `~/.pi/agent`), or a vendor key via `pass`. Orbit never renders `--api-key`. |
| Model | A `--model` pattern, optionally vendor-prefixed (`openai/gpt-4o`); list with `pi --list-models`. `effort` renders as `--thinking <level>`. |
| Flags | `--mode json --no-session --no-approve --offline`: ephemeral sessions, no inherited project trust (project-local `.pi` extensions don't run; `AGENTS.md`/`CLAUDE.md` still load), no startup network calls. |
| Sandbox | Write: `$PI_CODING_AGENT_DIR`, else `~/.pi`. |

Pi has no MCP client and `orbit mcp init` offers none; keep Pi's `bash` tool available, since Orbit tools go through it. Completion is read only from the final assistant `message_end` frame. No provider token usage is reported; raw output is kept in the audit blob store.

### Antigravity CLI

`antigravity` launches `agy`, Google's current terminal agent: a separate lane from `gemini` (the legacy Gemini CLI), with different flags, MCP config and model slugs. `gemini-*` models still attribute to the Gemini model family.

| | |
|---|---|
| Install | See the [Antigravity CLI docs](https://www.antigravity.google/docs/cli/headless/), then `agy --version` and `agy models`. Init prefers it over `gemini` when both are present. |
| Auth | One interactive `agy` login, cached under `~/.gemini/antigravity-cli/`. An unauthenticated headless run exits with `authentication required`. |
| Model | A slug from `agy models`; `effort` accepts `low`/`medium`/`high` only. To migrate a `provider = "gemini"` crew, change the provider and use an `agy models` slug. |
| Flags | `--input-format stream-json --output-format stream-json --dangerously-skip-permissions`, plus `--print-timeout` set to the activity's maximum deadline (runtime plus capped build-admission credit) minus 30 s. A shorter custom value is kept; Orbit still enforces the actual runtime and earned queue credit. Don't add `agy --sandbox` or Gemini CLI flags. |
| Sandbox | Write: `~/.gemini` (shared with the Gemini CLI). macOS: read `~/Library/Keychains`. |
| MCP | `~/.gemini/config/mcp_config.json` or `.agents/mcp_config.json`, not `.gemini/settings.json`. |

A terminal `result` with `status: "SUCCESS"` completes the step; a non-zero exit with a terminal `ERROR` surfaces the bounded, redacted `error` string. Running for the whole `--print-timeout` spends the provider budget and is never completion: a run that then exits 0 with `SUCCESS` but no Orbit completion envelope fails with a diagnostic naming the print-timeout and elapsed time, keeping the bounded `final_message`. An early exit without an envelope gets the ordinary completion-envelope message.

Background commands cannot be turned off for a headless run. `agy` 1.3.3 has no flag or setting for it (`agy --help` lists none), so Orbit passes nothing and instead tells every provider, in the shared prompt contract, to run commands in the foreground. `agy` itself waits for a background task it started only until `--print-timeout`, capped at 30 minutes, then ends the turn and exits 0 with the task still running and no envelope. When the last `manage_task`-style status report in stdout shows a `task-<id>` still running, the step fails naming that task, the elapsed time and the bounded last `response`, instead of the generic message. The exit code, a `SUCCESS` wrapper and a persisted summary never count as completion; whatever the run left in the checkout is unverified.

### OpenCode CLI

| | |
|---|---|
| Install | `opencode --version`, then `opencode models`. Served modes (`serve`, `--attach`, `web`) are not used. |
| Auth | `opencode auth login` (`auth.json` under `$XDG_DATA_HOME/opencode`), or a vendor key via `pass`. |
| Model | A fully qualified `<vendor>/<model>` (`anthropic/claude-sonnet-4-5`). `effort` renders as `--variant`: `high`/`max` only. |
| Flags | `run --format json --auto`. `--auto` is required unattended (otherwise every permission request is auto-rejected) and is not a security boundary. `--continue`, `--session` and `--share` are never passed. |
| Sandbox | Write: `$XDG_DATA_HOME/opencode`, the config root (`$OPENCODE_CONFIG_DIR`, else `$XDG_CONFIG_HOME/opencode`), `$XDG_STATE_HOME/opencode` and `$XDG_CACHE_HOME/opencode`. |

Orbit does not write OpenCode's `opencode.json` MCP config and `orbit mcp init` has no OpenCode target; Orbit tools go through the shell tool. Only assistant `text` parts are read; a terminal `error` event or non-zero exit fails the step. No provider token usage is reported.

### MCP client registration

`orbit workspace init --mcp` registers Grok through the project `.mcp.json` shared with Claude Code; `orbit mcp init --scope home --grok` uses `~/.claude.json` (both listed in Grok's [MCP compatibility documentation](https://docs.x.ai/build/features/mcp-servers)). Re-running init migrates an older Orbit-generated Grok entry out of `.grok/config.toml`, keeping other Grok settings and servers. Init keeps the native `.grok/config.toml` target when `[claude_compat] imported = true` in `~/.grok/config.toml` (Grok then ignores project `.mcp.json`), and the native home target when `[compat.claude] mcps` is disabled or `GROK_CLAUDE_MCPS_ENABLED=0`. `grok inspect` shows the loaded source. Removing the shared `orbit` registration through Claude or Grok removes it for both. Gemini CLI still uses `.gemini/settings.json` (`~/.gemini/settings.json` for home scope); its [configuration reference](https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/configuration.md) does not document the shared locations.

---

## Per-task crew override

`workflow.default_crew` is only the fallback. Each task has an optional `crew`, and `orbit run ship` resolves each task by the [resolution precedence](#resolution-precedence), so one ship can mix crews; each child run records its crew (`resolved_crew` in `orbit run show`). A single child pipeline whose tasks name different crews, or mix set and unset crews, fails rather than falling back to the default.

### Automatic crew pools by complexity

```toml
[workflow]
low_complexity_crews = ["luna"]
medium_complexity_crews = ["grok", "sol"]
hard_complexity_crews = ["astra"]
xhard_complexity_crews = ["fable", "astra"]
```

- **Selected at creation.** A task created without `crew` (`orbit task add`, `orbit.task.add`, an auto-task mint with no template crew, an import) draws from its complexity's pool, else `default_crew`, storing `task.crew` and its provenance `task.crew_source`; a `crew_assigned` history entry records the source (`explicit`, `pool:<complexity>` or `default`). With no crews configured the field stays unset.
- **Complexity changes redraw automatic crews.** When the pilot or `task update --complexity` changes the tier, a crew sourced from another pool or the `default` fallback is redrawn in the same mutation, logged as `crew_redrawn` with previous and new source and crew. Explicit crews stay pinned, even when an update names the crew a pool already chose; status transitions keep the selection. `task update --crew <name>` pins; [`--crew ""`](#setting-taskcrew) redraws. Every stored change records the actor, prior and new crew, and whether it was named or drawn.
- **Tiers:** `low`, `medium`, `hard`, `xhard`. Unset or `unassessed` complexity uses the default chain. The task pilot never demotes a task out of `xhard`.
- **Empty pool** (`[]`, the init scaffold) means no pool: the task gets `default_crew`, as it does when all members are [disabled](#disabled-crews). Blank entries and unknown crew names fail before dispatch.
- **Preferences, not allowlists.** An explicit `task.crew`, an explicit run crew, and system, review and preparation jobs keep the crew they name, except that a standing [provider failure hold](#provider-failure-holds) redirects a task-crew draw away from the crews it excludes, and a [provider usage limit](#provider-usage-limits) removes limited crews from the draw.
- **Admission** (drain or ship) checks a pool-sourced crew against the current tier and its enabled, positive-weight members. A stale pool assignment or `default` fallback, or no crew, draws from the current pool, else the default chain; a default fallback is never an explicit pin. Without `crew_source`, legacy assignment history recovers provenance, and a crew with no assignment evidence counts as explicit. Admission never writes back to the task.
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

**Allowlists.** With `--allow-crew`, the draw renormalizes over permitted members, keeping their ratios. A pool with no permitted positive-weight member is disjoint, and `orbit run readiness --allow-crew <crew>` reports `crew_not_allowed`. The allowlist is checked against `task.crew` and still applies to system and review activities at dispatch.

**Frozen per run.** The admitting run captures the effective pools in run input `auto_crew_pools`, inherited by descendants. Each admitted leaf records `crew` and `crew_selection` (task ID, complexity, source, and the eligible `[{name, weight}]` after renormalization), shown as `Crew Selection:` in `orbit run show`. Retries and resumes keep it.

### Provider failure holds

A local run can fail because of its provider rather than the work. The step error then carries a typed marker followed by `provider=<name>`. Only the failing provider CLI's own stderr, terminal error or failure frames set a marker, never the agent's transcript, tool output or Orbit envelopes (an agent reporting a GitHub rate limit is not a provider limit).

| Marker | Meaning | Excluded crews | Base backoff |
|---|---|---|---|
| `[provider_capacity]` | The selected model was at capacity. | The run's crew, or every crew of the failing step's provider when that step ran on another provider (a reviewer, say). | 15 min |
| `[provider_unavailable]` | Unusable on this host, such as failed authentication. | As for capacity. | 30 min |
| `[provider_refusal]` | Content policy refused the task: a Codex content-filter `error` or `turn.failed` frame (`This content was flagged for possible …`), or a Claude `result` with `stop_reason: "refusal"`. | Every crew of the provider, which would refuse the same content again. | 24 h |
| `[provider_limit]` | The account hit a usage limit (texts below). | Every crew of the provider; only crews whose model contains the named model (case-insensitive) when the limit names one. | Until the reported reset (below) |

Capacity, unavailability and refusal backoff doubles for each other hold the task got in the last 24 hours, up to 24 hours. A usage limit holds until the provider's reset, clamped to 5 minutes – 7 days after the failure; with no readable reset it starts at 30 minutes and doubles per earlier usage-limit hold on that provider in the last 24 hours, up to 6 hours. Provider labels are parsed first, so an alias is the same provider. Crews an earlier, still-standing hold excluded stay excluded.

Recognized usage-limit texts: Antigravity's `Individual quota reached … Resets in 1h37m37s`; Gemini's `TerminalQuotaError`; Codex's `You've hit your usage limit … try again at <time>` (an `error` and `turn.failed` pair); a Claude `result` with `is_error: true` starting `You've hit your` or `You've reached your`, or a `rate_limit_event` with status `rejected`; grok's `Too Many Requests (429)`. The marker then carries `model=<model>`, `window=<window>` and `resets_at=<RFC 3339 time>`. A relative reset is read from the failure time; a Codex `try again at` time is local to the host.

**Handoff.** Step and final recovery skip these failures. On the PR pipeline the handoff returns `held_provider_failure`: it commits the candidate on the run's branch, publishes no branch and opens no `[BLOCKED]` PR, but pushes the candidate to `refs/orbit/candidates/<task>/<run>` on `origin` so a claim on another host can continue it (a task comment says when that push failed). Run finalization moves the task to `backlog` under a `provider_failure_hold` history event, whose note carries the class, provider, excluded crews, `not_before` and run.

**Admission during the hold.** Until `not_before`, the crew is drawn from crews the hold does not exclude: the task's own crew or pool, then its complexity pool, then `default_crew`; `crew_selection.source` names the hold. With none left, the local drain defers the task and `orbit run readiness --json` reports `reason: "provider_backoff"` with the release time. A hold that excludes nothing (the failed run resolved no crew) still defers until `not_before`. An explicit run-input `crew` ignores the hold. A usage-limit hold on an explicit crew (`crew_source` `explicit`, or a legacy pin) follows [`workflow.provider_limit_explicit_crews`](#provider-usage-limits): under `wait` the task waits with `provider_backoff` instead of being redirected; under `pool` it draws as above. The hold ends at `not_before` or any later status change, and the next run resumes the committed candidate. Operator steps: [stuck job runs](runbooks/stuck-job-runs.md#a-task-held-after-a-provider-failure).

**The host's limit record.** A usage-limit failure also records an observation in the global store (`~/.orbit/orbit.db`, table `provider_limit_observations`): one row per provider, model and window with the reset, run, crew and provider text. A newer observation replaces the row, an older one never does. Recording is advisory: a failure to record logs a warning, and the run is still typed and held. After every Codex or Claude run, successful or not, the host also records the provider's own reading of each window (source `event`): percent used (can exceed 100), window length and reset.

- Codex: the last `token_count` with `rate_limits` in the run's own thread rollout (`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*-<thread_id>.jsonl`, `CODEX_HOME` from `[execution.env]`, else `~/.codex`), reading at most the last 1 MiB. `primary` and `secondary` windows are account-wide.
- Claude: the last stdout `rate_limit_event` (Orbit runs `claude --output-format stream-json --verbose`): each `unifiedWindows` entry (`five_hour`, `seven_day`, …) plus the currently limiting window if absent there. `seven_day_opus` and `seven_day_sonnet` carry their model; an overage window is recorded with `gating` off.
- A missing rollout, another thread's rollout or an unrecognized reading records nothing. Other providers report no usage window.

**Pull drains and claimed leaves.** An authentication failure releases the claim and excludes every crew of that provider (aliases included); a declared [`auth_probe`](#executor-authentication-recovery-probes) can re-admit them in the same window. A capacity failure excludes only the leaf's crew, since another model may have room. A usage limit releases the claim with failure class `provider` and `provider_limit: true`, which does not count against the task's release budget and excludes no crew itself; the follower's own limit record excludes the provider's crews until the reading lapses.

### Provider usage limits

A hold learns of a limit only after a run fails on it. Delivery admission also reads this host's [limit record](#provider-failure-holds) before dispatch and stops routing to a crew whose provider account, as logged in here, is at or near a window's limit.

```toml
[workflow]
provider_limit_max_used_pct = 90            # default
provider_limit_overrides = ["claude:80", "grok:100"]
provider_limit_budgets = ["grok:30usd/5h"]  # none by default
provider_limit_explicit_crews = "wait"      # or "pool"
```

**Limited crews.** A crew is limited while a live, gating reading of its provider is exhausted or at or above the provider's threshold (its `provider_limit_overrides` entry, else `provider_limit_max_used_pct`). A reading that names a model (Claude's `seven_day_opus`) limits only crews whose model contains it, case-insensitively; any other reading limits every crew of the provider (aliases included). At `100` only an exhausted window gates (99% gates nothing). An overage window (`gating` off) never gates.

**Lapse.** A reading counts until its `resets_at`. Without one it counts for its window's length (60 minutes when that is unknown), or, for an exhausted error reading, for the first usage-limit backoff, 30 minutes. The crew is eligible again at the next admission pass, with no provider probe.

**Gated draws.** Only delivery admission (the crew draw of drains and `orbit run ship`, and the pull window); system, review, final-recovery and task-pilot crews are not gated.

- **Pool and default tasks** (`crew_source` `pool:<tier>` or `default`, or no crew): limited members are removed and the rest renormalized, as with `--allow-crew`. A limited assigned crew is redrawn from the unlimited members of the current complexity pool; the task record is not rewritten.
- **No default fallback.** With every member limited the task waits; filtering never falls through to `workflow.default_crew`, usually the most expensive crew and often on the same provider.
- **Explicit crews** (`crew_source` `explicit`, or a legacy pin): `wait` (default) keeps the task in the backlog; `pool` draws from the unlimited members of its complexity pool.
- **Explicit run crews** (`orbit run ship --crew`) are the operator's decision and not gated; `crew_selection.provider_limit` records the limit.

A redrawn task's `crew_selection.source` names the limit. A task left with no unlimited crew waits with `reason: "provider_limit"` in `orbit run readiness --json` and the detail `<provider> <window> at <used>% (limit <threshold>%) until <reset>; crews <list> skipped`, lifting by itself after the reset. An `orbit run ship` naming such a task still runs it on its usual draw. A standing [provider failure hold](#provider-failure-holds) that leaves no crew reports `provider_backoff` first. Operator steps: [stuck job runs](runbooks/stuck-job-runs.md#a-task-waiting-on-a-provider-usage-limit).

**Pull drains.** Each pass of a follower's pull drain re-reads its own store and excludes each limited crew from its crew window with source `provider_limit` and an `until` time; unlike other exclusions, it lifts at `until` within the same drain. The owner never hands the follower a task on that crew meanwhile. The owner's before-PR reviewer is not gated.

**Surfaces** all share admission's view of live readings (liveness, threshold, crew rules), so none shows a crew runnable while admission skips it.

- `orbit run readiness`: a `Provider limits:` line per gated reading (`claude five_hour 93% >= 90% until 15:00Z: opus, sonnet skipped`; a reset on another UTC day includes its date) and each waiting task's detail. `--json` has a top-level `provider_limits` array of every live reading: `provider`, `scope` (the model a reading names, else `null`), `window`, `used_percent`, `exhausted`, `resets_at`, `source`, `observed_at`, `gating`, `partial`, `threshold`, `gated`, `until` and `crews` (enabled crews covered).
- `orbit run show <pull-drain>`: `excluded <crew> (provider_limit until <time>): <reading>`.
- `orbit doctor`: one `provider-limits:<provider>` row per provider an enabled crew uses (a provider with no usage windows and no budget, which is every provider but Codex and Claude, shows `info`; a budgeted one shows its `ledger` reading, such as `grok 5h budget 67%`), plus `provider-limits:workflow.system_crew` and `provider-limits:operation.review_crew` warnings when those ungated lanes use a gated provider. Row semantics: [health checks](runbooks/health-checks.md#run-orbit-doctor). `orbit doctor providers` shows the same per executor (`LIMITS` column, `provider_limits` in `--json`).
- Dashboard: the Settings › Crews Limit column (tightest window, used percent, reset, `gated` badge); the Drain card names gated readings and lists tasks waiting with `provider_limit`.

#### Budgets for providers that report no usage

Grok, Antigravity and Gemini don't report how close an account is to its limit; Orbit learns it only from the failure. `provider_limit_budgets` declares the operator's allowance per rolling window so Orbit gates the provider before it runs out.

- **Format.** `provider:<amount><unit>/<window>`: a positive amount in `usd` or whole `tokens`, a window of `<n>h` or `<n>d` up to 31 days, each provider once (`xai` is stored as `grok`). No defaults. Load and `orbit config set` reject a malformed entry.
- **Reading** (source `ledger`, account-wide): this host's spend on the provider in the trailing window ÷ the amount, gated at the provider's threshold. `usd` sums each invocation's reported cost; `tokens` sums input plus output tokens from the invocation ledger. Its reset is the earliest moment enough old spend ages out to drop below the threshold (at `100`, below the budget).
- **Partial.** A `usd` reading where some invocation recorded no cost is `partial` (`"partial": true`; surfaces note `partial: some invocations report no cost`), and its spend is a lower bound.
- **Precedence.** A live provider-reported reading or limit failure for the provider wins; the ledger applies again when it lapses. The ledger reading is computed on each read, never stored.
- **Attribution.** An invocation counts for the provider it ran on. Older invocations are attributed by agent name, so an earlier Antigravity run, recorded under its model's family, does not count toward an `antigravity` budget.
- **This host only.** Interactive use and other hosts on the same login are invisible. Set the budget below the account's allowance and keep the provider's own failure as the backstop.

### Final recovery pool

`workflow.final_recovery_crews` is the weighted pool the `final_recovery` activity draws one crew from, once per run, when a task fails after step-failure recovery is exhausted; that crew evaluates the failure and attempts a final typed decision and fix.

```toml
[workflow]
final_recovery_crews = ["sol:100", "opus:20"]
```

- **Unset:** `["sol:100", "opus:20"]`, filtered to crews defined in `[crews.*]`. With neither defined the pool is empty and final recovery is off, without a validation error.
- **Explicit:** standard pool grammar (`name` or `name:weight`); every named crew must exist in `[crews.*]`.
- **`[]`** disables final recovery.

### Setting `task.crew`

| Surface | How |
|---|---|
| Dashboard | The crew dropdown on each task card. `default: <crew>` means the task has no `crew` and inherits `default_crew`. |
| CLI | `orbit task add --crew <name>`, or `orbit task update <id> --crew <name>`. `--crew ""` on `update` redraws for the current complexity. |
| MCP | `crew` on `orbit.task.add` / `orbit.task.update`. An empty string on update redraws; `null` is rejected. Omitting `crew` keeps an explicit pin; a complexity change can redraw a pool assignment or default fallback. |

### What "ran" vs what "was selected"

`orbit.task.show` returns `crew` (the stored selection) and `crew_source` (provenance, absent on legacy records), plus `resolved_crew` and `crew_model` when this host can resolve it; an associated run's persisted resolution takes precedence over current config. A `fields: ["crew"]` projection returns the stored selection. `task.crew` is validated on write. Deleting an explicitly pinned crew makes `orbit run ship` fail at run start, before any agent dispatches. Pool assignments are revalidated at admission, so keep pools valid when deleting members.

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

`$CARGO_HOME` is the child's variable if passed through `[execution.env].pass`, else `~/.cargo`. The grant applies only to profiles that already grant some write (`implementer`, `unrestricted`, `docs_writer`, …); read-only profiles such as `reviewer` and `pure_compute` get none. On Linux, a cache path missing on the host is skipped, not created.

## Sandbox pseudo-tty allocation (macOS)

The macOS profile allows `pseudo-tty`, `/dev/ptmx` (read, write, ioctl), and read, write and ioctl on `/dev/ttys[0-9]+`, so `openpty`/`posix_openpt` work inside a worker (a test driving a CLI through a real terminal, say). Other PTYs remain subject to normal OS checks. Linux needs no equivalent rule.

---

## Executor authentication recovery probes

An executor may declare `spec.auth_probe`. Only a pull drain's **auth-excluded** providers run it: never at drain start, for healthy crews, after the window closes, or for capacity or refusal exclusions. The first attempt is due ten minutes after the failed leaf; failures wait twenty, then thirty minutes (the cap). A pass durably acknowledges those auth failures and re-admits the provider's crews within the window; it never clears a preflight, capacity, crew or host exclusion. Without a probe the exclusion lasts the whole window: re-login and start a new drain. Owner-local drains use timed, task-specific [provider holds](#provider-failure-holds) instead. Drain-side view: [distributed drain runbook](runbooks/distributed-drain.md).

Only the shipped **claude** executor declares one: a minimal `claude -p --model haiku` call with built-in and MCP tools disabled ([CLI contract](https://code.claude.com/docs/en/cli-reference)), a one-line prompt on stdin, and success on exit zero plus `ORBIT_AUTH_OK` on stdout. Each attempt costs one model request. `claude auth status` is not used: cached status can lag a real 401 and may refresh shared OAuth credentials. Codex, Gemini, Antigravity, Grok, Copilot, Cursor, Pi and OpenCode declare none (their minimal model-call and success contracts are not established); local-shell has no provider authentication.

Seeding preserves existing operator executor definitions; to opt in, add the declaration to the installed definition and start a drain:

```yaml
auth_probe:
  args: ["-p", "--model", "haiku", "--output-format", "text", "--tools", "", "--disallowedTools", "mcp__*", "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}', "--max-turns", "1", "--no-session-persistence"]
  stdin: "Reply with exactly ORBIT_AUTH_OK."
  timeout_seconds: 60
  success: { kind: stdout_contains, text: ORBIT_AUTH_OK }
  relogin_hint: "Run `claude auth login`, or renew `claude setup-token`."
```

- `args` replaces the activity argv and uses the same resolved executor command and launcher.
- `success.kind` is `exit_zero`, or `stdout_contains` with non-empty `text`; both require exit zero. `timeout_seconds` is 1–120.
- A probe gets the leaf runner's cleared environment plus `[execution.env].pass`, provider-required and fixed entries, sandbox resolution and provider carve-outs (the macOS login keychain included), bounded process-tree supervision, and no Orbit activity tool grants. Without explicit `allow_fallback`, a sandbox refusal keeps the exclusion and backoff.

`orbit doctor`, `orbit run show <drain>` and the dashboard drain panel show provider, host, exclusion time, auth class, re-login hint, credential origin and next probe time; doctor reads the record and calls no provider. Credential origin names a passed `CLAUDE_CODE_OAUTH_TOKEN` (or `ANTHROPIC_API_KEY`) by name only, else the macOS keychain or provider cache: the configured route, not a keychain read or proof of which credential the CLI used. To avoid sharing a rotating Desktop login, pass a dedicated `claude setup-token` credential as `CLAUDE_CODE_OAUTH_TOKEN`.

## `[execution.env]` — the agent subprocess environment

Every agent subprocess (bare, Bubblewrap or `sandbox-exec`) starts from a **cleared** environment and receives only:

| Group | Contents |
|---|---|
| Baseline | `HOME`, `LANG`, `LC_ALL`, `LOGNAME`, `PATH`, `SHELL`, `TERM`, `TMPDIR`, `TZ`, `USER`. `PATH` starts with any [`workflow.validation_env.path`](#agent-sessions-share-the-configured-path) entries. |
| `pass` | Names listed in `execution.env.pass`. Default: `HOME`, `PATH`, `CODEX_HOME`, `TMPDIR`, `USER`, plus `__CF_USER_TEXT_ENCODING` on macOS. |
| Provider extras | Variables the selected provider runtime declares it needs. |
| Orbit envelope | Named `ORBIT_*` variables: run, task and session identity (`ORBIT_RUN_ID`, `ORBIT_TASK_ID`, `ORBIT_SESSION_ID`, …), locators (`ORBIT_WORKSPACE`, `ORBIT_WORKTREE_ROOT`, `ORBIT_BIN`, `ORBIT_REGISTRY_ROOT`, …) and activity bindings (`ORBIT_ACTIVITY_*`, `ORBIT_STEP_INDEX`, …). Privilege-bearing names (`ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN`) are not admitted; an inherited `ORBIT_ROOT` is removed. |

This is an allowlist, not a secret filter: a benignly named credential such as `DATABASE_URL` never reaches an agent unless you name it. Agents keep network access, so this is the boundary against accidental exfiltration.

```toml
[execution.env]
pass = ["HOME", "PATH", "CODEX_HOME", "TMPDIR", "USER", "GITHUB_TOKEN"]
```

- `pass` replaces the default rather than extending it; restate the baseline names you want.
- A listed name that is unset is absent, not empty. Names must be valid identifiers.
- `pass` is a [security key](#where-config-lives): a workspace file that omits it gets the built-in default, not the global value.
- Inheriting the full environment is not configurable; a stale `execution.env.inherit` is ignored.

**Unset names.** A `pass` name the launching process lacks reaches no agent, and the provider falls back to another login (a missing `CLAUDE_CODE_OAUTH_TOKEN` falls back to the desktop login). Start drains from a login shell, or export the variable in the service's environment. `orbit run auto` (`--pull` included), `orbit run ship` and `orbit run job` warn once on stderr for each unset or empty name you added, and record the names on the run (`Unset env:` in `orbit run show`, `run.env_pass_unset` in JSON, the dashboard run detail). Built-in defaults are not reported (`CODEX_HOME` and macOS's `__CF_USER_TEXT_ENCODING` can legitimately be absent). `orbit doctor`'s `env-pass` row checks its own environment. These are warnings only; the start proceeds and no value is printed.

**Worker scope.** The same admitted names are given to a detached pipeline worker. On Linux, containment forwards them with name-only `systemd-run --setenv=NAME`, so the values stay in the child environment and never appear in `systemd-run` or worker argv. An explicit worker edit, such as a toolchain `PATH` or removing `ORBIT_ROOT`, wins over the admitted snapshot. See [worker containment](#worker-containment).

**Clock-started runs.** launchd (macOS) or a systemd user timer (Linux) starts `orbit clock tick` with no login environment. The tick reads `~/.orbit/clock.env` (created comment-only, mode `0600`, by `orbit routine init --install-clock`) and gives each discovered workspace's runs the names that workspace's effective `pass` admits, as child-environment defaults; the tick's own process environment is never changed. A non-empty value already in the tick's environment wins, and an empty one can be filled from the file. A dry run loads nothing. Unit files never contain a secret. A `clock.env` that is a symlink, owned by another user, or group- or world-readable is refused as a load error (the tick still runs). `orbit clock tick --json` reports loaded names as `clock_env_loaded`, never values. Sandboxed agents can't read `clock.env` itself ([SECURITY.md](../SECURITY.md#sandbox-model-and-known-limits)).

**Claude on macOS.** A Claude activity with neither `CLAUDE_CODE_OAUTH_TOKEN` nor `ANTHROPIC_API_KEY` in its environment is refused before `claude` starts, rather than fall back to the Claude Desktop login, which the Desktop revokes mid-run. `orbit doctor`'s `claude-worker-token` row warns ahead of time, naming the `pass` list or `clock.env` that lacks the token; a `clock.env` token under a name `pass` omits counts as lacking.

---

## Other sections

The remaining keys, grouped as `orbit config show` groups them.

### Execution

| Key | Default | What it does |
|---|---|---|
| `execution.codex.sandbox` | `workspace-write` | Codex sandbox mode: `read-only`, `workspace-write` or `danger-full-access`. Under `workspace-write`, Codex's extra writable roots are Orbit's runtime stores, never the whole workspace `.orbit` or `~/.orbit`. `orbit init` seeds `danger-full-access` globally. [Security key](#where-config-lives). |
| `execution.codex.approval_policy` | unset | `untrusted`, `on-request` or `never`. Security key. |
| `execution.env.pass` | `HOME`, `PATH`, `CODEX_HOME`, `TMPDIR`, `USER` (+ `__CF_USER_TEXT_ENCODING` on macOS) | Environment names passed into agent subprocesses. Security key. See [`[execution.env]`](#executionenv--the-agent-subprocess-environment). |
| `execution.proc_spawn_max_timeout_minutes` | `45` | Longest timeout one `proc.spawn` call may use inside a managed activity (1–1440), also capped at the activity's remaining wall-clock budget (passed to the agent as `ORBIT_ACTIVITY_DEADLINE_UNIX_MS`). Outside an activity the ceiling is 60 seconds. The result reports `timeout_ceiling_ms` and its source. |

### `[review]` and `[operation]` — automatic review

Both tables resolve built-in → global → workspace, and an unknown key in either fails load. Design: [review gate](design/review-gate/2_design.md); operations: [review-gate runbook](runbooks/review-gate.md).

| Key | Default | What it does |
|---|---|---|
| `review.before_pr` | `false` | Hold PR creation for a fresh reviewer that fixes what it finds. PR route only. Captured at submission, so a run in flight keeps its value. |
| `review.before_landing` | `false` | Open the PR first, review its published head while hosted CI runs, and merge only the head the review settled. A reviewer fix is validated and pushed onto the PR under a lease; any outcome but approve leaves the PR open and unmerged, with the task in `review` and a typed reason. PR route only; captured at submission. See [before-landing review](design/review-gate/2_design.md#32-before-landing-review-of-the-open-pr-orb-14849). |
| `review.before_landing_hosts` | `[]` | Owner policy for the distributed drain: machine ids (`hm_…`) whose claimed leaves review their open PR before it lands, while this owner's own deliveries keep `review.before_landing` off. See the per-host note below the table. |
| `review.minutes` | `30` | Wall-clock limit for one candidate's review, fix commit and final validation included (1..=1440). Retries and resumes share the minutes; the reviewer is stopped when they run out, and a spent review is not restarted. A changed candidate (after a completion rebase, say) is a new review. |
| `review.baseline_commands` | `[]` | Commands settlement may rerun on the host, besides `workflow.required_validation_commands`, to confirm a [reviewer's red-base claim](#a-red-base-is-held-not-blocked). A listed command is binding: it needs a passing record or a claim settlement reproduces, or the review settles incomplete. Captured when the delivery or claim is admitted. With the required commands, also the gates an implementer may hand off when its sandbox deferred only Bubblewrap tests ([required validation](#required-validation)). |
| `[[review.host_evidence]]` | `[]` | Checks a claimed leaf owes for the paths it changed; see [owed host evidence](#owed-host-evidence). A workspace list replaces the global one. |
| `operation.review_crew` | unset | Crew for automatic review: the before-PR or before-landing reviewer, and review tasks the after-landing `delivery-code-review` auto-task mints (unset: that definition's template crew). One crew name, or a weighted pool. See the review crew pool note below the table. |

**One review layer before landing.** Load fails, naming both keys and the layer that set each, while `review.before_pr` and `review.before_landing` are both on. `orbit config set --global` refuses a write that would turn both on against the current workspace's `config.toml`, so it cannot leave that workspace unloadable.

**Per-host before-landing review.** `review.before_landing_hosts` is read only by the owner, and only for claimed leaves. A leaf on a listed machine reviews its PR before it lands, as `review.before_landing` makes every leaf do; the owner's own deliveries and unlisted machines are unaffected. Entries are trimmed and de-duplicated. Each must be an `hm_` machine id, with letters, digits, `_` or `-` after the prefix; any other entry fails load, naming its index. The list is redundant while `review.before_landing` is on, and loading fails while `review.before_pr` is on and the list is non-empty. The owner resolves it for the caller's machine when a follower probes or pulls, and the claim captures the result, so a follower that echoes its probed contract matches. The machine label is caller-chosen, so this is policy, not a security boundary. Protocol revision is unchanged; the contract is in the [`orbit.task.pull` spec](design/distributed-drain/specs/task-pull.md).

**Review crew pool.** `operation.review_crew` is one crew name (`"sol"`) or a pool (`["sol", "grok"]`, or `["sol:3", "grok:1"]`), written like the [complexity pools](#weighting-a-pool): all bare or all weighted, and at least one member above weight `0`. Each review draws one crew by weight after three filters, each applied only when some member survives it: a member that is disabled or undefined here drops out; a member whose provider is at a usage limit here drops out; a member that implemented the reviewed work drops out (a distributed claim has no implementer, so it skips this filter). If no member survives the first filter, the review refuses to start with that member's refusal. A distributed owner draws one crew per claim, when it answers the probe. `[]` unsets it. Load refuses an empty name, a mix of bare and weighted entries, a weight that is not a non-negative whole number, a crew the registry does not define, a weighted pool that names a crew twice, and a pool whose weights are all `0`. Before-PR and before-landing review refuse to start while it is unset.

**After-landing review** is the `delivery-code-review` auto-task's `enabled` flag (`orbit auto-task toggle delivery-code-review on|off`), not a config key. `orbit config show`, `orbit doctor`'s `review` check, the dashboard Config tab and `orbit.drain.probe` report all three switches with their source (minutes for the first two, the next batch due for after-landing). The `review` check names the commit observed against `origin/<branch>` and fails once the oldest unobserved first-parent commit has waited the batch's `max_wait_minutes`, even if the tracking tip is recent. Doctor and auto-task inspection read the existing tracking ref without fetching (the local branch when there is no origin; a missing tracking ref leaves verification unknown). Delivery evaluation fetches, and defers on fetch failure without spending a coverage retry. A batch counts as reviewed only on [per-delivery evidence](design/automation-triggers/5_operations.md#evidence-submission) whose examined and skipped paths match each delivery's frozen diff, from a review task whose agent persisted its own execution summary; otherwise the batch stays owed with a typed reason. A landing on a base that moved after review is covered too, labelled `rebased_clean`, when the landed PR's head is the reviewed candidate and a conflict-free merge of that change onto the new base reproduces the landed tree exactly ([review gate §7](design/review-gate/2_design.md#7-delivery-coverage)). Any other base change, a head pushed after review, or a conflicting base stays an ordinary obligation.

**Local-only delivery.** Before-PR and before-landing review need a PR, so both are refused for local-only delivery ([captured timing](design/review-gate/2_design.md#2-captured-timing)). In a local-delivery workspace with either on, `orbit doctor`'s `review` check names the deciding layer (`review.before_pr (global)` or `(workspace)`) and the remedy (ship through the PR route, or turn the key off for local delivery); readiness reports otherwise eligible local tasks as `eligible: false` with `reason` `local_route_before_pr` or `local_route_before_landing` and the layer in `detail`; and the local drain withholds them before spawning delivery (earlier per-task exclusions keep their own reasons). A workspace `before_pr = false` clears the hold, but direct `task_local_pipeline` admission still fails closed whenever its captured policy enables before-PR review.

#### Owed host evidence

Each `[[review.host_evidence]]` rule has `kind` (`codeql` or `host_sandbox_test`), `name`, `paths` (workspace-relative globs), `os` (`linux` or `macos`), the exact `command`, and a result `artifact` (a relative `.json` path, unique across rules). A rule Orbit could never fulfil fails load.

- A `codeql` rule is owed on a host of another OS and fulfilled by the owner on `os`.
- A `host_sandbox_test` rule is owed on a host of `os`, which runs it outside the agent sandbox at settlement, so its command must be one settlement admits. A held `linux` rule the leaf could not run is fulfilled by a Linux owner whose host can create namespaces.
- The owner captures the rules on each claim. The reviewer records an owed check `not_run` and never attempts it, and a verdict whose only gaps are owed checks holds for them instead of blocking.

Example: the [CodeQL runbook](runbooks/codeql-local.md#non-linux-hosts) rule (`kind = "codeql"`, `paths = ["**/*.rs"]`, `os = "linux"`, `command = "scripts/codeql-rust-local.sh …"`, `artifact = "evidence/codeql-rust-linux.json"`). Design: [owed evidence](design/review-gate/2_design.md#4-what-the-validation-records-establish-orb-11528-orb-11545).

#### Distributed PR delivery

At claim admission the owner captures `review.before_pr` and `review.before_landing` and, when either is on, a review contract: its version, `operation.review_crew` (for a pool, one crew drawn when the owner answers the probe, which the pull that echoes it keeps), `review.minutes`, `[[review.host_evidence]]`, `workflow.required_validation_commands` and `review.baseline_commands`. The follower runs that contract, never its own settings: it needs a matching build and protocol, a before-PR gate and the captured reviewer crew (else `before_pr_reviewer_unavailable`; the implementer's crew is never substituted), and its own `before_pr` is declared for diagnostics only. The handoff's typed evidence (reviewed head and base, reviewer, certificate) must satisfy the contract, with a passing required record per captured command and the captured `review.baseline_commands` unchanged; if the owner's current lists no longer match the snapshot, acceptance fails closed with fresh-claim guidance. Protocol revision 8 carries the list: explicit `[]` means no required checks, and a contract without the field is refused. A claim on a machine listed in `review.before_landing_hosts` captures `before_landing` on while the owner's own `review.before_landing` is off; `before_pr` stays off for it. Under `before_landing` the leaf reviews its own PR after `pr_open`, merges nothing, and hands off evidence for the head it settled; evidence of the other timing or another head is refused. After-landing review stays owner-side and does not refuse a pull. Wire contract and refusal codes: [`orbit.task.pull` spec](design/distributed-drain/specs/task-pull.md).

### `[pr]` — pull requests

| Key | Default | What it does |
|---|---|---|
| `pr.task_url_template` | unset | URL template linking a task ID in PR descriptions. |
| `pr.close_on_terminal` | `true` | When a task lands (`done`), is rejected or is archived, close its open Orbit-authored delivery and `[BLOCKED]` preservation PRs (rules below). `false` disables. |
| `pr.delivery_authors` | `[]` | Forge logins (case-insensitive) whose PRs count as Orbit-authored. Empty: the login `gh` is authenticated as on this machine. Add a follower's login if it opens PRs this owner has not accepted as a handoff. |

`pr.close_on_terminal` closes a PR only if its head branch is `orbit/<TASK-ID>-…`, its body names the task, and it was opened by a `pr.delivery_authors` login or is the delivery PR of an accepted follower handoff. Human PRs and other tasks' PRs are never touched, and branches are never deleted. On `done` the landing PR stays open: any `#N` the done note names, or, when the note names none and the task leaves `review`, the PR it was reviewed through. The closing comment names the landing (the done note, such as `delivered by pull request #N merged as <sha>` or `already landed as <sha>`) or the state and reason. A new run, block or requeue closes nothing, so a re-run can resume from its `[BLOCKED]` PR. A task with no recorded PR makes no forge call, and a forge error is logged as a warning without failing the transition.

### Housekeeping

| Key | Default | What it does |
|---|---|---|
| `tasks.id_start` | unset | Forward-only floor for this machine's task-ID allocator, raised on every runtime build and never lowered, so machines can hold disjoint ranges. For the first seed prefer `orbit workspace init --task-id-start N`. See [task migration](design/task-migration/1_overview.md). |
| `ci_failure.operator_suppression_hours` | `6` | Hours a CI sweep finding archived or rejected without `covered_by` suppresses its exact failure key, from that event (0–720; 0 disables). An explicit task or GitHub PR cover instead holds the key while open, or until its landed commit is observed in the failing checkout; if it fails to hold, later completed repairs still own duplicates or defer older failures until their landing is tested. |
| `automation.stall_window_minutes` | `60` | How long a delivery-automation consumer may sit on a stuck deferral (`history_diverged`, `repository_changed`, `provider_identity_missing`, `state_missing`) before a warning and one deduped friction (1–1440). Transient backpressure never escalates. See [auto-tasks](../plugin/skills/orbit-setup/references/auto-tasks.md). |
| `scoring.enabled` | `true` | Record per-agent scoreboard metrics for task runs. |
| `runtime.log_retention_days` | `7` | Delete `orbit.jsonl` and `orbit-agent.jsonl` archives older than N days (≥ 1). |
| `runtime.log_max_total_mb` | `500` | Operational `orbit.jsonl` archive budget in MiB, pruned oldest first (≥ 1). |
| `runtime.log_max_file_mb` | `100` | Roll the active operational log past N MiB (≥ 1, ≤ `log_max_total_mb`). |
| `retention.audit_days` | `60` | Days `orbit gc audit` keeps audit rows (1..=36500). Older host-wide command-audit rows and this workspace's run-audit rows become reclaimable, with blobs no remaining row or pending write names. Nothing is deleted until an operator runs `orbit gc audit --apply` or enables the `store-gc` routine. |
| `retention.runs_days` | `60` | Days after a terminal run finishes before `orbit gc runs` may drop its pipeline state (1..=36500). The run row, steps and summary stay; held and non-terminal runs are never selected. |
| `worktree.reclaim` | `["target"]` | Relative rebuildable path globs reclaimed in kept terminal worktrees; an explicit list replaces earlier layers, `[]` disables. `*` stays within a component; a whole-component `**` spans components. Absolute, parent or root-matching patterns fail load. Registration, worker, symlink, confinement and Git content gates protect candidates and tracked files. See [worktree reclamation](runbooks/worktree-reclaim.md). |
| `worktree.reclaim_below_free_mib` | unset | Also reclaim during admission while free space beneath the state directory is below this, oldest terminal worktrees first; active runs stay protected. |
| `security_alert_sweep.min_severity` | `moderate` | Lowest Dependabot and Code scanning severity the security alert sweep files. See [below](#security_alert_sweep--security-finding-severity). |
| `plugin.legacy_callback_identity` | `false` | Deprecated. Also accept the environment token and process ancestry as a plugin callback credential. Removed next release. |

Agent relay output goes to `orbit-agent.jsonl` with its own 200 MiB archive budget and 50 MiB active-file limit, so it cannot consume the operational budget; each feed is pruned by its own size and age limits. The dashboard and `orbit log tail` merge both active feeds. Full rotation runs in long-lived processes (`orbit mcp serve`, `orbit clock tick`/`orbit sweep`, `orbit web serve`); short-lived commands only roll an oversized active file.

### `[security_alert_sweep]` — security finding severity

`security_alert_sweep.min_severity` (`low`, `moderate`, `high` or `critical`; default `moderate`) is the lowest severity `dependabot_alert_sweep_pipeline` files for Dependabot and Code scanning findings. Secret scanning findings are always filed.

```toml
[security_alert_sweep]
min_severity = "high"
```

Precedence: run/job `min_severity` input, then workspace, then global, then `moderate`; `orbit run job dependabot_alert_sweep_pipeline --input min_severity=critical` overrides one run. Scheduled routines use the configured floor without shadowing the job. An invalid value fails validation naming the key. The file step records `min_severity`, `min_severity_source` (`input`, `workspace`, `global` or `built-in`) and `excluded_below_min_severity`; `orbit run show <RUN_ID>` summarizes the filed count, floor and source, and excluded alert count and numbers, even when the sweep succeeded.

## Plugins — `.orbit/plugins.yaml` and `[plugins.<ns>]`

Plugins install once per machine (`~/.orbit/plugins/<ns>/<version>/`); enable state and grants are host-local. A checkout keeps only its gitignored pin file, and `orbit plugin sync` installs what it names. Linked Git worktrees share the main checkout's `.orbit/plugins.yaml`: runtime loading, `orbit plugin sync` and `orbit plugin doctor` read that file and ignore a worktree-local one. Operator guide (installs, grants, source builds, workspace toggles): the orbit-setup [plugins reference](../crates/orbit-core/assets/skills/orbit-setup/references/plugins.md). Manifest spec: [plugin standard](design/plugins/1_scope.md).

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
- **Grants and builds are per invocation, never config.** A manifest's `spec.permissions` is only a request: `--grant` on `plugin add --enable`, `enable`, `upgrade` or `sync` grants it, an ungranted plugin's tools register inactive, and an upgrade that widens the request disables the plugin until re-granted. A `git+<url>#<full commit id>` source declaring `spec.build` compiles in an isolated build sandbox (Bubblewrap on Linux, `sandbox-exec` on macOS) and needs `--allow-build` on that `orbit plugin add` or `upgrade` (else `build_consent_required`, showing the build plan); consent never comes from pins, `config.toml`, the environment or MCP, and managed runs and routines cannot give it (`build_consent_unavailable`).
- **`[plugins.<ns>]`** holds the plugin's settings, validated against its `spec.config.schema` over its defaults. `orbit config get`/`set plugins.<ns>.<key>` accepts only declared keys, layered workspace over global. An invalid value disables only that plugin. A section for a plugin not installed here is warned about and ignored.
- **`[plugin_enablement]`** (workspace file only) switches a host-enabled plugin off here with `<ns> = false`; write it with `orbit plugin disable|enable <ns> --scope workspace`. An unset entry inherits the host state, and `true` never enables a plugin the host has disabled (`enable --scope workspace` refuses instead). `orbit plugin sync` applies a pin's `enabled: false` here, never to the host row. Values must be booleans keyed by namespace.

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

**Retired keys warn and are ignored.** Delete them; `orbit config get`/`set` reject them with migration notes.

| Retired | Note |
|---|---|
| `operation.preset`, `completion`, `preparation`, `preparation_due_seconds`, `promotion`, `leaf_ceiling`, `recovery`, `recovery_episodes_per_task`, `recovery_minutes_per_task`, `delivery_cap` | Operation mode was removed; `[operation]` keeps only `review_crew`. |
| `operation.review_repair_cycles`, `operation.review_reviewer_starts` | Each candidate gets one review whose reviewer fixes its findings in one commit, bounded by `review.minutes`. |
| `[docs]` | The docs corpus was removed. |
| `[semantic]`, `search.model` | Search is lexical (SQLite FTS5) and needs no model. The legacy `semantic.db` path remains the lexical search database. |
| `workflow.pilot_max_complexity` | Route a tier with `workflow.<tier>_complexity_crews`, or pin `crew` on the task. |
| `[duel]`, `[duel.models]` | Retired. |
| `[routines]` (`role = "source"`) | Every registered owner checkout is a routine source. |
| `knowledge.task_id_pattern` | Deprecated. |
| `execution.env.inherit` | Inheritance is fixed off. |

**Deprecated keys warn and are translated.** Still honoured, warned on every load, and refused by `orbit config get`/`set`; a later release makes them errors.

| Deprecated | Translation |
|---|---|
| `operation.review_policy` | `before-pr` sets `review.before_pr = true`. `after-landing` enables the `delivery-code-review` auto-task while no operator has configured it; once its `enabled` flag is set by `orbit auto-task toggle` or any other edit, that flag decides. `none` turns neither on. A `[review]` table in the same file wins. |
| `operation.review_minutes` | Becomes `review.minutes`, now one candidate's review limit rather than a lineage total. |
