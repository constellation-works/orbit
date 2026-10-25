---
title: Activity and job YAML
description: "Reference shapes for schemaVersion 2 activity and job assets."
sidebar:
  order: 3
---

## Activity envelope

The examples illustrate schema shapes. `example_action` is a placeholder for
a registered action; `assess_a` and `assess_b` stand for catalog activities
you define. Replace those names before executing a job.

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

## Activity types

| Type | Required fields | Notes |
|------|-----------------|-------|
| `agent_loop` | `instruction`; optional `tools` or `tool_disallow_list`, `provider`, `model`, `wall_clock_timeout_seconds` | Runs the provider's CLI agent. Tool lists and retired fields are covered below. |
| `deterministic` | `action`; optional `config` | Runs a registered deterministic action. |

### Agent tool lists

`tools` is the activity's baseline allowlist. Declaring `tool_disallow_list`,
even as `[]`, selects deny mode instead: every registered agent-facing tool
except the listed ones. `tool_disallow_list` cannot be combined with a
non-empty `tools`.

For task-backed dispatch, Orbit adds the task's exact `required_tools` to the
baseline, removes duplicates, and rejects invalid requirements before the
provider launches. The resulting effective list is serialized as `tools` in
the CLI execution envelope and exported as `ORBIT_ACTIVITY_TOOLS`. The task's
requested list is serialized separately as `required_tools`. It cannot change
after the task is created, and audit evidence records both lists.

In deny mode the effective list is every callable tool, and the run also
exports `ORBIT_ACTIVITY_TOOL_POLICY=deny`, `ORBIT_ACTIVITY_TOOLS_DENY`, and
`ORBIT_ACTIVITY_NAME`. Disallow entries follow the same name and wildcard
rules as `tools`. A task's `required_tools` cannot re-grant a disallowed tool.
On `agent_implement`, and on any activity outside the shipped non-implementer
set, a covered requirement refuses the run before launch. `final_recovery`,
`step_failure_recovery`, `pr_conflict_recovery`, `agent_review_repair`,
`review_reconciliation_review`, `task_pilot` and `agent_invoke` drop that
requirement from the requested list and record a note; it stays out of the
callable set. The shipped `agent_explore` and `verify_finding` activities use
the same non-implementer rule.

An `agent_loop` activity that declares neither list still loads, with a
deprecation warning. Declare `tools` or `tool_disallow_list`.

Being on the allowlist never replaces runtime role, capability, policy,
filesystem, subprocess, or authentication checks.

Optional `steering_instruction` and `explore_instruction` fields add guidance
only when mid-run steering or read-only exploration is enabled and available.
See [Agent execution](../../concepts/agents/#runtime-path)
for the corresponding workspace settings.

### Retired agent fields

Agent execution uses the CLI path only. `backend: cli` still parses and is
ignored; `backend: http` and `backend: auto` fail catalog load, and
`orbit doctor --fix-retired-activity-backends` removes them. See
[Retired backend selection](../config/#retired-backend-selection).
`max_iterations` is accepted and has no effect: only
`wall_clock_timeout_seconds` bounds an invocation.

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

## Step bodies

Every step has an `id` and one body.

Reference an activity:

```yaml
- id: assess
  target: activity:agent_assess_diff
```

Inline a type-specific activity spec:

```yaml
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

`parallel.join` and `fan_in.join` use the same success requirements:

- `all` succeeds when every branch or worker succeeds, including an empty block.
- `any` requires at least one successful branch or worker, so an empty block fails.
- `quorum` requires at least `n` successes. `n` must be positive and cannot exceed
  a parallel block's declared branch count. Fan-out item counts are resolved at
  runtime, so a positive quorum fails when too few workers succeed, including
  when there are no items.

An empty fan-out records `[]` under its step id and optional `fan_in.collect`
alias, and emits dispatch and join audit events with zero workers. A failed
join stops subsequent steps.

Fan-in keeps one output slot per item in dispatch order. Failed workers have
`null` output slots; successful siblings remain available when `any` or
`quorum` admits the join. Set `fan_in.errors: worker_errors` to publish a
separate array at `steps.worker_errors.output` in the same order: successful
workers have `null` errors and failed workers have their dispatch error or
unsuccessful outcome's message. A worker panic records its panic diagnostic.
An empty block publishes `[]`. The errors array is retained in successful
compound checkpoints and restored on resume. Choose an errors key distinct
from the step's id and collect alias.

The shipped finding verification job passes these errors into apply as
`partition_errors`. A failed partition stays unverified with `verifier_failed`
and its crew and diagnostic; a successful verifier that omits a task's verdict
still reports `missing_verdict`. Native pinned-source inspection supports
Codex and Claude, so verifiers are drawn only from crews of those providers;
a tier without one passes to the next tier and then the default crew. A
verifier that still fails, for any reason, keeps its refusal alongside
successful partitions' verdicts.

## Fan-out and fan-in

Use fan-out when one step's output is a list and the same work must run
once per item, with bounded concurrency. A fan-out step always pairs
`fan_out` with `fan_in` on the same step:

```yaml
- id: verifiers
  when: "{{ steps.prepare.output.partition_count }} != 0"
  fan_out:
    items: "{{ steps.prepare.output.partitions }}"
    max_workers: 5
    worker:
      id: verifier
      target: activity:verify_finding
      default_input:
        task_ids: "{{ item.task_ids }}"
        partition_index: "{{ item.partition_index }}"
        workspace_path: "{{ steps.prepare.output.workspace_path }}"
  fan_in:
    join: { mode: any }
    collect: verdict_results
    errors: verdict_errors
```

- `fan_out.items` is a template that must render to a JSON array; text
  that is not JSON is read as a comma- or whitespace-separated list of
  strings. At most 256 items may be dispatched.
- `fan_out.max_workers` bounds how many workers run at once (default 4).
  It gates execution, not dispatch: every item gets a worker.
- `fan_out.worker` is a full step with its own `id` and body (`target` or
  inline `spec`), and may carry its own `default_input`, `when` and
  `retry`. Inside the worker, `{{ item }}` is the current item and
  `{{ iteration }}` its 0-based index. Workers see the pipeline as it was
  at dispatch; a worker's own step outputs do not leak into siblings or
  back into the parent.
- `fan_in.join` decides the step's success from the workers' results (see
  the join modes above). `fan_in.collect` additionally publishes the
  collected outputs under that pipeline key, readable downstream as
  `{{ steps.verdict_results.output }}`; the outputs are always recorded
  under the fan-out step's own `id` too. `fan_in.errors` publishes the
  position-aligned error array described above.

Downstream steps then consume the collection like any other step output:

```yaml
- id: apply
  target: activity:apply_finding_verification
  default_input:
    results: "{{ steps.verdict_results.output }}"
    partition_errors: "{{ steps.verdict_errors.output }}"
```

When to choose fan-out over `parallel`: `parallel` names a fixed set of
different branches in the YAML; fan-out derives its branches at runtime
from data, so use it whenever the item list is not known until the job
runs.

## Loops

Use a loop when a body must repeat — either once per item, like a serial
fan-out, or until a condition holds, like polling. `max_iterations` is
always required and bounds the body either way.

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
          status: "{{ input.terminal_status }}"
```

The rendered `items` list may not exceed `max_iterations`, so raise
`max_iterations` to the largest list you accept. Body steps run in order
and share the job's pipeline state, so a later body step — and the
`break_when` check — can read earlier steps' outputs. Inside the body,
`{{ item }}` is the current item and `{{ iteration }}` its 0-based index.
A body step that fails ends the loop and fails the step.

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

`break_when` uses the `when` grammar above and is checked after each full
pass through the body. Two optional bounds keep a polling loop finite in
wall-clock terms as well:

- `timeout_seconds` caps the loop's elapsed time. It is a template string
  that must render to a finite, non-negative number of seconds. `0` runs
  no iterations; an iteration already running is not interrupted, and the
  loop stops before the next one.
- `interval_seconds` waits between unsuccessful iterations (passes that
  did not break), capped by whatever timeout budget remains.

A loop records `{ iterations, broke, timed_out, last_output }` under its
step id, where `last_output` is the final body step's output from the
last pass. Reaching `max_iterations` without `break_when` ever holding is
not itself a failure — the step succeeds and reports `broke: false` — so
check that field downstream when convergence matters. Retired `session:`
bindings on loop or target steps are refused at load; conversation state
does not carry across iterations.

## Input and output wiring

A step passes data to its activity with `default_input`. Every string in
that mapping is rendered as a template before dispatch; numbers, booleans,
arrays and nested objects pass through, with templates rendered anywhere
inside them.

```yaml
- id: apply
  target: activity:apply_results
  default_input:
    prepared: "{{ steps.prepare.output }}"
    results: "{{ steps.collect_results.output }}"
    workspace_path: "{{ steps.prepare.output.workspace_path }}"
```

The template namespaces are:

| Token | Resolves to |
|-------|-------------|
| `{{ input.<path> }}` | The run's input (after the job's `default_input` merge below). |
| `{{ steps.<id>.output.<path> }}` | A completed step's recorded output. `{{ steps.<id>.output }}` alone forwards the whole output object. |
| `{{ steps.<id>.state.<path> }}` | A completed step's recorded state, when the step recorded any. |
| `{{ item }}` / `{{ item.<path> }}` | The current fan-out or loop item. Only available inside a fan-out worker or a loop body. |
| `{{ iteration }}` | The current 0-based fan-out worker or loop iteration index. |
| `{{ env.<NAME> }}` | One variable from the run's environment. A missing variable fails the step. |
| `{{ workspace_path }}` | The run's workspace path, when the run has one. |

A whole-string `{{ steps.<id>.output... }}` reference forwards the value
with its JSON type intact, so a string output such as `"618"` stays a
string. A template embedded in longer text renders to text, and text that
parses as JSON (a number, `true`, an array) is converted back to that JSON
value. A token that names an unknown namespace, a missing key or a step
that has not recorded data fails the step instead of rendering empty.

When a step declares no `default_input`, the activity receives the run's
input unchanged. The special whole-input form forwards the caller's input
verbatim, without rendering anything inside it:

```yaml
  default_input: "{{ input }}"
```

At the job level, `spec.default_input` supplies defaults under the run's
input: explicit run input wins key by key, and a run submitted with no
input uses the defaults as-is.

```yaml
spec:
  default_input:
    base_branch: agent-main
    mode: standard
```

## Modifiers

Any step may add `when` and `retry`.

### `when`: conditional steps

`when` is a boolean expression evaluated once, before any retry. A step
whose `when` is false is skipped: it records success with a `null` output
and never retries.

```yaml
- id: push
  when: "{{ input.auto_push }} == true"
  target: activity:git_push
```

The grammar is deliberately small:

- `true` and `false` literals.
- `<value> == <value>` and `<value> != <value>`. Each side is rendered on
  its own first, then compared as trimmed text, so a rendered value can
  never inject another operator.
- `&&` (binds tighter) and `||` combine comparisons.
- A lone template is allowed when it renders to `true` or `false`.

Anything else — an unknown operator, an unrenderable token, a lone value
that is not `true` or `false` — fails the step at evaluation time. The
same grammar drives a loop's `break_when` below.

```yaml
retry:
  max_attempts: 3
  initial_backoff_ms: 500
  backoff_cap_ms: 5000
  backoff_strategy: exponential
```

Retries re-run the step body with full-jitter backoff between attempts
(`initial_backoff_ms` defaults to 1000, `backoff_cap_ms` to 60000, and
`backoff_strategy` to `exponential`; `linear` is the other strategy).
Non-retryable failures — a denied tool, an unknown deterministic action,
a validation error — bypass retry entirely.

### Timeouts

Three different bounds apply at three different levels; set the one that
matches what you are bounding:

| Field | Where | Bounds |
|-------|-------|--------|
| `timeout_seconds` | On a `target` step, next to `target:` or `spec:` | That step's dispatch. The default `0` adds no step-level bound. |
| `wall_clock_timeout_seconds` | On an `agent_loop` activity spec | One CLI agent invocation. This is the only bound on an agent run; `max_iterations` is inert (see Retired agent fields). |
| `timeout_seconds` / `interval_seconds` | On a `loop:` block | The loop's total elapsed time and the pause between passes (see Loops). |
| `timeout_ms` | In a deterministic `local_shell` activity's `config` | One child process; see Deterministic local shell below. |

```yaml
- id: implement_one
  target: activity:agent_implement
  timeout_seconds: 21600
  default_input:
    task_id: "{{ item }}"
```

### Recovery hooks

Retries handle transient failure of the same step. Recovery hooks bring
in a different activity when a step has definitively failed. They are
declared by activity name and resolve against the catalog at load:

| Field | Where | Behavior |
|-------|-------|----------|
| `recovery_activity` | On a step, or on the job's `spec` | Runs once after the step's retries are exhausted (a step-level declaration overrides the job-level one for that step). It gets one attempt to repair the situation; the failed step's outcome stands if recovery does not succeed. |
| `final_recovery_activity` | On the job's `spec` | The last automated look at a failed run: invoked at most once per run, after step recovery is exhausted and before `failure_activity`. Its typed decision either resumes the run from an earlier step or settles the task without the failure hook. |
| `failure_activity` | On the job's `spec` | A best-effort terminal hook invoked once after a step failure. Unlike `recovery_activity`, it does not retry or replace the failed step — use it for notification and cleanup, not repair. |

```yaml
spec:
  failure_activity: notify_failure
  steps:
    - id: validate
      recovery_activity: step_failure_recovery
      target: activity:candidate_validate
```

Recovery is not a way to make a failing step pass silently: the original
failure, every retry and each recovery attempt are recorded in the run's
audit trail.

## Agent process policy

An `agent_loop` activity whose tools include `proc.spawn` must declare
which programs the spawned processes may run. The policy is enforced
before any process starts:

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

- `proc_allowed_programs` is an allowlist. An empty list denies every
  program — declaring `[]` is fail-closed, not permissive.
- `proc_disallowed_programs` instead selects deny mode: every program
  except the listed ones may run.
- Catalog load rejects an activity that grants `proc.spawn` without
  either policy, and a dispatch that somehow reaches execution without
  one is treated as deny-all.

Being allowed to spawn a program never replaces the tool allowlist,
filesystem profile or sandbox checks that also apply to the run.

### Completion contracts

Two activity-level flags say what a finished CLI invocation must prove:

- `require_completion_envelope` (default `true`) requires the invocation
  to terminate with a well-formed Orbit response envelope. An agent that
  exits 0 without one stopped mid-turn, so the step fails rather than
  checkpointing work that never happened. The check reads only the
  envelope frame — an agent that reports `status: "failed"` inside a
  well-formed envelope still satisfies it. Set it to `false` only for a
  genuinely decorative activity, and record why in the asset.
- `require_response_envelope` (default `false`) additionally requires a
  fully valid success envelope before the step may report success. Use
  it for activities whose result fields feed downstream templates; it
  implies the completion check above.

`on_denial` controls what happens when the agent requests a tool its
policy denies: `terminate` (the default) ends the invocation, while
`continue` returns the denial to the agent as a structured tool error
and lets it proceed.

## Deterministic local shell

The `local_shell` deterministic action runs one local command as a job
step, through the same process supervision as an agent CLI run. It is
not an agent: it receives no prompt, model or tool allowlist. Use it for
short, deterministic commands (`git status`, a build check) whose output
a later step consumes.

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

The safety contract is the point of the action:

- **The program comes from `config` only.** A step's templated input can
  choose the checkout (`workspace_path` in the step input, defaulting to
  the registered repository root) but can never reach argv, so agent-
  produced text cannot become a command.
- **Argv and shell are different declarations.** `command` + `args` is a
  direct exec with no shell anywhere — nothing is word-split, globbed or
  expanded. To run a script, name the interpreter and the script
  explicitly; Orbit invokes `<shell> -c <script>` and never joins `args`
  into a command line. Setting both `command` and `shell`, or `args`
  alongside `shell`, is rejected.

  ```yaml
  config:
    shell: /bin/sh
    script: |
      set -eu
      make ci-fast
  ```

- `cwd` resolves relative to the workspace root and must stay inside it;
  a `cwd` that escapes the checkout fails the step.
- `env` entries layer over the executor definition's `env`, which layers
  over the same credential-free environment validation commands run
  with — nothing ambient is inherited.
- `timeout_ms` defaults to 600000 (10 minutes) and must be between 1 and
  3600000 (1 hour); the executor definition's `timeout_seconds` is the
  fallback when the activity sets neither. A step that legitimately needs
  longer belongs in its own job.
- With `allow_nonzero_exit: false` (the default), a nonzero exit or a
  timeout fails the step with the program, working directory and status
  in the message. Set it to `true` when a later step should branch on the
  exit code instead.

The step's output is `{ success, exit_code, stdout, stderr, duration_ms,
timed_out, timeout_ms, argv, cwd, sandbox }`. The shipped `local-shell`
executor definition declares no OS sandbox, so steps run under the
process supervisor confined by the `cwd` check above, and each step
reports the backend it actually ran under in `sandbox` (`"none"` for the
shipped definition). Add a `sandbox` to the executor definition and an
`fsProfile` to the activity to confine shell steps at the kernel level.
