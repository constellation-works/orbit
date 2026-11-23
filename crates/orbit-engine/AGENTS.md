# orbit-engine

Run a v2 activity or job to completion: template resolution, dispatch to a provider CLI or deterministic action, step results and audit rows, retry/resume/fan-out/concurrency. Owns *how* work runs; catalog placement, authorization, and task lifecycle are Core's. Never depends on `orbit-core`, `orbit-cmd`, or a transport crate.

## Rules

- `RuntimeHost` (`context/hosts/runtime_host/mod.rs`) is the single capability boundary; Core implements it. A new capability is a new required trait method (no default body) that Core's host implements, plus its answer for a host without it in the test-only `NullRuntimeHost` (`context/hosts/null_host/defaults/host.rs`, behind `test-support`) — never a widened dependency, a threaded runtime handle, or a direct `orbit-store` read.
- Test doubles implement `test_support::NullRuntimeHost`, never `RuntimeHost` directly.
- The port owns its executor, sandbox, outcome, error and declared-failure evidence key (`DECLARED_FAILURE_EVIDENCE_KEY`) contracts in the leaf `context/dispatch_contract.rs`, which names no `crate::` module. Sandboxed child spawning lives in the crate-root leaf `sandbox_spawn.rs` (naming only `crate::context`), below both `executor/automation` and `activity_job`.
- The in-crate order is `context::dispatch_contract < sandbox_spawn < executor/automation < activity_job`, enforced by `scripts/check-dependency-direction.sh`. `activity_job` children never read `DECLARED_FAILURE_EVIDENCE_KEY` from the `activity_job` root. Per-edge detail: [architecture page](../../docs/architecture/orbit-engine.md#module-layout).
- `executor/automation/` holds deterministic step actions; each new operation gets a file, not another arm.
- The order `pr_answers < candidate_resume < pr` holds under `executor/automation/vcs/`. Candidate resume reads the pull-request answer and the source-repair identity predicate from `vcs::pr_answers` and does not name `pr`; required candidate validation remains in `candidate_validate.rs`.
- The private PR operations' answers (`pr.status`, `pr.view`, `pr.list`, `pr.merge_capabilities`, `pr.merge`, `pr.required_checks`) are parsed once in `vcs/pr_answers/answers.rs`; readers, Core's included, take those types and never index the response by provider field name. The source-repair identity comparison lives in `vcs/pr_answers/identity.rs`.
- `DispatchError`/`CatalogError` translators stay in this crate (`scripts/check-error-translation.sh`).
- No local `fn redact_*` in `cli_runner` (`scripts/check-artifact-redaction-guardrail.sh`); child environments are composed via `orbit_common::security::child_env` over a cleared env, never inherited.
- Step results are durable: any `job_executor` accounting change needs resume/recovery test coverage, not just the happy path.
- `pr_landing.rs` in the crate-root `tests/engine/` binary leaves `run_private_vcs_operation` at its `NullRuntimeHost` default, so the real provider adapter runs against a substitute `gh` backed by a bare Git remote.
