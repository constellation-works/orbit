# Machine checks — STD-04 Testing and verification

The gate that enforces each rule in [STD-04](STD-04.md), or `review-only`. Ship a change
with the gate its rule names, or say it is review-only.

Each gate below is real and can be adopted. "Orbit:" says whether Orbit enforces it today.

| Rule | Gate |
|---|---|
| R1 | review-only. In Rust, put end-to-end tests in crate-root `tests/` against the built binary (STD-02 §R20). |
| R2 | Before/after run: extract the base revision into its own directory (`git archive <base> \| tar -x -C <dir>`), or use a separate `git worktree`. Add only the new test, and record its failing run. Orbit: `scripts/cross-revision-check.sh`, and the review gate refuses a negative control recorded as `ExpectedFailure` that passed (`judgement.rs`). |
| R3 | review-only. A diff that deletes or relaxes an assertion or guard must say why the guard was wrong. |
| R4 | review-only. The widened check's negative tests appear in the validation record as run and failing where they should. |
| R5 | review-only (as STD-02 §R21). An optional CI grep can flag `include_str!` plus `.contains(`, or reads of source files in tests, against an allow-list of named structural guards. Orbit: no such grep. |
| R23 | review-only. Optionally report each crate's count of unit tests (tests below the public boundary) in metrics, so that growth is visible. This is not a gate. Orbit: the agent guide (`AGENTS.md#Code`) and `docs/design-patterns/test_strategy.md`. |
| R24 | review-only. A PR that regenerates goldens explains each diff. Orbit: `make goldens UPDATE=1` regenerates and `scripts/check-goldens.sh` detects drift. Whether the explanation is there is review-only. |
| R25 | review-only. A deletion PR names the boundary test that replaces each retired guard. Orbit: the retirement tasks ORB-13826 to ORB-13835 embed per-crate keep lists. |
| R6 | nextest `[test-groups]` with `max-threads = 1` for every module that mutates or reads the mutated state (Orbit: `stateful-runtime`). nextest's process-per-test model bounds the blast radius. Python: pytest `monkeypatch` plus `env=` on child processes. Readers left outside the group: review-only. |
| R7 | review-only. Optionally run the suite with no network (`unshare -rn`, or a CI job with networking disabled). Fixtures set `GIT_CEILING_DIRECTORIES` to their temp root. Orbit: no network-off job. |
| R8 | Rust: `#[ignore = "<capability>"]`, plus a CI step in a capable job running `cargo nextest run --run-ignored only -E '<filter>'`, or `cargo test -- --include-ignored`. Run `--include-ignored` only in jobs that have the capability, never as the default. Python: `pytest -rs` prints skip reasons, and a `REQUIRE_<CAPABILITY>=1` variable set in the capable job turns the skip into a failure. Orbit: the Linux sandbox/policy enforcement gate. |
| R9 | nextest: leave `retries` unset (0) in `.config/nextest.toml` and off the CI command line. pytest: no `--reruns` or `pytest-rerunfailures` in CI. Orbit: no retries configured. |
| R10 | nextest: `--no-tests=fail` (or `NEXTEST_NO_TESTS=fail`). Set it explicitly, because the `auto` default resolves to `fail` only on recent nextest. pytest exits 5 when nothing is collected; never mask that code. A per-filter count check fails any selection that matches zero tests. Every required tool is checked for presence at gate start. Orbit: `check-ci-macos.sh` per-filter counts, the `rg` presence check, the orphan-module check. The nextest flag is not set, so Orbit relies on the pinned version's default. |
| R11 | CI regenerates and diffs: `<generator> && git diff --exit-code`, or a `--check` mode. Orbit: `make goldens` (`check-goldens.sh`), `generate-doc-indexes.sh --check`, `sync-plugin-skills.sh --check`. |
| R12 | The fast gate calls the same check-mode commands as CI, from one script. Orbit: `make ci-fast` runs `ci-guardrails.sh --fast`, which differs from CI only by the compile-heavy steps. Adding a new CI parity check to the fast gate: review-only. |
| R13 | review-only. A testable claim, and every security claim, gets a test that exercises it. Help-text changes surface in goldens (STD-01 §R24). |
| R14 | Rust: `cargo test --doc` (Orbit: in `ci-guardrails.sh`). A link checker for relative doc links (Orbit: `check-crate-agent-guides.sh`; polaris: `scripts/check-links.sh`). Documented CLI command lines can be parsed through the real argument parser in a test. Orbit: no general extraction of documented commands. Otherwise review-only. |
| R15 | `set -o pipefail`, or capture `$?` from a redirect before any `tail`/`head`. A structured validation record whose outcomes are `passed`/`failed`/`denied`/`not_run`. Orbit: the review gate's `ValidationOutcome` and contradiction check. Otherwise review-only. |
| R16 | A handoff or PR template with a commands-and-outcomes section. Orbit: review reports carry each validation command with its `ValidationOutcome`. Otherwise review-only. |
| R17 | review-only. Orbit: `scripts/cross-revision-check.sh` runs one command on the base and the candidate. |
| R18 | review-only. Orbit's pipeline requires covering evidence (`already-landed.json` with validation logs) before it accepts an already-landed claim. |
| R19 | review-only |
| R20 | review-only. Where the store supports it, an idempotency key makes a repeated write safe. |
| R21 | review-only. Destructive commands default to a dry run and require `--confirm` (STD-01 §R5). |
| R22 | review-only. Orbit mounts `.git` read-only in managed runs, which prevents ref-level interference such as a shared stash. |
