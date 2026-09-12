# Auto-tasks: recurring work as data

An auto-task is a definition that **mints a task** on its own schedule. One
generic routine drives every definition in the workspace, so adding a recurring
chore is a new YAML file — never new code, and never a new routine.

Use an auto-task when the recurring thing is *work someone should do*: a QA
sweep, a dependency audit, a stale-branch review, doc validation. Use a
[routine](automation.md) when the recurring thing is *a pipeline to run*.

## Prerequisites

Definitions are inert until the generic scheduler routine is enabled. See
[automation.md](automation.md) — the `auto-task-scheduler` routine ships
disabled, like the rest.

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
  --required-tool github.run.list \
  --priority medium
```

Definitions land in `.orbit/auto_tasks/<name>.yaml` — a versioned file, so
review it in a PR like any other definition.

| Flag | Notes |
|---|---|
| `--name` | Unique in the workspace; lowercase alphanumeric plus `-`/`_`. |
| `--cron` / `--every-minutes` | Mutually exclusive. Cron is 5-field, host-local time. |
| `--title` / `--body` | The minted task's title and description. |
| `--criterion` | Repeatable. This is the acceptance criteria of every minted task — write them as observably as you would for a hand-authored task. |
| `--type` | `feature`, `bug`, `refactor`, `chore`. Defaults to `chore`. |
| `--tag` | Repeatable, and worth setting: it is how the minted tasks are found later. A provenance tag is added automatically. |
| `--required-tool` | Repeatable exact canonical tool name. Scheduled fires and manual `mint` copy the normalized list onto each task. |
| `--status` | Status the minted task enters. Defaults to `backlog`; use `proposed` when a human should approve each instance before it becomes shippable work. |
| `--crew` | Crew override for minted tasks. |
| `--dedupe` | `skip-if-open` (default) or `always`. `skip_if_open` is also accepted. The definition file, the JSON document, and `show` all print the canonical `skip_if_open` / `always` token. |

## Minted tasks carry `complexity: "unassessed"`

A definition's template does not carry `complexity` — minting is the one create
path allowed to leave the assessment unset, and it always does, storing the
explicit non-answer `unassessed` rather than defaulting to `medium` or leaving
the field blank. `unassessed` is a full `TaskComplexity` value: it round-trips
through `task.show`, `task.list`, and every persisted store, and it is exactly
the value `task.add`/`task.update` reject on the `complexity` field — those two
surfaces only accept `low`/`medium`/`hard`, so a human or agent can assess a
minted task but can never re-clear that assessment. This does not block
ordinary updates: `complexity` is patched only when a `task.update` call
supplies it, so filing a comment, changing status, or any other no-op on
`complexity` succeeds on a minted task exactly as it would on any other.

Auto crew-pool selection (`workflow.*_complexity_crews`) has no pool for
`unassessed`: the configured pools only cover `low` / `medium` / `hard`, so a
minted task always falls through to the default crew, never a complexity-keyed
pool. Assess the task through `task.update --complexity` first if it should
draw from a pool.

## Dedupe is the important field

`skip-if-open` skips the fire while a previously minted instance is still open.
Without it a stalled backlog accumulates identical tasks every cycle — a weekly
chore nobody has picked up becomes fifty copies by year's end. Choose `always`
only when each instance is genuinely independent of the last.

## Managing definitions

```bash
orbit auto-task list                    # every definition with its schedule and state
orbit auto-task show <name>
orbit auto-task update <name> --cron "0 9 * * 2"   # present fields only
orbit auto-task toggle <name> on|off    # the kill-switch — preserved, not deleted
orbit auto-task mint <name>             # mint one right now
```

`mint` ignores the schedule, the dedupe policy, and `enabled`, and leaves the
scheduler's cursor untouched — so it creates real work even for a disabled definition. Inspect with
`show` first; mint only when creating that task is intended. Over MCP: `orbit_auto_task_list` and
`orbit_auto_task_mint`.

Required tools in a template extend the selected agent activity's baseline;
they do not replace it or bypass runtime capability, policy, filesystem,
subprocess, or authentication checks. Invalid, inactive, wildcard, or
non-agent-facing names fail dispatch before the provider starts.

A template that declares exactly `github.auth.status`, `github.run.list`,
`github.run.view`, `github.run.logs`, and `github.pr.list` is the worked
example. A minted instance therefore runs under
`effective_tools = agent_implement baseline ∪ those five names`. Ordinary
implementation tasks that request nothing keep the original baseline and
cannot call GitHub tools. Inclusion is only allowlist membership — a
structured `github.auth.status` answer may still report `available: false` or
`authenticated: false` when the lane has no GitHub CLI or no credentials.
That is unavailable evidence, not a clean CI result.

## The four seeded definitions

`orbit workspace init` seeds all four, disabled:

- **`qa-sweep`** — hourly. Identifies recent changes, exercises them hands-on
  through their real user-facing paths rather than just re-running the test
  suite, and files a task for each non-duplicate issue found. In agent-executor
  sandboxes and linked job-run worktrees, the managed `.git` mount is read-only
  by design (must not be worked around by chmod or host-side gitdir writes).
  Use `mkdir -p /tmp/base && git archive <sha> | tar -x -C /tmp/base` to build a
  baseline revision without writing `.git`, and `git show HEAD:<path> > <path>`
  to revert a tracked file when `git checkout --` cannot take `index.lock`.
- **`friction-curation`** — daily. Deduplicates the open friction corpus against
  task history, verifies each survivor still reproduces, resolves the ones that
  don't, and files fix tasks for the ones that do. → [friction.md](../friction.md)
- **`security-review`** — weekly. Reviews applicable application code,
  dependencies, secret handling, and configuration with evidence; files a
  durable Orbit task for each non-duplicate finding with severity and impact; a
  clean review is a successful no-op.
- **`code-review`** — every six hours. Reviews the commits merged into the
  integration branch since the previous sweep's recorded cursor, verifies each
  candidate finding against the live code, files the non-duplicate ones as tasks
  tagged `code-review`, and records the new last-reviewed commit in its execution
  summary — that cursor is the next sweep's window start.

Read them before enabling. They are also the best worked examples of how much
instruction a minted task's body should carry.

Finding tasks tagged `qa-sweep`, `security-review`, `code-review`, or
`friction-curation` receive the matching bracketed title prefix at the shared
task-creation boundary. If more than one of those tags is present, the fixed
precedence is `qa-sweep`, `security-review`, `code-review`, then
`friction-curation`, regardless of tag order. Existing matching prefixes are
not duplicated. Scheduler-minted parent tasks continue to use `[auto-task] `,
which takes precedence when an `auto-task:<name>` provenance tag is present.

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

The scheduler ticks minutely, so each definition's own cron governs its cadence.
Catch-up collapses: a machine that was asleep through six firings mints once, not
six times. Per-definition cursors make the sweep idempotent, and the driving job
holds `max_active_runs: 1`, so definitions never fan out concurrently.

Fires are observable in `orbit run history` and on the dashboard's routines
surface, like any other run.
