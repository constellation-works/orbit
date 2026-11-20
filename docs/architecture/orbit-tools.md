---
type: context
summary: Architecture map for orbit-tools: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-tools]
---

# orbit-tools

## What it owns

Tool registry and built-in fs/exec/Orbit tool definitions; plugin loading (`plugin.yaml` v2, exec/mcp backends); sole owner of the `gh` CLI contract.

## Main modules and types

`registry` (`ToolRegistry` in `crates/orbit-tools/src/registry.rs`), `builtin`, `plugin`, `github_cli`, `mcp` (`builtin_mcp_rules`), `external`, `tool` (`Tool`, `ToolContext`). Shared mechanisms live in crate-root leaf modules so non-builtin surfaces need not depend on `crate::builtin`: `program_policy` (`enforce_program_allowlist`, called by `builtin/proc/spawn.rs`, `external` and the plugin backend) and `artifact_materialize` (`prepare_remote_payload` with its bounded/confinement helpers, called by `host::prepare_remote_task_artifact_put` and `builtin/orbit/task/operation.rs`; `prepare_remote_payload` runs the ordinary `source_path` check, `source_presence_error`, first; the separate preloaded-payload check is the `artifact_put` builtin's `preempt` in `builtin/orbit/task/artifact_put.rs`, which refuses `artifacts` payloads that carry `source_path` or come from neither the authenticated ssh-mcp spoke connector nor a worker invocation), `gh_request` (`gh_exec_request` and the GitHub throttle reader `read_throttle`, shared by `builtin/github` and `github_quota`), `github_run_log_cache` (`GITHUB_RUN_LOG_CACHE_DIR` and `remove_legacy_github_run_log_cache`, read by `builtin/github/log_cache.rs`, the plugin backend's read mask and the crate-root re-exports) and `task_guarded_selectors` (`GUARDED_SELECTOR_KEYS`, read by `builtin/orbit/task/guarded.rs` and `mcp/rules.rs`). `github_quota` is called by `builtin/github`, never the reverse. No crate-root module other than `registry` (through `register_builtins`), `lib.rs` and the documented `github_cli` re-exports names `crate::builtin`; `scripts/check-dependency-direction.sh` enforces it. See ``crates/orbit-tools/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-exec`, `orbit-policy`, `orbit-types`.
- Dev-only: itself, so its integration tests enable the `test-support` feature.
- Depended on by: `orbit-cmd`, `orbit-core`, `orbit-engine`, `orbit-mcp`, `orbit-runtime`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-tools/src/lib.rs`.

Design intent: [docs/design/granted-tools/](../../docs/design/granted-tools/) — this page describes the code as it is; the design folder holds intent.
