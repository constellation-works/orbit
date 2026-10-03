# orbit-engine

Run a v2 activity or job to completion: template resolution, dispatch to a provider CLI or deterministic action, step results and audit rows, retry/resume/fan-out/concurrency. Owns *how* work runs; catalog placement, authorization, and task lifecycle are Core's. Never depends on `orbit-core`, `orbit-cmd`, or a transport crate.

- `RuntimeHost` (`context/hosts.rs`) is the single capability boundary; Core implements it. A new capability is a new trait method with an `unsupported` default — never a widened dependency, a threaded runtime handle, or a direct `orbit-store` read.
- `activity_job::cli_runner` is the one place that names `orbit_agent` types, so Core stays clean of them; keep that edge inside `cli_runner`.
- `executor/automation/` holds deterministic step actions; each new operation gets a file, not another arm.
- `DispatchError`/`CatalogError` translators stay in this crate (`scripts/check-error-translation.sh`).
- No local `fn redact_*` in `cli_runner` (`scripts/check-artifact-redaction-guardrail.sh`); child environments are composed via `orbit_common::security::child_env` over a cleared env, never inherited.
- Step results are durable: any `job_executor` accounting change needs resume/recovery test coverage, not just the happy path.
- Crate-root `tests/` holds end-to-end runtime/CLI-agent/local-shell/name-resolution/worktree-lifecycle/PR-landing/history-note integration tests; `examples/` holds runnable smoke programs. `pr_landing.rs` leaves `run_private_vcs_operation` at its default, so the real provider adapter runs against a substitute `gh` backed by a bare Git remote.
