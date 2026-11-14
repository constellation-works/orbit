---
title: Activity and job YAML
description: "Reference shapes for schemaVersion 2 activity and job assets, and schemaVersion 3 jobs with auto-task steps."
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
Filing refuses an unliftable covered requirement up front, using the same
rule admission uses: a mutating tool, an always-operator-only tool, or a
name a wildcard entry still covers. A read/repeat-safe exact denial can
still be required, because a grant could lift it. Task-pilot reports a
stored requirement that would fail admission as a blocking validation
finding before dispatch. On `agent_implement`, and on any activity outside
the shipped non-implementer set, a covered requirement refuses the run
before launch. `final_recovery`,
`step_failure_recovery`, `pr_conflict_recovery`, `agent_review_repair`,
`review_reconciliation_review` and `task_pilot` drop that
requirement from the requested list and record a note; it stays out of the
callable set. The shipped
`image_input_probe`, `resident_cycle` and `triage_escalation` activities use
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

The optional `steering_instruction` field adds guidance only when mid-run
steering is enabled.
See [Agent execution](../../concepts/agents/#runtime-path)
for the corresponding workspace settings.

### Retired agent fields

Agent execution uses the CLI path only. `backend: cli` still parses and is
ignored; `backend: http` and `backend: auto` fail catalog load, and
`orbit doctor --fix-retired-activity-backends` removes them. See
[Retired backend selection](../config/#retired-backend-selection).
`max_iterations` is accepted and has no effect: only
`wall_clock_timeout_seconds` bounds an invocation.

Other removed keys fail the same way at load — for example
`trustedHostExecution: true`, whose error directs you to use a task
workflow instead. Where the stale file is a workspace-local copy under
`.orbit/resources/activities/` (including a copy of a bundled activity a
release retired), that one file is excluded from the catalog with a
warning naming its path and the remedy, so jobs that do not reference it
still run; a job that does reference it fails with the same named error.
`orbit doctor` reports the file with its path and the fix-or-remove
remedy. Fix the definition or remove the stale file; a removed activity
or key will not return.

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

### Custom task-delivery jobs

A workspace or plugin job opts into task delivery with `spec.task_delivery`.
While a run is live, that declaration holds the delivery slot of every task
in its `input.task_ids`; `orbit run ship` refuses to dispatch those tasks
again. Ordinary jobs without the declaration do not hold delivery slots.

Add these fields to your job envelope, alongside your delivery steps:

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

This is an illustrative declaration fragment, not a complete production
delivery pipeline. You must supply the steps and their input wiring for
implementation, validation, review, commit, publication or local delivery,
and recovery as appropriate. These fields grant neither permission to
dispatch work nor authority to skip review or merge a task.

`task_delivery.modes` lists the ship modes (`pr`, `local`) for which a task
may select the job. Declare only modes your pipeline implements. A job can
hold slots without being selectable through a task tag:

```yaml
spec:
  task_delivery:
    modes: []
```

`task_delivery: {}` has the same empty-modes meaning. Coordinators and
claim-bound delivery leaves use this distinction; an empty list does not
mean every mode.

To select `custom_delivery`, give the task one `delivery:custom_delivery`
tag. The named job must be available in the active workspace/plugin catalog,
declare `kind: workflow` (not `subroutine`), and include the requested ship
mode in `task_delivery.modes`. An installed but disabled or otherwise inactive
plugin does not provide an available route. Without a delivery tag, shipping
uses `task_pr_pipeline` or `task_local_pipeline` for the selected mode.

A malformed tag, multiple different delivery selections, an unavailable
job or a mode mismatch refuses shipping; Orbit does not fall back to the
default pipeline. All tasks delivered together must select the same route:
mixing a tagged task with an untagged task also conflicts. Ship those tasks
separately. The one cross-mode exception is
`delivery:task_local_pipeline` during PR shipping: it delivers that task
locally without a forge. See [Delivery workflows](../../getting-started/workflows/#ship-one-task).
That exception does not make an arbitrary local-only custom job selectable
in PR mode.

Keep singleton commit constraints when designing the steps. A job containing
`git_commit` with `scope: all` (also the default when `scope` is omitted)
accepts only one distinct task per run; admission refuses a larger bundle
before delivery effects. This applies to nested and inline actions too.
`orbit run ship` dispatches selections as singletons. Bundled local delivery
requires a custom pipeline with explicit `scope: per_task` and compatible
per-task validation, no-diff handling and recovery; the shipped local pipeline
is a singleton leaf.

Set `owns_task_worktree: true` only when the job creates and owns a task-scoped
worktree. It opts the job into guarded cleanup after successful delivery.
Collection still checks task settlement, terminal runs, worktree registration
and Git cleanliness, and protects paths shared with live runs. The flag does
not unconditionally delete a worktree or replace those checks; it defaults to
`false` and is independent of `task_delivery`.

When [mid-run steering](../config/#settable-keys) is enabled with
`workflow.mid_run_steering.enabled`, wire a promotion gate into your custom
job before committing the still-uncommitted candidate:

```yaml
spec:
  steering_gate:
    before: commit
    round: implement_bundle
    candidate: resume_candidate  # Optional: only if this job carries candidates.
```

These values name your job's top-level step IDs. `round` must precede
`before`; use the step that dispatches the implementer again with steering.
If present, `candidate` must precede `round`. It names the step whose output
describes a carried candidate: when that output has `implement: false`, it
must provide a non-empty `source_run_id` so the gate uses the source run's
steering watermark. Omit `candidate` when the job does not carry candidates
past its implementer.

With steering enabled, an operator comment newer than the delivered watermark
prevents `before` from starting and reruns from `round`, even if that step's
`when` is false. `workflow.mid_run_steering.max_rounds` bounds those rounds;
exhausted rounds or a round that delivers none of the outstanding comments
hold delivery as `steering_unconsumed`. An unreadable gate also stops before
promotion. A done or archived task stops the steering gate before the
`before` step only when outstanding operator comments remain; a terminal
task with none proceeds. Without `steering_gate`, or with
steering disabled, this promotion check does not run.

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

The shipped escalation triage job publishes its triagers' errors this way
(`errors: triage_errors`), so one failed partition does not hide its
siblings' results.

## Fan-out and fan-in

Use fan-out when one step's output is a list and the same work must run
once per item, with bounded concurrency. A fan-out step always pairs
`fan_out` with `fan_in` on the same step:

```yaml
- id: triagers
  when: "{{ steps.prepare.output.partition_count }} != 0"
  fan_out:
    items: "{{ steps.prepare.output.partitions }}"
    max_workers: 5
    worker:
      id: triager
      target: activity:triage_escalation
      default_input:
        task_ids: "{{ item.task_ids }}"
        partition_index: "{{ item.partition_index }}"
        workspace_path: "{{ steps.prepare.output.workspace_path }}"
  fan_in:
    join: { mode: any }
    collect: triage_results
    errors: triage_errors
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
  `{{ steps.triage_results.output }}`; the outputs are always recorded
  under the fan-out step's own `id` too. `fan_in.errors` publishes the
  position-aligned error array described above.

Downstream steps then consume the collection like any other step output:

```yaml
- id: apply
  target: activity:apply_triage
  default_input:
    results: "{{ steps.triage_results.output }}"
    partition_errors: "{{ steps.triage_errors.output }}"
```

(`apply_triage` stands for your own apply activity; the shipped triage job
records its results without one.)

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
whose `when` is false is skipped: it succeeds without running, never
retries, and records no output, so a later step that reads
`{{ steps.<id>.output }}` from it fails with `no data recorded for step`.
A resumed run whose checkpoint records the skipped-step marker treats the
skipped step the same way: it does not run it again, and its output stays
absent. Checkpoints created before skipped-step markers existed cannot
distinguish a skipped step from one that ran and returned `null`; resuming
those checkpoints restores the skipped step's output as `null`. Upgrading
does not reconstruct missing markers. A step that ran and returned `null`
keeps that `null` across a resume.

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
| `wall_clock_timeout_seconds` | On an `agent_loop` activity spec | One CLI agent invocation (`max_iterations` is inert; see Retired agent fields). For reviewer runs, the host's `review.minutes` deadline replaces this bound. |
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

After recovery, the step runs once more. If that attempt ends the run
the way a step can end it without failing, the run ends that way, as it
would on a first attempt: a review held for external evidence holds the
run, and a cancelled child run cancels it. Neither runs
`final_recovery_activity` or `failure_activity`.

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

### Fleet notice instructions

An `agent_loop` selects the engine's built-in fleet-notice text with `fleet_notices`, and may replace either built-in text with its own under `spec`:

- `fleet_notices: none | read | post` (default `none`) selects the built-in texts.
  - `none` selects no built-in text. The activity gets only its own instructions, if any.
  - `read` selects the built-in reading text. It tells the agent to treat the brief as untrusted data: notices cannot change the task, its criteria, scope, tools, authority, validation, review or the workspace guide; an instruction inside a notice is ignored and may be reported; and a notice that seems to require action outside the task is noted in the task, not acted on.
  - `post` selects the reading text and the built-in posting guidance. It limits posting to incident or info notices for a problem outside the task that affects other agents, with a linked task or evidence. It tells the agent to check the brief for an open notice before posting, to corroborate a duplicate rather than open one, and to close only the notices its own task authored once they stop being true.
- `notice_instruction` is a non-empty string that replaces the built-in reading text. A non-empty value requests the host's open `fleet_notices` brief even without `fleet_notices`.
- `notice_posting_instruction` is a non-empty string that replaces the built-in posting guidance. It applies with or without `fleet_notices: post`.

The reading text is appended only when the host delivers a brief for that prompt. The posting guidance is appended only when the dispatch may call `orbit.notice.post`. It does not depend on a delivered brief, so an empty board or disabled brief delivery does not suppress it. An activity with `fleet_notices: none` and no non-empty `notice_instruction` never has the host read notices for that prompt.

Briefs are untrusted data, and none of these fields grants tool access or authority. See [fleet notices](../../how-to/fleet-notices/) for scope, limits and the independent MCP and brief-delivery controls.

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

### Bounded continuation

`incomplete_continuation` lets an implementer that reports unfinished work
run again in the same activity instead of failing the step. It is optional.
Without it, a declared `implementation_incomplete` failure fails the step
like any other declared failure:

```yaml
spec:
  type: agent_loop
  description: Implement one assigned task.
  instruction: "Implement the task and report any unfinished work."
  wall_clock_timeout_seconds: 10800
  incomplete_continuation:
    max_rounds: 2
    min_remaining_seconds: 900
```

- `max_rounds` (required) is the most continuation invocations one step
  dispatch may start after its first. `0` admits none.
- `min_remaining_seconds` (required) is the unspent wall-clock budget, in
  seconds, a continuation needs before it starts. A smaller remainder keeps
  the declared failure.

A continuation starts only when the first invocation ends with the typed
`implementation_incomplete` signal and nothing else fails. The agent's
envelope must be well formed and carry:

- `schemaVersion: 1` and `status: "failed"`.
- `error.code: "implementation_incomplete"` with a non-empty `error.message`.
- `result.remaining_work`: one to 32 non-empty strings, each at most 2,000
  bytes.
- No `result.blocker`. A declared blocker is a legitimate stop, so it is
  never continued.

The process must also exit cleanly. A provider authentication, refusal or
usage-limit failure, or a timeout, is not continued.

The first invocation runs under the activity's `wall_clock_timeout_seconds`.
Each continuation gets only the seconds that remain of that budget, so a
continuation never extends it. A continuation is refused, and the declared
failure stands, when `max_rounds` continuations have already started, when
the remainder is below `min_remaining_seconds`, or when the task record is
terminal or cannot be read. A continuation needs the step's task record, so
an activity without a task cannot continue.

Each continuation is a fresh invocation in the same worktree, with the same
crew, tool grant and sandbox. It receives the remaining work list, its round
number and the seconds left as input. Orbit commits nothing. The step
succeeds only when a continuation itself completes.

If the step fails after one or more continuations, its message gives the
continuation count, the refusal reason when the bounds stopped a further
continuation (`rounds_exhausted`, `wall_clock_exhausted`, `task_terminal` or
`task_unavailable`), and the original error. The worktree keeps the partial
candidate. The shipped `agent_implement` activity declares
this policy: up to two continuations, each started only while 15 minutes of
its three-hour budget remain.

### Response correction

`response_correction` gives a read-only activity one fresh invocation when
the response parser rejects its reply. It answers a different failure from
`incomplete_continuation`. That policy acts on a well-formed declaration of
unfinished work. This one acts on a reply the parser does not accept, and a
rejected reply counts for nothing until a new one parses. It is optional.
Without it, a rejected reply fails the step.

```yaml
spec:
  type: agent_loop
  description: Review one named task without changing it.
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

- `max_rounds` (required) is the most correction invocations one step
  dispatch may start after its first. `0` admits none.
- `min_remaining_seconds` (required) is the unspent wall-clock budget, in
  seconds, a correction needs before it starts. A smaller remainder keeps
  the rejected reply's failure.

A correction starts only when the rejection is the step's only failure. The
rejection is one of:

- `contradictory_reply`: the envelope says `status: "success"` and also
  carries an `error` object.
- `missing_envelope`: the turn ended in prose with no envelope, and the
  output was not truncated.
- `structured_output_retry_exhausted`: Claude's structured-output retries ran
  out before it produced a valid result.

A reply is not corrected when its process failed for another reason: a
timeout, a provider authentication, refusal or usage-limit failure, or an
envelope that says `failed`. Those fail the step as they would without the
policy. The rejected reply goes back to the correction as data, bounded and
redacted, with its rejection kind and the parser's error. The step succeeds
only if the correction's own reply parses.

A correction is another run of the whole activity, so any write it made
would repeat. Asset load therefore refuses the policy unless all of these
hold:

- `require_response_envelope: true`, so something reads the reply.
- `fsProfile: reviewer`, the read-only [filesystem profile](../policy-format/).
- The activity's own `tools` allowlist grants no built-in mutating Orbit tool:
  a mutating tool is refused when it is listed there, including through a
  wildcard entry. A `tool_disallow_list` is admitted whatever it names. Dispatch
  denies every built-in mutating tool to each invocation, so a tool a later
  release adds cannot make an existing copy invalid.
  Mutating tools include `orbit.task.update`, `orbit.task.artifact` action `put`,
  `orbit.friction` action `add` and `orbit.command.exec`. See
  [agent tool lists](#agent-tool-lists).
- The activity does not also declare `incomplete_continuation`. Declare at
  most one of the two policies.
- The retired `trustedHostExecution` key is not set. Asset load refuses the
  combination.

Corrections share the first invocation's `wall_clock_timeout_seconds` with
the rest of the step. Each correction gets only the remainder. A correction
is refused when `max_rounds` corrections have started or the remainder is
below `min_remaining_seconds`. A correction keeps the same crew, tool grant,
sandbox and worktree as the first invocation.

If the step fails after one or more corrections, its message gives the
correction count, the refusal reason when the bounds stopped a further
correction (`rounds_exhausted` or `wall_clock_exhausted`), and the first
rejection's error. The shipped `task_pilot` activity declares one correction
with a 30-second minimum remainder, inside a 30-minute budget.

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

The step's output is `{ success, exit_code, stdout, stderr, log_tail,
log_tail_truncated, duration_ms, timed_out, timeout_ms, argv, cwd, sandbox }`. The shipped `local-shell`
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

**Running a script step.** At dispatch Orbit re-reads the
manifest and entry from the run's worktree (or the workspace root, then
read-only), snapshots them, and checks the grant before anything is
spawned. A missing or expired grant (`grant_missing`), a changed script
(`grant_stale`, with both digests), a request beyond the grant
(`grant_insufficient`), a moved interpreter (`interpreter_changed`), a
host without the sandbox or a start inside another sandbox
(`script_sandbox_unavailable`), an unusable worktree or root
(`script_root_unavailable`) and a pull follower without a `--follower`
grant (`script_follower_refused`) each fail the step without retry.

On Linux, the script runs under the `linux-bwrap-script-v1` Bubblewrap profile:
it sees the host runtime (`/usr`, the loader, resolver and CA files),
its interpreter, the snapshot, its cwd (a run worktree read-write, the
workspace root read-only), the cwd's Git metadata read-only, its
granted roots and a scratch directory at `/orbit-script` (`HOME`,
`TMPDIR`, `in/`, `out/`). `.orbit/`, the global Orbit root and the
credential paths under `HOME` are never visible.

On macOS, `macos-sandbox-script-v1` uses Seatbelt with the same granted
paths, but sees the scratch directory at its host path. `HOME` and `TMPDIR`
point to its `home/` and `tmp/` directories; `ORBIT_SCRIPT_IN` and
`ORBIT_SCRIPT_OUT` point to its `in/` and `out/` directories. macOS permits
only `network: none`; a network grant is refused before spawn with
`script_sandbox_unavailable`. A nested Seatbelt sandbox cannot apply the
profile and is also refused. With `machine.worker_containment = "strict"`,
macOS refuses every script step because it cannot provide memory or task
limits; `warn` or `off` allows the profile alone. The macOS profile also
exposes file metadata outside granted paths, and a timed-out descendant that
calls `setsid` can outlive the step while remaining sandboxed.

Granted read and write roots are probed at grant time and again by the job
process before spawn; write roots must allow creating and removing a file.
On macOS, privacy permissions (TCC) apply in addition to the profile and may
differ between the granting terminal and the process running the job. A
permission refusal identifies the relevant System Settings privacy pane for
the job process.

On Linux, the grant's `network` decides what the script can reach. Under `none`
(the default) it has its own network namespace with only its own
loopback, so neither the network nor services listening on the host's
`127.0.0.1` are reachable. Under `https` it shares the host network, and
a Landlock ruleset admits outbound TCP to port 443 only (no other TCP
port and no listening socket); it needs Landlock ABI 4, and an older
kernel refuses the step with `script_sandbox_unavailable`. Under `any`
it shares the host network with no rule. A manifest whose `requests`
ask for more network, or for an `env_pass` name, than the grant allows
refuses before spawn with `grant_insufficient`.

The script's environment is cleared and set to fixed values (`PATH`,
`HOME`, `TMPDIR`, `LANG`, `TZ`), the `ORBIT_SCRIPT_*` step context, and
each variable the grant's `env_pass` names, copied from the environment
of the process running the job. No other host variable reaches it. A
granted name that process does not hold stays unset and is recorded as
`env_pass_unset`; an `ORBIT_*` name or one of the fixed names is never
passed. Each passed value is redacted (`[REDACTED]`) from the captured
log, the stdout that becomes the step output, the recorded argv, and so
from the `script_execution` entry, `orbit run show` and `orbit script
history`. Declared output files are kept byte for byte and are not
redacted.

**Input files.** A manifest declares its read-only inputs under
`spec.files.inputs`, a list of `{from, as}` entries:

```yaml
spec:
  files:
    inputs:
      - from: "{{ input.brief }}"
        as: brief.md
      - from: site/dist
        as: site
```

`from` is rendered against the step input. A relative path resolves against
the step's cwd (the run worktree, or the workspace root). The result must
resolve physically, with symlinks followed, to a regular file or a directory
inside the cwd tree or a read root granted with `orbit script grant <name>
--read-root <path>`. Each input is available read-only at `$ORBIT_SCRIPT_IN/<as>`: Linux binds it
at `/orbit-script/in/<as>`, while macOS copies it into the host scratch
directory before spawn. The macOS copy counts against the scratch size limit. `as` is a plain
file name: not empty, not `.` or `..`, and with no `/`. No two inputs in a
manifest may share one. A file's digest is of its bytes; a directory's is a
digest over its whole tree, without following symlinks inside it.

An input that is missing, resolves anywhere else, overlaps `.orbit/`, the
global Orbit root or a credential path under `HOME`, or is a directory holding
an entry that is neither a file, a directory nor a symlink (or more than
100,000 entries) refuses the step before spawn with
`script_file_input_unavailable`, naming the input. Input admission runs before
the host profile check, so this refusal can come before
`script_sandbox_unavailable`. Successfully admitted inputs are recorded with
their names, sources and SHA-256 digests on the attempt's `script_execution`
entry.

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

### Output files

A manifest can return files as well as JSON. It declares output caps under
`spec.files.outputs`:

```yaml
spec:
  files:
    outputs:
      # Both caps are optional. max_files: 1 to 64.
      # max_file_bytes: 1 to 67108864 (64 MiB).
      max_files: 4
      max_file_bytes: 262144
```

Setting either cap declares outputs. A cap you leave out takes the grant's
value, and a manifest that sets neither cap declares no outputs. A value
outside those ranges fails the manifest when it loads, with the manifest file
named.

The script writes its files directly inside `out/`, which is
`$ORBIT_SCRIPT_OUT` (`/orbit-script/out` on Linux, or the host scratch
directory's `out/` on macOS). Subdirectories are
not output.

The grant sets the most a manifest may ask for. A grant without the output
flags allows the defaults: 8 files, each at most 1048576 bytes (1 MiB). To
allow more, grant the script with the flags it needs:

```bash
orbit script grant package --output-max-files 4 --output-max-file-bytes 262144
```

`--output-max-files` accepts up to 64, and `--output-max-file-bytes` accepts up
to 67108864. A set's total is bounded by files times bytes and never exceeds
256 MiB. Each grant states its limits again, so a re-grant without the flags
returns to the defaults. A manifest that asks for more than its grant allows is
refused before anything runs, with `grant_insufficient`, and the message names
the flag to add. `orbit script show <name>` reports the requested and granted
output limits.

Output files are supported on Linux and macOS. Orbit keeps them only when the
attempt exits 0 and its stdout passes the JSON rules above. A nonzero exit, a signal, a timeout, a scratch breach or a manifest with
no output caps keeps nothing. For a manifest without caps, `out/` is discarded
with the scratch directory, and the step's output is its JSON alone.

When an accepted attempt's manifest declares outputs, Orbit checks the whole
`out/` directory. If any check fails, the step fails and keeps none of its
files:

- Every entry in `out/` must be a regular file directly inside it. A symlink, a
  directory or another special entry fails with `script_outputs_invalid`.
- A file name is 1 to 128 bytes of ASCII letters, digits, `.`, `_` and `-`, and
  does not start with `.`. Any other name fails with `script_outputs_invalid`.
- The set may hold at most `max_files` files, each at most `max_file_bytes`
  bytes, and a total no larger than files times bytes, capped at 256 MiB.
  Exceeding any of these fails with `script_outputs_over_cap`.
- An empty `out/` keeps an empty set.

Each kept file is copied byte for byte and recorded with its size, SHA-256 and
a media type from its extension. `.png` gives `image/png` and `.json` gives
`application/json`; an unknown extension gives `application/octet-stream`. The
set is published whole, so a later step sees all of its files or none. A later
step reads the set as `steps.<id>.files`, a list of entries with `name`,
`bytes`, `media_type` and `sha256`. If the set cannot be stored, the step fails
with `script_outputs_unavailable`.

#### Attach output files to a task

The shipped `attach_script_files` activity attaches a script step's kept files
to a task as task artifacts, so the task holds the exact bytes the script
produced. Run it from a later step of the same job:

```yaml
schemaVersion: 2
kind: Job
metadata:
  name: publish_report
spec:
  state: enabled
  max_active_runs: 1
  kind: workflow
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

The `render_report` manifest must declare `files.outputs`. Without it, the
attach step refuses because the producer kept no files. The activity's inputs
are:

- `task_id`, required: the task that receives the artifacts.
- `step`, required: the id of the script step that kept the files. It must be a
  direct, earlier step of the same job, and it must have succeeded. Loop,
  fan-out and parallel steps cannot be producers.
- `files`, optional: the file names to attach. Each name must be in the set.
  Without this list, every file of the set attaches.
- `path_prefix`, optional: text placed before each file name to form its
  artifact path. It is used as written, so include the trailing `/`.

The activity finds the set in its own run's checkpoint for `step`, never in its
input. A set counts only for the run that produced it or a run that resumes it.
Before attaching, Orbit checks that the stored producer record matches the set
and that every file still has its recorded size and SHA-256. Each artifact is
written from those verified bytes.

Artifacts follow the task artifact rules:

- Each file must be at most 1048576 bytes (1 MiB), the task artifact limit.
- The artifact path, including any `path_prefix`, must be a relative path with
  no `..` segments.
- Orbit's own writers keep some paths: `mutation-proposals/`,
  `mutation-receipts/`, and any name that starts with
  `review-` in any case. The one exception is `review-report.json`, which must
  parse as a review report.

The activity is atomic. Orbit reads and checks every selected file before it
writes anything, then writes all the artifacts in one task update. If any file
is refused, the step fails with the reason and attaches nothing. A re-run
writes its artifacts again at the same paths, replacing any with those names.

The step's output carries `task_id`, `step`, `set_id` and `attached`, which has
one entry per artifact with `name`, `path`, `bytes`, `media_type` and `sha256`.
For how artifacts are stored and read, see
[Task artifacts](../../concepts/tasks/#task-artifacts). For the `orbit script`
commands, see the [CLI reference](../cli/).

## Auto-task steps

A step may name an auto-task definition: `target: auto-task:<name>`. Such
a step binds one task from that definition and waits for its accepted
completion, instead of running an activity.

A job that uses an `auto-task:` target, a wait key or `spec.expire_when`,
at any nesting, declares `schemaVersion: 3`. Under `schemaVersion: 2` any
of those keys fails to load, even with a null value or in a nested step. A
`schemaVersion: 3` job keeps that version even after its last auto-task
field is removed, until you declare `schemaVersion: 2` again. An Orbit release
older than this support refuses a `schemaVersion: 3` job as an unsupported
version, rather than loading it without its waits.

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

Orbit's source carries a fuller, disabled `release-readiness` example that
runs `qa-full-sweep`, `security-review` and `release-prep` as one gated run,
each step requiring `release-readiness/v1`. It is not seeded into workspaces:
install it only once those definitions declare the contract and their adapter
scripts are granted.

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
| `reuse_when` | CEL predicate over the named auto-task's latest accepted `result`; true lets the step settle on it, after Orbit checks it, instead of minting. |
| `require_result` | Typed result contract; `release-readiness/v1` is the only registered value, and it must match the auto-task definition's `completion.contract`. See the [evidence guide](../scheduling-yaml/#completion-evidence) for manifest and adapter examples. |
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
It writes nothing. `orbit job show` and the explain report both include
the job's `schema_version`.

### How an auto-task job runs

An enabled job with auto-task steps runs only as a detached submission
(`orbit run job <name>` or a routine), never as a foreground run such as
`orbit job run <path>`. Each execution freezes the definition it runs and
its admission time, so editing the job or a named auto-task affects later
executions only. The deadlines count from admission, including time spent
queued or with the host down.

When the job reaches an auto-task step, the step:

1. Settles first on its bound task's outcome: completed is `done`;
   archived, rejected or deleted ends the step without a new task. A
   completion recorded after the deadline does not count.
2. Ends as `timeout` once its own or the job's `expire_when` holds.
3. Otherwise waits while `wait_until` is false or unknown, or while the
   named auto-task is missing, disabled, or its own condition or schedule
   guard is false. A definition whose condition is task, host or file
   scoped holds. So does a step with `require_result` unless the definition
   declares the same contract under `completion` and its adapter script is
   granted.
4. When ready, first tries `reuse_when` against the named auto-task's latest
   accepted result. When it is true and Orbit confirms that result covers
   this execution's pinned revision, is within the contract's maximum age,
   and still passes the evidence and adapter checks, the step is `done` on
   that result without a new task. A result that fails those checks does not
   count, whatever the predicate says. With no accepted result the step goes
   on to bind or mint. When the accepted-result history can't be read, or the
   predicate is unknown, the step waits instead of minting.
5. Otherwise binds an open instance under `skip_if_open` dedupe, or mints
   one task. The mint uses no schedule slot and records which run and step
   it was for, so a retry after a lost reply finds the same task. A step
   mints at most one task per execution. With `require_result`, a finished task counts only once Orbit has checked
   its evidence against its own records and the adapter answers `passed`.
   Earlier results, reused ones included, are checked again for exact source
   and age before a later step mints.

While a step waits, the run stays `running` and counts against
`max_active_runs`, but no worker, provider, worktree or drain slot is held:
the run is parked, `orbit run show` reports it running, and the scheduler's
sweep resumes it within about a minute. A parallel block keeps its
branches that already succeeded and parks while any branch waits. Its join
mode decides the outcome, as before. A minted task starts, from a local
drain or a follower's claim, only while its step still admits it: an
ended run, an expired step or a guard that no longer holds keeps it in the
backlog.

Opening a workspace with this release moves it to workspace layout
version 9. Version 7 records parked runs and step bindings that an earlier
release would drop; version 8 adds the kept script output files and parallel
script-branch checkpoints (see [Output files](#output-files)); version 9 adds
the skipped-step checkpoint markers (see `when` under
[Modifiers](#modifiers)). Earlier Orbit
releases then refuse to open the workspace until
they are upgraded, and while an earlier release still holds it, for example
as a running MCP server, this release waits for that client to exit rather
than proceeding alongside it.
