---
summary: "Policy & Sandboxing — Overview"
type: design
title: "Policy & Sandboxing — Overview"
owner: claude
last_updated: 2026-09-06
last_validated: 2026-09-06
status: Draft
feature: policy-sandbox
doc_role: overview
tags: ["policy-sandbox"]
---

# Policy & Sandboxing — Overview

> **Sandbox backend status.** Shipped CLI-agent executors select `macos-sandbox-exec` on macOS and `linux-bwrap` on Linux; `local-shell` remains bare. Linux Bubblewrap provides fail-closed kernel write confinement after a capability probe, while read-policy parity stays delegated. Windows has no OS wrapper. Process supervision continues to apply on every platform. The former in-process `fs.*` builtins were retired ([ORB-10828], [ORB-10833]). [ORB-10552] [Use Bubblewrap for shipped Linux CLI write confinement](./4_decisions.md#use-bubblewrap-for-shipped-linux-cli-write-confinement)

Policy & Sandboxing is Orbit's safety surface for filesystem access and process execution. It combines v2 `PolicyDef` profiles, global `denyRead` / `denyModify` rules, optional macOS `sandbox-exec` wrapping for CLI agents, Linux Bubblewrap write confinement, and `orbit-exec` process supervision. [2_design.md](./2_design.md) documents what ships today; [3_vision.md](./3_vision.md) names the gaps to a fuller isolation contract.

---

## 1. Motivation

Orbit runs agents against user repositories, so the safety boundary is a product feature rather than an internal hygiene concern.

1. **Default paths stay explicit.** Omitting `fsProfile:` maps to `unrestricted`, then still runs through profile resolution and global denies.
2. **Profiles are activity-scoped.** A job can mix profiles by activity; evaluation happens per call, not by mutating a process-global mode.
3. **Deny rules are global.** `denyRead` and `denyModify` are injected into every resolved profile. A host policy may name a strictly nested `denyModify` exception, but the selected profile must already authorize it and workspace policy cannot expand the host exception surface.
4. **Execution has two layers.** `orbit-exec` always supervises child processes; CLI-agent writes are OS-confined where the configured executor uses macOS `sandbox-exec` or Linux Bubblewrap.
5. **Denials are evidence.** CLI sandbox denials and remaining in-process tool denials (for example `proc.spawn`) still feed audit channels; [auditability](../auditability/) owns durable storage. Historical `FsCallEvent` rows from retired `fs.*` builtins remain parseable.

---

## 2. Core Concepts

### 2.1 Policy is v2-only

`PolicyDef` accepts only schema v2: `denyRead`, `denyModify`, and named `FsProfile` entries with `read` / `modify` glob rules. Workspace profiles override globals by name; global denies accumulate.

### 2.2 Profile resolution materializes an implicit `unrestricted`

When an activity omits `fsProfile:`, the v2 host uses `UNRESTRICTED_FS_PROFILE`. If the policy does not define that profile, the resolver synthesizes `read: ["./**"]` and `modify: ["./**"]`, then injects global denies.

### 2.3 Path evaluation is last-match-wins over a normalized rule list

`PolicyDef::check_path` evaluates normalized workspace-relative paths against positive and negated rules. The last matching rule wins. Empty positive sets deny with `[]`; unmatched positive sets deny with `<no matching rule>`.

The shipped host policy keeps `.orbit/**` protected, then explicitly re-allows only repository-versioned definitions and configuration: `.orbit/auto_tasks/**`, `.orbit/routines/**`, `.orbit/config.toml`, and `.orbit/resources/**`. Checkout-local `.orbit/config.yaml` is runtime identity, is ignored by the initializer, and remains protected from managed agents. Runtime state, Orbit-owned records, databases, locks, and unknown `.orbit` paths stay protected. These exceptions intersect the activity profile; they do not derive authority from task `context_files`. [ORB-11376]

Linux provider launch materializes a missing write-grant anchor before spawning, because Bubblewrap cannot bind-mount a nonexistent child beneath the read-only `.orbit` parent. The set of anchors is read off the effective profile that compiles the same argv — every narrow re-allow nested under an earlier deny — so it cannot drift from what the kernel enforces, and it is re-derived at each spawn rather than snapshotted once. Materialization runs only inside the disposable worktree, never opens an existing target for writing, and rejects symlinks and filesystem-type mismatches. Task `context_files` are not consulted: they are planning selectors, not policy authority. A grant the plan cannot mount is reported against its path and rule, never silently dropped. [ORB-10602]

### 2.4 Enforcement depends on backend

Shipped agent activities run on the CLI path only. They do not call in-process `fs.*` builtins — that family was retired in [ORB-10828] and [ORB-10833]. Filesystem writes are constrained by harness delegation plus the configured executor sandbox: `sandbox-exec` on macOS and Bubblewrap write confinement on Linux. `FsCallEvent` / `FsAuditLogger` types remain on `ToolContext` for historical traces and a possible future harness, but nothing registered emits them.

When the default policy denies workspace `.orbit/**`, the v2 host re-allows only the narrow child Orbit runtime stores needed by currently activity-exposed write tools. It does not blanket-allow workspace `.orbit`; newly exposed Orbit write tools must add their store roots intentionally.

### 2.5 Exec supervision is not default OS isolation

`orbit-exec::run_process` spawns a process-group leader, drains stdout/stderr, installs SIGINT/SIGTERM handlers, and on timeout or signal sends SIGTERM to the group with a 5 second grace before SIGKILL. The default `Sandbox` impl remains `NoSandbox`; OS isolation is added by specific executor wrappers, not the default runner.

---

## 3. At a Glance

| Concern | Where it lives | Primary task ID |
|---------|----------------|-----------------|
| Policy schema and validation | `crates/orbit-common/src/types/policy_def.rs`, `crates/orbit-common/src/types/resource.rs` | [T20260416-0728] |
| Allow/deny enum | `crates/orbit-common/src/types/policy_decision.rs` | [T20260426-0622] |
| Policy facade | `crates/orbit-policy/src/{lib,engine,evaluator,decision}.rs` | [T20260416-0728] |
| Profile resolution + deny injection | `crates/orbit-common/src/types/policy_def.rs` (`effective_profile`, `check_path`) | [T20260416-0728] |
| Versioned `.orbit` modify boundary and missing-anchor preparation | shipped `default.yaml`, profile resolution, `cli_runner::spawn`, OS sandbox compilers | [ORB-10560], [ORB-10573], [ORB-10602] |
| Implicit `unrestricted` materialization | `crates/orbit-core/src/runtime/v2_host/mod.rs` (`tool_context_for_activity`) | [T20260419-0503] |
| Retired tool-layer fs enforcement | Removed with the `fs.*` builtins ([ORB-10828], [ORB-10833]); `FsAuditLogger` types remain in `crates/orbit-tools/src/lib.rs` | [ORB-10833] |
| Activity `fsProfile:` binding | `crates/orbit-engine/src/activity_job/{dispatcher,job_executor,agent_loop_driver}.rs` | [T20260419-0503] |
| Exec spawn primitive | `crates/orbit-exec/src/{lib,runner,process,sandbox}.rs` | [T20260417-0550] |
| Linux CLI write confinement | `crates/orbit-exec/src/linux_sandbox.rs` | [ORB-10552] |
| Process supervision | `crates/orbit-exec/src/supervision/{wait,cleanup,signal,tee}.rs` | [T20260417-0558-4], [T20260417-0558-5] |
| Filesystem denial audit channel | `crates/orbit-tools/src/lib.rs` (`FsAuditLogger`) → `docs/design/auditability/2_design.md §3` | [T20260426-0605] |

---

## Task References

- **[T20260416-0728]** — Align policy contract with runtime enforcement (v2 schema, effective profile resolution).
- **[T20260417-0550]** — Decompose `orbit-exec` supervision modules.
- **[T20260417-0558-4]** / **[T20260417-0558-5]** — Harden `orbit-exec` supervision (signal pipe, process-group reaping).
- **[T20260419-0503]** — Enforce `fsProfiles` across runtime and CLI.
- **[T20260426-0605]** — Add the auditability design folder cross-linked from §3.
- **[T20260426-0622]** — Add this policy & sandboxing design folder under claude ownership.
- **[T20260430-23]** — Shorten the policy sandbox design docs while preserving the shipped contract and ADR history.
- **[ORB-00129]** — Keep child Orbit runtime write roots narrow under the macOS sandbox while supporting activity-exposed learning, friction, and job-run state tools.
- **[ORB-10552]** — Add fail-closed Linux Bubblewrap write confinement for shipped CLI agents.
- **[ORB-10560]** — Permit only explicit versioned `.orbit` configuration beneath the default protected boundary.
- **[ORB-10573]** — Prepare only missing, exactly scoped versioned-config anchors that remain permitted by the effective host policy/profile.
- **[ORB-10602]** — Derive write-grant anchors from the effective profile at each spawn instead of a hardcoded table plus a context-file snapshot, and report every unmountable grant.
- **[ORB-11376]** — Keep checkout-local runtime identity outside the agent-write and sandbox-anchor surface; add exact-registration recovery for missing or corrupt identity.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
