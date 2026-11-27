# Authoring a task: the contract

Contents:

- [Establish the contract](#establish-the-contract)
- [Tasks that fix frictions](#tasks-that-fix-frictions)
- [Revise without accumulating contradictions](#revise-without-accumulating-contradictions)
- [Repairs that narrow a shared invariant](#repairs-that-narrow-a-shared-invariant)
- [Moves with companion fixtures and guards](#moves-with-companion-fixtures-and-guards)
- [Description template](#description-template)

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
  that result are implementation work, not an external dependency. A move's
  companion fixture, path-filter and tracing-filter edits follow
  [Moves with companion fixtures and guards](#moves-with-companion-fixtures-and-guards).
- **Prerequisites:** name actual task IDs and the capability each supplies. Record
  blocking edges in `dependencies` on `orbit.task.add` (or `orbit.task.update`
  after guarded creation), not only in prose. Do not require an upstream
  slice to demonstrate behavior that needs its downstream consumer: use the
  agreed interface and isolated fixtures, and assign end-to-end proof to integration.
- **Decisions and constraints:** separate settled product choices from choices the
  implementer can make. Before filing, resolve every consequential policy or
  contract choice named by the task, including either/or policy, ownership or
  sign-off, and contract-scope choices. Record the selected option and its
  authority or basis in the description; acceptance criteria must describe that
  outcome instead of asking the implementer to choose.
  - If only an operator can
    decide, make the handoff the task's explicit outcome: name the decision owner,
    the choice or action they own, and what work waits for their answer. File a
    finite-choice handoff as a human-crew decision task with `orbit.decision` (see
    [Work only a human can do](task-authoring-create.md#work-only-a-human-can-do)), and add a
    dependency from the implementation task. Keep ordinary implementation choices
    delegated.
- **Data and contract semantics:** for analytics or listing tasks, state in the
  acceptance criteria the denominator (or eligible record set), how absent, null
  and empty values differ for relevant fields, and how legacy records are
  treated. Example: “Completion rate uses all non-archived tasks; an absent or
  null owner is unknown, an empty owner is unassigned, and legacy rows without
  the owner field are unknown.”
  - When a change affects a persisted format, say whether a pre-change record
    will be read, migrated or refused. Include an acceptance criterion that
    exercises that outcome with a fixture containing an actual pre-change
    record, so the check reads the old representation rather than constructing
    a current-format record.
- **Acceptance:** define observable behavior and evidence, including important
  refusal/negative cases. Commands are validation methods, not substitutes for an
  expected result. Distinguish implemented, tested, merged and deployed when those
  differ; do not make an implementation task require an unauthorized live rollout.
  - For every executable check criterion, name the behavior or outcome the check
    exercises and give one input it must accept and one it must reject. A test or
    command name alone does not establish what the check proves.
  - If a criterion checks wording, layout, help text or another text surface,
    classify it as text review. Phrase presence alone is not behavioral evidence
    for an executable check.
  - A grep criterion states the exact command, searched path scope and match
    count at filing, plus the expected count after the change. Keep the path
    scope narrow. For example, if
    `rg -n -F 'old_setting' src/ | wc -l` returns 4 at
    filing, require it to return 0 after the removal. Use repository-wide scope
    only when filing-time results show that every match belongs to the target.
- **Timing and performance criteria:** a timing criterion must be stable under
  re-measurement of an unchanged binary: repeated measurement of the same binary
  on the same fixture must not be able to flip it across its bound.
  - Specify a
    re-measurement-stable protocol — a fixed fixture (same input, host class and
    build), warmup and sample counts, the number of independent repetitions and
    how they are aggregated, and a bound whose margin exceeds the spread shown by
    repeated measurement of an unchanged binary (a no-change control) — or state
    an absolute budget a lane can check deterministically (for example, p95 at
    most a named duration over a named number of paced samples on the fixed
    fixture).
  - Never file a single warm p95 ratio at a hard threshold (such as
    candidate/base <= 0.67 from one warm run each) as the sole acceptance gate;
    a ratio may support a stable gate, but cannot be that gate on one measurement.
    When no such stable bound is known yet, make the timing result informational,
    gate on a deterministic proxy (operation, request or payload counts), or file
    a measurement task first that establishes the fixture, spread and margin.
  - Before promotion, put the evidence route for each hard timing criterion in
    the brief.
  - For an Orbit CLI command or HTTP route, the default route is the
    checked-in [paired timing protocol](https://github.com/constellation-works/orbit/blob/agent-main/docs/runbooks/perf-timing-protocol.md)
    (`docs/runbooks/perf-timing-protocol.md` with `scripts/perf/fixture.py`,
    `build-pair.sh` and `paired.py` from the linked `agent-main` branch): paste its
    "Measurement protocol" paragraph and fill its slots (surface, command or
    path, parity mask, bound, artifact slug).
  - It supplies everything listed
    below, including the `implement_bundle` build: pass
    `--candidate-worktree <checkout>` to `build-pair.sh`, because HEAD is still
    the pinned base and the candidate is uncommitted. The script snapshots that
    working tree without moving HEAD, the index or files. A rerun after the
    delivery commit passes `--candidate <reviewed final head>`.
  - For any other measurement, name the fixture artifact or exact seed and reset commands
    for a disposable workspace and data set, the exact build and measurement
    commands, and how to select the baseline revision and reviewed final candidate revision
    with matching build profiles and settings. Specify the bounded same-fixture
    protocol for both builds, including pacing, warmups, samples, repetitions,
    aggregation and the unchanged-build control.
  - Name the lane and host or host
    requirements that can actually run it, who owns any necessary serial
    host-owned run, and the exact log or task-artifact path the reviewer will use.
    Have that run retain the resolved revisions, profiles, host environment,
    fixture identity, samples, aggregate and unchanged-build spread at that path.
  - Use required validation only if its configured command runs this benchmark
    on the reviewed candidate and retains the evidence. An empty command list or
    a passing behavioral check supplies no timing result. Use the existing
    post-deploy route only for a criterion that needs the deployed build, and
    record that measurement as owed until it is run.

Split work when independently deliverable outcomes or real dependency boundaries
justify it. A coherent cross-layer change can remain one task; file count and
breadth alone are not blockers. Each slice must be verifiable at its own boundary.
Keep incomplete entry points unavailable when later slices are needed for safety.

## Tasks that fix frictions

Friction resolution on landing is automatic on the owner. Name each friction
in the description or acceptance criteria, preferably also with
`relations: [{"type":"resolves","target":"<friction-id>"}]`. Targets are
workspace-local.

Landing resolves open or triaged records, preserves their
original bodies, and appends the task, delivered commit, PR number and time.
Already-resolved records stay unchanged. The move to `done` commits each target
as owed in the task history, so a failed write or a crash is retried by the
task's next write with the original delivery evidence.

Treat a criterion such as "Friction <friction-id> is resolved with the merge
sha" as a landing obligation. The implementer proves the repair and hands it
off; the owner supplies the merge evidence after delivery. Do not ask a worker
to obtain a future merge SHA or write the owner's friction corpus.

Local and
already-landed handoffs use the candidate or covering commit; a verified
no-diff handoff uses its synchronized base, and a final recovery's completion
uses the covering commit it verified. These paths record PR `none`.
Every valid friction ID in the description or criteria is a resolution target,
including background citations, so mention unrelated reports in comments
instead. See [friction](friction.md) for workspace ownership and retry rules.

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
update all of them together.

Scoping the fix to only the caller that
surfaced the symptom leaves the other callers to fail the same way later,
each discovered and repaired separately instead of once.

A pull request that is green only against its own base is not evidence the
merged tree is green. When the change shares an invariant with other
in-flight work, require comparing the per-commit suite on the integration
branch against its parent commit, not just the PR's own base, before trusting
the result.

## Moves with companion fixtures and guards

A behavior-preserving move can leave text that still names the old location: a
guard test's fixture tree or path list, a path filter, or a tracing filter that
targets the old module path. Updating that text is companion work the move needs.
Rewriting an assertion, an expected value or an unrelated test is not. Before
filing a move:

1. Search for the old path, module path and filter target (`rg -n` over tests,
   scripts, fixtures and filter directives), then classify each hit:
   - **Companion:** the text must change for the guard or filter to match the
     moved code. Name the exact file and the literal replacement, such as
     `old/path` → `new/path`.
   - **Unrelated:** the test only runs or passes through the moved code. Leave it
     unchanged and do not name it.
2. Write criteria that allow exactly the companion edits and forbid the rest. Do
   not limit test edits to "imports or call syntax" when a guard fixture, path
   filter or tracing filter literal must also change. Say instead: "Test edits
   are limited to the listed literals in `<file>`; no assertion, expected value,
   fixture body or other test changes." Workspace test rules, such as the UI test
   prohibition in the workspace guide, still apply to every companion edit.
3. Require an independent check that the guard still detects violations: keep or
   add a negative case that plants a violation at the moved location and expects
   the guard's existing failure. Do not regenerate the guard's fixture from the
   moved source, because that removes the check. A passing run on the clean tree
   is not evidence of this.
4. Require a tracing filter to keep its behavior: the same events and fields
   still appear under the new target, shown by a captured log or an assertion,
   and the filter directive names the new target.
5. A move that cannot pass without changing an assertion, an expected value or
   behavior is not behavior-preserving. Name that change as its own outcome, or
   file it as a separate task.

For example, a move that renames a module watched by a dependency-direction guard
can carry these criteria:

> - `scripts/check-dependency-direction.sh` still fails on a planted import that
>   crosses the moved module's boundary, and still passes on the clean tree.
> - Test edits are limited to the moved module's path literals in the guard's
>   fixture; no expected message or other test changes.

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
> from `<prerequisite-task-id>`.

A later task owns the CLI retry loop. Prove this
> slice with direct API fixtures: concurrent identical requests create one job;
> a retry after a simulated lost reply returns that job; a changed payload with
> the same ID is refused without another write. Preserve existing access checks.

This names a deliverable and its proof without prescribing every implementation
step or requiring the future CLI to exist first.
