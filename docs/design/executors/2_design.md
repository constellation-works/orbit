---
title: Executors — Design
owner: claude
created_date: 2026-11-28
updated_date: 2026-11-28
status: Draft
feature: executors
doc_role: design
type: design
summary: "How v2 dispatch resolves agent CLI and local-shell executor definitions on separate paths, and what the retired external type still means"
tags: [executors]
paths: ["crates/orbit-types/src/workflow/executor_def.rs", "crates/orbit-core/src/adapter/engine_host/actions/cli_executor.rs", "crates/orbit-engine/src/executor/automation/shell.rs"]
related_features: [executors, activity-job, policy-sandbox]
---

# Executors — Design

This doc describes how the executor definitions summarized in
[1_overview.md](./1_overview.md) are resolved at dispatch. The local-shell contract
(configuration keys, output, supervision and filesystem authority) is specified in
[specs/local-shell.md](./specs/local-shell.md).

## 1. Definitions

Each shipped definition is a `schemaVersion: 2`, `kind: Executor` YAML asset under
`crates/orbit-core/assets/executors/`. Its `spec.executor_type` selects one of the
`ExecutorType` variants in `crates/orbit-types/src/workflow/executor_def.rs`. The agent
definitions (for example `claude.yaml`) declare the provider command, its arguments, the
model flag and the sandbox. `local-shell.yaml` declares no `command` and no `sandbox`, so
each shell activity names its own program and runs bare under Orbit's process supervisor
unless an operator adds a sandbox.

## 2. Two resolution paths

v2 dispatch consults no executor registry. Agent steps resolve their provider through
`resolve_cli_executor`, and `local_shell` steps resolve their definition through
`resolve_local_shell_executor`, both in
`crates/orbit-core/src/adapter/engine_host/actions/cli_executor.rs`. Each rejects the
other kind, so a shell step can never acquire agent authority and an agent step can never
be launched as a bare shell command. Why the shell route is a deterministic action rather
than a revived v1 executor is recorded in
[4_decisions.md](./4_decisions.md#restore-local-shell-as-a-deterministic-v2-action-rather-than-a-v1-executor).

## 3. Retired types

`ExecutorType::External` survives only so pre-existing definitions keep parsing; nothing
spawns it. Dropping the variant is a separate release decision. The older `cli_command`
value is no longer accepted for `executor_type`; a definition that still uses it changes
it to `local_shell` before loading (see
[specs/local-shell.md](./specs/local-shell.md#migrating-cli_command-definitions)).

## 4. Concerns & Honest Limitations

- The shipped `local-shell` definition has no OS sandbox. `cwd` containment validates
  only the child's initial working directory; restrictions on subsequent filesystem
  access come from an explicitly configured OS sandbox or restrictions inherited by
  the Orbit process (see [specs/local-shell.md](./specs/local-shell.md)).
- There is no out-of-process extension point for new executor kinds; one would need a new
  design against v2 dispatch.

## Task References

- [ORB-10395] — retired the v1 executor stack and the External Executor Protocol.
- [ORB-11294] — restored local shell execution as the `local_shell` deterministic action.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
