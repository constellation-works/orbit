---
type: context
summary: Architecture map for orbit-engine: what it owns, main modules, dependencies and entry points.
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
- `activity_job::cli_runner::orchestrator/` splits provider dispatch under `dispatch/` into tool/context preparation, provider envelope/environment/argv and sandbox admission, subprocess supervision, and post-run persistence and completion, with the public entry wrapper in `entry.rs`, child environment helpers in `environment.rs`, and audit/output helpers in `evidence.rs`.
- `activity_job::job_executor::recovery/` groups step recovery retries, decisions, invocation, bounded input, repair commits, evidence and observation. The job-level final recovery hook is under `job_executor/final_recovery/`, split into types, input construction, orchestration, decision admission, and settlement/evidence helpers.
- `executor/automation/vcs/freshness/` separates PR rebasing, interrupted-rebase recovery, divergence and certified checkpoint lookup; `mod.rs` re-exports the existing VCS surface.
- `executor/automation/vcs/operations/` separates private operation dispatch, input parsing, process execution, push retries, and the PR operation family under `operations/pr/` (queries, create, merge, branch/close mutations, body edits, labels, and ready transitions). `vcs/candidate_resume/` separates candidate lookup from applying and recording a resume; required candidate validation remains in `candidate_validate.rs`.
- `executor/automation/vcs/landing/` splits the owner's handoff landing by phase; failed hosted-check classification lives in `vcs/checks/`, shared by landing, PR auto-fix and completion, base refresh and claim; `vcs/pr/complete/` splits PR completion the same way. `vcs/pr/auto_fix/` separates admission and stop verdicts, check classification, repair evidence, published-head adoption, and local publication; `vcs/pr/open/` separates create-or-reuse, templates, superseded PR closure, evidence, and lookup/output projection. `vcs/base/` groups the modules that keep a delivery current with its base, and `vcs/required/` runs a required validation command and keeps its bounded output and evidence.
- `vcs/pr/delivery/` separates delivery identity from check state; `vcs/git/` separates command execution, fetch and timeout budgets.
