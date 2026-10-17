# Authoring a task

Give the implementer a coherent outcome, enough evidence to start, and a way to
prove completion. A small fix may need a paragraph and two criteria; a larger
change needs explicit boundaries. The implementer authors the detailed plan at
pickup. Do not confuse a task description with a design document or a transcript.

## Establish the contract

- **Problem and outcome:** name the concrete trigger, current behavior and desired
  result. Include reproduction/evidence for a bug; distinguish observations from
  hypotheses. Cite the authoritative design when relevant and state any approved
  decisions that supersede it.
- **Owned work:** say what this task delivers and what it deliberately leaves to
  another task. Missing ordinary methods, types, adapters and tests needed for
  that result are implementation work, not an external dependency.
- **Prerequisites:** name actual task IDs and the capability each supplies. Record
  blocking edges in `dependencies` through `orbit.task.update` right after
  creation (`orbit.task.add` refuses that field), not only in prose. Do not require an upstream
  slice to demonstrate behavior that needs its downstream consumer: use the
  agreed interface and isolated fixtures, and assign end-to-end proof to integration.
- **Decisions and constraints:** separate settled product choices from choices the
  implementer can make. Include compatibility, authority, rollout or resource
  constraints only where they affect this task. Ask for a consequential missing
  decision; do not ask for permission to make ordinary implementation choices.
- **Acceptance:** define observable behavior and evidence, including important
  refusal/negative cases. Commands are validation methods, not substitutes for an
  expected result. Distinguish implemented, tested, merged and deployed when those
  differ; do not make an implementation task require an unauthorized live rollout.

Split work when independently deliverable outcomes or real dependency boundaries
justify it. A coherent cross-layer change can remain one task; file count and
breadth alone are not blockers. Each slice must be verifiable at its own boundary.
Keep incomplete entry points unavailable when later slices are needed for safety.

## Tasks that fix frictions

Friction resolution on landing is automatic on the owner. Name each friction
in the description or acceptance criteria, preferably also with
`relations: [{"type":"resolves","target":"<friction-id>"}]`. Targets are
workspace-local. Landing resolves open or triaged records, preserves their
original bodies, and appends the task, delivered commit, PR number and time.
Already-resolved records stay unchanged; the task history records resolutions
and failed writes for retry.

Treat a criterion such as "Friction <friction-id> is resolved with the merge
sha" as a landing obligation. The implementer proves the repair and hands it
off; the owner supplies the merge evidence after delivery. Do not ask a worker
to obtain a future merge SHA or write the owner's friction corpus. Local and
already-landed handoffs use the candidate or covering commit; a verified
no-diff handoff uses its synchronized base. These paths record PR `none`.
Every valid friction ID in the description or criteria is a resolution target,
including background citations, so mention unrelated reports in comments
instead. See [friction](friction.md) for workspace ownership and retry rules.

## Prepare and create

1. Select the authoritative workspace. For findings discovered by review, QA,
   friction or triage, search open and closed work before filing a new owner.
   For an explicit user request, reuse a supplied task; search when duplication is
   plausible rather than turning every request into an investigation.
   See [search](search.md) for lexical query guidance.
2. Inspect the likely modification anchors and relevant interfaces. Background
   reading belongs in the description, not the lock footprint. Use targeted
   reads; a task does not need a dump of the surrounding subsystem.
3. Write the contract and criteria. Confirm the assigned lane can obtain the
   evidence: deterministic fixtures for service/time/filesystem behavior,
   required repository gates, and explicit platform or operator handoffs where
   necessary. See [task fields](task-fields.md) for `required_tools` and capabilities.
4. Create through `orbit.task.add` with `model` attribution. Required inputs are
   title, description, workspace and assessed complexity (`low`, `medium`, `hard`,
   `xhard`).
   Use the advertised schema; detailed `plan` belongs to pickup, not task creation.
   Creation does not approve promotion, dispatch or completion.
5. Read the returned ID using `fields: ["id"]` or a JSON parser. Never truncate a
   write response with `head`/`cut`. If the result is uncertain, list/search before
   retrying: a lost reply does not mean the task was not created.
6. Wire `dependencies` (and any `child_of` relation) with `orbit.task.update`
   on the returned ID; creation refuses `dependencies`.

```bash
orbit tool run orbit.task.add --input '{
  "title": "<concrete outcome>",
  "description": "<problem, scope, prerequisites and constraints>",
  "acceptance_criteria": ["<behavior and evidence>"],
  "context_files": ["file:<verified-modification-target>"],
  "workspace": "<discovered-selector>",
  "complexity": "medium", "type": "feature", "model": "<agent-family>",
  "fields": ["id"]
}'
```

Assign `crew` to the actual intended implementer. If you will implement the task
personally, use your own configured crew rather than the crew you would have
chosen for delegation. On an authorized takeover, correct a stale assignment
before continuing; do not leave another crew named for work it is not doing.
`model` is tool-call provenance (agent family), while `orchestrator` identifies
who prepared/supervised the work; neither substitutes for execution `crew`.

Task IDs come from the store. Use task tools to change records, never edit their
filesystem projections. Reuse existing tags. Use `feature`, `bug`, `refactor` or
`chore` for type. An update may reassess complexity but cannot reset it to
`unassessed`. See [task fields](task-fields.md) for dependency/relationship semantics,
duplicate handling and additional tool grants.

## Host OS routing

Tag a task `os:linux`, `os:macos` or `os:windows` when only a host running that
OS can do it: a `sandbox-exec` failure needs macOS, a systemd change needs Linux.
No `os:` tag means any host; several mean any one of them. The namespace is
reserved: `task.add` and `task.update` reject any other `os:*` value (`os:mac`,
`os:osx`), so check the spelling the error names. Matching ignores case.

Admission reads the tags every time it considers a task. A local drain, ship
discovery and `orbit run ship` start the task only on a host of a named OS, and
an owner hands a pull-drain claim only to a follower of one. Elsewhere the
task stays in `backlog` with the wait named (`waits for a macos host
(os:macos)`) in readiness, `orbit run show` and the dashboard Drain card.
When task-pilot finds a criterion that needs native evidence from one OS and
the task has no `os:` tag, it adds that tag itself; it never replaces an `os:`
tag you set. If your tag names another OS, a host that cannot produce the
evidence leaves the task waiting as `native_os_required`. When only the
owner's own machine can produce the evidence (timings against its live store,
say), the pilot records a machine requirement and the owner refuses pull
claims from every other machine.

For a criterion that needs a test to run under nested Seatbelt or Bubblewrap,
name the required validation gate that runs that test. The worker sandbox may
prevent the nested sandbox from starting; the sanctioned deferred-sandbox
replay then runs that gate on the claim host, outside the worker sandbox. A
passing host replay is the native-sandbox evidence and satisfies the criterion,
including on a verified clean-base NoDiff handoff (a `no-diff-expected` skip
runs no gate, so it replays nothing). The in-worker skip is not evidence by
itself; the passing replay is. Do not write acceptance criteria that require
"no sandbox deferral" or say a skip cannot count as evidence for these tests.
The replay covers only a required gate named by the deferred record, not a
standalone test outside that gate.

To route a task an auto-task or a sweep already minted, retag it with
`orbit.task.update`, sending the full tag list (it replaces the stored one):

```bash
orbit tool run orbit.task.update --input '{"id": "<task-id>",
  "tags": ["<existing tags>", "os:macos"], "workspace": "<selector>", "model": "<agent-family>"}'
```

The next admission honours it. A task already running or claimed is not moved.

## Modification footprint

`context_files` is the task's lock footprint: list only likely creation,
modification and deletion targets that are a substantive part of the change
inside this workspace using `file:`, `dir:` or `symbol:path#name:kind`. Omit
read-only files, tests and fixtures only consulted, and shared reference docs
that at most need an incidental line update. Do not list `docs/CONFIG.md`;
the implementer must still update affected docs in the same PR, and auto-fix
resolves textual merges of incidental edits. Put supporting evidence paths
in the description instead of reserving locks for them. Prefer
precise verified files/symbols; a directory is appropriate for a genuinely owned
area, not a shortcut for all possibly relevant code.

For review-filed tasks, task-pilot keeps selectors the finding names as change
targets. It may omit a selector it verifies as read-only evidence or an
incidental shared-doc touch by recording its exact selector and inspection
reason in `context_evidence_dropped`. Apply validates each record against
omitted, resolving review selectors and saves it in the assessment audit;
unexplained omissions remain in filed order as `context_evidence_retained`.
A record cannot override a proposed change target or an operator-declared
creation target. Keep evidence references in the finding's description.

Check helper filenames against `.github/codeql/codeql-config.yml` exclusions before prescribing them; production helpers must remain in CodeQL's source selection.

For a known new target, use the supported `allow_missing_context` option and
explain creation intent; a rejected selector's message names that option. The
option records each missing selector as durable creation intent for that exact
canonical selector (at most 32 per task), committed with the scope change.
Later writes may re-send a declared selector without the option, and
task-pilot keeps it as a target to create. Maintained task writes carry intent
forward for kept selectors and revoke selectors removed from the replacement
list. The grant is bound to the task revision: an older writer that changes the
stored task without maintaining the grant invalidates it, even if it later
restores the same selector list. Reauthorize through `orbit.task.update` with
the full `context_files` list plus `allow_missing_context: true`. Syntax,
containment and file/directory kind are still checked, and comments, plans or
agent proposals never grant intent. When task-pilot reports a missing selector
that carries no such record, re-declare it through that update path.

A worker updating its own task through `orbit.task.update` or `orbit task
update` from its run's linked worktree gets the existence relaxation
automatically for that task, without recording creation intent, and the
response lists the stored-but-unresolved selectors as
`context_files_unverified`. A dashboard request is never bound to a run, so it
keeps the strict check. Task writes on every surface return the same advisory
`warnings` (a disabled required tool, an unreadable parent or dependency, a
non-chore task added without `context_files`, a backlog task with no
footprint, and task-pilot blockers on approval); they never refuse the write.
Missing-file selectors remain valid declarations; do not prune them because a
checkout cannot yet resolve them. Empty context holds no context lock, so a
drain or ship with more than one slot holds such a task: it waits for an
enabled task pilot to prepare it (`awaiting_footprint`), and otherwise runs
only alone (`awaiting_exclusive_slot`). A single-slot run, an explicit `orbit
run ship <id>` and owner pull admission still admit it. Do not invent paths to
satisfy admission. Task-pilot can prepare unknown targets, and a live
pilot's preparation (its reservation from prepare, then its checkpoint)
still holds its tasks until that run settles.

**Never file with `--status backlog`, or promote to `backlog`, a task with
empty `context_files` while a drain is live.** File it `proposed`, run the
pilot or set selectors, then promote. On 2026-10-10 three tasks filed into a
live four-slot drain's backlog with no selectors all edited
`website/src/styles/custom.css` concurrently, and two of their runs were
cancelled. Add and update warn when they leave such a task in the backlog.

A `proposed` or `backlog` task reports its `readiness`: `{ready, gaps}`,
each gap with `code`, `severity`, `message` and `fix`. The `orbit.task.add`
and `orbit.task.update` responses carry it, `orbit task show` prints a
`Readiness:` line and the dashboard shows a chip. `missing_context_files`
blocks drain approval of a proposed task and is advisory in the backlog,
where it names the missing file lock;
`unassessed_complexity` blocks both. A `no-diff-expected` tag clears both.
Read the gaps after filing and fix what blocks, or leave it to task-pilot.

Put read-only designs, conventions and examples in prose links. A design document
belongs in the footprint only if this task will change it. Cross-workspace edits
need separate tasks in their owning workspaces, with explicit dependencies when
one supplies the other; one managed worktree cannot deliver another repository.

## Revise without accumulating contradictions

When an authorized decision changes, rewrite the canonical description and
reconcile criteria, dependencies and any stale plan. Preserve comments as audit
history and identify what was superseded. A worker should not have to reconstruct
the current contract from a pile of contradictory addenda.

Before changing an active task's scope, inspect its run and coordinate with the
worker/operator. Do not silently expand an admitted footprint or invalidate live
work. Re-prepare changed scope before another admission. Preserve existing user
choices and delivery evidence.

## Repairs that narrow a shared invariant

A repair that narrows a shared invariant — what the host will create, what
URLs are accepted, what counts as first-party, or any other predicate with
more than one caller — must name every caller and fixture found by searching
that predicate, and the acceptance criteria must require the same change to
update all of them together. Scoping the fix to only the caller that
surfaced the symptom leaves the other callers to fail the same way later,
each discovered and repaired separately instead of once.

A pull request that is green only against its own base is not evidence the
merged tree is green. When the change shares an invariant with other
in-flight work, require comparing the per-commit suite on the integration
branch against its parent commit, not just the PR's own base, before trusting
the result.

## Description template

Use only the sections that help this task:

```markdown
## Outcome
<trigger/current behavior → observable desired behavior; supporting evidence>

## Scope and prerequisites
<what this task owns; prerequisite IDs and their supplied interfaces>
<downstream work excluded here and how this slice is independently tested>

## Constraints and decisions
<settled choices, compatibility/authority limits, implementation discretion>

## Acceptance
<observable positive/negative behavior and the evidence the lane can produce>
```

Example — retry-safe export submission:

> A client retry after a lost reply currently starts a second export. Persist
> one submission result per request ID and return it on retry. This task owns
> the submission transaction and lookup API; it consumes the durable job store
> from `<prerequisite-task-id>`. A later task owns the CLI retry loop. Prove this
> slice with direct API fixtures: concurrent identical requests create one job;
> a retry after a simulated lost reply returns that job; a changed payload with
> the same ID is refused without another write. Preserve existing access checks.

This names a deliverable and its proof without prescribing every implementation
step or requiring the future CLI to exist first.
