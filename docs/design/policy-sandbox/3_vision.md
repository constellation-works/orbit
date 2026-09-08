---
summary: "Policy & Sandboxing — Vision"
type: design
title: "Policy & Sandboxing — Vision"
owner: claude
last_updated: 2026-08-15
status: Draft
feature: policy-sandbox
doc_role: vision
tags: ["policy-sandbox"]
last_validated: 2026-09-07
---

# Policy & Sandboxing — Vision

This document captures the questions Orbit must answer before policy and sandboxing become a fuller safety contract. [2_design.md](./2_design.md) describes today's implementation; this file keeps future work distinct from shipped guarantees.

---

## 1. Open Questions

1. **How far should Linux sandboxing go after the first backend?** §1.1 records the shipped Bubblewrap write-confinement boundary. Full read-policy parity, network policy, seccomp, and generic `run_process` adoption remain separate decisions.
2. **Should enforcement move below the tool layer?** The in-process `fs.*` helper is gone ([ORB-10833]). A revived harness, or any future tool that performs filesystem work, is unguarded unless Orbit adds a `PolicyAwareFs` trait, syscall interception, or linting.
3. **Should `proc.spawn` consult policy?** Activity program allowlists are not `PolicyDef`; future shapes include `allowExec` / `denyExec` or env access tied to `fsProfile`.
4. **What is the symlink contract?** `workspace_relative_path` follows symlinks and denies out-of-workspace targets, but the invariant is not yet specified.
5. **Should glob syntax grow?** Character classes, braces, and broader `**` forms would reduce user surprise but may re-evaluate existing profiles differently.
6. **Should `PolicyDecision` and `FsPolicyEvaluation` converge?** A unified outcome could serve future network, exec, and env policy checks.
7. **Should profiles be composable?** `extends:`, `includes:`, or mixins would reduce repetition but add resolution-order questions.
8. **Should empty rule lists warn?** `read: []` / `modify: []` safely denies everything, but a load-time warning would catch likely mistakes earlier.
9. **What is the dry-run / explain story?** A command like `orbit policy explain --profile <name> --op modify --path <path>` would shorten policy authoring loops.
10. **Should all denials share one audit shape?** Fs denials, task-lock denials, program allowlists, and future exec denials still report through different channels; auditability asks the same question.
11. **How should concurrent exec handle signals?** Answered: refcounted SIGINT/SIGTERM install with a lock-free live-pgid table. The handler `killpg`s every registered group; each waiter polls a generation counter. The install mutex is not held across the child's lifetime.
12. **How far should CLI policy coverage go?** macOS `sandbox-exec` narrows writes, but alternatives include trapping CLI fs calls or moving more work to HTTP-backed activities.

### 1.1 Shipped answer: a `linux-bwrap` CLI backend

Orbit uses Bubblewrap as the first Linux OS-level sandbox for CLI-backed agent loops. It
reuses the existing executor declaration → `ResolvedSandbox` → CLI spawn path rather than
starting with the generic `Sandbox` trait: the exposed gap is provider CLIs, and that is already
the seam where macOS turns an activity's resolved `FsProfile` into an outer process wrapper.

The shipped first version is deliberately a **write-confinement backend**, not a claim of byte-for-byte
SBPL parity. It materially improves today's bare Linux execution while keeping unsupported policy
semantics visible instead of silently calling them enforced.

#### Backend and availability contract

- Add `linux-bwrap` as a concrete `ExecutorSandboxKind`; shipped agent executors select it on
  Linux while `local-shell` remains explicitly unsandboxed. Custom executor definitions keep
  their concrete backend choice.
- Resolve Bubblewrap only from a trusted absolute location, initially `/usr/bin/bwrap`. Never
  accept a `PATH`-shadowed wrapper as the security boundary.
- Probe capability, not just file existence. The probe must prove that the installed binary can
  create the required user and mount namespaces and execute a trivial child on the running host.
  A present binary with disabled unprivileged user namespaces is unavailable.
- Preserve the existing `allow_fallback` field. The default remains fail-closed; an unavailable
  backend is a permanent dispatch error with the trusted path and failed probe named. If an
  operator explicitly permits bare fallback, Orbit must retain provider-native sandbox flags
  rather than neutralizing them for an outer wrapper that did not start.

#### Namespace and filesystem shape

The wrapper should construct a deterministic Bubblewrap argv with these properties:

1. Start from the host filesystem mounted read-only, then bind only resolved positive `modify`
   roots and Orbit-owned provider/runtime state roots back as writable.
2. Give the child private user, PID, IPC, and UTS namespaces, a fresh session, parent-death
   cleanup, a minimal `/proc` and `/dev`, and isolated scratch space. Keep the host network
   namespace because CLI agents must reach provider APIs; network restriction is not smuggled
   into this filesystem task.
3. Canonicalize every host path before it becomes a mount argument. Reject missing or escaping
   policy roots unless the root is an Orbit-owned state directory that Orbit creates before
   spawn. Preserve the active-worktree and narrow inherited-Orbit write allowances already
   appended by the v2 host.
4. Apply exact-path and subtree `denyModify` rules as later read-only mounts so they override a
   broader writable parent. Expand non-subtree deny globs over the pre-spawn filesystem snapshot
   and mount every existing match read-only. Record that snapshot-bound enforcement in audit
   metadata.

This gives a kernel-enforced guarantee that the child cannot mutate the host outside the
materialized writable mounts. Non-subtree filename globs such as `**/*.env` have one unavoidable
Bubblewrap limitation: a mount namespace cannot reject a matching filename that the child creates
later inside an otherwise writable directory. Orbit must not describe those rules as fully
kernel-enforced. For a managed shipment worktree, the implementation must add a post-run policy
check that rejects forbidden newly-created paths before commit; the disposable worktree is the
containment boundary, and the design assumes it has one writer for the duration of the invocation.
A direct invocation without that disposable boundary must fail closed when a non-subtree
`denyModify` overlaps a writable root rather than downgrading silently.

#### Read-policy boundary

The initial backend keeps the host's executable, library, certificate, and provider state surface
readable so existing CLIs can start. It may hide concrete existing matches for `denyRead`, but it
does not claim general `read` allowlist or arbitrary negative-glob parity. The invocation audit
must distinguish `write_enforced` from `read_delegated`; there is no remaining in-process `fs.*`
evaluator on the shipped path, so CLI read restrictions remain a harness responsibility.

This is preferable to either extreme: mounting the whole host read-write would not be a sandbox,
while constructing a minimal read tree for several independently-updated provider CLIs would
silently become a brittle container runtime. If secret-read isolation becomes the next priority,
evaluate a second Landlock layer or a filtered filesystem view in its own ADR. Landlock is a good
hierarchical allowlist, but it also cannot directly reproduce Orbit's ordered arbitrary path globs,
and kernel ABI/boot enablement varies by host.

#### Integration and test boundary

- Keep platform compilation and trusted-wrapper spawn in `orbit-exec`; keep `FsProfile`
  resolution, active-worktree detection, and provider/runtime side roots in `orbit-core`; keep
  wrapper selection, provider flag handling, audit argv, and supervision in `orbit-engine`.
- Audit the effective backend, trusted wrapper path, probe outcome, read/write enforcement level,
  and redacted argv. Do not emit `write_enforced` when bare fallback ran.
- Unit-test argv compilation and mount ordering on every platform. Linux runtime tests must use a
  real `/usr/bin/bwrap` when the capability probe succeeds and otherwise skip with the probe
  reason. End-to-end tests must prove an allowed worktree write succeeds, an outside write fails,
  a subtree denial such as `.orbit/**` stays read-only beneath a writable workspace, provider
  network access is not isolated, a forbidden new glob match is rejected before worktree commit,
  a non-worktree invocation with the same unrepresentable rule fails closed, and missing/disabled
  Bubblewrap follows fail-closed versus explicit-fallback behavior.

Bubblewrap is the pragmatic first backend because it is an unprivileged policy-construction tool,
not a policy by itself. Orbit remains responsible for the exact mount and namespace arguments.
The trusted-path rule, `--new-session`, namespace selection, and explicit read-policy limitation
are therefore part of the security contract rather than incidental implementation details.
[ORB-10552] implements this boundary; [Use Bubblewrap for shipped Linux CLI write confinement](./4_decisions.md#use-bubblewrap-for-shipped-linux-cli-write-confinement) records the choice and its deferred read-policy cost.

---

## 2. Prior Work

### 2.1 Orbit-Internal

The [activity-job audit-envelope spec](../activity-job/specs/audit-envelope.md) defines how filesystem and tool denials surface as `V2AuditEvent` entries. The auditability folder ([../auditability/2_design.md §3](../auditability/2_design.md)) documents durable storage.

The current policy schema and merge contract live in `crates/orbit-common/src/types/policy_def.rs` and `crates/orbit-common/src/types/resource.rs`.

### 2.2 OS-Level Sandboxes

`bubblewrap`, `sandbox-exec`, `firejail`, and seccomp-bpf are the near-term isolation options under the `Sandbox` trait. gVisor and Firecracker are heavier options when a workload tolerates a microVM boundary. Bubblewrap's own documentation emphasizes that it constructs namespaces and mounts but leaves the security policy to its caller; that makes the exact Orbit-generated argv part of the design surface, not an implementation detail.

### 2.3 Capability Systems

POSIX capabilities, Capsicum, and Linux Landlock express process rights as capabilities rather than path globs. Landlock is attractive because it is hierarchical and works without root, but it is Linux-only, depends on kernel ABI and boot-time enablement, and cannot directly express Orbit's ordered arbitrary filename globs.

### 2.4 Build Sandboxes

Bazel `exec.sandbox`, Buck2 hermetic execution, and Nix sandboxing treat the workspace as a closed input set. They are stricter than Orbit's allowlist-plus-global-deny model.

### 2.5 Process Supervision Patterns

`tini`, `dumb-init`, and Kubernetes termination grace model the same SIGTERM-then-SIGKILL escalation that `orbit-exec` implements. Per-activity grace periods are a plausible future extension.

---

## 3. What May Be Distinctive

1. **Activity-bound profiles.** Every activity declares its profile, and the resolver re-evaluates per call.
2. **Project-shaped globs.** Profiles use paths such as `./src/**`, trading capability precision for readable project intent.
3. **Global negative denies.** `denyRead` / `denyModify` inject into every resolved profile; no profile opts out of them locally.
4. **Auditable by construction.** CLI sandbox decisions and remaining in-process tool denials still emit audit events; historical `FsCallEvent` rows from retired `fs.*` builtins stay parseable.
5. **Workspace-relative resolution.** Profiles stay portable because paths are evaluated relative to the active workspace.

---

## 4. References

Orbit-internal:

- [1_overview.md](./1_overview.md) — feature purpose and concept map.
- [2_design.md](./2_design.md) — shipped implementation and limitations.
- [specs/fs-profile-resolution.md](./specs/fs-profile-resolution.md) — prescriptive resolution and evaluation contract.
- [specs/sandbox-exec-contract.md](./specs/sandbox-exec-contract.md) — exec spawn and supervision contract.
- [../auditability/2_design.md](../auditability/2_design.md) — how policy denials surface to durable audit.
- [../activity-job/2_design.md](../activity-job/2_design.md) — how activities thread `fsProfile:` through dispatch.

External reference categories:

- OS-level sandboxes: bubblewrap, sandbox-exec, firejail, seccomp-bpf, gVisor, Firecracker.
- Capability systems: POSIX capabilities, Capsicum, Linux Landlock.
- Bubblewrap primary references: [project security model and limitations](https://github.com/containers/bubblewrap/blob/main/README.md#sandbox-security) and [command reference](https://github.com/containers/bubblewrap/blob/main/bwrap.xml).
- Landlock primary reference: [Linux kernel userspace API](https://docs.kernel.org/userspace-api/landlock.html).
- Build sandboxes: Bazel exec.sandbox, Buck2 hermetic execution, Nix build sandbox.
- Supervision patterns: tini, dumb-init, Kubernetes terminationGracePeriodSeconds.

---

## Task References

- **[T20260416-0728]** — Established the v2 policy contract that this document extends.
- **[T20260419-0503]** — Made `fsProfiles` enforcement runtime-wide.
- **[T20260417-0558-4]** / **[T20260417-0558-5]** — Hardened the supervision contract that §1.11 wants to evolve.
- **[T20260426-0605]** — Auditability folder linked from §1.10.
- **[T20260426-0622]** — Add this folder and name the open questions.
- **[T20260430-23]** — Shorten the policy sandbox design docs while preserving the shipped contract and ADR history.
- **[ORB-10552]** — Implement the shipped fail-closed Linux Bubblewrap write-confinement backend for CLI agents.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
