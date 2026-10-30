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

An operator's task grant is a narrow exception for `agent_implement`: it can
lift an exact deny entry for a read/repeat-safe tool, but a mutating grant
remains denied and goes through a reviewed proposal instead. Grants do not
override wildcard denials or the tool's other authorization checks. See
[Task tool grants](../../concepts/tasks/#task-tool-grants) for the change
window, placement and proposal workflow.

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

Reference a workspace script (see [Script steps](#script-steps)):

```yaml
- id: package
  target: script:package
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
| `wall_clock_timeout_seconds` | On an `agent_loop` activity spec | One CLI agent invocation (`max_iterations` is inert; see Retired agent fields). For reviewer runs, the host's `review.minutes` deadline replaces this bound; a trusted-host or explorer run input `timeout_seconds` can also shorten it. |
| `timeout_seconds` / `interval_seconds` | On a `loop:` block | The loop's total elapsed time and the pause between passes (see Loops). |
| `timeout_ms` | In a deterministic `local_shell` activity's `config` | One child process; see Deterministic local shell below. |

Agent steps are bounded by `wall_clock_timeout_seconds` on the referenced
activity spec, not by step-level properties. The executor does not read
`timeout_seconds` on a `target` or inline `spec` step as a dispatch deadline:

```yaml
- id: implement_one
  target: activity:agent_implement
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
| `final_recovery_activity` | On the job's `spec` | The last automated look at a failed run: invoked at most once per run, after step recovery is exhausted and before `failure_activity`. It returns one typed decision: `resume` (reruns the run from an earlier step), a settled decision (`complete_no_diff`, `reject`, `archive`, or `requeue`, which does not run `failure_activity`), or `escalate` (which, like a missing or malformed result, still runs `failure_activity`). |
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

- **The program comes from static declarations, never step input.** The
  activity's `config` may declare `command` + `args` or `shell` + `script`,
  or neither: when the activity names no program, the selected executor
  definition's `command` is the fallback program and its `args` are the
  argv. For an explicit `command`, the definition's static `args` prefix
  the activity's `args`; `shell` + `script` runs exactly
  `<shell> -c <script>`. A step's templated input can
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

## Script steps

A step may instead name a workspace script: `target: script:<name>`. The
name resolves to a manifest at `.orbit/resources/scripts/<name>.yaml`
(workspace-only; the global root holds no scripts):

```yaml
schemaVersion: 1
kind: Script
metadata:
  name: package
spec:
  description: Package the built site into a tarball and report its digest.
  entry: tools/package.sh            # workspace-relative; resolved physically
  interpreter: [bash]                  # optional; or the entry's `#!` line
  args: ["--out", "out/", "--label={{ input.label }}"]
  requests:                            # asks only; the grant decides
    network: none
    timeout_seconds: 300
```

The manifest names code in a file, never inline code:

- `entry` must resolve physically (after symlinks) to a regular file
  beneath the workspace root, outside `.git/` and `.orbit/state/`. A
  manifest that is missing, whose entry escapes, or whose interpreter
  carries an inline-code flag (`-c` for `sh`, `bash`, `zsh`, `dash` and
  `python*`; `-e` for `perl`, `ruby` and `node`; `-E` for `perl`; `-p`,
  `--eval` and `--print` for `node`), including bundled spellings such as
  `bash -ec`, fails to load, with the
  manifest file named. Unknown fields are refused, so a `command:` or
  `script:` text field cannot be added.
- `interpreter:` is a launcher argv (`[bash]`, `[uv, run, --offline,
  --script]`), or comes from the entry's `#!` line in the honored forms
  `#!/abs/path [one-arg]` and `#!/usr/bin/env <name>`; neither means a
  native executable. Interpreter arguments are literal.
- Each `args:` element is exactly one argv element; `{{ … }}` templates
  render against the step input and are never split or globbed.

A script runs only under an operator grant bound to the digests of the
manifest and the entry file. `orbit script grant <name>` records it
(host-local, witnessed outside the database; agent sessions cannot
grant or revoke), `orbit script list` and `orbit script show <name>`
report each script as `granted`, `stale` (either file changed since the
grant, with both digests), `ungranted` or `orphaned`, and `orbit doctor`
carries the same findings. `orbit script revoke <name>` withdraws a
grant.

Routines never name a script directly — wrap it in a one-step job and
target `job:<name>` — and `type: shell` remains a load error. An older
Orbit binary meeting `target: script:` reports a load error and runs
nothing.

**Running a script step (Linux).** At dispatch Orbit re-reads the
manifest and entry from the run's worktree (or the workspace root, then
read-only), snapshots them, and checks the grant before anything is
spawned. A missing or expired grant (`grant_missing`), a changed script
(`grant_stale`, with both digests), a request beyond the grant
(`grant_insufficient`), a moved interpreter (`interpreter_changed`), a
host without the sandbox or a start inside another sandbox
(`script_sandbox_unavailable`), an unusable worktree or root
(`script_root_unavailable`) and a pull follower without a `--follower`
grant (`script_follower_refused`) each fail the step without retry.

The script runs under the `linux-bwrap-script-v1` Bubblewrap profile:
it sees the host runtime (`/usr`, the loader, resolver and CA files),
its interpreter, the snapshot, its cwd (a run worktree read-write, the
workspace root read-only), the cwd's Git metadata read-only, its
granted roots and a scratch directory at `/orbit-script` (`HOME`,
`TMPDIR`, `in/`, `out/`). `.orbit/`, the global Orbit root and the
credential paths under `HOME` are never visible. It has its own network
namespace with only its own loopback, so neither the network nor
services listening on the host's `127.0.0.1` are reachable. `network:
https|any`, `env_pass` and `files.inputs` are refused for now.

The step's rendered input is validated against `input_schema_json` and
written to stdin. On exit 0, stdout (at most 1 MiB) must be one JSON
document matching `output_schema_json` and becomes the step's output;
empty stdout with no schema gives `{}`, and anything else fails the step
without retry (`script_output_invalid`). A nonzero exit, a signal, a
timeout (`script_timeout`: `SIGTERM`, then `SIGKILL` after 10 s) or a
scratch breach is an ordinary failed attempt with `{exit_code, signal,
timed_out}` as output, retried per the step's `retry`. Every attempt,
refusals included, is listed by `orbit run show` and `orbit script
history <name>` with the script's digest, grant, argv, exit, duration
and its capped, redacted log.

## Auto-task steps (defined and explained, not run)

A step may name an auto-task definition: `target: auto-task:<name>`. Such
a step binds one task from that definition and waits for its accepted
completion, instead of running an activity. This Orbit loads, checks and
explains these steps, but cannot run them yet: a job with one must keep
`state: disabled`, `enabled` fails to load, and submitting it is refused
before any run is recorded or task minted.

```yaml
spec:
  state: disabled
  expire_when: "now >= job.started_at + duration('24h')"
  steps:
    - id: qa
      target: auto-task:full-qa-sweep
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

An auto-task step admits only `id`, `stage`, `when`, `target`,
`wait_until`, `expire_when`, `reuse_when` and `require_result`; any other
key, including `retry`, `recovery_activity`, `default_input` and a nonzero
`timeout_seconds`, is refused rather than ignored. The four wait keys are
refused on any other step, and `spec.expire_when` on a job without an
auto-task step.

| Field | Meaning |
|---|---|
| `when` | Unchanged: evaluated once, false skips the step. A skipped step is `skipped`, never accepted evidence, so use `wait_until` for prerequisites. |
| `wait_until` | CEL readiness predicate: false or unknown waits, true is ready. Absent means ready when the job reaches the step. |
| `expire_when` | CEL predicate: true ends the unfinished step as `timeout`. |
| `reuse_when` | CEL predicate over a candidate `result` that lets the step reuse accepted evidence. |
| `require_result` | Typed result contract; `release-readiness/v1` is the one registered. |
| `spec.expire_when` | Required on a job with auto-task steps: `now >= job.started_at + duration('<d>')` or `now >= timestamp('<RFC3339>')`, positive and at most 168h after admission. |

The CEL fields use the condition-trigger language with its 4 KiB, 200-node
and nesting limits (see [Condition triggers](../scheduling-yaml/#condition-triggers)),
over a typed job environment:

- `now`, `job.started_at`, and `job.source_revision`. The revision is
  pinned at admission when any step requires `release-readiness/v1`;
  otherwise it may be absent and needs `has()`.
- `self.wait_started_at`, `self.status`, optional `self.task_id`, and
  `self.prev` (`status`, optional `end_time`). `self.prev` is the previous
  sibling in a sequential list. It is always present on a later step and
  never on a first step or a parallel branch, where reading it is refused.
- `steps['<id>']` (`status`, optional `end_time` and `task_id`), with a
  literal ID naming an earlier step in the same list or an enclosing one,
  or a branch of an earlier parallel block. Forward, self, enclosing,
  sibling-branch, loop/fan-out and computed references are refused.
- `board` with `empty` (no backlog, in-progress or review task) beside the
  condition `board` fields, and `tasks['<ID>']`.
- `result` in `reuse_when` only: `status`, `revision` (required under
  `release-readiness/v1`, else optional), `completed_at`, `contract`,
  `definition_digest`, `task_id`, optional `run_id`.

A step status is one of `waiting`, `queued`, `running`, `done`, `held`,
`failed`, `timeout`, `cancelled` and `skipped`; a misspelled literal fails
to load. An auto-task step inside a `loop` or `fan_out` is refused, and so
is a job with more than 32 of them. Every refusal names the job file and
the field (`spec.steps.<id>.wait_until`), and for an expression its
line:column.

`orbit automation explain job:<name>` shows the checked steps: the outer
bound, whether the revision is pinned, and per step its target auto-task,
resolved now with its file digest, enabled state and own condition guard
(or reported `missing`), its guards, waits and the earlier steps it reads.
It writes nothing. An older Orbit binary shows such a job without the wait
fields and will not run it while it is disabled. An enabled copy fails
target resolution in the worker before its first step.
