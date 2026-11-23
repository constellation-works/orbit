# Executing a task: criteria owed after deploy

Contents:

- [Criteria owed after deploy](#criteria-owed-after-deploy)

## Criteria owed after deploy

A criterion the task's latest task-pilot assessment lists in `post_deploy` can
be evidenced only by the deployed build on the host it names, so no candidate
meets it before merge. Do not fail the run or return a blocker for it. Gather
what the candidate can show and return the criterion in the implementer output:

```json
{"owed_criteria": [{"criterion": 1, "host": "<host>", "command": "<measurement command>",
  "target": "p95 <= 150 ms", "candidate_evidence": "<what the candidate showed>"}]}
```

The implement step admits each entry against the pilot's finding, with the
host it names, and fails the step for an owed criterion the pilot did not type.
Every other unmet criterion fails as before. An owner run records the admitted
entries as a `post_deploy_owed` history event; a claimed leaf carries them in
its typed handoff, which the owner checks again before it accepts it.

The
delivery proceeds through PR, review and landing, then settles in `review`
instead of `done`. Reusing that candidate in a fresh delivery run preserves
its unresolved owed criteria even when implementation is skipped.
`orbit task show` and the dashboard task detail list each
owed criterion with its host, command, target and state; external-merge
settlement leaves the task in `review` too.

Measuring afterwards is the operator's job; nothing re-measures on its own. A
human task comment whose first line is `post-deploy: met <N>` or
`post-deploy: failed <N>`, with the measurement's evidence on the lines after
it, records `post_deploy_recorded`. An agent comment of that shape records
nothing.

Once every owed criterion is recorded met, the operator approves the
task to `done` (`orbit task update <id> --approve`); a failed one is repaired
through an ordinary new task. In the execution summary mark each owed criterion `not_run`
(owed after deploy), never passed.

**Keep `context_files` current.** You may declare newly identified modification targets
through the task tools before editing, within the approved scope and activity
rules. `orbit.task.update` replaces the whole context list when `context_files`
is supplied. Omitting `context_files` preserves the list; use
`context_files: []` to clear it.

A comma-only string such as `","` also clears
the list. An empty string is rejected by the tool (the human CLI's
`--context ""` clears it). The string `"[]"` is treated as a selector, not an
empty list. A `context` input is refused; the only field is `context_files`.

To extend scope, first read the current durable `context_files` with
`orbit.task.show`. Send the deduplicated union of **all existing selectors and
the additions**, never just the additions. For example, if the current list is
`["file:src/existing.rs", "dir:tests"]` and the task creates `src/new.rs`:

```bash
orbit tool run orbit.task.show --input '{"id":"<task-id>","fields":["context_files"]}'
orbit tool run orbit.task.update --input '{"id":"<task-id>","context_files":["file:src/existing.rs","dir:tests","file:src/new.rs"],"allow_missing_context":true}'
orbit tool run orbit.task.show --input '{"id":"<task-id>","fields":["context_files"]}'
```

Re-read after every context update and verify that every prior selector and
every addition is present. If one is absent, repeat the read → full-union
update → verify sequence. Use `allow_missing_context: true` to declare files
before creation; it records durable creation intent for exactly those
selectors, so later writes and task preparation keep them.

Selectors are a starting point, not a limit: you may change
any path the work requires. Delivery commits every changed path except
`.orbit/tmp/` scratch and gitignored output, and widens the selectors with an
exact `file:` entry for each uncovered path, recording which step introduced
it. Declaring a path yourself is optional, but keeps the scope readable.

A source-repair run (an operator-admitted repair of an already published PR)
cannot declare: its publication authority is bound to the admitted scope, so
`orbit.task.update` refuses a changed `context_files` from that run and writes
nothing. Resend the summary or comment without `context_files` and keep the
change; delivery widens the selectors over it.

In claimed mode, use the injected list and report additions in
`context_files_added`; they are recorded for the owner without changing the
frozen footprint. A declaration does not acquire a lock. Paths outside the
footprint still deliver: the owner widens the footprint from the published
candidate at handoff.

Only Git or `.orbit` metadata, environment files and
symlinks are refused. Protected metadata names and environment patterns (including
`.envrc`) ignore ASCII case on every host: `.Orbit/`, `.GIT/`, `.ENV` and
`.Env.local` are refused even on Linux.

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
  each revision into its own scratch directory under `.orbit/tmp/`:
  `mkdir -p .orbit/tmp/base && git archive <sha> | tar -x -C .orbit/tmp/base`. That reads
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
