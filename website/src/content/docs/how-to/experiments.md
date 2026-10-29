---
title: Run an experiment
description: "Compare agent treatments with orbit experiment: define control and treatment arms, enroll tasks, read the verdict, and conclude without changing configuration automatically."
sidebar:
  order: 8
---

An experiment is a randomized comparison of agent treatments in one
workspace. You define arms — one control and one or more treatments, each
with a weight and an overlay — and Orbit assigns each enrolled task to an
arm before its delivery crew is drawn. A readout then compares the arms on
one primary metric, with guardrails, from the records Orbit already keeps.

Run an experiment when you want evidence before you make a treatment the
default: a different instruction file, a replaced skill, a changed tool
grant, a different crew pool, or one admitted configuration value. Run it in
`shadow` first when you only need the assignment and outcome records; run it
`live` when the treatment itself must change what dispatched agents receive.

Every lifecycle write is the operator's. The CLI and the MCP tools reach the
same governed operations, and an agent session is refused for every write
below. `show`, `list`, and `readout` are read-only and any caller may use
them. In MCP terms the tools are `orbit.experiment.create`, `update`,
`clone`, `start`, `pause`, `resume`, `reweight`, `assign`, `conclude`,
`abandon`, `show`, `list`, and `readout`; the writes carry the same
operator-only refusal there.

## Before you start

- Set `experiments.enabled` to `true` in the workspace. It defaults to
  `false`. While it is `false` you can still define, edit, show, and abandon
  drafts, but `orbit experiment start` is refused with an error naming the
  key, and forced assignment is refused too:

  ```bash
  orbit config set experiments.enabled true
  ```

- Leave room under `experiments.max_running` (default `3`, range `1`–`20`).
  Starting or resuming past it is refused. A paused experiment does not
  count. Conclude, pause, or abandon one first.
- If a treatment sets a `config` overlay, list every key it sets under
  `experiments.config_keys` (default `[]`). Starting refuses an arm that
  sets any other key. The overlay itself only admits keys Orbit has opted
  in for experiments; today that is `workflow.explore.max_calls_per_run`,
  an integer from `1` to `100`.
- For a `live` experiment, have the treatment ready at the base revision:
  instruction files must exist there and pass the instruction admission
  rules, skill bundle directories must exist there, added tools must be
  registered, enabled, and agent-facing, and removed tools must be known
  tools the agent contract does not require. Skill replacement supports
  Claude on Unix only, so a live skill overlay needs a Claude-only
  implementation crew pool or it is refused at start.

## Define the experiment

This example compares the shipped instructions (control) with a treatment
that gives implementers a different instruction file. The keys, names, and
paths are illustrative; the treatment file must exist in your repository
before you start a `live` run.

```bash
orbit experiment create brief-v2 \
  --hypothesis "A shorter implement brief cuts tokens without lowering first-pass acceptance." \
  --arms '[
    {"name": "control", "control": true, "weight": 1, "overlay": {}},
    {"name": "brief-v2", "control": false, "weight": 1,
     "overlay": {"instructions": {"implement": "docs/brief-v2.md"}}}
  ]' \
  --population '{"complexity": ["medium", "hard"], "task_type": ["feature", "bug"]}' \
  --primary-metric implement_tokens \
  --guardrails '{"delivery_done_rate": "-5pt", "escaped_regressions": "+0"}' \
  --min-sample 20 \
  --max-days 28 \
  --mode shadow
```

`create` makes a draft. A draft assigns nothing. Names are unique in the
workspace, for the experiment and for each arm: 1–64 characters of letters,
digits, `-`, `_`, or `.`, starting with a letter or digit, and an experiment
name may not start with `exp-`, which experiment IDs use.

Edit a draft with `orbit experiment update <id>`, passing only the fields
you change; a given `--arms` list replaces every arm. Only a draft can be
edited. Once an experiment has started, its definition is frozen and it
changes only through `pause`, `resume`, `reweight`, `conclude`, or
`abandon` — or you `clone` it:

```bash
orbit experiment clone brief-v2 --name brief-v3
```

`clone` copies the hypothesis, arms with their current weights and
overlays, population, metrics, sample rules, and mode into a new draft
under the new name, with a fresh assignment salt. Retired arms are not
copied, and the source is unchanged. `<id>` everywhere below is the
experiment's ID or its name.

### Population

`--population` is a JSON object; omit a field to place no restriction on
it, and omit the object to enroll every task.

| Field | Matches |
|---|---|
| `complexity` | One of `low`, `medium`, `hard`, `xhard`. Any other value is refused. |
| `task_type` | The task's type, such as `feature` or `bug`. |
| `tags_include` | Every listed tag must be on the task. |
| `tags_exclude` | No listed tag may be on the task. A tag in both lists is refused. |
| `origin` | The task's origin. |

Unknown population fields are refused.

### Arms and overlays

Each arm is `{name, control, weight, overlay}`. `control` defaults to
`false`, `weight` to `1`, and `overlay` to empty. Weights are positive
integers; a weight of `0` is refused on write, and at start exactly one
arm must be the control (more than one `control: true` is already refused
on write). Each overlay's digest is recorded when you write it, and
`orbit experiment show` prints it per arm.

An overlay is an object with any of these fields. Unknown fields are
refused everywhere.

| Field | Shape | Effect |
|---|---|---|
| `instructions` | Map of role to a repository file path, or `false` | Append the file's text after the selected tag instructions for that role. Set `false` to suppress Orbit's shipped default instructions for that role; file text does not replace the base activity instructions or selected tag instructions. Roles are `implement`, `review`, and `recovery`. Paths must be relative to the repository root, with no `..` and no surrounding whitespace. |
| `skills` | `{"replace": {skill name: bundle directory}, "roles": [...]}` | Replace each named skill with the bundle at that repository directory. `replace` must name at least one skill. |
| `tools` | `{"add": [...], "remove": [...], "roles": [...]}` | Add or remove tools for the listed roles. It must add or remove at least one tool, and no tool may be in both lists. |
| `crew` | `{"pool": [{"crew": name, "weight": n}]}` | Draw the implementing crew from this weighted pool instead of the usual draw. The pool must name at least one crew, no crew twice, each with a positive weight. |
| `config` | `{"values": {key: value}, "roles": [...]}` | Set task-local configuration values for the listed roles. It must set at least one key, with a non-blank key. |

`roles` defaults to `["implement"]` on `skills`, `tools`, and `config`,
and must name at least one role. A blank skill name, crew, tool, or config
key is refused.

### Metrics, guardrails, and sample rules

`--primary-metric` is the one metric the verdict is read on. The metrics
an experiment may name, for the primary metric, guardrails, and
`readout --metric`, are: `implement_tokens`, `total_tokens`, `cost_usd`,
`lead_time`, `implement_duration`, `turns_before_first_edit`,
`result_bytes_before_first_edit`, `first_pass_acceptance`,
`review_repairs`, `step_recoveries`, `final_recoveries`, `failed_checks`,
`merge_conflicts`, `delivery_done_rate`, `escaped_regressions`, and
`lines_changed`. Lower is better for all of them except
`first_pass_acceptance` and `delivery_done_rate`, which are rates where
higher is better. Naming any other metric is refused.

`--guardrails` maps a metric name to the threshold its difference from
control may not pass, as a signed number with an optional `pt` or `%`
unit, such as `"-5pt"` or `"+0"`. A threshold without a sign, or with any
other unit, is refused. For a rate, `pt` means percentage points; `%`
means that percent of the control's mean.

`--min-sample` is the tasks every arm needs before a verdict.
`--max-days` is the days after start at which a verdict is given whatever
the sample. Both are optional; set at least one, or the verdict stays
`collecting`.

## Start it: shadow and live

```bash
orbit experiment start brief-v2
```

`start` opens segment 1 and freezes the definition. It is refused unless
`experiments.enabled` is true, a `max_running` slot is free, and the
definition is complete: at least two arms with unique names, exactly one
control, positive weights, a primary metric, and config overlays only for
keys on `experiments.config_keys`. It also checks the live prerequisites
above — base-admissible instruction files and skill bundles, registered
enabled agent-facing added tools, removal only of tools the handoff
contract does not require, and resolvable overlay crews — and refuses
with the arm and the reason.

`--mode` decides what an assignment does:

- `shadow` (the default) assigns and records arms but applies no overlay.
  Use it to validate the population, weights, and readout on real tasks
  before any agent's behavior changes.
- `live` applies overlays on matching dispatches for the roles they name:
  the arm's instruction file joins that dispatch's instructions (or the
  shipped default is suppressed), its tool delta applies after the
  activity's own tool grant, its skill bundles are carried to the agent,
  its crew pool replaces the crew draw, and its config values apply as
  task-local overrides. Start and resume are supported in both modes;
  skill replacement supports Claude on Unix only.

## Assignment is sticky

When Orbit admits a task, each running experiment whose population matches
enrolls it once, before any delivery crew draw. The draw is a hash of the
experiment's salt, its ID, and the task ID, taken against the arms'
current weights, so the same task always lands in the same arm while the
weights stand. The assignment records the arm, the current segment, and
the arm's overlay digest, and it never changes afterwards: pausing,
concluding, or abandoning stops new enrollment, and tasks already assigned
keep their arm. Terminal tasks are never enrolled.

Experiments reserve the surfaces their arms touch — `instructions`,
`skills`, `tools`, `crew`, `config` — counting retired arms too. A task a
second experiment would enroll on an occupied surface is instead excluded
for that experiment with a surface conflict, and that exclusion is final
for the task. Give overlapping treatments one experiment with more arms,
not two experiments.

To place one task by hand, force it:

```bash
orbit experiment assign brief-v2 --task <task-id> --arm brief-v2
```

Forced assignment needs a running experiment in either mode (shadow or live),
a nonterminal task without an arm assignment or with an `explicit_crew`
exclusion for this experiment, and an active arm. It records `forced: true`.
The force replaces only that `explicit_crew` exclusion; existing arm assignments
and other exclusions for the experiment, including `surface_conflict`, stay
sticky. A task whose surface another experiment owns is still refused.
Forced assignments are left out of every readout estimate and counted
separately (see below), so use them to smoke-test an arm, not to gather
evidence.

## Pause, reweight, resume

```bash
orbit experiment pause brief-v2 --reason "Backlog is all xhard this week"
orbit experiment reweight brief-v2 --weights '{"brief-v2": 2}' --reason "Shift traffic to the treatment"
orbit experiment resume brief-v2
```

`pause` stops new enrollment until you resume; assigned tasks keep their
arm. `pause`, `reweight`, `conclude`, and `abandon` each require
`--reason`; every lifecycle call records its reason on the event log that
`show` prints.

`reweight` changes arm weights, retires arms, or both, on a running or
paused experiment. Each call opens a new segment, recorded with the
weights before and after, so tasks assigned under different weights stay
distinguishable in the readout. Arms you do not name keep their weight. A
new weight must be a positive integer — retire an arm instead of giving
it `0`. The control cannot be retired, and at least one treatment must
remain; retiring or reweighting an arm that is not active is refused, as
is naming one arm in both `--weights` and `--retire`. Retired arms take
no new tasks; tasks already in them keep their arm.

## Read the readout

```bash
orbit experiment readout brief-v2
orbit experiment readout brief-v2 --metric delivery_done_rate
```

The readout is computed when you ask, from the experiment's assignments
and the records Orbit already keeps. It is intention-to-treat by task:
every enrolled, unforced task counts in its arm whatever its outcome.
Forced assignments are left out and counted as `forced_excluded`;
surface-conflict exclusions and unreadable task records are counted too.
`--metric` reports only that metric, but the verdict is still read on the
primary metric and the guardrails. The JSON carries `schema_version`.

Per arm and per segment you get `n`, the mean and median with 95%
intervals, how many outcomes are still open and how many settled tasks
have no measured value, and the crew mix. Intervals are percentile
bootstrap with a fixed seed for means and medians, and Wilson intervals
for rates. Each treatment's difference from control is post-stratified by
complexity, overall and per complexity, so arms that drew harder tasks
are compared on like work; a complexity cell only one side has values in
is left out of the overall difference and named in `cells_dropped`.

The verdict is one of:

- `collecting` — not every active arm has reached `min_sample` on the
  primary metric, and `max_days` has not passed since start. Retired arms
  do not hold the verdict back.
- `withheld` (label `no verdict`) — the sample-ratio check failed; see
  below. No winner is named however the differences look.
- `winner <arm>` — an arm whose primary-metric interval excludes zero in
  the better direction and that breaches no guardrail. When several arms
  qualify, the largest improvement wins.
- `no difference` — every arm was read and none qualifies, with nothing
  left unread.
- `inconclusive` — the horizon was reached but something cannot be read:
  the primary metric is not measured, an arm has no interval, or a
  qualifying arm's guardrail is breached, straddles its threshold, or
  cannot be read.

Guardrails report `ok`, `breached` (the point estimate is past the
threshold), `straddles` (the estimate is inside but its interval crosses
the threshold), or `unknown` (there is no difference to read) per arm.
Only `ok` lets an arm win.

Read the warnings before the verdict:

- **Sample ratio.** Per segment, a chi-square test compares the observed
  arm counts with the weights. Below p = 0.001 the assignment is suspect
  — the counts do not match the weights — and the verdict is withheld.
  Check the population and any reweights before you trust the arms.
- **Lagging (open) outcomes.** Enrolled tasks still in flight, and landed
  tasks still inside the 14-day escaped-regression window, have open
  outcomes. Their tasks count in `n` but contribute no settled value yet,
  so early differences move as they settle. `escaped_regressions` is the
  metric most affected.
- **Drift.** An arm whose assignment rows or runs carry more than one
  overlay digest received more than one treatment; its runs before and
  after the change are not the same treatment. Conclude or clone rather
  than reading through a drift warning.
- **Unmeasured first edits.** `turns_before_first_edit` and
  `result_bytes_before_first_edit` score each task's first implement
  invocation from its recorded tool calls: the calls before the first
  editor tool, with a first-call edit scoring zero. A settled task whose
  first invocation has no recorded tool calls, no recognized editor tool
  (a shell command such as `command_execution` is not an edit), or no
  implement invocation at all is unmeasured and stays out of `n`. A task
  still in flight is open. Older runs may lack tool-call traces, so their
  `n` can be smaller than their settled tasks.
  If the invocation scan is incomplete because of a record limit or pagination stall,
  the readout reports a `first_edit_incomplete` coverage warning. The scan
  cannot confirm which tasks' earliest invocations it holds, so both first-edit
  metrics are unmeasured for every settled task in that readout; in-flight
  outcomes remain open. These values contribute no samples to estimates,
  comparisons, the primary metric's chart, or the verdict and its guardrails.
  Other metrics remain available.

The readout is descriptive, not a significance oracle. It reports no
p-value for the treatment difference itself; the interval excluding zero
in the better direction is the winner rule. Small samples give wide
intervals, and a crew overlay changes who does the work as well as how,
so compare the crew mix before you credit the treatment alone.

## Conclude or abandon

```bash
orbit experiment conclude brief-v2 --arm brief-v2 --reason "Winner on implement_tokens, guardrails ok"
orbit experiment abandon brief-v2 --reason "Population was wrong; superseded by brief-v3"
```

Both take a running or paused experiment; `abandon` also takes a draft.
Concluding records the chosen arm (or none), your reason, and the readout
JSON at that moment on the conclusion event, which
`orbit experiment show` returns. Assignment stops either way, and tasks
already assigned keep their arm.

Concluding with an arm files one proposed task naming the experiment, the
arm, the readout verdict, and the arm's overlay, asking an operator to
make that overlay the workspace default. That is all it files. It changes
no configuration and no repository file: the overlay becomes the default
only when someone does that proposed task's work — editing the shipped
instructions, skill, tool grant, crew, or configuration the overlay
points at. Review the proposed task like any other before you approve it.
Concluding with no `--arm` records no winner and files nothing, and
concluding again with a different arm after a promotion task exists is
refused until that task is removed. `abandon` files nothing and records
no winner.

## Watch it in the dashboard

Open **Analytics → Experiments** (`#analytics/experiments`). Experiments
are per workspace, so the **All workspaces** view shows the usual
placeholder asking you to select one. See
[Use the dashboard](../dashboard/#experiments).

The list sorts running first, then paused, draft, concluded, and
abandoned, newest first within a status. Each entry shows the hypothesis,
status and mode badges, the current segment, the primary metric, the
minimum sample and maximum days, and one progress bar per arm toward
`min_sample`, with forced assignments left out and surface-conflict
exclusions counted underneath.

Open one experiment (`#analytics/experiments/<id>`) for its readout:
status, mode, segment, primary metric, minimum sample, start and horizon,
the verdict badge with its reasons, a health card, one card per arm, a
forest plot of every measured metric's difference from control, the
primary metric's cumulative per-arm series, the per-complexity table, the
crew mix, and the event timeline. An operator session also gets the
conclude form described in
[Use the dashboard](../dashboard/#experiments), and can define, edit, and
start drafts there with **New experiment**, **Edit**, and **Start**. The
dashboard applies the same checks as `orbit experiment create`, `update`,
and `start`. See
[Create, edit, and start an experiment](../dashboard/#create-edit-and-start-an-experiment).

## See also

- [CLI commands: Experiments](../../reference/cli/#experiments) — every
  verb and flag.
- [Configuration](../../reference/config/) — `experiments.enabled`,
  `experiments.max_running`, and `experiments.config_keys`.
