# Authoring a task: tasks that create files

Contents:

- [Split, migration and other tasks that create files](#split-migration-and-other-tasks-that-create-files)

## Split, migration and other tasks that create files

This rule binds every filer — an operator, an orchestrator, a review, QA or
health sweep, and any other auto-task template — not only the implementer who
later creates the file. Before calling `orbit.task.add` for a split, a move or
migration, a deduplication that unifies copies into a new module, or any other
task whose outcome creates a file:

1. Scan the draft title, description, scope and acceptance criteria for every
   path the task names as new, moved-to, split-out or to-be-created. Naming a
   new file only in prose grants nothing.
2. List each such path exactly once as its canonical `file:` selector (or
   `dir:` for a new directory) in `context_files`, alongside the verified
   existing targets. A missing `file:` whose parent directory exists gets an
   automatic creation grant for that exact path, with a warning. Use
   `allow_missing_context: true` when an intended missing target has no
   existing parent or is another missing selector type; that explicit grant
   covers every missing selector in the list, so review them all before filing.
3. If an exact destination cannot be named yet, do not file the task. Record
   the unnamed destination as an evidence gap in your own execution summary or
   artifact instead; never file first and leave task-pilot to hold promotion
   for an operator to authorize a footprint the filer could have granted.
4. Read the filed task back with `orbit.task.show` and confirm its history
   carries a creation grant for each new target that is still missing. If any
   lacks one, reauthorize through `orbit.task.update` with the full
   `context_files` list plus `allow_missing_context: true`, and note the
   correction in your summary.

Without `allow_missing_context`, a newly submitted missing `file:` whose
parent directory exists receives an automatic grant for that exact selector
and a warning. A task's existing grant is accepted again without the flag and
retained while a replacement `context_files` list keeps that exact selector;
omitting it revokes the grant.

Other missing targets are refused unless the
task's own worker update qualifies for the linked-worktree relaxation below.
Explicit `allow_missing_context: true` grants every missing selector in the
supplied list, including targets without an existing parent; review the whole
list before using it.

A worker updating its own task through `orbit.task.update` or `orbit task
update` from its run's linked worktree gets an existence relaxation for that
task. A newly declared missing file with an existing parent still receives
an automatic grant; other missing selectors accepted only by this worker relaxation are
returned as `context_files_unverified` and do not receive creation grants.

A dashboard request is never bound to a run, so it uses the ordinary
missing-selector rules.

Task writes on every surface return the same advisory
`warnings` (on add: a disabled required tool, an unreadable parent, or a
proposed task added without `context_files` and without the `no-diff-expected`
tag, regardless of type; on add and update: an unreadable dependency; on add,
approval, or updates to status, tags, context files or complexity: a backlog
task with no footprint; and on approval: task-pilot blockers).

Comments and
other metadata edits do not repeat the footprint warning; readiness remains
visible in the task response. Warnings never refuse the write.
`warnings` is optional write-response metadata: it is absent when empty and is
returned only on an object response, so omit `fields` on the write to receive it.

It is not a projection selector, so a request selecting `warnings` is refused
before persistence; fix the request by dropping that selector rather than
replaying a write that already succeeded.
Missing-file selectors remain valid declarations; do not prune them because a
checkout cannot yet resolve them.

Empty context holds no context lock, so a
drain or ship with more than one slot holds such a task: it waits for an
enabled task pilot to prepare it (`awaiting_footprint`), and otherwise runs
only alone (`awaiting_exclusive_slot`), unless an operator has tagged it
`readiness-waived`. A single-slot run, an explicit `orbit run ship <id>` and
owner pull admission still admit it.

Do not invent paths to satisfy admission,
and do not add `readiness-waived` yourself: only an operator adds it, and an
agent's write that adds it is refused. Task-pilot can prepare unknown
targets, and a live pilot's preparation (its reservation from prepare, then
its checkpoint) still holds its tasks until that run settles.

**Never file with `--status backlog`, or promote to `backlog`, a task with
empty `context_files` while a drain is live.** File it `proposed`, run the
pilot or set selectors, then promote. On 2026-10-10 three tasks filed into a
live four-slot drain's backlog with no selectors all edited
the website's then-shared stylesheet concurrently, and two of their runs were
cancelled. Creation, approval and readiness-field updates warn when they leave
such a task in the backlog.

A `proposed` or `backlog` task reports its `readiness`: `{ready, gaps}`,
each gap with `code`, `severity`, `message` and `fix`. The `orbit.task.add`
and `orbit.task.update` responses carry it, `orbit task show` prints a
`Readiness:` line and the dashboard shows a chip. `missing_context_files`
blocks drain approval of a proposed task and is advisory in the backlog,
where it names the missing file lock;
`unassessed_complexity` blocks both.

A `no-diff-expected` tag clears both.
An operator's `readiness-waived` tag makes a backlog task's gaps advisory: the
task is admitted with its configured or default crew, even with unassessed
complexity. It does not reach the proposed stage, so approval still needs a
prepared task. Host OS, labels, browser kit, human-crew assignment,
dependencies, real lock holders and live pilot holds still apply.
Read the gaps after filing and fix what blocks, or leave it to task-pilot.
