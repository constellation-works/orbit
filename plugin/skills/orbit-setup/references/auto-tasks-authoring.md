# Auto-tasks: authoring a definition

## Contents

- [Prerequisites](#prerequisites)
- [Creating one](#creating-one)
- [Minted tasks inherit template complexity](#minted-tasks-inherit-template-complexity)
- [Dedupe is the important field](#dedupe-is-the-important-field)
- [Skipping a tick when nothing landed](#skipping-a-tick-when-nothing-landed)
- [Proving a job step with a completion contract](#proving-a-job-step-with-a-completion-contract)
- [Skipping a tick when nothing is blocked](#skipping-a-tick-when-nothing-is-blocked)
- [Writing the body](#writing-the-body)
- [Scheduling notes](#scheduling-notes)

An auto-task is a definition that **mints a task** on its own schedule. The host
clock tick evaluates every definition in each registered owner checkout, so
adding a recurring chore is a new YAML file — never new code or a new routine.

Use an auto-task when the recurring thing is *work someone should do*: a QA
sweep, a dependency audit, a stale-branch review, doc validation. Use a
[routine](automation-scheduler.md) when the recurring thing is *a pipeline to run*.

## Prerequisites

Definitions fire when their `enabled` switch is on and the host clock
is enabled. See [automation-scheduler.md](automation-scheduler.md). No scheduler routine or job is
required.

## Creating one

```bash
orbit auto-task add \
  --name dependency-audit \
  --description "Weekly check for outdated or vulnerable dependencies" \
  --cron "0 9 * * 1" \
  --title "Audit dependencies" \
  --body "<the instruction the executing agent receives>" \
  --criterion "Every outdated dependency is either upgraded or has a filed exception" \
  --criterion "The audit command and its output are recorded in the execution summary" \
  --type chore \
  --tag dependency-audit \
  --required-tools github.run.list \
  --priority medium \
  --complexity medium
```

Definitions land in `.orbit/auto_tasks/<name>.yaml`. `.orbit/` is per-user
checkout state that git ignores, so the file is not reviewed in a PR; read it
back with `orbit auto-task show <name>` before enabling it.

| Flag | Notes |
|---|---|
| `--name` | Unique in the workspace; lowercase alphanumeric plus `-`/`_`. |
| `--cron` / `--every-minutes` / `--deliveries-landed` | Mutually exclusive; `add` requires exactly one. Cron is 5-field, host-local time. `--deliveries-landed` takes the delivery-trigger JSON. |
| `--title` / `--body` | The minted task's title and description. |
| `--criterion` | Repeatable. This is the acceptance criteria of every minted task — write them as observably as you would for a hand-authored task. |
| `--type` | `feature`, `bug`, `refactor`, `chore`. Defaults to `chore`. |
| `--tag` | Repeatable, and worth setting: it is how the minted tasks are found later. A provenance tag is added automatically. |
| `--required-tools` | Repeatable exact canonical tool name. `--required-tool` is an alias. Scheduled fires and manual `mint` copy the normalized list onto each task. |
| `--complexity` | Optional assessed complexity: `low`, `medium`, `hard`, or `xhard`. It is copied to every minted task. |
| `--status` | Template status stored on the definition. The serialized default is `backlog`, but at mint a `backlog` template becomes `proposed` unless the normalized minted tags include `no-diff-expected`; explicit non-`backlog` statuses are preserved. Implementation mints therefore enter `proposed` and need approval before becoming shippable work. |
| `--crew` | Crew override for minted tasks. |
| `--granted-tools` | Operator only. Exact canonical tool names an operator grants every minted task (the tasks' `granted_tools`); see [Granting tools to every minted task](auto-tasks-managing.md#granting-tools-to-every-minted-task). `--granted-tool` is an alias. |
| `--dedupe` | `skip-if-open` (default) or `always`. `skip_if_open` is also accepted. The definition file, the JSON document, and `show` all print the canonical `skip_if_open` / `always` token. |

## Minted tasks inherit template complexity

Set `template.complexity` (or `--complexity`) to `low`, `medium`, `hard`, or `xhard` to
give every minted task an explicit assessment. The value round-trips through
the definition YAML, list/show surfaces, and both scheduled and manual minting.
It also allows automatic crew selection to use the matching
`workflow.*_complexity_crews` pool when no template crew overrides it.

Legacy and custom definitions may omit `complexity`. They remain valid and mint
the explicit non-answer `unassessed`, preserving the historical behavior rather
than silently treating omission as `medium`. `unassessed` round-trips through
task persistence, but human and agent `task.add`/`task.update` surfaces still
accept only assessed values (`low`, `medium`, `hard`, `xhard`); they can assess such a minted task but cannot
re-clear the assessment.

Ordinary updates that omit complexity continue to
succeed. An omitted template falls through to the default crew because there is
no `unassessed` complexity pool.

## Dedupe is the important field

`skip-if-open` skips the fire while a previously minted instance is still open.
Without it a stalled backlog accumulates identical tasks every cycle — a weekly
chore nobody has picked up becomes fifty copies by year's end. Choose `always`
only when each instance is genuinely independent of the last.

## Skipping a tick when nothing landed

`dedupe` cannot tell whether there is anything to do. A sweep that reviews or
validates what landed on an integration branch has nothing to do while that
branch is quiet, yet it still boots a worktree and an agent to say so. The
optional `skip_if_unchanged` block — a definition-file field, edited in the
YAML rather than through a CLI flag — suppresses that mint:

```yaml
skip_if_unchanged:
  ref: agent-main                      # integration branch whose tip is compared
  cursor:
    tags: [code-review, no-diff-expected, auto-task:code-review]
    legacy_tags: [code-review-sweep, no-diff-expected]
```

The scheduler resolves the ref's tip, finds the newest `done` chore carrying
every tag in `cursor.tags` (falling back to `legacy_tags` only when the current
tags select nothing), and reads that sweep's `sweep-cursor.json` artifact —
`{"schema_version":1,"ref":"<branch>","cursor":"<commit SHA>"}`, which the
sweep's own template is responsible for writing. When the tip is already
covered by that cursor the tick skips without consuming its slot, so the first
new commit fires the pending occurrence.

The skip is reported by
`orbit clock tick` and kept on the cursor, so `orbit auto-task show <name>`
and the dashboard name the reason and both SHAs.

It **fails open**: no completed sweep, a missing or malformed cursor, an
unresolvable ref or commit, or any probe failure mints as before and records
why. A precondition that cannot be answered never stops a sweep.

Include the definition's `auto-task:<name>` provenance tag in `cursor.tags`:
only tasks minted from the definition carry it, so a hand-filed chore that
happens to share the other tags can never become the cursor task.

The shipped `code-review`, `qa-sweep` and `qa-full-sweep` defaults carry the
block against the workspace's base branch. Delivery-triggered definitions do not need it — they
never fire on a quiet tree.

## Proving a job step with a completion contract

A definition may declare `completion`, a definition-file field, so a job step can
require more than a `done` task. Without it a definition behaves as before.
The job checker accepts only contracts in its result-contract registry;
currently, `release-readiness/v1` is the only registered contract.

```yaml
completion:
  contract: release-readiness/v1 # the job step's require_result
  adapter: my-adapter            # a granted script, .orbit/resources/scripts/my-adapter.yaml
  form: my-form                  # passed to the adapter
  evidence_artifact: readiness-evidence.json
  max_age_minutes: 1440
  read_task_tags: []             # tagged tasks the adapter may read
  evidence_instructions: |
    What this workspace's run must leave.
```

Orbit adds the required evidence binding to the minted task's description. When the
task finishes, Orbit compares that evidence with its own records of the task, run,
definition digest, source revision and completion time. The workspace adapter then
reads the evidence on stdin and answers `passed`, `blocked` or `unavailable`. Only
`passed` is accepted, and the task's final message is never read.

Anything else, including
a missing grant, a stale source or a blocked verdict, holds the step until its expiry.
Before a later step mints, each earlier accepted result is checked again for exact
source and age, even after a checkpoint. A probe that succeeded without doing the
contract's work answers `blocked` and adds nothing to accepted-result history.

`orbit automation explain job:<name> --json` shows each step's contract, adapter grant
and latest accepted result, and tells a missing history from an unreadable one.
`orbit script grant <name>` is the owner's decision; Orbit never grants on its own.

## Skipping a tick when nothing is blocked

`skip_unless_blocked_candidates: true` — also a definition-file field — mints
only when at least one blocked task's current block episode awaits triage,
under the rule final recovery's [block handling](automation-built-in.md#what-a-block-gets-mechanical-steps-a-recheck-then-a-decision-card)
uses. Otherwise the tick skips with reason `no_blocked_candidates` and a
detail naming how many blocked tasks it read.

The skip is reported and kept
on the cursor like a `skip_if_unchanged` skip, and `orbit auto-task show
<name>` displays both the precondition and the last skip. It fails open too:
a probe that cannot read the blocked tasks mints and records why. No shipped
default carries it: the retired `blocked-task-triage` auto-task did, and
final recovery now decides blocked tasks
([automation-built-in.md](automation-built-in.md#what-a-block-gets-mechanical-steps-a-recheck-then-a-decision-card)).

## Writing the body

The body is the entire brief the executing agent gets. It arrives with no
conversation, no clarifying questions, and no memory of why the definition
exists. So:

- State what durable output counts as success. If the real deliverable is filed
  tasks or resolved records, say so — narrative output is advisory.
- Name the exact commands and tool surfaces to use, especially where a plausible
  wrong path exists (editing a file instead of calling a tool).
- Say what a clean no-op looks like. A recurring chore that finds nothing should
  succeed, not fail or invent work to justify the run.
- Require a dedupe check against existing tasks before filing anything.

## Scheduling notes

The host tick runs minutely by default, so each definition's own cron governs its cadence.
Catch-up collapses: a machine that was asleep through six firings mints once, not
six times. Per-definition cursors and their sidecar lock make evaluation
idempotent and single-flight within the workspace.

Fires are reported by `orbit clock tick` and in the dashboard auto-task panel;
the minted task is the durable record. Evaluation creates no scheduler job run.
