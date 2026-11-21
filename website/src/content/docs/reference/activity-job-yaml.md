---
title: Activity and job YAML
description: "Fields, defaults and examples for hand-authored activity, job and script YAML."
---

Use this page to write or check an activity, a job or a script manifest by hand. An activity is one agent run or one registered action. A job is an ordered list of steps.

In the examples, `example_action`, `assess_a` and `assess_b` stand for an action and activities in your catalog.

## Activity envelope

```yaml
schemaVersion: 2
kind: Activity
metadata:
  name: example_activity
spec:
  type: deterministic
  description: Run a registered deterministic action.
  action: example_action
  input_schema_json:
    type: object
    properties: {}
  output_schema_json:
    type: object
    properties:
      status:
        type: string
```

| Field | Default | Meaning |
|---|---|---|
| `type` | Required | `agent_loop` or `deterministic`. |
| `description` | Required | What the activity does. |
| `input_schema_json`, `output_schema_json` | None | JSON Schema for the activity's input and output. |
| `fsProfile` | None | A [filesystem profile](/reference/policy-format/) name, such as `reviewer`. |

## Activity types

| Type | Type-specific fields | Runs |
|------|-----------------|-------|
| `agent_loop` | See [Agent loop fields](#agent-loop-fields). | The provider's CLI agent. |
| `deterministic` | `action` (required), `config` (optional). | A registered deterministic action. |

### Agent loop fields

| Field | Default | Meaning |
|---|---|---|
| `instruction` | None | The prompt for the agent. |
| `steering_instruction` | None | Extra guidance, added only when [mid-run steering](/concepts/agents/#runtime-path) is enabled. |
| `tools`, `tool_disallow_list` | `[]`, absent | See [Agent tool lists](#agent-tool-lists). |
| `on_denial` | `terminate` | When the agent requests a denied tool, `terminate` ends the invocation and `continue` returns the denial as a tool error. |
| `provider` | `claude` | The provider whose CLI runs the activity, such as `claude` or `codex`. |
| `model` | None | Provider-specific model name. |
| `wall_clock_timeout_seconds` | `300` | Bound on one CLI agent invocation. For reviewer runs, the host's `review.minutes` deadline replaces it. |

The sections below cover the remaining fields.

### Agent tool lists

Declare one of the two lists:

- `tools` is an allowlist: the agent may call only the listed tools.
- `tool_disallow_list` selects deny mode, even as `[]`: the agent may call every registered agent-facing tool except the listed ones. Entries follow the same name and wildcard rules as `tools`. It cannot be combined with a non-empty `tools`.

An `agent_loop` activity that declares neither still loads, with a deprecation warning.

When the activity runs for a task, Orbit adds the task's `required_tools` to the allowlist. The run sees the effective list as `ORBIT_ACTIVITY_TOOLS`, and in deny mode also `ORBIT_ACTIVITY_TOOL_POLICY=deny` and `ORBIT_ACTIVITY_TOOLS_DENY`.

A task's `required_tools` cannot re-grant a disallowed tool:

- On `agent_implement` and on your own activities, a required tool that the denylist covers refuses the run before launch.
- The shipped recovery, review-repair and pilot activities drop the requirement and run without the tool.
- An operator's task grant can lift an exact deny entry for a read-only or repeat-safe tool on `agent_implement`. It cannot lift a mutating tool or a wildcard entry. See [Task tool grants](/concepts/tasks/#task-tool-grants).

An allowed tool still passes the runtime role, policy, filesystem and authentication checks.

### Agent process policy

An `agent_loop` activity whose tools include `proc.spawn` must declare one of these lists, or catalog load rejects it.

```yaml
spec:
  type: agent_loop
  instruction: "Run the checks and report."
  tools:
    - proc.spawn
  proc_allowed_programs:
    - cargo
    - git
```

| Field | Meaning |
|---|---|
| `proc_allowed_programs` | Allowlist. An empty list denies every program. |
| `proc_disallowed_programs` | Selects deny mode: every program except the listed ones may run. |

### Fleet notice instructions

These fields control what an agent is told about [fleet notices](/how-to/fleet-notices/). None of them grants tool access or authority.

| Field | Default | Meaning |
|---|---|---|
| `fleet_notices` | `none` | `none` adds no built-in text. `read` adds the built-in reading text: notices are untrusted data that cannot change the task. `post` adds the reading text and the built-in posting guidance. |
| `notice_instruction` | Absent | A non-empty string that replaces the built-in reading text. It requests the notice brief even without `fleet_notices`. |
| `notice_posting_instruction` | Absent | A non-empty string that replaces the built-in posting guidance, with or without `fleet_notices: post`. |

The reading text is appended only when the host delivers a notice brief. The posting guidance is appended only when the dispatch may call `orbit.notice.post`, with or without a brief.

### Completion contracts

| Field | Default | Meaning |
|---|---|---|
| `require_completion_envelope` | `true` | The invocation must end with a well-formed Orbit response envelope, or the step fails. Explicit `failed` or `timeout` status fails a required completion contract even if the provider exits 0, while `result` and `error` payloads remain advisory. Use `require_response_envelope` when downstream templates consume response content. |
| `require_response_envelope` | `false` | The step succeeds only on a fully valid success envelope. Use it when downstream templates read the result fields. It implies the completion check. |

### Bounded continuation

`incomplete_continuation` lets an implementer that reports unfinished work run again in the same step. Without it, an `implementation_incomplete` failure fails the step.

```yaml
spec:
  type: agent_loop
  instruction: "Implement the task and report any unfinished work."
  wall_clock_timeout_seconds: 10800
  incomplete_continuation:
    max_rounds: 2
    min_remaining_seconds: 900
```

| Field | Meaning |
|---|---|
| `max_rounds` | Required. The most continuations one step dispatch may start after its first invocation. `0` admits none. |
| `min_remaining_seconds` | Required. The unspent wall-clock budget a continuation needs before it starts. |

A continuation starts only when the invocation exits cleanly and its envelope carries:

- `schemaVersion: 1` and `status: "failed"`.
- `error.code: "implementation_incomplete"` with a non-empty `error.message`.
- `result.remaining_work`: 1 to 32 non-empty strings, each at most 2,000 bytes.
- No `result.blocker`.

A timeout or a provider failure is not continued, and the step needs a task record.

Each continuation is a fresh invocation in the same worktree with the same crew, tools and sandbox. It receives the remaining work list and runs inside what is left of `wall_clock_timeout_seconds`. The step succeeds only when a continuation completes.

If the step still fails, its message gives the continuation count, the original error and the stop reason: `rounds_exhausted`, `wall_clock_exhausted`, `task_terminal` or `task_unavailable`. The worktree keeps the partial work. The shipped `agent_implement` activity declares two rounds and a 900-second minimum.

### Response correction

`response_correction` reruns a read-only activity when the response parser rejects its reply. Without it, a rejected reply fails the step.

```yaml
spec:
  type: agent_loop
  instruction: "Review the named task and return the result envelope."
  require_response_envelope: true
  fsProfile: reviewer
  wall_clock_timeout_seconds: 1800
  tools:
    - orbit.search
    - orbit.task.show
  response_correction:
    max_rounds: 1
    min_remaining_seconds: 30
```

`max_rounds` and `min_remaining_seconds` work as in [Bounded continuation](#bounded-continuation).

A correction starts when the rejection is the step's only failure and is one of:

| Rejection | Cause |
|---|---|
| `contradictory_reply` | The envelope says `status: "success"` and also carries an `error` object. |
| `missing_envelope` | The turn ended in prose with no envelope, and the output was not truncated. |
| `structured_output_retry_exhausted` | Claude's structured-output retries ran out before a valid result. |

A timeout, a provider failure or an envelope that says `failed` is not corrected. The step succeeds only if the correction's own reply parses.

A correction reruns the whole activity, so load refuses the policy unless the activity is read-only:

- `require_response_envelope` is `true`.
- `fsProfile` is `reviewer`.
- `tools` grants no built-in mutating Orbit tool, directly or through a wildcard. Mutating tools include `orbit.task.update`, `orbit.task.artifact` action `put`, `orbit.friction` action `add` and `orbit.command.exec`.
- The activity does not also declare `incomplete_continuation`.

The shipped `task_pilot` activity declares one round and a 30-second minimum.

### Retired agent fields

| Field | What happens |
|---|---|
| `backend` | `cli` parses and is ignored. `http` and `auto` fail catalog load. See [Retired backend selection](/reference/config/#retired-backend-selection). |
| `max_iterations` | Accepted, with no effect. Only `wall_clock_timeout_seconds` bounds an invocation. |
| `trustedHostExecution: true` | Fails catalog load. |

A workspace-local activity under `.orbit/resources/activities/` that fails to load is left out of the catalog with a warning, and jobs that do not reference it still run. `orbit doctor` reports the file. Correct the definition or delete it.

## Deterministic local shell

The `local_shell` action runs one local command as a step, with no prompt, model or tools.

```yaml
schemaVersion: 2
kind: Activity
metadata:
  name: git_status
spec:
  type: deterministic
  description: Report the checkout's porcelain status.
  action: local_shell
  config:
    command: git
    args: ["status", "--porcelain"]
    cwd: "."
    env:
      GIT_OPTIONAL_LOCKS: "0"
    timeout_ms: 60000
    allow_nonzero_exit: false
```

To run a script, name the interpreter. Orbit runs exactly `<shell> -c <script>`:

```yaml
config:
  shell: /bin/sh
  script: |
    set -eu
    make ci-fast
```

| `config` key | Default | Meaning |
|---|---|---|
| `command`, `args` | None | A direct exec with literal arguments. No shell is involved, so nothing is split, globbed or expanded. |
| `shell`, `script` | None | An interpreter and the script text passed to it with `-c`. Cannot be combined with `command` or `args`. |
| `executor` | `local-shell` | The executor definition. When `config` names no program, its `command` and `args` run. Its `args` come before an explicit `command`'s `args`. |
| `cwd` | Workspace root | Relative to the workspace root. A path that escapes the checkout fails the step. |
| `env` | None | Variables layered over the executor definition's `env`. The host environment is not inherited. |
| `timeout_ms` | `600000` | 1 through 3600000 (1 hour). The executor definition's `timeout_seconds`, when set, replaces the default. |
| `allow_nonzero_exit` | `false` | `false` fails the step on a nonzero exit or timeout. `true` returns the exit code as output. |

Step input can choose the checkout with `workspace_path`, but it never reaches the argument list.

The step's output is `{ success, exit_code, stdout, stderr, log_tail, log_tail_truncated, duration_ms, timed_out, timeout_ms, argv, cwd, sandbox }`. The shipped `local-shell` executor declares no OS sandbox. To confine shell steps, add a `sandbox` to the executor definition and an `fsProfile` to the activity.

## Job envelope

```yaml
schemaVersion: 2
kind: Job
metadata:
  name: example_job
spec:
  state: enabled
  max_active_runs: 1
  kind: workflow
  steps:
    - id: run_action
      target: activity:deterministic_reference
```

`schemaVersion` is `2`, or `3` for a job with [auto-task steps](#auto-task-steps). The `spec` fields:

| Field | Default | Meaning |
|---|---|---|
| `state` | Required | `enabled` or `disabled`. |
| `kind` | `workflow` | `workflow` or `subroutine`. |
| `max_active_runs` | `1` | How many runs of this job may be active at once. |
| `steps` | Required | The steps, in order. See [Step bodies](#step-bodies). |
| `default_input` | None | Defaults under the run's input. See [Input and output wiring](#input-and-output-wiring). |
| `recovery_activity`, `final_recovery_activity`, `failure_activity` | None | See [Recovery hooks](#recovery-hooks). |
| `task_delivery`, `owns_task_worktree`, `steering_gate` | None, `false`, none | See [Custom task-delivery jobs](#custom-task-delivery-jobs). |
| `expire_when` | None | Outer deadline for a job with auto-task steps. |

### Custom task-delivery jobs

A workspace or plugin job opts into task delivery with `spec.task_delivery`. While a run is live, it holds the delivery slot of every task in its `input.task_ids`, and `orbit run ship` refuses to dispatch those tasks again.

```yaml
schemaVersion: 2
kind: Job
metadata:
  name: custom_delivery
spec:
  state: enabled
  kind: workflow
  max_active_runs: 1
  task_delivery:
    modes: [pr]
  owns_task_worktree: true
  default_input:
    task_ids: []
  # Supply your complete delivery steps here.
```

These fields grant no permission to dispatch, skip review or merge.

| Field | Default | Meaning |
|---|---|---|
| `task_delivery.modes` | `[]` | The ship modes (`pr`, `local`) for which a task may select this job. An empty list, or `task_delivery: {}`, holds slots but is not selectable. |
| `owns_task_worktree` | `false` | Set `true` only when the job creates and owns a task worktree. It opts the job into guarded worktree cleanup after delivery. |

To route a task to `custom_delivery`, give it one `delivery:custom_delivery` tag. The job must be in the active catalog, be `kind: workflow` and list the requested ship mode. Without a delivery tag, shipping uses `task_pr_pipeline` or `task_local_pipeline`.

Shipping refuses a malformed tag, two different delivery tags, an unavailable job or a mode mismatch. It does not fall back to the default pipeline. Tasks shipped together must select the same route. See [Delivery workflows](/getting-started/workflows/#ship-one-task).

A job that contains `git_commit` with `scope: all` (the default) accepts one task per run. To deliver several, use `scope: per_task`.

#### Steering gate

When [mid-run steering](/reference/config/#settable-keys) is enabled, a gate reruns the implementer if an operator comment arrived before the commit:

```yaml
spec:
  steering_gate:
    before: commit
    round: implement_bundle
    candidate: resume_candidate
```

| Field | Meaning |
|---|---|
| `before` | Required. The top-level step that must not start while an operator comment is undelivered. |
| `round` | Required. An earlier top-level step that dispatches the implementer again. |
| `candidate` | Optional. A step before `round` whose output describes a carried candidate. When that output has `implement: false`, it must give a non-empty `source_run_id`. |

A new operator comment reruns the job from `round`, even if that step's `when` is false. `workflow.mid_run_steering.max_rounds` bounds the reruns. When the rounds run out, or a round delivers none of the comments, delivery holds as `steering_unconsumed`.

## Step bodies

Every step has an `id` and exactly one body.

| Body key | Runs |
|---|---|
| `target: activity:<name>` | A catalog activity. |
| `target: script:<name>` | A workspace script. See [Script steps](#script-steps). |
| `target: auto-task:<name>` | One task from an auto-task definition. See [Auto-task steps](#auto-task-steps). |
| `spec` | An inline activity spec. |
| `parallel` | A fixed set of branches, at once. |
| `fan_out` with `fan_in` | One worker per item of a runtime list. |
| `loop` | A body repeated per item or until a condition holds. |

A step may also carry these fields:

| Field | Meaning |
|---|---|
| `stage` | The stage this step belongs to: an id string, or `{ id, label }`. Following steps inherit it. |
| `when`, `retry` | See [Modifiers](#modifiers). |
| `recovery_activity` | See [Recovery hooks](#recovery-hooks). |
| `default_input` | On `target` and `spec` steps. See [Input and output wiring](#input-and-output-wiring). |
| `fsProfile` | On `spec` steps: the filesystem profile for the inline activity. |

`timeout_seconds` on a `target` or `spec` step is not a dispatch deadline. Bound an agent step with its activity's `wall_clock_timeout_seconds`.

Reference an activity, or inline one:

```yaml
- id: assess
  target: activity:agent_assess_diff
- id: run_action
  spec:
    type: deterministic
    action: example_action
    config: {}
```

Run branches in parallel:

```yaml
- id: parallel_assessment
  parallel:
    join: { mode: all }
    branches:
      - id: branch_a
        target: activity:assess_a
      - id: branch_b
        target: activity:assess_b
```

### Join modes

`parallel.join` and `fan_in.join` take the same modes. A failed join stops the steps after it.

| Mode | Succeeds when |
|---|---|
| `{ mode: all }` | Every branch or worker succeeds, or the block is empty. |
| `{ mode: any }` | At least one succeeds. An empty block fails. |
| `{ mode: quorum, n: 2 }` | At least `n` succeed. `n` is positive and at most a parallel block's branch count. |

## Fan-out and fan-in

Fan-out runs the same work once per item of a list that is known only at run time.

```yaml
- id: triagers
  fan_out:
    items: "{{ steps.prepare.output.partitions }}"
    max_workers: 5
    worker:
      id: triager
      target: activity:triage_escalation
      default_input:
        task_ids: "{{ item.task_ids }}"
  fan_in:
    join: { mode: any }
    collect: triage_results
    errors: triage_errors
```

| Field | Default | Meaning |
|---|---|---|
| `fan_out.items` | Required | A template that renders to a JSON array, at most 256 items. Text that is not JSON is read as a comma- or whitespace-separated list. |
| `fan_out.max_workers` | `4` | How many workers run at once. Every item still gets a worker. |
| `fan_out.worker` | Required | A full step with its own `id` and a `target` or `spec` body. Inside it, `{{ item }}` is the current item. |
| `fan_in.join` | Required | A [join mode](#join-modes). |
| `fan_in.collect` | None | An extra pipeline key for the collected outputs, read as `{{ steps.triage_results.output }}`. They are always recorded under the step's `id` too. |
| `fan_in.errors` | None | A pipeline key for an error array in the same order as the outputs. Choose a key distinct from the step's `id` and `collect`. |

A worker sees the pipeline as it was at dispatch, and its outputs do not reach sibling workers. The collected output has one slot per item, in dispatch order. A failed worker's slot is `null`, and its slot in the errors array holds the failure message.

## Loops

A loop repeats its body once per item or until a condition holds. `max_iterations` is always required.

Once per item:

```yaml
- id: mark_review
  loop:
    items: "{{ input.task_ids }}"
    max_iterations: 100
    steps:
      - id: mark_review_one
        target: activity:update_task
        default_input:
          task_id: "{{ item }}"
```

Until a condition holds:

```yaml
- id: drain
  loop:
    max_iterations: 6000
    break_when: "{{ steps.refill.output.done }} == true"
    steps:
      - id: window
        target: activity:drain_window
      - id: refill
        target: activity:pull_refill
```

| Field | Meaning |
|---|---|
| `max_iterations` | Required. The most passes through the body. A rendered `items` list longer than this is refused. |
| `steps` | Required. Body steps, run in order. They share the job's pipeline state. |
| `items` | A template that renders to a list. The body runs once per item. |
| `break_when` | A [`when`](#when-conditional-steps) expression, checked after each full pass. |
| `timeout_seconds` | A template that renders to a non-negative number of seconds. It caps the loop's elapsed time without interrupting a running pass. |
| `interval_seconds` | The wait between passes that did not break, capped by the remaining timeout. |

A failed body step ends the loop and fails the step. A loop records `{ iterations, broke, timed_out, last_output }` under its step id. Reaching `max_iterations` without breaking is not a failure: the step succeeds with `broke: false`.

## Input and output wiring

A step passes data to its activity with `default_input`. Every string in the mapping is rendered as a template before dispatch.

```yaml
- id: apply
  target: activity:apply_results
  default_input:
    prepared: "{{ steps.prepare.output }}"
    workspace_path: "{{ input.workspace_path }}"
```

| Token | Resolves to |
|-------|-------------|
| `{{ input.<path> }}` | The run's input, after the job's `default_input` merge. |
| `{{ steps.<id>.output.<path> }}` | A completed step's output. `{{ steps.<id>.output }}` forwards the whole object. |
| `{{ steps.<id>.state.<path> }}` | A completed step's recorded state, when it recorded any. |
| `{{ steps.<id>.files }}` | The files a script step kept. See [Output files](#output-files). |
| `{{ item }}`, `{{ item.<path> }}` | The current fan-out or loop item. |
| `{{ iteration }}` | The current 0-based worker or loop index. |

- A token that is the whole string keeps its JSON type, so the string `"618"` stays a string.
- A token inside longer text renders to text. Text that parses as JSON, such as a number, `true` or an array, becomes that JSON value.
- A token that names an unknown namespace, a missing key or a step with no recorded data fails the step.
- A step with no `default_input` receives the run's input unchanged. `default_input: "{{ input }}"` forwards it without rendering anything inside it.

The job's `spec.default_input` supplies defaults for the run's input. Explicit run input wins key by key.

## Modifiers

### `when`: conditional steps

`when` is a boolean expression, evaluated once before any retry. A step whose `when` is false is skipped: it succeeds without running and records no output. A later step that reads its output fails with `no data recorded for step`. A resumed run does not run a skipped step again.

```yaml
- id: push
  when: "{{ input.auto_push }} == true"
  target: activity:git_push
```

The grammar:

- `true` and `false`.
- `<value> == <value>` and `<value> != <value>`. Each side is rendered first, then compared as trimmed text.
- `&&` and `||`. `&&` binds tighter.
- A lone template that renders to `true` or `false`.

Anything else fails the step when the expression is evaluated.

### `retry`

```yaml
retry:
  max_attempts: 3
  initial_backoff_ms: 500
  backoff_cap_ms: 5000
  backoff_strategy: exponential
```

| Field | Default | Meaning |
|---|---|---|
| `max_attempts` | Required | Total attempts for the step body. |
| `initial_backoff_ms` | `1000` | First wait between attempts. Waits use full jitter. |
| `backoff_cap_ms` | `60000` | Longest wait. |
| `backoff_strategy` | `exponential` | `exponential` or `linear`. |

A denied tool, an unknown deterministic action and a validation error are not retried.

### Recovery hooks

A recovery hook names a catalog activity to run after a step has failed.

| Field | Where | Behavior |
|-------|-------|----------|
| `recovery_activity` | A step, or the job's `spec` | Runs once after the step's retries are exhausted, then the step runs once more. A step-level value overrides the job-level one. |
| `final_recovery_activity` | The job's `spec` | Runs at most once per run, after step recovery. It returns one decision: `resume`, `complete_no_diff`, `reject`, `archive`, `requeue` or `escalate`. Only `escalate`, or a malformed result, goes on to `failure_activity`. |
| `failure_activity` | The job's `spec` | A best-effort hook run once after a step failure. Use it for notification and cleanup, not repair. |

```yaml
spec:
  failure_activity: notify_failure
  steps:
    - id: validate
      recovery_activity: step_failure_recovery
      target: activity:candidate_validate
```

## Script steps

`target: script:<name>` runs a program file from the workspace under a sandbox. The name resolves to a manifest at `.orbit/resources/scripts/<name>.yaml`.

```yaml
schemaVersion: 1
kind: Script
metadata:
  name: package
spec:
  description: Package the built site into a tarball and report its digest.
  entry: tools/package.sh
  interpreter: [bash]
  args: ["--out", "out/", "--label={{ input.label }}"]
  requests:
    network: none
    timeout_seconds: 300
```

| Field | Default | Meaning |
|---|---|---|
| `entry` | Required | Workspace-relative program file. After symlinks it must be a regular file inside the workspace, outside `.git/` and `.orbit/state/`. |
| `description` | Empty | What the script does. |
| `interpreter` | The entry's `#!` line | Launcher argv, such as `[bash]` or `[uv, run, --offline, --script]`. Inline-code flags, such as `-c`, `-e` and `--eval`, are refused. |
| `args` | None | One argv element each. `{{ … }}` templates render against the step input and are never split or globbed. |
| `input_schema_json`, `output_schema_json` | None | JSON Schema for the step input and the script's JSON output. |
| `files.inputs` | None | Read-only inputs. See [Input files](#input-files). |
| `files.outputs` | None | Output file caps. See [Output files](#output-files). |
| `requests.network` | `none` | `none`, `https` or `any`. |
| `requests.read_roots`, `requests.write_roots` | None | Extra paths the script asks to read or write. |
| `requests.env_pass` | None | Host environment variable names the script asks for. `ORBIT_*` names are never passed. |
| `requests.timeout_seconds` | None | Requested time limit, at least 1. |

Unknown fields are refused, so the manifest cannot hold inline code. `requests` are asks: the grant decides. A routine cannot target a script; wrap it in a one-step job.

### Grant a script

A script runs only under an operator grant bound to the digests of the manifest and the entry file. Editing either file makes the grant stale. Agent sessions cannot grant or revoke.

```bash
orbit script grant package
orbit script list
orbit script show package
orbit script history package
orbit script revoke package
```

`orbit script grant` takes `--network`, `--read-root`, `--write-root`, `--env-pass`, `--follower`, `--output-max-files` and `--output-max-file-bytes`. `list` and `show` report each script as `granted`, `stale`, `ungranted` or `orphaned`. `history` lists every attempt, refusals included.

### How a script runs

Orbit checks the grant before it spawns anything. These refusals fail the step without retry:

| Code | Cause |
|---|---|
| `grant_missing` | No grant, or an expired one. |
| `grant_stale` | The manifest or entry changed since the grant. |
| `grant_insufficient` | The manifest requests more than the grant allows. |
| `script_sandbox_unavailable` | The host cannot apply the sandbox, or Orbit itself runs inside one. |
| `script_follower_refused` | A pull follower without a `--follower` grant. |

The script sees its interpreter, its working directory (a run worktree read-write, or the workspace root read-only), its granted roots and a scratch directory. It never sees `.orbit/`, the global Orbit root or credential paths under `HOME`.

Its environment holds only fixed `PATH`, `HOME`, `TMPDIR`, `LANG` and `TZ`, the `ORBIT_SCRIPT_*` step context and each granted `env_pass` variable. Passed values are redacted from the log, the step output and the recorded argv, but not from output files.

| | Linux | macOS |
|---|---|---|
| Sandbox | Bubblewrap | Seatbelt |
| `network: none` | Own network namespace. The host's `127.0.0.1` is unreachable. | Supported. |
| `network: https` | Outbound TCP to port 443 only. Needs Landlock ABI 4. | Refused with `script_sandbox_unavailable`. |
| `network: any` | Shares the host network. | Refused with `script_sandbox_unavailable`. |
| Scratch directory | `/orbit-script` | A host path. Read it from `ORBIT_SCRIPT_IN` and `ORBIT_SCRIPT_OUT`. |

On macOS, `machine.worker_containment = "strict"` refuses every script step, and privacy permissions (TCC) apply to the process that runs the job.

The step's rendered input is written to stdin.

| Result | Outcome |
|---|---|
| Exit 0, and stdout is one JSON document (at most 1 MiB) that matches `output_schema_json` | Stdout becomes the step's output. Empty stdout with no schema gives `{}`. |
| Exit 0 with any other stdout | The step fails without retry: `script_output_invalid`. |
| Nonzero exit, signal or timeout (`script_timeout`) | A failed attempt with `{exit_code, signal, timed_out}` as output, retried per the step's `retry`. |

### Input files

```yaml
spec:
  files:
    inputs:
      - from: "{{ input.brief }}"
        as: brief.md
      - from: site/dist
        as: site
```

| Field | Meaning |
|---|---|
| `from` | A path, rendered against the step input. A relative path resolves against the step's working directory. It must be a file or directory inside that tree or inside a root granted with `--read-root`. |
| `as` | A plain file name, unique in the manifest, with no `/`. The script reads the input at `$ORBIT_SCRIPT_IN/<as>`. |

An input that is missing, resolves elsewhere, overlaps `.orbit/` or a credential path, or holds more than 100,000 entries refuses the step with `script_file_input_unavailable`.

### Output files

A manifest returns files by declaring caps under `files.outputs`. The script writes them directly inside `$ORBIT_SCRIPT_OUT`. Subdirectories are not output.

```yaml
spec:
  files:
    outputs:
      max_files: 4
      max_file_bytes: 262144
```

| Field | Range | Grant default |
|---|---|---|
| `max_files` | 1 to 64 | 8 |
| `max_file_bytes` | 1 to 67108864 (64 MiB) | 1048576 (1 MiB) |

Setting either cap declares outputs, and a cap you leave out takes the grant's value. A manifest with no caps keeps no files. To allow more than the defaults, grant with the flags:

```bash
orbit script grant package --output-max-files 4 --output-max-file-bytes 262144
```

A re-grant without the flags returns to the defaults. A set's total size is at most files times bytes, and never more than 256 MiB.

Orbit keeps files only when the attempt exits 0 and its stdout is valid. It then checks the whole output directory, and one failure keeps no files:

| Code | Cause |
|---|---|
| `script_outputs_invalid` | An entry is not a regular file, or its name is not 1 to 128 bytes of ASCII letters, digits, `.`, `_` and `-`, or starts with `.`. |
| `script_outputs_over_cap` | Too many files, a file over `max_file_bytes`, or a total over the cap. |
| `script_outputs_unavailable` | The set could not be stored. |

A later step reads the kept set as `steps.<id>.files`, a list of entries with `name`, `bytes`, `media_type` and `sha256`.

#### Attach output files to a task

The shipped `attach_script_files` activity attaches a script step's kept files to a task as [task artifacts](/concepts/tasks/#task-artifacts).

```yaml
steps:
  - id: render
    target: script:render_report
  - id: attach
    target: activity:attach_script_files
    default_input:
      task_id: "{{ input.task_id }}"
      step: render
      files: ["report.pdf", "summary.json"]
      path_prefix: "report-outputs/"
```

| Input | Meaning |
|---|---|
| `task_id` | Required. The task that receives the artifacts. |
| `step` | Required. The script step that kept the files: a direct, earlier, successful step of the same job. |
| `files` | The file names to attach. Default: every file in the set. |
| `path_prefix` | Text placed before each file name to form the artifact path. Include the trailing `/`. |

The activity attaches all the selected files or none. It refuses a file over the 1 MiB task artifact limit, a path with `..` segments, and paths Orbit reserves: `mutation-proposals/`, `mutation-receipts/` and names that start with `review-` (except a valid `review-report.json`). A re-run replaces the artifacts at the same paths.

## Auto-task steps

`target: auto-task:<name>` binds one task from that auto-task definition and waits for its accepted completion. A job that uses it declares `schemaVersion: 3`. Under `schemaVersion: 2`, an `auto-task:` target, a wait key or `spec.expire_when` fails to load.

```yaml
schemaVersion: 3
kind: Job
metadata:
  name: release-readiness
spec:
  state: disabled
  expire_when: "now >= job.started_at + duration('24h')"
  steps:
    - id: qa
      target: auto-task:qa-full-sweep
      wait_until: "board.empty"
      require_result: release-readiness/v1
      reuse_when: >-
        result.status == 'passed' && result.revision == job.source_revision
        && now - result.completed_at <= duration('24h')
    - id: security
      target: auto-task:security-review
      wait_until: "self.prev.status == 'done' && board.empty"
      expire_when: "now >= self.wait_started_at + duration('8h')"
```

An auto-task step admits only `id`, `stage`, `when`, `target` and the four wait keys below. The wait keys are refused on any other step.

| Field | Meaning |
|---|---|
| `when` | Evaluated once; false skips the step. A skipped step is not accepted evidence, so use `wait_until` for prerequisites. |
| `wait_until` | Readiness predicate. False or unknown waits. Absent means ready when the job reaches the step. |
| `expire_when` | True ends the unfinished step as `timeout`. |
| `reuse_when` | Predicate over the auto-task's latest accepted `result`. True lets the step settle on that result instead of filing a task, after Orbit checks it. |
| `require_result` | Result contract. `release-readiness/v1` is the only registered value. It must match the definition's `completion.contract`. See [Completion evidence](/reference/scheduling-yaml/#completion-evidence). |
| `spec.expire_when` | Required on the job: `now >= job.started_at + duration('<d>')` or `now >= timestamp('<RFC3339>')`, at most 168h after admission. |

A job holds at most 32 auto-task steps, and none inside a `loop` or `fan_out`.

### What a wait expression reads

The predicates use the [condition trigger](/reference/scheduling-yaml/#condition-triggers) language and limits.

| Read | Fields |
|---|---|
| `now` | The evaluation time. |
| `job` | `started_at`, `source_revision`. The revision is pinned when any step sets `require_result`. Otherwise test it with `has()`. |
| `self` | `wait_started_at`, `status`, optional `task_id`, and `prev` (`status`, optional `end_time`). `prev` is the previous sibling step. It does not exist on a first step or a parallel branch. |
| `steps['<id>']` | `status`, optional `end_time` and `task_id`. The id is a literal naming an earlier step in the same list or an enclosing one, or a branch of an earlier parallel block. |
| `board`, `tasks['<ID>']` | The condition fields, plus `board.empty`: no backlog, in-progress or review task. |
| `result` | In `reuse_when` only: `status`, `revision`, `completed_at`, `contract`, `definition_digest`, `task_id`, optional `run_id`. |

A step status is `waiting`, `queued`, `running`, `done`, `held`, `failed`, `timeout`, `cancelled` or `skipped`. A misspelled status literal fails to load.

`orbit automation explain job:<name>` shows the checked steps and each resolved auto-task.

### How an auto-task job runs

An enabled job with auto-task steps runs only as a detached submission: `orbit run job <name>` or a routine. A foreground run, such as `orbit job run <path>`, is refused. Each run freezes the job definition, so an edit affects later runs only. Deadlines count from admission, including time spent queued.

When the job reaches an auto-task step, the step does the first of these that applies:

1. Settles on its bound task's outcome: a completed task is `done`, and an archived, rejected or deleted task ends the step.
2. Ends as `timeout` once its own or the job's `expire_when` holds.
3. Waits while `wait_until` is false or unknown, or the auto-task is missing, disabled or held by its own guard. With `require_result`, it also waits until the definition declares the same contract and its adapter script is granted.
4. Reuses the latest accepted result when `reuse_when` is true and the result covers the pinned revision, is within the contract's maximum age and still passes the evidence checks.
5. Binds an open task of the definition under `skip_if_open`, or files one task. A step files at most one task per run. With `require_result`, a finished task counts only after Orbit checks its evidence and the adapter answers `passed`.

While a step waits, the run stays `running` and counts against `max_active_runs`, but it holds no worker, worktree or drain slot. The scheduler resumes it within about a minute. After the run ends or the step expires, a task the step filed stays in the backlog.
