# Authoring a task: tasks that create files

Contents:

- [Split, migration and other tasks that create files](#split-migration-and-other-tasks-that-create-files)
- [Rust parent module declarations](#rust-parent-module-declarations)
  - [Deterministic parent-declaration check](#deterministic-parent-declaration-check)

## Split, migration and other tasks that create files

This rule binds every filer — an operator, an orchestrator, a review, QA or
health sweep, and any other auto-task template — not only the implementer who
later creates the file. Before calling `orbit.task.add` for a split, a move or
migration, a deduplication that unifies copies into a new module, or any other
task whose outcome creates a file:

1. Scan the draft title, description, scope and acceptance criteria for every
   path the task names as new, moved-to, split-out or to-be-created. Naming a
   new file only in prose grants nothing. For any new or moved Rust module
   (including a `dir:` destination), also trace its parent declaration chain
   under [Rust parent module declarations](#rust-parent-module-declarations).
2. List each such path exactly once as its canonical `file:` selector (or
   `dir:` for a new directory) in `context_files`, alongside the verified
   existing targets and any self-owned parent declaration selectors required by
   the parent check below. A missing `file:` whose parent directory exists gets
   an automatic creation grant for that exact path, with a warning. Use
   `allow_missing_context: true` when an intended missing target has no
   existing parent or is another missing selector type; that explicit grant
   covers every missing selector in the list, so review them all before filing.
   Never add a creation grant for a missing parent owned by another task; link
   the verified predecessor in `dependencies` instead.
3. If an exact destination (including a required new parent layout) cannot be
   named yet, do not file the task. Record the unnamed destination as an
   evidence gap in your own execution summary or artifact instead; never file
   first and leave task-pilot to hold promotion for an operator to authorize a
   footprint the filer could have granted.
4. Read the filed task back with `orbit.task.show` and confirm its history
   carries a creation grant for each new target that is still missing (and no
   duplicate grant for a predecessor-owned path). If any self-owned target
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

## Rust parent module declarations

When a task creates or moves a Rust source file or module directory under `src/`
(including `src/**/tests/*.rs` under the sibling-test layout), a `dir:` or child
`file:` selector grants only the child path itself — it does not authorize or
lock the enclosing parent module files (`main.rs`, `lib.rs`, `mod.rs`, or
`<dir>.rs`) needed to declare `mod <child>;` (or `#[path = "..."]`) so the
crate compiles and stays free of orphan modules. Skip this check only for
Cargo-discovered entrypoints (`src/lib.rs`, `src/main.rs`, `src/bin/*.rs`,
`src/bin/*/main.rs`, and crate-root `tests/*.rs` integration binaries).

Starting from each new or moved Rust module destination at the pinned base
revision, trace its parent declaration chain upward until reaching an existing
file already reachable from the crate root. For each missing or unwired parent
declaration on that chain, resolve it under exactly one of these three cases:

1. **New child module needing a new parent (owned by this task):** neither the
   immediate parent (`<dir>/mod.rs` or `<dir>.rs`) nor a verified predecessor
   supplies it at the pinned revision.
   - Choose the single exact parent path matching the crate layout or approved
     design (`<dir>/mod.rs` or `<dir>.rs`; never list both or guess). If the
     parent layout cannot be determined, do not file: record an evidence gap
     instead.
   - Add `file:<parent-path>` to `context_files`, keep walking up to declare the
     existing enclosing file (`src/lib.rs`, `src/main.rs`, or an existing
     `mod.rs`) as a verified existing `file:` selector (or add each intermediate
     missing parent the same way), and pass `allow_missing_context: true` so
     every new selector receives an explicit `context_creation_authorized`
     grant. Confirm in `orbit.task.show` history that both the child and every
     self-owned new parent appear in the grant.
2. **Existing parent needing wiring:** the declaration file (`main.rs`, `lib.rs`,
   `mod.rs`, or `<dir>.rs`) already exists at the pinned revision and does not
   yet declare `mod <child>;` (or the `#[path = "..."]` attribute or re-exports
   required to reach the child).
   - Declare that existing parent as a verified existing `file:` selector in
     `context_files` and explain the wiring edit in the description. Never
     request a creation grant for an existing file, and omit the selector when
     the parent already declares the child.
3. **Verified predecessor supplying the parent (owned by another task):** the
   parent declaration file does not exist at the pinned revision, and another
   task owns creating it.
   - **Never grant a path owned by another task** and never assume ownership from
     a task title or prose reference alone. Inspect the candidate predecessor
     with `orbit.task.show` to confirm both that it is recorded in this task's
     `dependencies` and that either it is already `done` (so the parent is
     present at the base — re-classify as case 2) or its own `context_files`
     and `context_creation_authorized` history explicitly cover creating that
     exact parent selector and wiring its enclosing root.
   - If the predecessor is missing from `dependencies`, add it to `dependencies`.
     If its grants do not cover creating the parent, it is not a verified
     supplier: update or coordinate the owning predecessor first, or record an
     evidence gap rather than duplicating the creation grant here.
   - When verified, omit the predecessor-owned parent from this task's creation
     grants, name the predecessor task ID and supplied parent path under
     `## Scope and prerequisites` in the description, and keep the blocking edge
     in `dependencies`. If this task must also wire its own `mod <child>;`
     line into that parent after the predecessor lands, record that wiring
     dependency in `## Scope and prerequisites` so execution and re-piloting
     wire it as an existing file once the predecessor lands without creating a
     duplicate creation grant.

### Deterministic parent-declaration check

Run this fixed check at the pinned base revision `<rev>` (for example `HEAD`)
before calling `orbit.task.add` and again after filing:

1. **Check parent existence and existing declarations at `<rev>`:**

   ```sh
   git ls-tree -r --name-only <rev> -- <crate>/src/lib.rs <crate>/src/main.rs <crate>/src/<parent>.rs <crate>/src/<parent>/mod.rs
   git grep -n -E '\bmod[[:space:]]+<child>\b' <rev> -- <crate>/src/<parent-file>
   ```

   An absent parent with no `mod <child>;` match requires either case 1 (new
   parent owned here) or case 3 (verified predecessor); an existing parent
   without a `mod <child>;` match requires case 2 (existing `file:` selector).
2. **Verify every claimed predecessor before omitting a missing parent:**

   ```sh
   orbit tool run orbit.task.show --input '{"id":"<predecessor-id>","fields":["id","status","dependencies","context_files","history"]}'
   ```

   Confirm `<predecessor-id>` is listed in this task's `dependencies` and its
   `context_files` plus latest `context_creation_authorized` history entry
   explicitly cover `file:<parent-path>`.
3. **Confirm filed grants and non-duplication after `orbit.task.add` or `orbit.task.update`:**

   ```sh
   orbit tool run orbit.task.show --input '{"id":"<filed-id>","fields":["context_files","dependencies","history"]}'
   ```

   Verify that every self-owned new child and new parent appears once in
   `context_files` and in the latest `context_creation_authorized` history
   entry, every existing parent needing wiring appears as an existing `file:`
   selector, and no predecessor-owned parent carries a duplicate creation grant
   on this task.
