# Executing a task

Carry a task from intent to verified implementation with explicit lifecycle
tracking. Every `orbit.task.*` call needs `model` — your agent family.

No task ID yet? Use the authorized intent, clarify only missing requirements,
then use
[task-authoring.md](task-authoring.md). Reviewing someone else's work instead?
[task-review.md](task-review.md).

## Step 1 — Load

`orbit.task.show`, then extract `description` and `acceptance_criteria` (the
required outcome), `plan` (author one if blank or placeholder), `context_files`,
and `status`.

Also read `comments` (chronological, each with `by` and `at`) before trusting
the description. A description is written once at filing time and is never
rewritten when an orchestrator later posts a refinement, so a comment that
postdates the description always takes precedence over a "Suggested
direction"/"Suggested fix" section still sitting in that description —
implement the comment's direction, not the description's.

Read each `file:` target with the provider-native file-read tool. For a `dir:`
selector, do not call the file-read tool on the directory: after verifying it
resolves beneath the workspace root, use `rg --files <directory>` to list it,
then read its key files individually.

Then look for related work the author did not link. Treat historical documents
as context, not authority over current requirements:

```bash
orbit tool run orbit.search --input '{"semantic":"<task-id>","limit":5,"model":"<agent-family>"}'
```

Non-blocking — skip it if nothing is relevant, or if the task has no vectors yet.

## Step 2 — Plan

`orbit.task.update` with a concrete markdown `plan` — target files, validation
commands, risks — if one doesn't exist.

## Step 3 — Start

The operator owns pipeline submission. Under a managed activity envelope,
perform the assigned leaf mandate and leave dispatch and final delivery
transitions to the pipeline. Direct execution below applies only when the user
and repository policy authorize it; a technical lifecycle transition is not a
substitute for human approval.

For first-time pickup or resuming eligible work, use `orbit.task.start` with a
`note`. It moves `proposed`, `backlog`, `someday`, or `blocked` → `in-progress`
and records approval. Starting from `proposed` still requires a real plan:

```bash
orbit tool run orbit.task.start --input '{"id":"<task-id>","note":"<why work is starting or resuming>","model":"<agent-family>"}'
```

`rejected` is not a pickup state for `orbit.task.start`. Reconsider it only
when a reviewer or other authorized decision explicitly requests a revision;
use `orbit.task.update` to move the task to `in-progress`, recording that
authorization in the update:

```bash
orbit tool run orbit.task.update --input '{"id":"<task-id>","status":"in-progress","comment":"Authorized revision: <reviewer decision>","model":"<agent-family>"}'
```

## Step 4 — Implement and validate

Follow the plan, inspecting files with the provider-native file-read tool.
Verify transitive impact with `rg` or by reading callers directly. Run the
repo-approved verification commands, honoring repo instructions if tests are
forbidden.

**Keep `context_files` current.** If implementation surfaces a file outside the
original list, append its canonical selector via `orbit.task.update` as soon as
you discover it — don't wait until handoff. Conflict detection reads live from
in-flight tasks, not only from reservation records, so an up-to-date list is how
a worker excludes others from files it owns. Reservation itself is the system's
job: there is no worker-callable lock tool, and none should be reached for. One
caveat — an added entry binds only reservations requested *after* the update; it
cannot retroactively revoke one a concurrent run already holds.

**In a linked pipeline worktree, never use positional `git stash` /
`git stash pop`.** Refs and the stash list are repository-global, so a positional
pop can restore another session's work. Record the worktree's initial
`git rev-parse HEAD` and compare against that explicit baseline with
`git diff <baseline-sha> -- <paths>`.

**Managed `.git` mounts are read-only by design.** In agent-executor sandboxes
and linked job-run worktrees, the repository's `.git` mount is read-only and
must not be worked around by chmod or host-side gitdir writes. Commands that
write to `.git` fail:
- Do not use `git worktree add` to inspect or build other revisions; use
  `mkdir -p /tmp/base && git archive <sha> | tar -x -C /tmp/base` to extract a
  baseline revision without touching `.git` or creating `.git/worktrees/*`,
  then build with a separate build cache/target directory.
- When `git checkout -- <path>` fails because it cannot acquire `index.lock`,
  revert the tracked file with `git show HEAD:<path> > <path>`.
- Read-only inspection commands succeed with `GIT_OPTIONAL_LOCKS=0 git status --short`.

## Step 5 — Summarize and hand off

Persist `execution_summary` via `orbit.task.update` **first**.

Then consider friction: if the task surfaced a contradicted assumption, a
recurring failure mode, a non-obvious gotcha, or an incident root cause, record
it — see [friction.md](friction.md). Then:

- **Under an activity envelope** (e.g. `agent_implement`): persist the summary
  only. The pipeline owns the `review` transition after commit/merge/PR steps
  succeed.
- **Direct execution** (no envelope): persist the summary *and* move to `review`
  via `orbit.task.update`.

The generated PR body already supplies `## Task` / `## Execution Summary` /
`## Validation` / `## Branch Freshness`, so don't duplicate those headings.
Required content:

```markdown
Outcome: success | failed
Changes:
- <what changed and why>
Assessment: <short quality assessment>
```

Include when relevant: `Strategic decisions:`, `Design weaknesses / risks:`
(with Severity/Mitigation), `Deviations from original plan:` (with
Justification), `Recommended follow-ups:`.

## Lifecycle rules

One task per activity invocation — no multiplexing. Ask clarifying questions
before implementing if material ambiguity remains. If approval for `proposed`
work can't be obtained, stop after recording that state. Use `orbit.task.start`
only for `proposed`, `backlog`, `someday`, or `blocked` pickup; an authorized
`rejected` revision uses `orbit.task.update` to `in-progress` instead. Direct
execution must persist a non-empty `execution_summary` before or with the
review transition.

Exit: eligible work started via `orbit.task.start`, or an authorized rejected
revision moved to `in-progress` via `orbit.task.update`; execution summary
persisted; friction checkpoint considered; direct execution advanced to
`review`.
