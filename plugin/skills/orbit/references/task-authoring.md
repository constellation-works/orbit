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

To route a task an auto-task or a sweep already minted, retag it with
`orbit.task.update`, sending the full tag list (it replaces the stored one):

```bash
orbit tool run orbit.task.update --input '{"id": "<task-id>",
  "tags": ["<existing tags>", "os:macos"], "workspace": "<selector>", "model": "<agent-family>"}'
```

The next admission honours it. A task already running or claimed is not moved.

## Modification footprint

`context_files` declares intended creation, modification and deletion targets
inside this workspace using `file:`, `dir:` or `symbol:path#name:kind`. Prefer
precise verified files/symbols; a directory is appropriate for a genuinely owned
area, not a shortcut for all possibly relevant code.

For a known new target, use the supported `allow_missing_context` option and
explain creation intent; a rejected selector's message names that option. A
worker updating its own task through `orbit.task.update` from its run's linked
worktree gets that relaxation automatically for that task, and the response
lists the stored-but-unresolved selectors as `context_files_unverified`.
Missing-file selectors remain valid declarations; do not prune them because a
checkout cannot yet resolve them. Do not invent paths to satisfy admission. Unknown targets can be prepared by task-pilot before
execution; empty context does not guarantee eligibility for every admission path.

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
