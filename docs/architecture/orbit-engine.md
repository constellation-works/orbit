---
type: context
summary: Architecture map for orbit-engine: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
tags: [architecture, orbit-engine]
---

# orbit-engine

## What it owns

Executes activities/jobs: template rendering, retries, subprocess + tool-aware automation, CLI agent runner; candidate identity, reviewer repair commits, `pr_open`/`pr_complete` rechecks, CI evidence (`ci_log_signature`).

## Main modules and types

`activity_job` (`dispatcher`, `job_executor`, `cli_runner`, `crew`, `audit_writer`), `executor`, `context` (`RuntimeHost`), `template`, `condition`. See ``crates/orbit-engine/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-agent`, `orbit-common`, `orbit-exec`, `orbit-store`, `orbit-tools`, `orbit-types`.
- Depended on by: `orbit-core`, `orbit-runtime`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-engine/src/lib.rs`.

Design intent: [docs/design/activity-job/](../../docs/design/activity-job/) — this page describes the code as it is; the design folder holds intent.

## Module layout

How the modules under `crates/orbit-engine/src/` split. The dependency guard, the compatibility shims and the crate-root `tests/engine/` binary are in the [crate guide](../../crates/orbit-engine/AGENTS.md).

- `orbit_agent` is used in `activity_job::cli_runner`, `activity_job::audit_writer`, and `activity_job::sqlite_sink`; the crate root also re-exports `provider_supports_continuity`.
- `activity_job::cli_runner::supervisor/` runs provider children on `orbit_common::process::supervisor`, which owns the wait loop, pipe workers and stdin writer. It keeps only the engine's side: the provider request and hooks (`spawn.rs`), the rolling head/tail capture with redaction and protocol tail (`capture.rs`), and the per-line trace events of its output sink (`output.rs`). Its sibling `tests/` files mirror those concerns.
- `activity_job::cli_runner::orchestrator/` splits provider dispatch under `activity_job/cli_runner/orchestrator/dispatch/` into tool/context preparation, provider envelope/environment/argv and sandbox admission, subprocess supervision, and post-run persistence and completion, with the public entry wrapper in `entry.rs`, child environment helpers in `environment.rs`, and audit/output helpers in `evidence.rs`.
- `activity_job::job_executor::recovery/` groups step recovery retries, decisions, invocation, bounded input, repair commits, evidence and observation. The job-level final recovery hook is under `activity_job/job_executor/final_recovery/`, split into types, input construction, orchestration, decision admission, and settlement/evidence helpers.
- `executor/automation/vcs/freshness/` separates PR rebasing, interrupted-rebase recovery, divergence and certified checkpoint lookup; `mod.rs` re-exports the existing VCS surface.
- `executor/automation/vcs/operations/` separates private operation dispatch, input parsing, process execution, push retries, and the PR operation family under `executor/automation/vcs/operations/pr/` (queries, create, merge, branch/close mutations, body edits, labels, and ready transitions). `executor/automation/vcs/candidate_resume/` separates candidate lookup from applying and recording a resume; required candidate validation remains in `candidate_validate.rs`.
- `executor/automation/vcs/landing/` splits the owner's handoff landing by phase; failed hosted-check classification lives in `executor/automation/vcs/checks/`, shared by landing, PR auto-fix and completion, base refresh and claim; `executor/automation/vcs/pr/complete/` splits PR completion the same way. `executor/automation/vcs/pr/auto_fix/` separates admission and stop verdicts, check classification, repair evidence, published-head adoption, and local publication; `executor/automation/vcs/pr/open/` separates create-or-reuse, templates, superseded PR closure, evidence, and lookup/output projection. `executor/automation/vcs/base/` groups the modules that keep a delivery current with its base, and `executor/automation/vcs/required/` runs a required validation command and keeps its bounded output and evidence.
- `executor/automation/vcs/pr/delivery/` separates delivery identity from check state; `executor/automation/vcs/git/` separates command execution, fetch and timeout budgets.
- Functions in `executor/automation/vcs/{pr,checks,commit}/` declare the host capabilities used by their call paths. Code that runs Git locally also requires `RuntimeHostProcessEnvironment` when it reads the host's process environment.

## Ownership by module

- `activity_job`: v2 activity/job dispatcher, CLI provider runner, job executor, retry/recovery, and audit sinks; imports `condition`, `context`, `executor`, `sandbox_spawn`, `template`, and lower workspace crates.
- `condition`: Expression-based step condition evaluator over `TemplateContext`; imports `template` and `orbit-common` only.
- `context`: `RuntimeHost` capability boundary, dispatch contracts, null host, and execution-context primitives; imports `executor` (never `activity_job`) and lower workspace crates.
- `executor`: Built-in deterministic automation and VCS/PR actions invoked by job steps; imports `context`, `sandbox_spawn` (never `activity_job`), and lower workspace crates.
- `sandbox_spawn`: Sandboxed child-process spawn and permanent/transient spawn failure classification; imports `context` only inside the crate plus `orbit-common`, `orbit-exec`, and `orbit-types`.
- `template`: Template context and `{{...}}` interpolation renderer for job steps; imports `orbit-common` and `serde_json` only.
