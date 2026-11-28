---
type: context
summary: "Refuse, not degrade: when Orbit cannot verify a fact a step depends on, it refuses with a remediation hint and does not substitute a weaker path"
last_validated: 2026-11-27
tags: [principle, refusal, sandbox, providers, admission]
related_features: [policy-sandbox, executors, activity-job, plugins, auditability]
---

# Refuse, Not Degrade

When Orbit cannot verify something a step depends on (the sandbox, a credential, ownership, delivery evidence, the provider a crew names), it refuses with a remediation hint. It does not substitute a weaker path.

## Why

Orbit runs provider CLIs inside the operator's repositories, with the operator's credentials within reach. What matters most is whether the boundary held, and a step that quietly runs weaker than declared hides that.

The cost of the alternative was observed. On a host that restricts unprivileged user namespaces, every dispatch failed at spawn with a permission error that read, to anyone above the spawn layer, like an executor defect. The most recent occurrence appears to have been cleared out of band, with no record of what changed. The [sandbox decision](../design/policy-sandbox/4_decisions.md#sandbox-availability-is-a-host-precondition-not-a-runtime-fallback) records both and chose refusal. The alternative it rejected, a best-effort sandbox that falls back to an unsandboxed run, would have kept dispatch running on such hosts. It would also have made the missing sandbox invisible, because the work succeeded.

The code-level split between checks that must refuse and side channels that may continue is [STD-02 §R31](../standards/STD-02-rust-architecture-and-errors/STD-02.md). This page does not restate it.

## In practice

- **The sandbox probe.** Before a Linux executor's provider starts, Orbit runs a real Bubblewrap capability probe. When the probe fails, dispatch is refused with the probe's detail, and the refusal names the opt-in below. The remedies are in the [Linux sandbox runbook](../runbooks/linux-sandbox.md#if-the-probe-still-fails), and the mechanism is in the [policy-sandbox design](../design/policy-sandbox/2_design.md#71-linux-bubblewrap-backend). The macOS `sandbox-exec` check follows the same rule. Checked in `crates/orbit-engine/src/sandbox_spawn.rs` (`prepare_linux_sandbox_for_dispatch_with_probe` and `prepare_sandbox_for_dispatch`).

- **The opt-ins.** An operator can choose a weaker path on a named resource. A failure never chooses it.
  - `allow_fallback: true` on an executor permits bare execution when the probe fails. It defaults to false, and the invocation's sandbox metadata records `bare-fallback`. Checked in `crates/orbit-types/src/workflow/executor_def.rs` and `sandbox_spawn.rs`.
  - `spec.sandbox: off` on an executor resource disables the wrapper for that executor on the host. The runbook calls it an operator opt-out, not a fallback, and the audit reports `sandbox_backend: off`. See [disabling worker sandboxing](../runbooks/linux-sandbox.md#explicitly-disable-worker-sandboxing).
  - A plugin backend with `sandbox: none` runs unsandboxed only when the operator grants `unsandboxed`. A brokered call to such a backend is refused before spawn ([agent-call broker](../design/plugins/specs/agent-call-broker.md#5-confinement-of-a-brokered-backend)). Checked in `crates/orbit-tools/src/plugin/backend/brokered.rs`.

- **A declared default.** `local-shell` declares no `sandbox`, so its steps run bare and are confined only by the working-directory check and by whatever contains the Orbit process. This is the resource's own declaration, not a runtime fallback, and the [local-shell spec](../design/executors/specs/local-shell.md) says how to add one. Checked in the no-sandbox branch of `prepare_sandbox_for_dispatch`.

- **Provider substitution.** Never substituting another provider silently is one case of this rule. An explicit provider that cannot run fails with a stable diagnostic and never falls back to another runtime: an HTTP-only or unsupported provider, or an unknown or retired name. A retired backend selector is refused at config load, not remapped onto the CLI path ([backend resolution](../design/activity-job/specs/backend-resolution.md)). When every member of a crew pool is usage-limited, the task waits rather than drawing the workspace default, which is usually the most expensive crew ([CONFIG: provider usage limits](../config/crews.md#provider-usage-limits)). The provider rules are in [CONFIG: no silent fallback](../config/providers.md#no-silent-fallback). Checked in `crates/orbit-core/src/adapter/engine_host/actions/cli_executor.rs`, `crates/orbit-types/src/workflow/activity_job/activity_v2.rs`, `crates/orbit-config/src/resolved/config.rs` (`reject_retired_backend_overrides`) and `crates/orbit-core/src/application/task/provider_limit/gate.rs`.

- **Credentials.** On macOS, a Claude activity with neither `CLAUDE_CODE_OAUTH_TOKEN` nor `ANTHROPIC_API_KEY` is refused before `claude` starts. It does not fall back to the Claude Desktop login, which the Desktop revokes mid-run ([CONFIG: provider credentials](../CONFIG.md#provider-credentials)). Checked in `crates/orbit-engine/src/activity_job/cli_runner/orchestrator/environment.rs`.

- **Fail-open cases.** A few failures are logged or reported, and the operation goes on. None of them gates a claim Orbit has verified.
  - A step checkpoint that cannot be written logs a warning, and the run continues without a durable checkpoint for that step, so a later resume cannot skip it ([activity-job design](../design/activity-job/2_design.md#86-persisted-state-for-v2-job-runs)). Checked in `crates/orbit-engine/src/activity_job/job_executor/execute.rs`.
  - An unreadable, stale or malformed validation-environment warning cache is recomputed, and an unwritable one only logs at debug level ([CONFIG](../CONFIG.md)). Checked in `crates/orbit-runtime/src/orbit_runtime/validation_warning.rs`.
  - An audit row that cannot be written does not fail a successful call. A read-only result returns with a warning. A mutating call returns success with an `audit_row_unpersisted` warning, because the mutation has already committed ([command audit rows](../design/auditability/2_design.md#2-command-audit-rows)). Checked in `crates/orbit-core/src/adapter/command/dispatch/execute/finalize.rs`.

## What it costs

- A host that cannot provide the sandbox stops dispatching until someone fixes it. An operator whose host cannot be reconfigured has no supported path except opting an executor out, which is a coarser risk than a fallback would have carried. The sandbox decision accepts that cost.
- Work waits, and some of it does not finish. A task whose crew is limited waits once no unlimited member of its pool is left, and a run that could have completed on a weaker path reports failure. The operator, not the step, decides whether a weaker path is acceptable.
- Fail-open cases leave gaps that only a record shows. A committed mutation can lack its audit row, and a resume can re-run a step that lost its checkpoint. The warning in the log is the only trace.

## Applying it

For a new step or path, ask: if it cannot verify a fact, does it stop and say what to fix? If it would continue on a weaker path, that continuation must be an operator's opt-in on a named resource, or a fail-open case that logs or reports its failure and gates no verified claim. The weaker path is never the one taken on failure, and a provider, runtime or sandbox is never chosen because it happens to be installed.

The boundary is the difference between a precondition and a declaration. A resource that declares no sandbox, such as `local-shell`, has made its choice visibly. A cache, metric or display detail has no precondition to verify. Neither is a degrade.
