# Auto-tasks: recurring work as data

An auto-task is a definition that **mints a task** on its own schedule. The host
clock tick evaluates every definition in each registered owner checkout, so
adding a recurring chore is a new YAML file — never new code or a new routine.

Use an auto-task when the recurring thing is *work someone should do*: a QA
sweep, a dependency audit, a stale-branch review, doc validation. Use a
[routine](automation.md) when the recurring thing is *a pipeline to run*.

## Prerequisites

Definitions fire when their versioned `enabled` switch is on and the host clock
is enabled. See [automation.md](automation.md). No scheduler routine or job is
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

Definitions land in `.orbit/auto_tasks/<name>.yaml` — a versioned file, so
review it in a PR like any other definition.

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
| `--status` | Status the minted task enters. Defaults to `backlog`; use `proposed` when a human should approve each instance before it becomes shippable work. |
| `--crew` | Crew override for minted tasks. |
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
re-clear the assessment. Ordinary updates that omit complexity continue to
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
    tags: [code-review, no-diff-expected]
    legacy_tags: [code-review-sweep, no-diff-expected]
```

The scheduler resolves the ref's tip, finds the newest `done` chore carrying
every tag in `cursor.tags` (falling back to `legacy_tags` only when the current
tags select nothing), and reads that sweep's `sweep-cursor.json` artifact —
`{"schema_version":1,"ref":"<branch>","cursor":"<commit SHA>"}`, which the
sweep's own template is responsible for writing. When the tip is already
covered by that cursor the tick skips without consuming its slot, so the first
new commit fires the pending occurrence. The skip is reported by
`orbit clock tick` and kept on the cursor, so `orbit auto-task show <name>`
and the dashboard name the reason and both SHAs.

It **fails open**: no completed sweep, a missing or malformed cursor, an
unresolvable ref or commit, or any probe failure mints as before and records
why. A precondition that cannot be answered never stops a sweep.

The shipped `code-review` and `qa-sweep` defaults carry the block against the
workspace's base branch. Delivery-triggered definitions do not need it — they
never fire on a quiet tree.

## Managing definitions

```bash
orbit auto-task list                    # every definition with its schedule and state
orbit auto-task show <name>
orbit auto-task update <name> --cron "0 9 * * 2"   # present fields only
orbit auto-task toggle <name> on|off    # the kill-switch — preserved, not deleted
orbit auto-task mint <name>             # mint one right now
orbit auto-task delete <name> --reason "<why>"   # remove it for good
orbit auto-task restore <name>          # reinstate a deleted shipped default
```

`mint` ignores the schedule, the dedupe policy, and `enabled`, and leaves the
scheduler's cursor untouched — so it creates real work even for a disabled definition. Inspect with
`show` first; mint only when creating that task is intended. Over MCP: `orbit_auto_task_list` and
`orbit_auto_task_mint`.

### Deleting a definition

`toggle off` pauses a definition and keeps it listed. `delete` removes it: the
YAML file, its scheduler cursor, and — for a `deliveries_landed` definition —
its consumer state. Every delete writes an audit record naming who deleted
what and the optional `--reason`.

- It refuses while a task minted from the definition is still open and names
  those tasks. Finish or close them, or pass `--force`; a forced delete leaves
  the open tasks as they are.
- A delivery consumer is torn down through the audited `reset` (see below),
  and every pinned `refs/orbit/automation/*` ref it held is deleted. Delete
  therefore refuses whenever that reset would — an executing action, or a
  consumer owned by another machine — and points at
  `orbit auto-task reset <name>` to preview it. `--force` abandons an
  executing action, exactly as it does for `reset`.
- Deleting one of the shipped defaults below records an opt-out in
  `.orbit/auto_tasks/.orbit-managed-assets.json`. `orbit workspace init
  --force` and `orbit workspace sync` leave an opted-out default absent, and
  `orbit doctor` does not report it missing. Use this to drop the code
  sweeps from a workspace whose repository holds no code.
- `orbit auto-task restore <name>` is the way back for a deleted shipped
  default. It writes the shipped content (disabled, as shipped) and clears the
  opt-out, so later reseeds manage it again. `restore` refuses a name Orbit
  does not ship and a definition that already exists. `auto-task add` under a
  deleted default's name creates your own definition instead; the opt-out
  stays, so reseeds never overwrite it.

A user-authored definition records no opt-out: delete simply removes it. Over
MCP: `orbit_auto_task_delete` (`name`, optional `reason` and `force`).
`restore` is CLI-only.

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

## Definitions a plugin seeds

An installed plugin may ship auto-task definitions. `orbit plugin enable <ns>`
seeds each one as `.orbit/auto_tasks/<ns>-<name>.yaml` with `enabled: false`
and a `# provenance: plugin:<ns>@<version>` header; a plugin may not ship a
definition that is already enabled. Review it, then `orbit auto-task toggle
<ns>-<name> on` like any other.

An upgrade re-seeds a seeded file only while it still matches what the plugin
wrote; a file you edited is preserved with a warning until
`orbit plugin enable <ns> --force`. While the plugin is disabled or removed
the definition stays on disk and is skipped: `orbit auto-task list` shows it
as `skipped` with a reason naming the plugin. Tasks minted from one carry
`plugin:<ns>` beside `auto-task:<name>`, so their provenance survives the
plugin being removed.

## The nine seeded definitions

`orbit workspace init` seeds all nine, disabled, except any you deleted (see
[Deleting a definition](#deleting-a-definition)):

- **`qa-sweep`** (`medium`) — hourly. Identifies recent changes, exercises them hands-on
  through their real user-facing paths rather than just re-running the test
  suite, and files a task for each non-duplicate issue found. In agent-executor
  sandboxes and linked job-run worktrees, the managed `.git` mount is read-only
  by design (must not be worked around by chmod or host-side gitdir writes).
  Use `mkdir -p /tmp/base && git archive <sha> | tar -x -C /tmp/base` to build a
  baseline revision without writing `.git`, and `git show HEAD:<path> > <path>`
  to revert a tracked file when `git checkout --` cannot take `index.lock`.
- **`friction-curation`** (`medium`) — daily. Deduplicates the open friction corpus against
  task history, verifies each survivor still reproduces, resolves the ones that
  don't, and files fix tasks for the ones that do. A survivor owned by another
  workspace is re-homed there, or marked `rehome_to` when that workspace is not
  reachable, instead of blocking the run. → [friction.md](../../orbit/references/friction.md)
- **`security-review`** (`hard`) — weekly. Reviews applicable application code,
  dependencies, secret handling, and configuration with evidence; files a
  durable Orbit task for each non-duplicate finding with severity and impact; a
  clean review is a successful no-op.
- **`code-review`** (`hard`) — every six hours. Reviews the commits merged into the
  integration branch since the previous sweep's recorded cursor, verifies each
  candidate finding against the live code, files the non-duplicate ones as tasks
  tagged `code-review`, and records the new last-reviewed commit in its execution
  summary — that cursor is the next sweep's window start.
- **`delivery-qa`** (`hard`) — exercises each frozen delivery batch and records
  typed coverage evidence.
- **`delivery-code-review`** (`hard`) — reviews each frozen delivery batch and
  records typed coverage evidence.
- **`doc-duties`** (`low`) — daily. Validates a small batch of the oldest
  workspace documentation against current behavior, and corrects factual drift
  and broken links. A batch whose claims are already accurate is a successful
  no-diff run.
- **`backlog-hygiene`** (`medium`) — weekly. Writes one read-only report of
  stalled and untriaged tasks. It does not change task status or dispatch work.
- **`run-failure-patterns`** (`medium`) — weekly. Mines this workspace's run
  evidence for recurring failures that nobody has filed, and records one
  friction or proposed task per untracked pattern.

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

## Delivery consumers: stalls, automatic replay, and reset

A definition scheduled on `deliveries_landed` (the shipped `delivery-qa` and
`delivery-code-review` pair) is not on a clock: it watches a branch, records
which landings it has examined, and mints an examination task when the debt is
due. That recorded position — "observed commit X" — is what a rewritten branch
history breaks.

### A rewritten history is proved, or it stalls

When the observed commit is no longer reachable from the configured branch head,
the evaluator runs a replay proof: every observed commit must map onto a commit
on the new head with the same `.orbit` tree and the same parent-relative patch.

- **Proof succeeds** (a rebase or amend that preserved content) — the consumer
  reconciles itself in that tick, under an audit record attributed to
  `system:automation`, keeping every obligation. One friction is filed so the
  rewrite is visible, and the next tick observes normally. Nothing to do.
- **Proof fails** (a commit was dropped, squashed away, or its debt can no
  longer be identified) — the consumer **stalls**: evaluation suspends, one
  friction is filed listing the obligations that could not be mapped, and the
  tick prints a single `stalled: history_diverged` line instead of an error
  every 60 seconds. Nothing resumes it without an operator.

Frictions are deduped per rewrite, so repeated ticks and the sibling consumer on
the same branch share one record rather than filing hundreds.

Find stalled consumers with `orbit doctor` (the `automation-consumers` row) or
`orbit auto-task recover <name>`, whose stall reason names the cause. A stall
that persists past `automation.stall_window_minutes` (default 60) is also logged
at `warn`, so `orbit log tail --level warn` shows it.

### Clearing a stall

Two commands clear it, and both write an audit record:

```bash
# Retain every obligation: reconcile a rewrite the proof can verify.
orbit auto-task recover delivery-qa --replay-history          # preview
orbit auto-task recover delivery-qa --replay-history --reason "<why>"

# Forget the debt: re-baseline at the current branch head.
orbit auto-task reset delivery-qa                            # preview
orbit auto-task reset delivery-qa --reason "<why>"
```

Prefer `recover`. Use `reset` when no recovery can repair the consumer — the
rewrite destroyed the debt's identity, or the state predates the recorded
trigger and `recover` refuses it as `coverage_unverifiable`.

### What `reset` does

`reset` is the only Orbit operation that discards coverage debt, so it previews
until it carries `--reason`. The preview prints the consumer key, its
generation, everything that would be forgotten (pending deliveries and commits,
unresolved evidence, waived and excluded landings, accepted receipts), any
executing action, and the head it would re-baseline at. Read it before
authorizing.

An apply writes one `reset` recovery record (who, when, why, the previous
epoch/generation, the forgotten inventory, the new baseline), drops the consumer
state, and deletes the `refs/orbit/automation/<digest>/*` pins of the forgotten
batches. The next tick reports `baselined` at the branch head. Landings before
that baseline are never examined — that is the cost of the command, and the
record is the only trace they existed.

It refuses while an action is executing unless `--force` is passed (the admitted
task is abandoned, not cancelled), and refuses a state-member consumer. Never
hand-edit the `automation_consumers` row instead: that leaves no audit record.

Automation frictions are tagged `automation`, plus `history-diverged` for a
rewritten history. Those tags ship in the default vocabulary; loading the
taxonomy merges any missing defaults into an existing `.orbit/frictions/tags.yaml`
without removing operator-added tags.

## Scheduling notes

The host tick runs minutely by default, so each definition's own cron governs its cadence.
Catch-up collapses: a machine that was asleep through six firings mints once, not
six times. Per-definition cursors and their sidecar lock make evaluation
idempotent and single-flight within the workspace.

Fires are reported by `orbit clock tick` and in the dashboard auto-task panel;
the minted task is the durable record. Evaluation creates no scheduler job run.
