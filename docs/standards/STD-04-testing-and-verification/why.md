# Why — STD-04 Testing and verification

Why each rule in [STD-04](STD-04.md) exists, tied to the incident behind it where there
was one. Read the entry for a rule when its intent is unclear or before deviating. The
rules themselves are in `STD-04.md`; this file explains them and adds none.

The Orbit evidence below is cited by task (`ORB-`), friction (`F`) and decision-log line.
`D:<area>:<n>` means `codebases/orbit/docs/design/<area>/4_decisions.md` line n. The
candidates were mined and dispositioned in ORB-13088
(`projects/standards/tacit-extraction-2026-09-26.md`).

**R1.** In ORB-12582, the distributed drain's read-only surface refused every `orbit tool run`
call, including the owner's own, while MCP served the same calls normally. The unit test that
should have caught it synthesized a capability set for its "owner-local session", which is "a
session no production CLI caller sends" (D:operations-as-data:271). A test that builds its own
convenient entry state proves only that the convenient state works. Other cases:
- ORB-11021: a classifier was "never enforced at MCP dispatch".
- ORB-12791 and ORB-12149.
- F2026-08-005: a feature shipped with no reachable caller.
- F2026-08-139.

In a CLI, the production entry point is the built binary. STD-02 §R20 is where those tests
live in Rust.

**R2.** D:activity-job:1071: "A fixture hand-built with a singular `task_id` passes against
the broken code and proves nothing." Only a failing run on the pre-fix code shows that the
test can see the defect at all. The evidence is the same as R1: ORB-12582, ORB-11021,
ORB-12791, ORB-12149, F2026-08-005 and F2026-08-139.

**R3.** D:activity-job:784 rejects weakening `pipeline_success_guard`: "The guard behaved
correctly on false input; the input was the defect." D:activity-job:1495 rejects relaxing a
guard on the local path because it "deletes the evidence rather than producing it." A loosened
assertion turns a signal into silence and leaves the defect upstream. See also ORB-12297 and
ORB-12539. Orbit's development guide states the test-side form: "Never weaken the assertion
taken when the probe *is* available."

**R4.** F2026-07-179 was a near-miss safety regression. A task's suggested fix widened a
fail-closed worktree guard to accept any change disjoint from the run's own paths. Implemented
literally, it turned five provider-escape tests green that must stay red, because every escape
was also disjoint. The failing negative cases were the only thing that surfaced it. The
friction's guardrail is the rule: "these existing fail-closed cases stay red", enumerated for
all of the guard's guarantees.

**R5.** A text-matching test breaks on every wording edit and proves nothing runs. A test
that pins crew, model or schedule turns every operations edit into red CI. Orbit's agent guide
bans both and allows the narrow structural-guard exception (`codebases/orbit/AGENTS.md` lines
13 and 31; `docs/design-patterns/test_layout.md` §What tests assert). STD-02 §R21 carries this
for Rust only. This rule makes it universal, because the failure mode is the same in any
language.

**R6.** ORB-12123: `GitShim` mutated the process-wide `PATH`, which redirected `git` for
every concurrent test in the same binary. Those tests then failed once the shim's temp
directory was removed. F2026-09-107 was a flake at about 50%. See also ORB-12479. A mutex that
only writers take still lets a reader see a half-applied change. Running the mutation in a
child process removes the shared state entirely. F2026-07-065 is the host-facing version:
"validation must never rewrite the operator account['s] live skill links."

**R7.** Evidence for R7 to R9 is shared, from candidate X4:
- F2026-09-148: tests queried the real per-user systemd manager, and they failed with HTTP 500
  on a worker without a user bus. Orbit fixed it by injecting a clock observer into the
  fixture.
- F2026-09-215: non-Git fixtures had no Git discovery boundary. With `TMPDIR` inside a
  managed worktree, `git rev-parse` walked upward and found the managed checkout, and 33 of
  80 tests failed on a read-only lock there.
- ORB-11390, ORB-10835, ORB-12532, ORB-12306.

A test whose result depends on the host is a report on the host, not on the code.

**R8.** ORB-11390: a test "hard-fails instead of skipping in restricted" environments.
Failing for a reason unrelated to the code under test teaches people to ignore red. The
opposite fault is just as bad. A silent early return reads as a pass. Orbit's Linux CI
sandbox gate exists "instead of reporting those tests as passed after an early return". An
ignored test with a reason, plus a named job that runs it with `--run-ignored only`, gives
both properties.

**R9.** ORB-12306 is titled "Remove artificial SQLite write admission": it removed an
artificial throttle. A retry that makes a flaky test pass also makes a real
intermittent defect pass. The remaining evidence is shared with R7.

**R10.** Evidence:
- ORB-11609: "two of its test steps run zero tests."
- ORB-12130: a gate "passes vacuously when its hand-written test-name literal goes stale."
- ORB-12452: a gate was inert because an action was pinned to a commit that does not exist.
- F2026-08-083: a gate "reports PASS while asserting nothing."
- F2026-08-108: test files never declared in their module compiled to nothing and reported
  `0 passed`.

`cargo test` with a filter that matches nothing exits 0. That is the default hazard this rule
removes.

**R11.** Evidence: ORB-12242, ORB-12221, ORB-12589 and F2026-07-185. ORB-11402 is the
incident: the integration branch went red on the sync gate. A source change without its
regenerated artifact is half a change, and it turns the next author's gate red.

**R12.** Evidence is shared with R11. If a parity check runs only in CI, drift is found after
the push, by someone else, on a shared branch. The `--check` modes cost seconds, so the fast
gate can afford every one of them.

**R13.** Evidence:
- ORB-12775: a documented sandbox guarantee did not hold. `ORBIT_ALLOWED_TOOLS` was enforced
  through an environment variable that the sandboxed child could unset.
- ORB-12183: stale evidence-rule descriptions.
- ORB-12046: "Restore honest validation dates."
- ORB-12055: a doc was stamped validated while it still carried a broken path.

Readers act on a claim without reading the code. A false security claim is worse than no
claim, because it removes the reason to check.

**R14.** Evidence:
- ORB-12673: "the documented `backlog` retry" is unreachable.
- ORB-12581.
- ORB-12043: the "mutable-fixture template does not compile."
- F2026-07-108: a rule that points at a removed command "is worse than no rule."

The candidate recorded more than 11 such cases. Documented commands are the ones agents
copy verbatim.

**R15.** F2026-09-077: a build was started in the background as
`cargo build … 2>&1 | tail -40`, and the pipeline's status was `tail`'s, not the build's.
See also F2026-09-127, ORB-12527 and ORB-12548 (QA validation tasks). A check that never
finished, or whose failure was filtered away, looks exactly like a pass in a summary.
Orbit's review gate records `denied` and `not_run` as distinct outcomes for this reason. It
also refuses a claim whose outcome contradicts its role.

**R16.** Orbit's agent guide requires it: "Report commands and outcomes at handoff — passed,
failed, not run — never 'tested'" (`codebases/orbit/AGENTS.md` line 35). A reviewer can
re-run a listed command and can see what was not run. "Tested" offers neither.

**R17.** The candidate recorded about 20 records. Evidence:
- F2026-09-036: both gates failed on untouched code.
- F2026-08-084: "`make ci-fast` was already red on HEAD."
- F2026-07-129: the author isolated the cause by "spinning a temporary git worktree at the
  branch point". The failure came from an earlier merge that left a snapshot stale.

Without a baseline, an author either blocks on someone else's breakage or folds an unrelated
fix into an unreviewable diff.

**R18.** The candidate recorded 13 or more records. Evidence:
- F2026-08-053: the dated root cause no longer existed.
- F2026-09-092: the defect was already remediated.
- F2026-09-213.

Orbit's review sweep states the rule: "Verify every finding against the live code before
filing" (`crates/orbit-core/assets/auto_tasks/code-review.yaml` line 72). Task text ages. The
code at head is the only authority.

**R19.** F2026-09-158: `orbit.task.add … | head -c 800` truncated the JSON before the `id`
field, so the created task looked like a failure. A write whose result is not read back
cannot be told apart from one that silently half-applied.

**R20.** In F2026-09-158, re-running the same command created a second identical task
(ORB-12450 and ORB-12451). The agent surface could not delete the duplicate. F2026-08-002 was
a duplicate friction from an accidental repeat call. A retry must first look for the record
the retry would create.

**R21.** Evidence is shared with R22: F2026-07-142 and F2026-07-160. Capture is what makes a
mistaken destructive step recoverable. Orbit's implement activity records the starting HEAD
and a full `git status --short` inventory before any write, for this reason.

**R22.** F2026-07-142: `refs/stash` is shared across linked worktrees. A failed
`git stash push -- <path>` made the next `git stash pop` restore an unrelated session's work
into the wrong tree. See also F2026-07-160. Shared repositories, trackers and hosts have
other actors, and an operation that is not scoped to one's own changes eventually lands on
theirs.

**R23.** At `e40faaf3e` (2026-10-03), Orbit's unit tests under `crates/*/src/**` came to
about 295k non-blank lines and 7,675 test functions. That was more than the 274k lines of
production code they covered. Crate-root integration tests were 37k lines.

Most of those unit tests pinned internals:
- A refactor that kept behaviour unchanged still broke dozens of them.
- Agents spent their budget repairing tests rather than finding defects.
- The suite only grew. ORB-12965 measured the ten largest test files and found hundreds of
  tests with zero unique line coverage.

A per-crate review kept 180 unit tests (2.3%). Every one of them fell into one of R23's four
admission classes.

Two reasons stop the rule from going further and banning unit tests outright:
- **Edge cases.** Edge-case tables and seam-driven races are far cheaper below the boundary.
- **Failure localisation.** A failing e2e test localises a fault poorly, which costs the
  debugging budget the deletion saved.

R1 already puts regression tests at the production entry point. R23 extends the same reasoning
to all new tests.

**R24.** A golden is only as strong as the review of its diff. With R23 moving more
guarantees into goldens, re-blessing becomes the cheapest way for an author, human or agent,
to turn a gate green. An unexplained regenerate is the golden form of the loosened assertion
R3 forbids.

**R25.** The 2026-10-03 review found two hazards in retiring tests:
- **Sole guards.** Several tests were the only guard of a security or incident invariant.
  Examples are symlink-escape refusal in destructive cleanup, and redaction across chunk
  boundaries.
- **Hidden dependencies.** Some kept tests depend on support code that looks dead: `#[cfg(test)]`
  seams in production code, and an `#[ignore]`d test that a kept test re-execs as a crashing
  child (`orbit-store` `fs/lock/tests/file_lock.rs`). Deleting it silently breaks the kept test.

