### Hosted CI

Hosted CI runs the work of the full `make ci` on open PRs, split across jobs as described below. Each PR-triggered workflow starts with a checkout-free `Live PR state` job (`pull-requests: read` only); if the live read says the PR merged or closed while queued, the build and analysis jobs are skipped under unchanged check names, which GitHub treats as passing. In `ci.yml`, `desktop.yml` and `website-check.yml`, an API or projection error instead publishes `unknown` with a warning and admits the build jobs. The event payload never supplies fallback state: stale closed or merged data could suppress a reopened PR's required checks, and stale open data is not a live read. A PR that closes after the gate read `open` can still finish. Push, scheduled and manual runs skip the gate, so agent-main/main CI and CodeQL still cover merged changes, narrowed in `ci.yml` as described next. `make ci-fast` selects `scripts/test-pr-state-workflows.py` when self-test inputs change; full `make ci` always runs it. The script replays each gate against recorded API responses, rate limits and failures.

The same live response supplies `changed_files` to `ci.yml`'s change-scope job, avoiding a second PR-object request. An unavailable count runs in full; a numeric count narrows only when the files listing agrees. Orbit's absent-context landing proof accepts the exact fail-soft command, both output bindings and the dependent `open`-or-`unknown` condition. A changed fallback or a condition that excludes `unknown` refuses the proof; the historical strict live-read form remains supported for older workflow revisions.

PR jobs use two revisions: the primary checkout, build and tests use
`github.event.pull_request.head.sha`, while workflow orchestration helpers use
`github.sha`, the revision supplying the workflow (the PR merge ref). In
`ci.yml`, a separate sparse checkout under `.orbit/tmp/workflow-ci` supplies
`ci-download-verified.sh`, `ci-repack-nextest.py`, `ci-probe-bubblewrap.sh`,
`ci-rust-dependency-cache-key.py` and `ci-workspace-cache.py`; their invocations use that path. The Check job's
`Await the test build and nextest shards` step instead runs
`scripts/ci-await-jobs.py` as a dependency-producer from a full checkout of the
base revision under `.orbit/tmp/dependency-producer`. The scope job checks out `github.sha` directly. Keep this rule when adding
workflow-coupled `scripts/ci-*.py` or `scripts/ci-*.sh` helpers to any workflow:
check them out before their first call and invoke them from that checkout.
Candidate validation scripts such as `ci-guardrails.sh` stay in the primary
checkout so they validate the PR's code and tests. Workflow/helper interface
changes therefore require neither compatibility with older PR-head helpers nor
rebasing older PRs. `scripts/test-pr-state-workflows.py` guards helper ownership
and executes an old-head/two-revision checkout fixture. The CI-failure sweep
reads the workspace-root checkout as the commit under test and ignores a
checkout whose directory is a strict descendant of another checkout directory
in the same job log, so that nested helper checkout is not a second identity.
Two workspace checkouts that name different commits stay ambiguous.

In `ci.yml`, a `Change scope` job ([ci-change-scope.sh](../../scripts/ci-change-scope.sh)) narrows each run before build runners start:

- **`docs`**: every changed path, on a PR or in a push, is Markdown outside `crates/` and `plugin/`. A push additionally requires a successful completed GitHub Actions `Check / Clippy / Test` on its `before` commit. A missing, pending, cancelled, failed or skipped check, or an API error, runs the full gate instead: the previous tip may contain code whose pending run was replaced. Markdown under `crates/` is compiled in as assets and `plugin/` is the generated skill mirror. Every Markdown path declared in `FILE_READERS` in `scripts/ci-test-affected.py` is also outside the docs-only lane; the classifier derives that exclusion from the list, including `docs/design/auto-tasks/1_overview.md` and `website/src/content/docs/reference/config.md`. `RELEASING.md`, `npm/README.md` and `docs/agents/records/qa/path-leases-listener-sudo.md` are not in `FILE_READERS`; the `scripts/test-*` self-tests execute or digest them, so they leave the docs-only lane. The self-test pages take the full gate. Only `Docs guards` runs (`ci-guardrails.sh --static`: doc links and indexes, the skill mirror and the other file and metadata guards).
- **`affected:<crate>,...`**: a PR whose changed paths all sit inside workspace crates or are declared in `FILE_READERS`, apart from Markdown outside `crates/` and `plugin/` that `FILE_READERS` does not declare. Every job runs, but `Test build` compiles and archives only the build set of the changed crates, their reverse dependents and the always-run crates, and every partition runs only those crates' tests (see [PR test selection](#pr-test-selection)). Pushes are never narrowed this way.
- **`skip`**: a push whose commit has exactly the tree of the merged PR's head, when that head's latest `Check / Clippy / Test` succeeded in a run that tested every crate. The same bytes already passed the full gate, so nothing reruns. A PR run that tests every crate says so in its `Change scope` job with a `Test selection` notice reading `full`, and the push requires that notice from the same run (check suite) as the passing check. An `affected:` PR run, or a head whose run left no notice, gets a full push run. A merge whose tree differs from what the PR tested also runs in full. With non-strict required checks, most merges differ: the PR was tested against an older base.
- **`full`**: everything else, including any classification error. The job never fails, and an empty mode admits the full run.

Push runs of `ci.yml` share one group per branch without cancelling the running workflow, as `ci-macos.yml` does: one run executes, one waits, and a newer push replaces the waiting run. The surviving tip runs in full unless its entire tree already passed on the PR head, or its changes from a successfully checked `before` commit qualify for docs mode. Replacing a pending code push therefore cannot let a later Markdown-only delta hide its untested code. Skipped jobs keep their check names and report as skipped, which GitHub's required checks treat as passing. Orbit's landing pays an owed check only with an explicit success, so a skipped owed check keeps it waiting; its absent-context repair recognises this exact scope job when it proves that a missing required check will report after a branch update. `ci-macos.yml` and `codeql.yml` run the same classifier before their build and analysis jobs. They set `DOCS_BEFORE_CHECKS` (one GitHub check name per line) so a docs-only push is `docs` only when every named check on `BEFORE` is a successful completed GitHub Actions run, and that result overrides `skip`. Unset, as in `ci.yml` and `ci-cache-seed.yml`, the parent check stays `Check / Clippy / Test` and `skip` still wins. macOS names `macOS Clippy (informational)` and `macOS Sandbox`. CodeQL names `Analyze (actions)`, `Analyze (javascript-typescript)` and `Analyze (rust)`. An empty mode, or a missing, failed, cancelled or unfinished parent check, keeps the heavy jobs. `workflow_dispatch` and the CodeQL `schedule` event run those jobs even when the mode is `docs`. `scripts/test-pr-state-workflows.py` runs the classifier against recorded API answers and checks which jobs each mode admits.

#### Getting a completed result for a chosen agent-main SHA

RB-15 deploys only an exact agent-main SHA with a completed successful `CI` result, plus a completed successful `macOS Platform` result for the Mac. On a busy day that SHA may never get one from its push: each workflow keeps one running and one pending push run per branch, so the next merge replaces the pending run and the replaced SHA ends `cancelled` with no jobs started. A `gh run rerun` of the cancelled run re-enters the same push group and can be replaced again the same way. Use a manual dispatch instead. Both workflows accept `workflow_dispatch`, and a dispatch run gets a concurrency group per run (`run-<run_id>`: the existing fall-through in `ci.yml`, the dispatch branch of the group in `ci-macos.yml`) with cancellation off for that event, so later agent-main pushes neither replace nor cancel it. Ordinary push and `pull_request` groups and cancellation are unchanged, no extra macOS push runs are added, and the required PR checks and ruleset are untouched. A dispatch run always takes the `full` change scope, so its `ci.yml` result covers every crate. It is not a push, so macOS and CodeQL also run every build and analysis job rather than the docs skip. A docs-only push whose parent already completed both macOS checks starts no macOS build job; this dispatch is how that SHA gets a macOS result.

If the chosen SHA is still the `agent-main` head, dispatch both workflows on that branch and confirm the runs report that SHA before relying on them (a merge landing between choosing and dispatching moves the head):

```bash
SHA=<full agent-main commit>
gh workflow run ci.yml --ref agent-main
gh workflow run ci-macos.yml --ref agent-main
gh run list --workflow ci.yml --branch agent-main --event workflow_dispatch --limit 1 --json databaseId,headSha,status,conclusion
gh run list --workflow ci-macos.yml --branch agent-main --event workflow_dispatch --limit 1 --json databaseId,headSha,status,conclusion
```

If the chosen SHA is no longer the head, pin it with a throwaway `deploy-candidate/*` ref first. Pushing that ref alone starts no workflow (it is not a push-trigger branch); it only names the commit for the dispatch:

```bash
SHA=<full agent-main commit>
git push origin "$SHA:refs/heads/deploy-candidate/${SHA:0:12}"
gh workflow run ci.yml --ref "deploy-candidate/${SHA:0:12}"
gh workflow run ci-macos.yml --ref "deploy-candidate/${SHA:0:12}"
```

Wait for both runs, then verify each run's `headSha` is exactly `$SHA` and its `conclusion` is `success` before treating the SHA as deployable under RB-15. A dispatch on `agent-main` whose `headSha` differs from `$SHA` tested the newer head, not the chosen commit: redo it through the `deploy-candidate/*` ref.

```bash
gh run watch <ci-run-id> --exit-status
gh run watch <macos-run-id> --exit-status
gh run view <ci-run-id> --json headSha,conclusion,event
gh run view <macos-run-id> --json headSha,conclusion,event
git push origin --delete "deploy-candidate/${SHA:0:12}" # only for the pinned-ref route, after both runs complete
```

Delete the candidate ref only once both runs have completed. Workflow helpers still run from the workflow's own revision (the dispatched ref's `github.sha`), as on push and pull-request runs.

The ephemeral Linux runner installs sandbox packages with needrestart's
per-invocation `NEEDRESTART_MODE=l` (list-only) setting through `sudo env`.
This avoids restarting unrelated image services while retaining signed apt
installation, the new Bubblewrap binary's actual namespace probe, and the
guarded packaged AppArmor profile checks. See the
[Ubuntu needrestart manual](https://manpages.ubuntu.com/manpages/noble/man1/needrestart.1.html)
for the environment override; this does not change a persistent host setting.

The dev profile and the test profile that inherits it build dependencies at
optimization level 1. This reduces cold dependency compilation compared with
level 2; measure build time and shard runtime together when changing it.
Workspace members retain their existing unoptimized debug profile, and release
builds retain their release profile. The profile change also applies to local
and macOS/Windows dev and test builds.
The macOS target-cache keys include the installed Rust toolchain, lockfiles,
all Cargo manifests and `.cargo/config.toml`, so a profile change can save a
new cache after rebuilding. Toolchain-specific and legacy restore prefixes
still allow compatible artifacts to be reused.

The test gate in `ci.yml` runs on parallel runners from one build:

- `Test build` runs `ci-guardrails.sh --test-archive PATH`. It runs the folded
  workflow test build, then `cargo nextest archive` of the workspace's lib, bin
  and test targets under the same lint environment, so the archive costs no
  compile. The original archive uses Zstandard level −7 for fast local
  creation. `ci-repack-nextest.py pack SOURCE DESTINATION --parts N` re-encodes
  the same tar stream with zstd level 3, a 512 MiB history window and every
  core for transfer, then splits it into `DESTINATION.1` to `DESTINATION.N`
  (`NEXTEST_ARCHIVE_PARTS` in `ci.yml`).
  Level 3 reduces encoding time while retaining long-distance deduplication;
  compare pack/restore time and transfer bytes together on hosted candidates.
  This captures repeated linked executable sections beyond the native
  archive's 2 MiB window. It requires the runner's `zstd` CLI; the decoder
  is capped at the same 512 MiB window the encoder may emit. As soon as the
  original archive exists, the job starts `ci-guardrails.sh --test-partitions
  PATH` in the background, leaving the original archive intact, and runs
  `ci-repack-nextest.py pack` and the one-day part uploads concurrently while it
  runs; after the uploads, `Prove the shards cover every test` waits for it
  (using a lock acquired before the worker starts and held until it exits),
  prints its output and exits with its status, or fails if it recorded none. The
  proof reads only the original archive:
  it extracts the archive once, and
  [check-nextest-partitions.py](../../scripts/check-nextest-partitions.py) lists
  that extraction (`--binaries-metadata` and `--cargo-metadata`) once
  unpartitioned and once per `--partition count:k/N`
  (`NEXTEST_PARTITIONS` in `ci.yml`). It fails unless every test the full list
  selects is in exactly one partition, every partition selects at least one
  test (except for an [affected-crates archive](#pr-test-selection)), and no
  partition selects a test outside the full list. A failure fails
  `Test build`, and so every test job's first step and the Check job's wait.
  Doctests are disabled workspace-wide (`[lib] doctest = false`), so the
  build job runs no doctest step; `scripts/check-doctest-fences.sh` (in the
  guard pool) fails a runnable fence instead. Cargo unit timings from the
  folded compile (`--timings`) are uploaded as `test-build-timings`,
  including when a later step fails. This separates compilation critical
  paths from filter listing and archive time. Each test job needs the whole
  `Test build` job, so it starts only once that job has finished, including
  the partition proof it waits for after the upload.
- `Test shard k/N` jobs, one per shard, each need the `Test build` job (`needs: [pr-state, scope, test-build]`), so each starts once the whole build job has finished, including the partition proof the build waits for after the upload. If the build did not succeed, the job fails in its first step, `Require a successful test build`, with `ci-await: Test build concluded <result>: <run url>`. Each job then downloads every part at once (one
  stream per artifact; a single stream ran at 3–10 MB/s on some runners),
  in at most three download attempts of one minute each (the slowest of 240
  measured transfers took 31.8 s). Each attempt
  downloads to its own directory and verifies artifact digests. A failure or
  timeout in the first two attempts triggers the next one. A failed or timed-out
  third attempt fails the job before extraction. The shard reads every required
  part from the successful attempt's directory, decodes their concatenation
  once and extracts it under the checkout's `target/` with
  `ci-repack-nextest.py extract SOURCE DESTINATION --parts N` (only regular
  files and directories under `target/`, through tarfile's `data` filter),
  and runs `cargo nextest run --profile ci --partition count:k/N` from that
  extraction's `--binaries-metadata` and `--cargo-metadata`,
  where N is `NEXTEST_PARTITIONS` and k is the job's `PARTITION`.
  The `Test extra partition`, `Test partition 2` and `6`–`13` jobs
  run the same steps on ten more test runners, selecting partitions 5, 2 and
  6–13 and uploading `nextest-junit-extra` and `nextest-junit-partition-k`.
  They start after the whole build job has finished, including the partition proof the build waits for after the upload. None of them is a `Test shard k/N` job, so
  `NEXTEST_SHARDS` and the ruleset's names stay unchanged; the Check job
  awaits them like shards. Profile `ci`
  starts the measured long tests first, using nextest's per-test priority
  (requires nextest 0.9.91 or newer).
  This changes scheduling within each shard, keeping every test identity,
  ignored-test rule, count partition, group cap and isolated process. The
  default local profile keeps its ordinary order. See the
  [timing notes](../design-patterns/test_strategy.md#quality-and-the-400-second-budget)
  before interpreting a local replay as hosted budget evidence.
  A build that did not succeed fails each test job in that first step at once instead of running nextest.
  Repacking preserves every tar byte; it never extracts members or replaces
  an existing destination. Both zstd stages must succeed before publishing
  the destination, and corrupt input, a missing part, missing zstd or encoder
  failure fails the job. The original build-side archive remains the input
  to the partition proof. The transfer's larger
  window cannot be passed directly to nextest's default decoder.
  Extracting into the checkout puts `target/debug/orbit` and the test binaries
  at the paths they were built for. Each shard uploads its nextest JUnit report
  (`target/nextest/ci/junit.xml`) as `nextest-junit-k`, which carries the
  per-test durations. The Linux sandbox/policy enforcement gate runs in shard 1
  from that extraction (`--binaries-metadata` and `--cargo-metadata` under
  `target/nextest/`), without extracting the archive again. The shards are
  separate literal jobs with no matrix, gated on `Live PR state` like the other
  jobs: Orbit's PR-landing recovery certifies a rerun after a failed gate only
  for such jobs. agent-main's ruleset requires the exact names
  `Test shard 1/3` to `Test shard 3/3`, so changing `NEXTEST_SHARDS` or a shard
  name needs the same ruleset change before merge. Requiring only
  `Check / Clippy / Test`, which awaits every shard, would not depend on the
  shard count. `scripts/test-pr-state-workflows.py` runs each test job's nextest and
  archive-restore steps (every additional partition job's too), the build's pack
  step and the Check job's wait with stub tools, and fails unless
  those jobs together select `count:1/N` through `count:N/N` once each, the
  build's last upload precedes the wait for the partition guard, each test job needs the whole
  build job with `Require a successful test build` as its first step and downloads
  every uploaded part with no archive-poll wait, and the Check
  job's wait names the build, every shard, every additional partition job, `CI guardrails` and `CI QA guardrails`. After a change to this layout, re-measure the budget over
  the latest 20 completed PR runs with `scripts/ci-p90.py` (nearest-rank p90
  of the executed Check jobs against 400 s, with skipped and cancelled runs
  reported as gaps).
- `CI guardrails` runs `ci-guardrails.sh --guards-core` on its own runner, beside
  `Test build`: the repository guards and, with the bundled auto-task body
  history, the self-tests, except the QA sweep. `CI QA guardrails` runs
  `ci-guardrails.sh --guards-qa` on another standard runner: its inventory
  check and selected fixture self-test. Check awaits both jobs. Local
  `--guards`, `--fast` and `--static` keep the QA check in their guard pool.
  On a pull request (`CI_SELF_TESTS=changed`) each hosted route runs
  the `scripts/test-*` self-tests only when their inputs changed against the
  PR base, by the rule `make ci-fast` uses (`scripts/`, `.github/`, `Makefile`,
  `.cargo/`, the root manifests, `.config/nextest.toml`, `crates/orbit-engine/tests/engine/pr_landing/infrastructure/fixtures/legacy-codeql.yml`, `install.sh`, `RELEASING.md`, `docs/agents/records/qa/path-leases-listener-sudo.md`, `docs/standards/`,
  `AGENTS.md`, `CLAUDE.md`, `plugin/`, `npm/`, `.agents/`, `.claude-plugin/`, `server.json`, `docs/development/examples/release-readiness/scripts/release-readiness.yaml`); `test-qa-full-sweep.py --check`
  always runs its inventory check and runs its fixture self-test under the
  same rule. That process fixture accepts an identity-bound pidfd signal only
  on Linux. On macOS and other hosts it records an unsupported identity-signal
  result and does not send a numeric kill. A push runs every self-test. The core job builds no workspace target, so
  it carries the compiler-cache namespace regression's sccache and Bubblewrap
  setup. Its shared Bubblewrap step calls
  `scripts/ci-probe-bubblewrap.sh` from the workflow revision, keeping repeated
  setup out of expanded workflow source without changing its probes or bounds.
- `Check / Clippy / Test` runs `ci-guardrails.sh --checks`: formatting,
  production Clippy, documentation, an Apple-target type-check and the
  supply-chain gate, with no
  workspace test build, guard or nextest. The type-check is
  `cargo check --workspace --lib --bins --tests --target aarch64-apple-darwin`
  on the Linux runner. That job's toolchain step installs the Apple target.
  Its C compiler, `scripts/ci-apple-target-cc.sh`, is a stub: build scripts
  run, C is not compiled, and nothing is linked. Rust gated on
  `cfg(target_os = "macos")`, including tests, is type-checked. Hosted macOS
  is ARM, and no gate pairs that `cfg` with one `target_arch`, so this single
  target covers those items. It then
  waits for `Test build`, every shard, `Test extra partition`, `Test partition 2`,
  `6`–`13`, `CI guardrails` and `CI QA guardrails`, and fails as soon as one of them
  completes without success. Its elapsed time therefore covers the whole gate,
  including any queue wait for the other jobs' runners. That is the time the
  400-second budget measures, and its
  conclusion is the gate's result. The wait prints one
  `ci-await: <job> succeeded started=... completed=... elapsed_s=...` line per
  job. The Check job still awaits the build, every test job and both
  guardrails jobs.
  `--timeout-seconds` (default 2400) is one absolute budget for the whole
  Check wait: every `gh api`
  listing runs under the remaining budget, the poll sleep is clipped to it, and a
  listing still running at expiry is killed and reaped. A blocked listing
  therefore ends the wait at the timeout plus at most 5 s of cleanup, with a
  `timed out` diagnostic naming the unavailable listing or the jobs still
  pending. A listing that arrives after expiry is never accepted as success,
  even when every awaited job succeeded. Failed, skipped or cancelled jobs
  still fail the wait at once. A named job missing from a complete
  listing stays pending while any other listed job (apart from the Check job
  itself) is unfinished. Once every other listed job has completed, it stays
  pending for `--absent-grace-seconds` (default 120 s), because GitHub creates
  a job shortly after its `needs` finish. A job listed within that time is
  awaited normally. One still missing after the grace period fails the wait
  with `ci-await: <job> is absent from the run` (several names use `are`).
  An incomplete listing or an API error is not absence: it is retried within
  the remaining budget.
  Transient API errors are retried within the remaining budget.
  For a single-page job-listing response with an ETag, the job wait issues
  authenticated conditional requests every three seconds. An unchanged
  `304 Not Modified` reuses the last complete listing and consumes no primary
  GitHub API quota
  ([GitHub conditional requests](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api#use-conditional-requests)).
  This shortens the detection delay between the test jobs and
  the final Check result. A changed response still checks every named job.
  Missing ETags, API errors, fewer than 100 remaining primary requests,
  or a paginated job listing use the fallback interval, `--poll-seconds`
  (default 15 seconds). A first-page ETag never stands
  in for later pages: those runs use the full paginated listing. Server
  `X-Poll-Interval`, `Retry-After` and, once the primary quota is exhausted,
  `X-RateLimit-Reset` minimums raise the job interval. A 403
  or 429 without `Retry-After` also sets a 60-second minimum on single-page and
  paginated job listings alike, so its interval is at least 60
  seconds. The absolute
  deadline applies to the job interval. The wait prints
  `ci-await-api: listings=... not_modified=... paginated=...` for the hosted
  phase and quota readout. A three-second cadence makes at
  most 80 job-listing calls per minute before API latency, below the secondary
  [REST limit of 900 points per minute](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api#about-secondary-rate-limits) for these GET requests; other repository
  traffic shares the limits, and changed responses still consume primary
  quota. Check those counters and response fallback behavior on candidate
  hosted jobs instead of assuming every poll is free.

Locally, `make ci` keeps the single-process full mode: the folded build, then
nextest from its binary listing.

In full mode, `ci-guardrails.sh` repeats its phase breakdown in the final log
tail, including on failure: format, workflow test listing/build (which folds in
the all-targets Clippy lint), the exclusive self-test lane, workspace test
listing, workspace tests, production Clippy, documentation,
supply-chain checks and repository guards. `--test-archive` mode records only
`workflow-test-list-build` and `workspace-test-archive`, and `--test-partitions`
mode `workspace-test-archive-extract` followed by `workspace-test-partitions`.
An extraction failure stops before the proof, so no `workspace-test-partitions` record is emitted. `--checks` mode records `fmt`, `clippy-production`,
`documentation`, `apple-target-check` and `supply-chain`, and `--guards`/`--guards-core` modes
`guard-pool`, `exclusive-self-tests` and `repository-guards`. The dedicated
`--guards-qa` mode records `qa-guards` and explicitly reports whether its
fixture self-test ran or was skipped by input selection.
`apple-target-check` is the Apple-target type-check: plain rustc, with the
Clippy wrapper unset, lowered like the other check-chain commands. Full mode
does not run it, so `make ci` stays on the host target. The informational
macOS Clippy job is unchanged.
`guard-pool` times the background pool from its start to its join. In fast mode
that is the pool's own wall time. In full mode the join follows the workflow
test build, so `guard-pool` includes the overlapped build.
`exclusive-self-tests` times the exclusive lane the same way, from its start
to its join.
`workspace-test-listing` lists the test binaries of the folded build, and
`workspace-tests` runs nextest from that listing without invoking Cargo.
Meanwhile production Clippy and then documentation run in the
background, with each command launched in Linux's idle scheduling class
(or directly under `nice -n 19` where idle scheduling is unavailable).
The idle class uses only CPU time the tests leave idle; nice scheduling
lowers competition for CPU without changing a parent process's priority.
Production Clippy checks lib and bin targets and writes no test binary nextest
executes. The production check, folded test build and documentation use
`clippy-driver` through [with-test-build-lint.sh](../../scripts/with-test-build-lint.sh).
Its checked modes reject compiler warnings outside the source-order baseline,
including Cargo's replay of diagnostics for fresh units. The repository
guardrails also run [the Rust module-order check](../../scripts/check-rust-module-order.py)
on changed files in both Rust workspaces, including declarations in inactive
`cfg` branches. See [the source-order decision](../design/architecture-restructure/4_decisions.md#source-order-and-review-coverage)
for the tests-last rule, macro expectations and literal review checks.
Production code denies
`clippy::disallowed_methods` through crate-root attributes that override the
command-line `-A`. Cargo fingerprints the lint environment, so
`cargo doc` reuses that check of every workspace library and only runs rustdoc. Their records time each
command from its own start and overlap `workspace-tests`; the chain joins
before the supply-chain gate, and a failure in it still reports the later
phases.

The workflow sets `CARGO_PROFILE_DEV_DEBUG` and `CARGO_PROFILE_TEST_DEBUG` to
`0`, so the job's dev and test builds, dependencies included, carry no
debuginfo. It also sets `CARGO_PROFILE_DEV_STRIP` and
`CARGO_PROFILE_TEST_STRIP` to `symbols`: symbol tables and symbol names still
occupied 103 MiB of a 268 MiB CLI binary without debuginfo. Stripping at link
time reduces the binaries that the test build archives, the transfer repacks
and every shard extracts. Both profiles use the same setting so Cargo can
reuse their artifacts. Tests keep their compiled-in names and panic source
locations; no job sets `RUST_BACKTRACE`. Coverage runs in its own workflow
with line tables, and release builds retain their own profile settings. A
local build keeps the `[profile.dev]` line tables from `Cargo.toml`.
`repository-guards` runs from the start of the pool and the exclusive lane to
the later of their joins; in full mode that includes the overlapped build.
These are wall seconds at Bash's one-second resolution with
the original command exit status. They let bounded hosted excerpts retain build/check costs
separately from nextest testcase sums and the full GitHub job elapsed time.

The Rust dependency caches are the `Swatinem/rust-cache`
archives, one each for `Test build`, the Check job and CI guardrails in `ci.yml`,
plus the MSRV entry saved by `msrv.yml` push runs. They hold dependency artifacts
only. Every one, and each seeder in `ci-cache-seed.yml`, takes its `prefix-key`
from `scripts/ci-rust-dependency-cache-key.py`, so a dependency added to the
root manifest keeps the restore namespace while a profile change starts a
new one. Each step sets `save-if: ${{ github.event_name == 'push' }}`, so a
pull request restores the base branch's entries and never writes one: only a
push to `main` or `agent-main` saves. A missed key or a failed restore leaves a
cold or partial `target/`, and Cargo rebuilds what is missing. Hosted CI keeps
no compiler-object cache: `Test build` runs `rustc` directly
(`ORBIT_COMPILER_CACHE=0`), and the Check job installs sccache only for the
compiler-cache namespace regression, with a runner-local cache that nothing
restores or saves. sccache cannot cache the workspace members, which Cargo
compiles through `clippy-driver`, and rust-cache already supplies every
dependency. The
[compiler-cache runbook](../runbooks/compiler-cache.md#ordinary-linux-ci) records
the measurements, including why sccache cannot cache folded workspace compilations.
`scripts/test-pr-state-workflows.py` fails if any cache step in `ci.yml` can
save on a non-push event.

`Test build` separately restores workspace outputs into `target/`. The
v2 cache holds a tar stream compressed with a 512 MiB Zstandard history, which
can match repeated linked code across test executables. A v2 miss falls back
to restoring the legacy v1 target cache. The key binds the toolchain, Cargo
manifests and lockfile, Cargo config, and the lint and source-restore
implementations. Changes to `ci.yml` retain the cache key so job scheduling
edits can reuse workspace outputs. Only protected-branch pushes save this
cache. A fresh checkout gets old source mtimes back only after
`ci-workspace-cache.py` verifies identical bytes and modes. Changed inputs
get a fresh mtime. Cargo still
invalidates compiler, features and codegen environment changes. A workflow
file is an ordinary tracked input: changed bytes get a fresh mtime, and Cargo
rebuilds a consumer only if it records that file, through an `include_str!`
path or a build script's `cargo:rerun-if-changed`.

The restore step prints one JSON object whose fields depend on whether the
restored `target/` holds a source manifest. With no manifest, which includes a
cold cache miss, it prints only `cache: "miss"` and `reused_inputs: 0`: there
is no `source_head` and no `changed_inputs`. A restore prints
`cache: "restored"`, `reused_inputs`, `changed_inputs` and `source_head`, the
commit the manifest was captured at. A partial `restore-keys` match can carry a
manifest from an older protected-branch commit than the base.

The two lists differ. `git diff --name-only <source_head> <tested_head>` names
the tracked paths whose content differs between those commits, deletions
included; it never names an untracked file. `changed_inputs` counts the current
regular files, tracked or non-ignored untracked, whose bytes, mode or size
differ from the manifest or that the manifest does not record. Each counted
file gets a fresh mtime. A deleted input is not counted, and a new non-ignored
untracked file is counted without appearing in the commit diff. Use the commit
diff for the committed change list, and read `changed_inputs` only as the
number of inputs refreshed; neither is an exact inventory of the other.

The folded list-build (`check-ci-macos.sh --workspace-build`) reads Cargo's
JSON messages, so Cargo's own progress does not reach the log; it prints
`check-ci-macos: workspace units rebuilt=N fresh=M rebuilt_packages=…`
instead. A change to a library's `src/` rebuilds that library and every
selected workspace dependent, while a test-only change rebuilds its test
target. `orbit-cli`'s build script also reruns whenever a watched path under
`crates/` or `scripts/` (or the Git index) changes, so its units appear in
almost every PR's rebuilt set. Attribute list-build time from these two
lines before comparing runner CPUs.

The 2026-11-17 investigation found a workspace-cache miss preceding a 279 s
folded test build in hosted run 44043933622 (Check: 516 s). The workflow file
was part of the cache hash, so workflow edits invalidated the previous cache
identity. A local six-run replay of a workflow-only edit, using a
dependency-free two-target fixture under the folded lint environment, reduced
median restore-plus-build time from 0.279 s to 0.073 s and compiler
invocations from two to zero. This measures cache invalidation, not hosted
seconds saved: artifact download/copy costs are excluded, the fixture is
small, and an executed protected-push `Test build` must populate the new
namespace (skip-only seeders populate dependencies, not workspace outputs).
Changed codegen settings and older changed embedded workflow bytes both
rebuilt in the replay. No checks or tests were retired. Measurement commands,
logs and the replay script are retained in the task's `ci-budget/` artifacts.

After merge, the operator should collect the latest 20 completed PR CI
workflows carrying the change and run
`python3 scripts/ci-p90.py --limit 20 --budget-seconds 400`. Retain full run,
head and job identities and timestamps; report executed, failed, skipped,
cancelled and missing measurements, with gaps never counted as zero. Report
nearest-rank p90, median and maximum; keep Check's pre-start queue wait and
awaited jobs' queue wait after Check starts separate from elapsed time, or
mark unavailable timestamps as gaps. Compare workspace and dependency cache
hits, selected crates, runner image/CPU, compile/guard/transfer/partition
phases, and cold-cache runs before attributing any change. The
[budget measurement handoff](../design-patterns/test_strategy.md#quality-and-the-400-second-budget)
gives the collection details; this implementation lane does not certify a
post-merge hosted p90.

#### PR test selection

On a pull request that is not docs-only, the `Change scope` job runs `scripts/ci-test-affected.py --pull-request-scope`, the same graph walk as `make ci-test-affected`, over the PR's changed paths and the merge ref's Cargo metadata (read with the runner's stable toolchain). It selects the changed crates, their transitive reverse workspace dependents, the crates `FILE_READERS` declares as readers of a changed path, and the `ALWAYS_RUN` crates, and outputs `affected:<crate>,...`.

`ALWAYS_RUN` is the security and recovery set every PR runs whatever it changed: `orbit-exec` (the sandbox and Bubblewrap boundary; its presence also admits shard 1's Linux sandbox gate), `orbit-policy` and `orbit-auth` (authorization), and `orbit-store` (store migrations). The partition guard runs on every test build. Their tests are about 2% of the suite's test time. In the 2026-10-28 to 10-31 CI history, every failing migration test was in `orbit-store`; the other failures were in `orbit-engine`, `orbit-mcp`, `orbit-cli` and `orbit-web`, which the reverse-dependency walk selects for any change they can observe.

It runs every test instead (`full`) when any of these holds:

- a path outside `crates/` that `FILE_READERS` does not declare: the root `Cargo.toml` or `Cargo.lock`, `rust-toolchain.toml`, `.cargo/`, `.github/`, `scripts/`, `plugin/` or any other repository file, since a test may read it at run time. Markdown outside `crates/` and `plugin/` that `FILE_READERS` does not declare is the exception. A declared path selects its readers, or no crate (`scripts/qa-full-sweep-inventory.json`, read only by the guardrail sweep; `scripts/unit-test-baseline.json`, read only by the always-run `unit-test-inventory.py --check` guard; `plugin/evals/`, the `claude plugin eval` suite no CI job reads; `scripts/ux-rules/ui/`, `scripts/ux-rules/cli/`, `scripts/ux-rules/tests/`, `scripts/ux-rules/README.md`, `scripts/ux-rules/lib/ui-context.mjs`, `scripts/ux-rules/lib/cli-context.mjs` and `scripts/ux-rules/lib/manifest.mjs`, which no Cargo test reads since the ux-rules routine was removed [ORB-22844]), so its PR runs the always-run set alone. `scripts/ux-rules/run.mjs` is not declared, so a change to it runs the full suite.
- a member's `Cargo.toml`, which can change the features of every crate in the workspace build;
- a path under `crates/` that no current member owns, such as a removed crate;
- every workspace crate is selected anyway (for example, a change to `orbit-types` or `orbit-common`);
- the PR file listing is incomplete, or the selection fails for any reason, including unreadable Cargo metadata.

Cargo metadata cannot see a test reading another crate's files. The ones that matter are those whose reader does not already depend on the owner, and they are declared in `FILE_READERS`. `orbit-engine`'s tests read `orbit-core`'s job and activity assets, and `orbit-mcp` compiles in `orbit-web`'s vendored dashboard scripts. Every other cross-crate read found (`orbit-cli` reading `orbit-core` and `orbit-tools` fixtures, `orbit-web` compiling an `orbit-store` fixture, `orbit-core` running an `orbit-engine` fixture) is by a dependent, so the reverse-dependency walk covers it. Reads of repository files outside `crates/` are declared there too (`scripts/build-budget.py`, `server.json`, `plugin/hooks/`, `plugin/skills/`, the skill mirror the sync guard checks against its source and `orbit-core`'s default catalog test reads, the configuration reference page, `docs/design/auto-tasks/1_overview.md`, the QA sweep script and its inventory, the guard baselines and allowlists (`scripts/unit-test-baseline.json`, `scripts/too-many-arguments-baseline.json`, `scripts/rust-source-order-baseline.tsv`, `scripts/architecture-ratchet-baseline.json`, `scripts/test-child-runner-allowlist.json`, `scripts/test-shell-waits-allowlist.json`, `scripts/dashboard-css-literals-baseline.json`, `scripts/removed-dashboard-labels-allowlist.json`, `scripts/removed-code-identifiers-allowlist.json`), the plugin evals, and the UX rule files named above); any other such path runs the full suite.

`Test build` passes the mode to `ci-guardrails.sh --test-archive` as `CI_TEST_SCOPE`. For an `affected:` scope, `ci-test-affected.py --test-build-packages` checks the crates against the PR head's metadata and adds the unselected members that keep every dependency's resolved features equal to the workspace build's (`cargo tree`). Resolver 2 unifies features over the selected packages only, so a narrower set would rebuild dependencies the CI caches hold with the workspace's features. `orbit-cli` depends on every other crate, so it is in nearly every selection, and its dev-dependencies name the features only other crates' tests enable (`tokio`'s `test-util`, `rusqlite`'s `hooks`, the `test-support` of `orbit-exec` and `orbit-mcp`), so it normally needs no anchor: an anchor compiles its unit-test harness, which no target flag excludes. A new gap adds an anchor at that compile cost, and the build's reason line names it. Both the folded build (`check-ci-macos.sh --workspace-build --packages`, which lists only the macOS workflow's filters for crates in the build) and `cargo nextest archive` then take `-p` for that set. A crate the head lacks, or a feature gap no member closes, builds the whole workspace. The archive's package list is written to `PATH.packages`, and the partition guard then runs with `--allow-empty`: an archive with few tests can leave a count partition empty, but every test must still be in exactly one shard.

The archive holds the build set's tests, but every partition (the shards and the ten additional test runners) passes `-E 'package(a) | package(b) …'` for the `affected:` crates, so a feature anchor's tests are built, not run; the partition guard lists the same filter. The shards keep their names and run with `--no-tests pass`, so a shard whose partition is empty runs nothing and passes. The full archive cannot have an empty shard, because its partition guard fails `Test build`. Shard 1's Linux sandbox/policy enforcement gate runs on every full run and on every `affected:` run that includes `orbit-exec`; without `orbit-exec`, neither the sandbox code nor its dependencies changed, and the gate is skipped. Production Clippy, documentation and the repository guards stay workspace-wide, and every test job still runs: a failed or skipped one fails the `Check / Clippy / Test` wait. The MSRV check is no longer part of pull-request runs; it runs push-only in `.github/workflows/msrv.yml`.

A PR run can still miss a failure only the full suite shows, such as a test reading another crate's file through a path neither a dependency edge nor `FILE_READERS` records. The `agent-main` push run after the merge is the check of the merged tree and always tests every crate.

`scripts/test-pr-state-workflows.py` replays the selection against a recorded crate graph (`scripts/fixtures/ci-crate-graph.json`): a leaf crate, a mid-graph and a core crate, workspace-level and toolchain files, a member manifest, non-crate paths, cross-crate fixture paths, a removed crate and a selection error. It also checks the push `skip` evidence and the shards' empty-partition and sandbox-gate conditions.

### Hosted CI budget handoff

After a CI scheduling change merges, the operator collects the latest 20
completed `ci.yml` PR runs, waiting for a cohort that uses the merged layout.
Record collection time, each run ID, reported head, conclusion and workflow
revision. Save raw job timestamps and replay the same population offline:

```bash
mkdir -p .orbit/tmp/ci-budget-after
# Collect the completed PR population, including executed failures.
gh run list --workflow ci.yml --event pull_request --status completed \
  --limit 20 --json databaseId,headSha,conclusion,createdAt \
  > .orbit/tmp/ci-budget-after/list.json
python3 - <<'PYTHON'
import json
import subprocess
from pathlib import Path
root = Path('.orbit/tmp/ci-budget-after')
runs = []
for entry in json.loads((root / 'list.json').read_text()):
    run = str(entry['databaseId'])
    # Keep the raw REST data: gh run view's job projection omits fields that
    # may be useful when diagnosing scheduling, such as job created_at.
    data = subprocess.run(['gh', 'api', '--paginate', '--slurp',
        f'repos/{{owner}}/{{repo}}/actions/runs/{run}/jobs?filter=latest&per_page=100'],
        capture_output=True, text=True, check=True).stdout
    (root / f'{run}-jobs.json').write_text(data)
    runs.append(dict(entry, jobs=[job for page in json.loads(data) for job in page['jobs']]))
(root / 'runs.json').write_text(json.dumps(runs, indent=2) + '\n')
PYTHON
python3 scripts/ci-p90.py --from-json .orbit/tmp/ci-budget-after/runs.json \
  --budget-seconds 400 > .orbit/tmp/ci-budget-after/p90.log
```

Preserve the p90 command's exit status: 0 means within budget, 1 over budget,
2 no executed Check job, 3 a `gh` failure (not over budget). Report
nearest-rank `ceil(0.9 * n)`, the ascending
executed Check start-to-completion durations, median and maximum, with
skipped, cancelled, missing/invalid jobs and executed failures counted
separately. Never treat a gap as a zero-duration pass. Include earlier attempts
separately if reruns occurred; the command above explicitly selects the latest
attempt. Mixed workflow revisions and fewer than 20 usable runs are limits
to disclose, not evidence to discard slow runs.

Report queue wait independently from Check elapsed. When job `created_at`
is available, record Check `started_at - created_at` separately. For each
awaited job report its queue interval's overlap with Check's elapsed window:
`max(0, min(job.started_at, check.completed_at) - max(job.created_at,
check.started_at))`. Queue intervals overlap, so report each and their union,
not their sum as extra elapsed time. Job creation can precede dependency
readiness: label these intervals as scheduling/queue upper bounds unless an
actual ready/queued timestamp is available. If the API lacks `created_at`,
report exact queue wait as unavailable; `workflow.createdAt` to job start
includes dependencies and is only an upper bound. Also report the lag from
`Change scope` completion to each runner's start, which can reveal the
additional test runner waiting for capacity. None of these fields subtracts
queue time from the 400 s Check budget. Attach raw records, the replay log
and gaps to the operator's measurement record. A p90 over 400 s requires a
follow-up repair; it must not be recorded as restored.
