# Executing a task

Carry a task from intent to verified implementation with explicit lifecycle
tracking. Every `orbit.task.*` call needs `model` — your agent family. The
operator close of a rescued blocked task omits `model`, so the write is not
attributed to an agent; see
[Close a rescued blocked task](../../orbit-orchestrate/references/recovery.md#close-a-rescued-blocked-task).

No task ID yet? Use the authorized intent, clarify only missing requirements,
then use
[task-authoring.md](task-authoring.md). Reviewing someone else's work instead?
[task-review.md](task-review.md).

## Step 1 — Load

`orbit.task.show`, then extract `description` and `acceptance_criteria` (the
required outcome), `plan` (author one if blank or placeholder), `context_files`,
and `status`.

Task tools take `id`, including `orbit.task.show`, `orbit.task.update`,
`orbit.task.artifact.put`, and `orbit.task.artifact.get`. Copy the identifier
from workflow input `task_id`, `ORBIT_TASK_ID`, or the task snapshot's `id`
into that argument; workflow and evidence keys named `task_id` are not
task-tool argument names. Unknown fields are rejected with suggestions,
not accepted as aliases. GitHub run tools take `run` despite returning
`run_id`; task authoring takes `type` (feature, bug, refactor, or chore).
Search statuses require the corpus prefix, such as `task:rejected` or
`friction:resolved`, including when `kind` is supplied separately.

To attach evidence, write a local file under `.orbit/tmp/` and use:

```bash
orbit tool run orbit.task.artifact.put --input '{"id":"<task-id>","source_path":".orbit/tmp/<file>","path":"<artifact name>","model":"<agent-family>"}'
```

`source_path` is the local file; `path` is the stored artifact name.
`orbit tool show <tool.name>` prints the tool's schema. List artifacts with
`orbit.task.show` and `field: "artifacts"` before fetching optional evidence;
read only paths present in the returned metadata array (MCP may wrap it in
`value`).

Read `comments` (chronological, each with `by` and `at`) alongside the
canonical description. An orchestrator or operator refinement posted after the
description supersedes a stale "Suggested direction"/"Suggested fix" section
still sitting in it: implement the comment's direction. An arbitrary comment is
not authority by timestamp alone; resolve material contradictions before
implementing, without reopening already settled decisions.

Use `context_files` as starting targets, not a limit or a demand to ingest the
whole repository before editing. Verify paths and inspect the interfaces needed
for the next increment; for directories use `rg --files` to find those targets.
Read enough of each affected file and its consumers to make a correct change.

When prior context is needed, search distinctive title or description terms with
`orbit.search` using `query`, `kind: "task"`, and a small `limit`. Inspect useful
hits before reformulating. Include `all: true` when checking closed history.

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
`status: "in-progress"` and a `note`. Legal sources are `proposed`, `backlog`,
`someday`, `blocked`, `rejected`, and `review`. The transition records existing
authorization, not a new grant. Supply a real plan from every source except
`backlog`:

```bash
orbit tool run orbit.task.update --input '{"id":"<task-id>","status":"in-progress","plan":"<implementation and validation steps>","note":"<authorized pickup>","model":"<agent-family>"}'
```

Reconsider `rejected` only when a reviewer or other authorized decision
explicitly requests a revision. The table allows `backlog` or `in-progress`.
`in-progress` needs a plan on that write. Returning to `backlog` does not, and
a later pickup from `backlog` is a separate update:

```bash
orbit tool run orbit.task.update --input '{"id":"<task-id>","status":"backlog","comment":"Authorized revision: <reviewer decision>","model":"<agent-family>"}'
```

## Step 4 — Implement and validate

Follow the plan, inspecting files with the provider-native file-read tool.
Verify transitive impact with `rg` or by reading callers directly. Run the
repo-approved verification commands, honoring repo instructions if tests are
forbidden. When the envelope lists `task.final_validation_commands`, the
delivery pipeline runs those on the final candidate after base
synchronization; for a diff, leave them to it and run focused checks plus every
required check the list omits. A clean no-op or already-landed handoff skips
that validation, so it still runs every required check. For a task tagged `no-diff-expected`, in owner or claimed mode,
run no workspace gates or required validation commands and attach no
`no-diff.json`, `already-landed.json`, or workspace validation logs. Run only
commands the task's own brief requires: a QA sweep may build and run the
product, and a review may reproduce a finding when its brief asks for it.
Keep the brief's evidence artifacts, persist the execution summary, and leave
the worktree clean; the pipeline hands off on the tag.

For tasks without `no-diff-expected`, when a clean task checkout is already
covered by this task's or a sibling task's landed commit, verify the current
acceptance criteria and attach
`already-landed.json` plus captured validation logs with `orbit.task.artifact.put`
in the current run, including the first attempt. The artifact names the current
task and the covering task separately. `git_commit` checks the pinned HEAD,
covering commit marker and ancestry, unchanged task scope, clean tree, and
required validation logs. A success summary alone does not satisfy that gate.
`already-landed.json` has `schema_version` 1, `task_id`, `run_id`,
`tested_head`, `covering_commit`, `covering_task_id`, `scope`,
`required_commands`, `validation` and `criteria_evidence`; unknown fields are
refused. `scope` must equal the task's current scope exactly: `title`,
`description`, `acceptance_criteria`, `plan`, `context_files`, `tags`,
`relations`, `required_tools`, `type` and `comments`, where each comment is
reduced to `at`, `by` and `message`. Drop the receipt ids (`operation_id`)
that `orbit.task.show` adds to comments; a comment with any extra field is
refused. `criteria_evidence` is an array of non-empty strings, one per
acceptance criterion in order, never objects. Each `validation[]` entry
flattens `command`, `outcome`, `role` and `log_artifact` as siblings; every
required command's entry carries `outcome: "passed"` and `role: "required"`.
The role vocabulary is `required`, `expected_failure`, `excluded`,
`superseded` and `diagnostic`; other values such as `acceptance` or `gate` are
refused.
The already-landed verifier requires exactly one passed `required` entry per
`required_commands` command and refuses extra required commands. It also accepts
additional `expected_failure` and `diagnostic` entries, without counting them
as required coverage. Every accepted entry must reference a JSON log carrying
the report's exact `run_id`, `tested_head`, and `command`, an integer `exit_code`,
and string `output`. Required checks must exit zero; expected failures must exit
nonzero. Diagnostic outcomes and exit status do not determine acceptance.
`excluded` and `superseded` entries are not accepted in already-landed evidence.
If a required check or covering evidence is unavailable, record the blocker;
do not claim a verified already-landed result.

Implement in testable increments. Breadth or an ordinary missing API is not a
blocker when the task owns that outcome. A real authority refusal, unavailable
prerequisite or unresolved product decision is: record the specific evidence and
what is needed to proceed. A clean baseline check is not feature validation.

### Checking a UI in a browser

When the host has a verified headless browser kit, your environment carries
`PLAYWRIGHT_BROWSERS_PATH`, `PLAYWRIGHT_MODULE` and `ORBIT_BROWSER_KIT_ENV`.
Check a changed page by serving it on loopback and scripting headless
Chromium. In the same shell, source the kit first, then import `chromium`
from the module:

```bash
. "$ORBIT_BROWSER_KIT_ENV" && node --input-type=module -e '
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE);
// launch, goto http://127.0.0.1:<port>/, assert, screenshot to .orbit/tmp/
'
```

Save screenshots under `.orbit/tmp/`, open them to look at the result, and
attach the ones that prove a criterion. Don't install Playwright, download a
browser or run `sudo` yourself. When the variables are absent, the host has no
verified kit: record the browser check as unavailable, naming
`orbit browser-kit install` as the host fix. A missing `lib*.so`, a
Skia/FontConfig crash or `Executable doesn't exist` is a host kit defect, not
a defect in the code under test.

### Verified no-change implementation

This section applies only to tasks without `no-diff-expected`. When the
implementation correctly needs no file changes, and all required
validation passes, attach `no-diff.json` and one captured validation log per
check with `orbit.task.artifact.put`. This is only for a genuine validated
no-op; never use it to bypass failed validation or pending worktree changes.
The evidence must match the pinned task, current run (or its recorded retry
lineage), and tested HEAD:

```json
{
  "schema_version": 1,
  "task_id": "<current-task-id>",
  "run_id": "<current-run-id>",
  "tested_head": "<pinned-head-sha>",
  "reason": "The requested behavior already holds; no change is required.",
  "validation": [
    {
      "command": "<required validation command>",
      "exit_code": 0,
      "log_artifact": "validation.json"
    }
  ]
}
```

Each referenced validation log is JSON with `run_id`, `tested_head`,
`command`, `exit_code` set to `0`, and captured `output`. Commands must be
non-empty and unique. The commit verifier also requires a clean worktree and
the current HEAD to equal `tested_head`; otherwise reconcile the changes and
rerun validation before attaching the evidence.

### Criteria owed after deploy

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
its typed handoff, which the owner checks again before it accepts it. The
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
nothing. Once every owed criterion is recorded met, the operator approves the
task to `done` (`orbit task approve`); a failed one is repaired through an
ordinary new task. In the execution summary mark each owed criterion `not_run`
(owed after deploy), never passed.

**Keep `context_files` current.** You may declare newly identified modification targets
through the task tools before editing, within the approved scope and activity
rules. `orbit.task.update` replaces the whole context list when `context_files`
or the legacy `context` alias is supplied. Omitting both preserves the list;
use `context_files: []` to clear it. A comma-only string such as `","` also
clears the list through either field. An empty string is rejected by the tool
(the human CLI's `--context ""` clears it). The string `"[]"` is treated as a
selector, not an empty list.

To extend scope, first read the current durable `context_files` with
`orbit.task.show`. Send the deduplicated union of **all existing selectors and
the additions**, never just the additions. For example, if the current list is
`["file:src/existing.rs", "dir:tests"]` and the task creates `src/new.rs`:

```bash
orbit tool run orbit.task.show --input '{"id":"<task-id>","model":"<agent-family>","fields":["context_files"]}'
orbit tool run orbit.task.update --input '{"id":"<task-id>","model":"<agent-family>","context_files":["file:src/existing.rs","dir:tests","file:src/new.rs"],"allow_missing_context":true}'
orbit tool run orbit.task.show --input '{"id":"<task-id>","model":"<agent-family>","fields":["context_files"]}'
```

Re-read after every context update and verify that every prior selector and
every addition is present. If one is absent, repeat the read → full-union
update → verify sequence. Use `allow_missing_context: true` to declare files
before creation; it records durable creation intent for exactly those
selectors, so later writes and task preparation keep them. Selectors are a starting point, not a limit: you may change
any path the work requires. Delivery commits every changed path except
`.orbit/tmp/` scratch and gitignored output, and widens the selectors with an
exact `file:` entry for each uncovered path, recording which step introduced
it. Declaring a path yourself is optional, but keeps the scope readable.

In claimed mode, use the injected list and report additions in
`context_files_added`; they are recorded for the owner without changing the
frozen footprint. A declaration does not acquire a lock. Paths outside the
footprint still deliver: the owner widens the footprint from the published
candidate at handoff. Only Git or `.orbit` metadata, environment files and
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

## Step 5 — Summarize and hand off

Persist `execution_summary` via `orbit.task.update` **first**.

Then consider friction: if the task surfaced a contradicted assumption, a
recurring failure mode, a non-obvious gotcha, or an incident root cause, record
it — see [friction.md](friction.md). Then:

- **Under an activity envelope** (e.g. `agent_implement`): persist the summary
  only. The pipeline owns the `review` transition after commit/merge/PR steps
  succeed.
- **Claimed mode** (`input.claimed` is true, a distributed-drain leaf):
  another machine owns the task. Work from the injected envelope; you are not
  granted `orbit.task.update`. `orbit.task.show` reads only the claimed task
  through the run's coordinator. Return the summary as the output's
  `execution_summary` (with `context_files_added` and `comment` for anything
  you would have written to the task); the pipeline's handoff carries it to
  the owner. When the task asks you to record a `regression_from` relation,
  return the culprit task IDs as `regression_from` and name the culprit commit
  in the summary; the owner checks and records the relation at handoff. An owner-routed tool that answers unreachable is not a task
  failure. A task you file must be `spawned_from` the claimed task; a claimed
  `delivery-code-review` or `code-review` task may also give each finding a
  `regression_from` relation to its culprit. `orbit.search` does not reach the
  owner, so skip the duplicate search and say so; the owner's triage dedupes.
  Return a finding the owner still refuses in `unfiled_findings` as
  `{title, description}` objects (plain strings fail the implement step before
  anything is committed or pushed); the handoff attaches it to the claimed task
  as `unfiled-findings.json`.
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
with `status: "in-progress"` for `proposed`, `backlog`, `someday`, `blocked`,
`rejected`, or `review`. A plan is required except from `backlog`. An
authorized `rejected` revision may instead return to `backlog` first. Direct
execution must persist a non-empty `execution_summary` before or with the
review transition.

Exit: eligible work started via `orbit.task.update`, including an authorized
`rejected` revision moved to `backlog` or `in-progress`; execution summary
persisted; friction checkpoint considered; direct execution advanced to
`review`.
