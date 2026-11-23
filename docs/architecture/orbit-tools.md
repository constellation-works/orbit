---
type: context
summary: Architecture map for orbit-tools: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-tools]
---

# orbit-tools

## What it owns

Tool registry and built-in fs/exec/Orbit tool definitions; plugin loading (`plugin.yaml` v2, exec/mcp backends); sole owner of the `gh` CLI contract.

## Main modules and types

`registry` is the crate's sole builder of builtin registries. It calls
`builtin` to register tool definitions, which use `ToolRegistry` to register
themselves. `registry` derives the canonical MCP definitions and caches one
catalog of declared routes for `builtin_mcp_rules` and tool names for
`PluginValidationPolicy::host_default`. The dependency guard prevents
`mcp` and `plugin` from referencing `registry` or `ToolRegistry`.

Shared mechanisms live in crate-root leaf modules:

- `program_policy` provides `enforce_program_allowlist` for `builtin/proc/spawn.rs`, `external` and the plugin backend.
- `artifact_materialize` owns `prepare_remote_task_artifact_put` and the bounded, confined `prepare_remote_payload` shared with `builtin/orbit/task/operation.rs`. That helper first checks `source_path` via `source_presence_error`. The artifact-put builtin's `preempt` in `builtin/orbit/task/artifact_put.rs` refuses preloaded `artifacts` that carry `source_path`. It accepts preloaded payloads only from an authenticated SSH MCP spoke connector or a worker invocation.
- `gh_request` provides `gh_exec_request` and the throttle reader `read_throttle` for `builtin/github` and `github_quota`. The GitHub builtins call `github_quota`.
- `github_run_log_cache` provides `GITHUB_RUN_LOG_CACHE_DIR` and `remove_legacy_github_run_log_cache` for the crate-root re-exports. The GitHub log cache and the plugin backend's read mask use the cache path.
- `task_guarded_selectors` supplies `GUARDED_SELECTOR_KEYS` for the task guarded-write parser and the MCP workspace-echo rules.
- `timeouts` defines the shared network timeouts used by the GitHub and process builtins, `external` and `plugin`.

The dependency guard limits references to `builtin` to `registry`, the
crate-root exports and the documented `github_cli` re-exports. The
`host` module owns `OrbitToolHost` and `ReservationOwnerContext`.
`tool` defines `Tool` and `context` defines `ToolContext`.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-exec`, `orbit-policy`, `orbit-types`.
- Dev-only: itself, so its integration tests enable the `test-support` feature.
- Depended on by: `orbit-cmd`, `orbit-core`, `orbit-engine`, `orbit-mcp`, `orbit-runtime`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-tools/src/lib.rs`.

Design intent: [docs/design/granted-tools/](../../docs/design/granted-tools/) — this page describes the code as it is; the design folder holds intent.
