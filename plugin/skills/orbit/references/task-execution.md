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

Read relevant decision comments alongside the canonical description. Later
explicitly authorized changes supersede older suggestions; timestamps alone do
not turn an arbitrary comment into authority. Resolve material contradictions
before implementing, without reopening already settled decisions.

Use `context_files` as the modification boundary, not a demand to ingest the
whole repository before editing. Verify paths and inspect the interfaces needed
for the next increment; for directories use `rg --files` to find those targets.
Read enough of each affected file and its consumers to make a correct change.

Then look for related work the author did not link. Treat historical documents
as context, not authority over current requirements:

```bash
orbit tool run orbit.search --input '{"semantic":"<task-id>","limit":5,"model":"<agent-family>"}'
```

Non-blocking — skip it if nothing is relevant, or if the task has no vectors yet.

For personal execution or an authorized takeover, check that the task's `crew`
represents your configured crew; correct stale delegation metadata through the
task tools. A managed worker follows its assigned run and must not reassign
itself to evade the operator's crew choice. Keep `model` provenance separate.

## Step 2 — Plan

`orbit.task.update` with a concrete markdown `plan` — target files, validation
commands, risks — if one doesn't exist.

## Step 3 — Start

The operator owns pipeline submission. Under a managed activity envelope,
perform the assigned leaf mandate and leave dispatch and final delivery
transitions to the pipeline. Direct execution below applies only when the user
and repository policy authorize it; a technical lifecycle transition is not a
substitute for human approval.

For authorized direct pickup (not an already-started managed activity), use `orbit.task.update` with
`status: "in-progress"` and a `note`. It moves `proposed`, `backlog`, `someday`,
or `blocked` → `in-progress`; the transition records existing authorization, not
a new grant. Supply a real plan when starting from `proposed`, `someday` or
`blocked`:

```bash
orbit tool run orbit.task.update --input '{"id":"<task-id>","status":"in-progress","plan":"<implementation and validation steps>","note":"<authorized pickup>","model":"<agent-family>"}'
```

`rejected` is not a pickup state. Reconsider it only when a reviewer or other
authorized decision explicitly requests a revision; first use
`orbit.task.update` to move the task to `backlog`, recording that authorization,
then take it from the backlog in a separate update:

```bash
orbit tool run orbit.task.update --input '{"id":"<task-id>","status":"backlog","comment":"Authorized revision: <reviewer decision>","model":"<agent-family>"}'
```

## Step 4 — Implement and validate

Follow the plan, inspecting files with the provider-native file-read tool.
Verify transitive impact with `rg` or by reading callers directly. Run the
repo-approved verification commands, honoring repo instructions if tests are
forbidden.

Implement in testable increments. Breadth or an ordinary missing API is not a
blocker when the task owns that outcome. A real authority refusal, unavailable
prerequisite or unresolved product decision is: record the specific evidence and
what is needed to proceed. A clean baseline check is not feature validation.

**Keep `context_files` current.** Declare newly identified modification targets
through the task tools before editing, within the approved scope and activity
rules. A declaration does not acquire a lock or expand an already frozen claim
footprint. If the admitted boundary cannot cover the change, request coordinated
re-preparation; do not bypass the conflict or claim guard.

**In a linked pipeline worktree, never use positional `git stash` /
`git stash pop`.** Refs and the stash list are repository-global, so a positional
pop can restore another session's work. Record the worktree's initial
`git rev-parse HEAD` and compare against that explicit baseline with
`git diff <baseline-sha> -- <paths>`.

**Managed `.git` mounts are read-only by design.** In agent-executor sandboxes
and linked job-run worktrees, the repository's `.git` mount is read-only and
must not be worked around by chmod or host-side gitdir writes. Commands that
write to `.git` fail:
- Do not use `git worktree add` to inspect or build other revisions. Extract
  each revision into its own scratch directory outside the checkout:
  `mkdir -p /tmp/base && git archive <sha> | tar -x -C /tmp/base`. That reads
  `.git` without writing it or creating `.git/worktrees/*`.
- When `git checkout -- <path>` fails because it cannot acquire `index.lock`,
  revert the tracked file with `git show HEAD:<path> > <path>`.
- Read-only inspection commands succeed with `GIT_OPTIONAL_LOCKS=0 git status --short`.

**A bare extract is not yet before/after evidence.** Each step below answers a
recorded false result, not a hypothetical one. Before comparing two revisions:

- Give each revision its own build/output directory. Sharing one lets the
  second arm reuse the first arm's artifacts and embedded fixture paths, so it
  never exercises the revision it names.
- Refresh timestamps after extracting (`find <dir> -exec touch {} +`).
  `git archive` and `tar` write the archive's own mtimes, so an extract placed
  beside an existing build can look up to date and rerun a stale binary.
- Turn off compiler caches and wrappers for both arms. A cached baseline has
  been observed producing a binary that contained the *other* tree's tests.
- Verify provenance in the output instead of assuming it: require a string only
  the intended revision can emit, and treat the sibling revision's string
  appearing in an arm as a failed comparison.
- Capture the producer's exit status before trimming output. Redirect to a log,
  record the status, then read the tail. A filter at the end of a pipeline
  reports the filter's success, not the build's failure — this turns a compile
  error into a green validation line.

If the workspace ships a helper for this comparison, use it rather than
hand-rolling the steps; check the workspace's build and CI instructions.

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
work can't be obtained, stop after recording that state. Use `orbit.task.update`
with `status: "in-progress"` only for `proposed`, `backlog`, `someday`, or
`blocked` pickup; an authorized `rejected` revision must return to `backlog`
first. Direct
execution must persist a non-empty `execution_summary` before or with the
review transition.

Exit: eligible work started via `orbit.task.update`, or an authorized rejected
revision returned to `backlog` before pickup; execution summary
persisted; friction checkpoint considered; direct execution advanced to
`review`.
