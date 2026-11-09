---
id: STD-04-testing-and-verification
title: Testing and verification — test design, hermetic tests, honest gates, docs as contracts, and evidence-based verification conduct
summary: Normative rules for how a change is tested and verified — boundary-first tests with unit tests only by admission, regression tests that drive the real entry point and fail before the fix, no weakened guards, behaviour-only and hermetic tests, visible skips, root-caused flakes, gates that cannot pass vacuously, regenerated artifacts, docs claims and examples as contracts, and the evidence whoever makes the change (human or agent) must produce before calling it done; follow it in any project with tests, CI or docs.
status: active
tags: [standard, testing, verification, ci, docs, agents]
version: 2
created: 2026-09-26
updated: 2026-10-03
last_validated: 2026-10-03
related: [STD-01-cli-surface, STD-02-rust-architecture-and-errors, STD-03-concurrency-and-process-safety, STD-05-security-boundaries, STD-06-design-docs-and-feature-tracking]
---

# Testing and verification

These rules cover how a change proves it works: what a test must exercise, what keeps a
test honest and hermetic, what makes a gate trustworthy, when documentation counts as a
contract, and what evidence the author of a change owes before calling it done. The
§Verification conduct rules address whoever makes the change. That is a person or an agent,
and the rules do not distinguish.

Every rule is portable. A single-crate CLI or a small Python tool with no CI runner can
comply. The minimum is:
- tests that were shown to fail before the fix;
- one local check script that CI, when it exists, also runs;
- no test retries;
- a handoff that lists each command and its outcome.

Orbit is the worked example, not the audience. Orbit paths are relative to
`codebases/orbit/`.

Other standards own the neighbouring rules, and this one does not repeat them:
- **Test layout and behaviour-only assertions in Rust** are
  [STD-02](../STD-02-rust-architecture-and-errors/STD-02.md) §R19 to §R21. §R5 below extends the
  behaviour-only rule to every language. STD-02 says *where* a Rust unit test lives; §R23 below
  decides *whether* one should exist.
- **Test waits, fixture process guards, the self-re-exec ban and host-state isolation** are
  [STD-03](../STD-03-concurrency-and-process-safety/STD-03.md) §R17 to §R20. Runtime containment and
  wall-clock limits for test runs are STD-03 §R21 and §R22.
- **Help text and golden-tested CLI surfaces** are [STD-01](../STD-01-cli-surface/STD-01.md) §R22 and
  §R24.
- **Security boundaries** are STD-05-security-boundaries. Only the documented claim about a
  boundary (§R13) is in scope here.

The keywords MUST, MUST NOT and SHOULD are normative. A rule marked *review-only* in
[`checks.md`](checks.md) has no automated gate, so a reviewer enforces it.

This standard is a directory. This file holds the binding text: the rules, where they
apply, and how to deviate. Beside it, [`why.md`](why.md) gives the reason for each rule,
[`checks.md`](checks.md) the gate that enforces it, [`exemplars.md`](exemplars.md) worked
examples in Orbit, and [`CHANGELOG.md`](CHANGELOG.md) what changed in each version.

## Rules

### Test design

- **R1.** A regression test MUST drive the entry point that production callers use: the
  same surface (CLI, API handler, tool dispatch), the same session or authority shape, and
  the same input shape a real caller sends. It builds the hostile state directly rather than
  approximating it.
- **R2.** A regression test MUST be shown to fail against the unfixed code before the fix is
  accepted. The failing run is part of the change's evidence.
- **R3.** When a correct guard or test blocks a change, the change MUST fix the upstream
  defect. It MUST NOT relax the guard, delete or loosen the assertion, or widen the
  accepting path to get past it.
- **R4.** A change that deliberately widens a fail-closed check MUST first run that check's
  existing negative cases, and they MUST still fail after the change. A negative case exists
  for every guarantee the check makes, not only the one next to the reported bug.
- **R5.** In every language, a test MUST assert behaviour by running the code under test.
  STD-02 §R21 states the rule for Rust. Two exceptions are allowed:
  - A text-matching test over source, assets or UI copy is allowed only as a narrow
    structural safety guard, and its assertion message names what it protects.
  - A test that pins operational policy owned by config or prompts (agent crew or model,
    schedule, prompt or UI wording) is allowed only if its assertion message cites the
    incident it guards.
- **R23.** Behaviour MUST be tested at a stable boundary by default:
  - an integration test through the project's public API or built binary;
  - a golden for an output contract (help, machine-readable output, schemas, generated
    policy, parser output over captured input);
  - an end-to-end test for a critical flow.

  A unit test, meaning one that reaches below that boundary, is admitted only when the boundary
  cannot prove the same guarantee economically and the test guards one of:
  - combinatorial pure logic (a transition table, a parser's edge cases, policy or selector
    matching, scheduling arithmetic), written as one table-driven test per invariant;
  - an interleaving or fault that needs a test seam: a race, a crash mid-write, a lost reply,
    a TOCTOU swap;
  - a security or safety invariant the boundary cannot reach cheaply;
  - platform or kernel behaviour that no integration test runs.

  An admitted unit test's name or assertion message names the invariant it guards. The
  following are not admitted as unit tests:
  - serialization round-trips, defaults, getters and builders;
  - mock-wiring assertions;
  - a second test of an invariant that already has one;
  - behaviour that a boundary test already covers.
- **R24.** Every golden or snapshot diff in a change MUST be explained by that change.
  Regenerating goldens to turn a gate green without reviewing and explaining the diff is
  relaxing a guard (§R3).
- **R25.** A unit test MAY be retired when a boundary test or golden now covers its behaviour,
  or when it fails §R23's admission criteria. It MUST NOT be retired while it is the only guard
  of a security invariant or a cited incident: either that guard moves to the boundary in the
  same change, or the test stays. A kept test keeps the support code it depends on, including
  test seams and child-process entry points.

### Hermeticity and isolation

- **R6.** A test MUST NOT change process-global state in the shared test process unless
  every test in that process that can observe the state is serialized with it, readers
  included. Process-global state covers the environment, `PATH`, the current directory,
  statics and global overrides. Otherwise the test runs the code in a child process with the
  value set on the child command. This extends STD-03 §R20: a lock that only writers take is
  not isolation.
- **R7.** A test MUST NOT depend on a live service, the host's service manager, the network,
  or discovery that walks above its temporary root, such as VCS or config-file search. The
  fixture injects the dependency or sets the boundary itself. STD-03 §R20 covers the host's
  own state directories.
- **R8.** A test that cannot run in the current environment MUST skip visibly: its output
  names the missing capability. At least one CI job, or the release checklist where there is
  no CI, MUST run it where the capability exists, in a mode where a skip fails.
- **R9.** A flaky test MUST be fixed at its root cause. Retries, sleeps and artificial
  throttles MUST NOT be added to make it pass. While the fix is pending, the test MAY be
  quarantined: marked ignored, with a reason that names the tracking issue.

### Gates and CI

- **R10.** A gate that ran zero tests or asserted nothing MUST fail. Every test selection in
  a gate (a filter, a named group, an ignored-only leg) MUST select at least one test. A tool
  the gate depends on MUST be present, or the gate fails. A missing tool never degrades to
  empty, "clean" output.
- **R11.** Generated and derived artifacts MUST be regenerated in the same change as their
  source. These include goldens, snapshots, mirrored or vendored copies, generated indexes
  and lockfiles.
- **R12.** The fast local gate an author runs before handoff MUST include every parity and
  drift check that CI runs: the cheap `--check` modes of generators, mirrors and indexes.
  Only checks that need a full build MAY be CI-only.

### Docs as contracts

- **R13.** Comments, help text, documentation and validation stamps are claims, and a claim
  the code does not uphold is a defect. The change that breaks a claim MUST fix the code or
  the claim, and it MUST NOT restamp a claim as validated without validating it. This binds
  hardest on security guarantees.
- **R14.** Every documented command and code example MUST run against the current version.
  A documented rule or procedure that points at a removed command MUST be deleted or
  rewritten in the same change that removes the command.

### Verification conduct

- **R15.** A validation step counts only with positive evidence that it completed and
  passed: the producer's own exit status, or an explicit completion marker. A step that was
  skipped, backgrounded, denied by a sandbox, or whose status was hidden behind a pipe is not
  a pass.
- **R16.** A handoff MUST list each validation command with its outcome: passed, failed, or
  not run with the reason. "Tested", "all green" or "CI passes" without the commands is not
  a report.
- **R17.** Before treating a red gate as caused by the change, or as a blocker, the author
  MUST reproduce it on a pristine tree at the base commit. A failure that reproduces there is
  pre-existing. The author records it and hands it off separately, and does not fold the
  unrelated fix into the change.
- **R18.** Before acting on a task, issue or finding, the author MUST verify its premises
  against the live code at the current head. That includes checking whether the work already
  landed.
- **R19.** After a write to durable or shared state (a tracker record, a remote, a shared
  file), the author MUST read the result back and confirm it took effect. Nothing, including
  a display filter such as `| head`, may hide the write's result.
- **R20.** Before retrying a write whose result is unknown, the author MUST list or query
  for the record the write would have created, so a retry cannot create a duplicate.
- **R21.** Before a destructive operation on shared state, the author MUST capture what it
  destroys in a recoverable form: a commit SHA, a copy, an export, or a dry-run inventory.
- **R22.** The author MUST NOT modify, stash, clean or discard another actor's work. That
  work includes another session's uncommitted edits, pre-existing changes in a checkout, and
  another person's branch. Use operations scoped to the author's own changes.

## Applies to

- `universal`: R1 to R25. Any project with tests, CI or documentation, of any size or
  language, and any author of a change, human or agent.
- `rust`: the nextest, `#[ignore]`, `cargo test --doc` and `--include-ignored` gates in
  [`checks.md`](checks.md). Rust projects also follow STD-02 §R19 to §R21 for test layout.
- `cli`: R13 and R14 bind on help text and documented command lines, alongside STD-01 §R22
  and §R24.
- `stateful`: R19 to R22 bind hardest where the state is shared: trackers, remotes and
  repositories with more than one actor.

## Deviations

An adopting repo that departs from a rule records a decision in its own design docs, citing
`STD-04@<version> §Rn` and the reason, for example `STD-04@1 §R8`. It never edits its
vendored copy of this standard.

Some deviations are expected, and the decision still needs to be recorded:
- A project with no runner that has a capability (for example no Linux host with user
  namespaces) cites `STD-04@1 §R8`. It names the manual release-checklist step that runs the
  ignored tests.
- A parity check that needs a full build may stay CI-only under `STD-04@1 §R12`, and the
  decision names it.

Known deviations in the exemplar itself, so adopters do not copy them:
- At `e40faaf3e` (2026-10-03), Orbit had about 7,675 unit tests, and most of them are not admitted under
  §R23. A per-crate review kept 180. Adding the replacement boundary coverage and retiring the
  rest is tracked by ORB-13812 to ORB-13836.
- `crates/orbit-cli/tests/mcp_roundtrip.rs#readonly_state_mount_keeps_cli_and_mcp_reads_observational`
  returns early, printing nothing, when bubblewrap mount namespaces are unavailable. No CI mode
  fails that skip (against R8).
- `scripts/check-workflow-action-pins.sh` warns and exits 0 with no network, or when
  rate-limited, and it makes no exception for CI (against R10).
