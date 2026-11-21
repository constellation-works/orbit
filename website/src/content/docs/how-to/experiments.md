---
title: Run an experiment
description: "Compare agent treatments with orbit experiment: define arms, enroll tasks, read the verdict and conclude."
---

Run an experiment when you want evidence before you make a treatment the
default: a different instruction file, a replaced skill, a changed tool grant
or a different crew pool. You define one control arm and one or more treatment
arms. Orbit assigns each enrolled task to an arm, and a readout compares the
arms on one primary metric, with guardrails.

Only an operator can create or change an experiment; an agent session is
refused. `show`, `list` and `readout` are read-only and open to any caller.
The same operations exist as `orbit.experiment.*` MCP tools, except `delete`,
which is CLI only.

## Before you start

- **Enable experiments.** A new install turns them on in the global config.
  Check with `orbit config get experiments.enabled`, and set it if needed:

  ```bash
  orbit config set experiments.enabled true
  ```

  While it is `false` you can still define and edit drafts, but `start` and
  forced assignment are refused.
- **Leave room.** At most 3 experiments run at once in a workspace. A paused
  experiment does not count.
- **Have the treatment ready**, in either mode. Instruction files and skill
  bundle directories must exist in the repository at the base revision, and
  added tools must be registered, enabled and agent-facing. Skill replacement
  supports Claude on Unix only, so a live skill overlay needs a Claude-only
  implementation crew pool.

## Define the experiment

This example compares the shipped instructions (control) with a treatment that
gives implementers a different instruction file:

```bash
orbit experiment create brief-v2 \
  --hypothesis "A shorter implement brief cuts tokens without lowering first-pass acceptance." \
  --arms '[
    {"name": "control", "control": true, "weight": 1, "overlay": {}},
    {"name": "brief-v2", "control": false, "weight": 1,
     "overlay": {"instructions": {"implement": "docs/brief-v2.md"}}}
  ]' \
  --population '{"complexity": ["medium", "hard"], "task_type": ["feature", "bug"]}' \
  --primary-metric total_tokens \
  --guardrails '{"delivery_done_rate": "-5pt", "escaped_regressions": "+0"}' \
  --min-sample 20 \
  --max-days 28 \
  --mode shadow
```

`create` makes a draft, which assigns nothing. Experiment and arm names are
unique in the workspace: 1–64 letters, digits, `-`, `_` or `.`, starting with
a letter or digit. An experiment name may not start with `exp-`.

In every command below, `<id>` is the experiment's ID or its name.

To edit a draft, pass only the fields you change. A given `--arms` list
replaces every arm:

```bash
orbit experiment update <id> --min-sample 30
```

Starting an experiment freezes its definition. To change a started
experiment, clone it into a new draft:

```bash
orbit experiment clone brief-v2 --name brief-v3
```

The clone copies the definition, with the arms' current weights and without
retired arms. The source is unchanged.

### Population

`--population` is a JSON object. Omit a field to place no restriction on it;
omit the object to enroll every task. Unknown fields are refused.

| Field | Matches |
|---|---|
| `complexity` | Any of `low`, `medium`, `hard`, `xhard`. |
| `task_type` | The task's type, such as `feature` or `bug`. |
| `tags_include` | Every listed tag must be on the task. |
| `tags_exclude` | No listed tag may be on the task. |
| `origin` | The task's origin. |

### Arms and overlays

Each arm is `{name, control, weight, overlay}`. `control` defaults to `false`,
`weight` to `1` and `overlay` to empty. Weights are positive integers, and
exactly one arm must be the control.

An overlay is an object with any of these fields. Unknown fields are refused.

| Field | Shape | Effect |
|---|---|---|
| `instructions` | Map of role to a repository file path, or `false` | Appends the file's text after the role's selected tag instructions. `false` suppresses Orbit's shipped default instructions for that role. Paths are relative to the repository root, with no `..`. |
| `skills` | `{"replace": {skill name: bundle directory}, "roles": [...]}` | Replaces each named skill with the bundle at that repository directory. |
| `tools` | `{"add": [...], "remove": [...], "roles": [...]}` | Adds or removes tools. No tool may be in both lists. |
| `crew` | `{"pool": [{"crew": name, "weight": n}]}` | Draws the implementing crew from this weighted pool instead of the usual draw. |
| `config` | `{"values": {key: value}, "roles": [...]}` | Sets task-local configuration values. Each key must be listed in `experiments.config_keys`. The only keys Orbit admits, `workflow.explore.max_calls_per_run` and `visual_review.reviewer`, are kept for old experiments and ignored. |
| `memory` | `{"roles": [...], "global": bool}` | Delivers the model's memory notes to the listed roles; `global: true` adds the shared notes. Memory is kept only while an experiment with a memory arm runs. |

Roles are `implement`, `review` and `recovery`. `roles` defaults to
`["implement"]`.

`orbit experiment show` prints each arm's overlay digest.

### Metrics, guardrails, and sample rules

`--primary-metric` is the metric the verdict is read on. Lower is better for
all of them except `first_pass_acceptance` and `delivery_done_rate`.

| Scope | Metrics |
|---|---|
| Whole task | `total_tokens`, `cost_usd`, `lead_time`, `first_pass_acceptance`, `review_repairs`, `step_recoveries`, `final_recoveries`, `failed_checks`, `merge_conflicts`, `delivery_done_rate`, `escaped_regressions`, `lines_changed` |
| Whole task, per line | `cost_per_line`, `lead_time_per_line` |
| One step (not allowed as primary) | `implement_tokens`, `implement_duration`, `turns_before_first_edit`, `result_bytes_before_first_edit` |

The one-step metrics are exploratory, because a treatment can move work
between steps without improving the whole task. Use them in guardrails and with
`readout --metric`.

The per-line metrics divide an arm's total cost, or total lead time in hours,
by its total reviewable lines: the lines the task's own commits added and
deleted, minus pure renames and paths in `workflow.non_reviewable_paths`
(empty unless you set it). A run with zero reviewable lines, or with no
recorded line count, is left out and counted on its arm. A per-line primary
requires a `lines_changed` guardrail, for example `{"lines_changed": "+0"}`.
You choose the threshold; an arm that breaches it is not named the winner.

`--guardrails` maps a metric to the threshold its difference from control may
not pass: a signed number with an optional `pt` or `%` unit, such as `"-5pt"`
or `"+0"`. For a rate, `pt` means percentage points and `%` means that percent
of the control's mean.

`--min-sample` is the number of tasks every arm needs before a verdict.
`--max-days` is the number of days after start at which a verdict is given
whatever the sample. Set at least one, or the verdict stays `collecting`.

## Start it: shadow and live

```bash
orbit experiment start brief-v2
```

`start` freezes the definition and begins enrollment. It is refused, naming
the arm and the reason, unless the definition is complete (at least two arms,
exactly one control, a primary metric) and every prerequisite under
[Before you start](#before-you-start) holds.

`--mode`, set on `create` or `update`, decides what an assignment does:

| Mode | Effect |
|---|---|
| `shadow` (default) | Assigns and records arms but applies no overlay. Use it to check the population, weights and readout on real tasks before any agent's behaviour changes. |
| `live` | Applies each arm's overlay to matching dispatches for the roles it names. |

## Assignment is sticky

When Orbit admits a task, each running experiment whose population matches
enrolls it once, before the crew draw. The same task always lands in the same
arm while the weights stand, and its assignment never changes afterwards.
Tasks that are already finished are never enrolled.

What ends or overrides a treatment:

- **Pausing or retiring an arm** stops new enrollment. Tasks already assigned
  keep their treatment.
- **Concluding or abandoning** stops the treatment for fresh admissions. Runs
  already admitted keep the treatment they captured, including on resume.
- **An explicit crew** on the run or the task wins over an arm's crew
  overlay. Orbit then suppresses the arm's crew treatment for that admission,
  records the override on the run, and does not count it as delivered.

Two experiments cannot treat the same surface (`instructions`, `skills`,
`tools`, `crew`, `config`, `memory`) of one task. A task that a second
experiment would enroll on an occupied surface is excluded from that
experiment with a surface conflict, for good. Give overlapping treatments one
experiment with more arms.

### Place one task by hand

```bash
orbit experiment assign brief-v2 --task <task-id> --arm brief-v2
```

A forced assignment needs a running experiment, an active arm, and an
unfinished task that has no arm in this experiment, or was excluded from it
only for its explicit crew. A task whose surface another experiment owns is
still refused. Forced assignments are left out of
every readout estimate, so use them to smoke-test an arm, not to gather
evidence.

## Pause, reweight, resume

```bash
orbit experiment pause brief-v2 --reason "Backlog is all xhard this week"
orbit experiment reweight brief-v2 --weights '{"brief-v2": 2}' --reason "Shift traffic to the treatment"
orbit experiment resume brief-v2
```

`pause` stops new enrollment until you resume. `pause`, `reweight`,
`conclude`, `abandon` and `delete` require `--reason`; `show` prints the
reasons in the event log.

`reweight` works on a running or paused experiment and opens a new segment,
so tasks assigned under different weights stay distinguishable in the
readout. Arms you do not name keep their weight. A weight must be a positive
integer: to stop an arm, retire it with `--retire <arm>` (repeatable). The
control cannot be retired, and at least one treatment must remain.

## Read the readout

```bash
orbit experiment readout brief-v2
orbit experiment readout brief-v2 --metric delivery_done_rate
```

The readout is computed when you ask. It is intention-to-treat: every
enrolled task counts in its arm whatever its outcome. `--metric` reports only
that metric; the verdict is still read on the primary metric and the
guardrails.

Per arm and per segment you get `n`, the mean and median with 95% intervals,
the number of outcomes still open, and the crew mix. Each treatment's
difference from control is adjusted for complexity, so an arm that drew harder
tasks is compared on like work.

Some tasks are left out of every estimate and counted separately:

| Count | Tasks |
|---|---|
| `forced_excluded` | Forced assignments. |
| `interactive_excluded` | Tasks worked in an interactive session. Only a job applies an overlay, so these were never treated. |

Surface-conflict exclusions are counted too. A complexity level that only one
side has values in is left out of the overall difference and named in
`cells_dropped`.

### The verdict

| Verdict | Meaning |
|---|---|
| `collecting` | Not every active arm has reached `min_sample`, and `max_days` has not passed. |
| `withheld` (shown as `no verdict`) | The sample-ratio check failed, or a treatment did not reach enough of its runs. No winner is named, however the differences look. |
| `winner <arm>` | The arm's primary-metric interval excludes zero in the better direction, and it breaches no guardrail. When several arms qualify, the largest improvement wins. |
| `no difference` | Every arm was read and none qualifies. |
| `inconclusive` | The horizon was reached but something cannot be read: the primary metric is unmeasured, an arm has no interval, or a qualifying arm's guardrail is not `ok`. |

An older experiment whose primary metric is a one-step metric also gets
`no verdict`.

Each guardrail reports one status per arm. Only `ok` lets an arm win.

| Status | Meaning |
|---|---|
| `ok` | The difference is inside the threshold. |
| `breached` | The point estimate is past the threshold. |
| `straddles` | The estimate is inside, but its interval crosses the threshold. |
| `unknown` | There is no difference to read. |

### Warnings to read before the verdict

**Treatment delivery.** An arm is only treated if its overlay reached its
runs. For an arm whose overlay sets instructions, skills or tools, a task
counts as delivered when one of its runs dispatched the targeted role with
the overlay and that step succeeded. The arm's `delivery` reports `assigned`,
`delivered`, `pending` and `rate`; the CLI shows it in the `DELIVERED` column.
When any arm's rate is below 0.9, the readout adds an `undelivered` warning
and withholds the verdict. A crew overlay counts as delivered only when the
run used the experiment's crew draw; an explicit choice of the same crew does
not count. Config-only and empty arms have no delivery check. Delivery shows
that the overlaid step ran, not that the agent followed the instruction.

**Per-protocol view.** `per_protocol` is the primary metric with each checked
arm reduced to its delivered tasks. The verdict stays intention-to-treat. When
the two views disagree, the difference depends on the treatment actually
running.

**Sample ratio.** Per segment, Orbit tests the observed arm counts against the
weights. Below p = 0.001 the assignment is suspect and the verdict is
withheld. Check the population and any reweights.

**Open outcomes.** Tasks still in flight, and landed tasks inside the 14-day
escaped-regression window, count in `n` but have no settled value yet. Early
differences move as they settle; `escaped_regressions` moves most.

**Drift.** An arm whose tasks carry more than one overlay digest received more
than one treatment. Conclude or clone instead of reading through the warning.

**Unmeasured first edits.** `turns_before_first_edit` and
`result_bytes_before_first_edit` are scored from the recorded tool calls of a
task's first implement invocation. A settled task with no recorded tool calls
or no recognized edit is unmeasured and stays out of `n`, so `n` can be
smaller than the settled tasks. Two warnings mark gaps:

- `first_edit_incomplete`: the scan of invocations was cut short. Both
  metrics are unmeasured for every settled task in this readout. Other metrics
  are unaffected.
- `tool_trace_incomplete`: a provider's output outgrew the capture limit, so
  its early tool calls were lost and its task is unmeasured. Raise
  `ORBIT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES` for later runs.

**Memory arms.** A memory arm's readout also reports, per task, the digest
of the memory captured for it and the revisions it carried, and counts the
cost of the memory-writing tasks against that arm.

## Conclude or abandon

```bash
orbit experiment conclude brief-v2 --arm brief-v2 --reason "Winner on total_tokens, guardrails ok"
orbit experiment abandon brief-v2 --reason "Population was wrong; superseded by brief-v3"
```

Both take a running or paused experiment; `abandon` also takes a draft.
Enrollment stops either way, and the recorded assignments and results stay
readable. `conclude` also stores the readout at that moment, which
`orbit experiment show` returns.

Concluding changes no configuration and no repository file:

- **With `--arm`**, it files one proposed task that asks an operator to make
  that arm's overlay the workspace default. The overlay becomes the default
  only when someone does that task's work. Review it like any other proposed
  task.
- **Without `--arm`**, it records no winner and files nothing.

`abandon` files nothing and records no winner.

## Delete a finished experiment

```bash
orbit experiment delete hard-tier-by-model --reason "old shadow trial"
```

`delete` removes a draft, concluded or abandoned experiment with its arms,
event log and assignments. It refuses an experiment that:

- is running or paused;
- ran live and assigned tasks to arms, because their runs refer to it; or
- was concluded with an arm, even in `shadow`, because its promotion task
  names it.

## Watch it in the dashboard

Select a workspace and open **Analytics → Experiments**. The list shows each
experiment's status, mode and per-arm progress toward `min_sample`. Open one
for its readout: the verdict with its reasons, one card per arm, each
metric's difference from control, and the event timeline. An operator session
can also create, edit, start and conclude experiments there. See
[Use the dashboard](/how-to/dashboard/#experiments).
