# Exemplars — STD-02 Rust architecture and errors

Worked examples of the rules in [STD-02](STD-02.md), as pointers into the Orbit
codebase. Orbit is the example, not the audience: this file is optional reading, and
useful only with an Orbit checkout.

Paths are relative to `codebases/orbit/`.

- R1 — `ARCHITECTURE.md#architecture` (tier diagram and rule); `ARCHITECTURE.md#crates` (per-crate table)
- R2 — `ARCHITECTURE.md#application-and-surfaces`; `docs/design/orbit-core/4_decisions.md#extract-the-cli-facing-command-layer-into-orbit-cmd`
- R3 — `ARCHITECTURE.md#domain` (orbit-config: "callers pass explicit roots"); `scripts/check-dependency-direction.sh` lines 201–206 (`ResolvedConfig::load` ban in Core runtime)
- R4 — `ARCHITECTURE.md#foundation-and-kernel`; `scripts/check-dependency-direction.sh#allowed_internal_deps` (`orbit-policy | orbit-exec` → common, types only)
- R5 — `ARCHITECTURE.md#foundation-and-kernel` (orbit-types: "does no I/O of any kind"); `crates/orbit-types/Cargo.toml` `[dependencies]`
- R6 — `scripts/check-dependency-direction.sh` (crate allowlist lines 8–84; single-crate module rules lines 183–234); `AGENTS.md#rules` ("don't add cross-crate dependencies without updating ARCHITECTURE.md"); `crates/orbit-mcp/tests/dep_boundary.rs#mcp_depends_only_on_its_leaf_domains`
- R7 — `docs/design/orbit-core/4_decisions.md#north-star-architecture-bearing-operations-as-data-behind-an-operation-registry` (bearing 5); `ARCHITECTURE.md#orbit-store-internals`
- R24 — `docs/design/activity-job/4_decisions.md#ship-duplicate-dispatch-guard-lives-in-the-shared-submission-path`; `crates/orbit-core/src/application/job/pipeline/submit.rs#submit_ship_run` (its guard `in_flight_ship_run_for_tasks` reads the persisted run input, so every surface is covered); `docs/design/operations-as-data/4_decisions.md#capability-chokepoint-for-destructive-operations-outside-mcp`; `crates/orbit-automation/src/auto_tasks/schedule.rs#interval_period` ("the single range rule" shared by validation and both slot calculations)
- R25 — `docs/design/activity-job/4_decisions.md#derive-the-delivery-execution-summary-from-the-change-not-from-the-agent`; `crates/orbit-engine/src/executor/automation/vcs/commit/summary.rs#ensure_durable_execution_summary`
- R8 — `AGENTS.md#code` ("Default to `pub(crate)`"); `crates/orbit-store/src/lib.rs` module declarations (`pub mod contracts`, private `mod driver`, `mod repository`)
- R9 — `Cargo.toml#workspace.dependencies` and `#workspace.package`; `crates/orbit-types/Cargo.toml` (`serde.workspace = true`, `version.workspace = true`)
- R10 — `crates/orbit-automation/src/error.rs#AutomationError`; `crates/orbit-types/src/plugin/version.rs#VersionError`; `crates/orbit-agent/src/types/response/envelope.rs#is_timeout` (reads the supervisor's `timed_out` flag, not stderr)
- R11 — `crates/orbit-common/src/error.rs#OrbitError` (`#[non_exhaustive]`, 128-byte size budget)
- R12 — `docs/design-patterns/error_translation.md#crate-boundary-error-translation`; `crates/orbit-automation/src/error.rs#automation_error_to_orbit`; `crates/orbit-engine/src/activity_job/dispatcher.rs#dispatch_error_to_orbit`; `scripts/check-error-translation.sh`
- R13 — `Cargo.toml#workspace.lints.clippy` (`unwrap_used`, `expect_used`); `crates/orbit-common/src/lib.rs` line 3 (`cfg_attr(test, allow(...))`)
- R14 — `docs/design-patterns/newtype.md#newtype-wrapper`; `crates/orbit-types/src/plugin/version.rs#Version` (`FromStr` + `#[serde(try_from = "String")]`); `crates/orbit-core/src/application/automation/preparation.rs#InstructionSnapshot` (private inner)
- R26 — `crates/orbit-engine/src/executor/automation/vcs/commit/mod.rs#empty_stage_error` (reports observed staged/unstaged/untracked counts, HEAD and base; shares no wording with `unrelated_history_error`); `docs/design/activity-job/4_decisions.md#pipeline-steps-consume-a-base-commit-pinned-at-worktree-setup-never-a-moving-ref-name`
- R27 — `crates/orbit-types/src/task/model/status.rs#dependency_dead_end` (every `TaskStatus` variant named, no wildcard); `docs/design/activity-job/4_decisions.md#dispatch-admission-separates-unmet-dependencies-from-unsatisfiable-ones`
- R28 — `crates/orbit-automation/src/auto_tasks/schedule.rs#validate_schedule` and `#interval_period` (range check, then `i64::try_from`); `docs/design/routines/4_decisions.md` line 106
- R29 — `crates/orbit-core/src/metrics/reliability.rs#Rate` (`value` is `None` when the denominator is zero); `crates/orbit-core/src/application/routines/clock/inspect.rs#ClockUnitVerdict` (probes the unit's real program; `Unrunnable { reason }` is its own verdict); `docs/design/routines/4_decisions.md#host-local-sweep-clock-configuration`
- R30 — `docs/design/activity-job/4_decisions.md#classify-independent-review-startup-separately-from-reviewer-rejection`; `crates/orbit-types/src/task/model/status.rs#dependency_dead_end`
- R31 — `docs/design/host-registry/4_decisions.md#durable-registry-input-fails-closed`; `docs/design/policy-sandbox/4_decisions.md#sandbox-availability-is-a-host-precondition-not-a-runtime-fallback`; `crates/orbit-engine/src/activity_job/audit_writer.rs#note_telemetry_failure` (fail-open, documented as such at the decision)
- R32 — `crates/orbit-web/src/state/registry.rs#UnavailableCheckout` and `#report_unavailable` (one broken checkout is warned about and skipped; healthy workspaces keep serving); `crates/orbit-engine/src/activity_job/audit_writer.rs#note_telemetry_failure` (the failure is counted and recorded on the run, not dropped)
- R33 — `crates/orbit-engine/src/executor/automation/vcs/freshness.rs#discard_unauthenticated_rewrite` and `#perform_rebase_onto_base` (an uncertified pre-boundary state is restored and redone, not refused)
- R34 — `crates/orbit-cli/src/command/init/command.rs#reject_invalid_fresh_identity_inputs` (runs before `orbit init` writes anything); `docs/design/activity-job/4_decisions.md#the-runtime-reports-its-deterministic-action-registry-and-job-validation-gates-on-it`; `docs/design/policy-sandbox/4_decisions.md#sandbox-availability-is-a-host-precondition-not-a-runtime-fallback`
- R15 — `crates/orbit-common/src/lib.rs` line 1 (`#![deny(clippy::print_stderr, clippy::print_stdout)]`); `crates/orbit-cli/src/main.rs` lines 3–4 (the binary's print allow, with its reason); `crates/orbit-common/src/observability/logging.rs#init_default_subscriber`
- R16 — `ARCHITECTURE.md#foundation-and-kernel` ("serde shapes are persisted contracts"); `crates/orbit-types/src/task/plan.rs#TaskPlanDocument`; `crates/orbit-config/src/registry/keys.rs#REMOVED_CONFIG_KEYS`; `crates/orbit-types/src/policy/policy_def.rs#PolicyDef` (`created_at: Option<DateTime<Utc>>`, no `Utc::now` default); `crates/orbit-types/src/identity/actor.rs#agent_label_round_trips` (switches to the tagged form when a bare label would read back as another variant)
- R17 — `AGENTS.md#code` ("Prefer the fewest moving parts"); `docs/design/orbit-core/4_decisions.md#remove-operation-mode-rather-than-keep-an-unused-authorization-layer`; `scripts/check-dependency-direction.sh` (retired ownership paths, lines 236–256)
- R18 — `AGENTS.md#code` ("~800 lines per file is a split signal")
- R19 — `docs/design-patterns/test_layout.md#per-module-sibling-tests-directory`; `crates/orbit-mcp/src/adapter/tests/`; `scripts/check-orphan-modules.sh`
- R20 — `docs/design-patterns/test_layout.md#when-not-to`; `crates/orbit-cli/tests/`
- R21 — `AGENTS.md#rules` (text-matching ban); `docs/design-patterns/test_layout.md#what-tests-assert-invariants-vs-policy`
- R22 — `Cargo.toml#workspace.lints.clippy`; `Makefile#ci-lint`; `scripts/ci-guardrails.sh` (`cargo clippy --workspace --all-targets -- -D warnings`)
- R23 — `deny.toml`; `scripts/cargo-deny.sh`; `Makefile#audit`
- R35–R38 — none yet: Orbit predates them and is listed under *Deviations* in [`STD-02.md`](STD-02.md#deviations).
