---
title: External Executor Protocol v1 (Retired)
owner: claude
last_updated: 2026-08-09
last_validated: 2026-09-19
status: Retired
feature: executors
type: design
summary: "RETIRED: External Executor Protocol v1 was never a supported surface; the executor_type external transport was deleted in ORB-10395. Kept as a historical record of the retired wire contract."
tags: [executors, extensibility, protocol, retired]
paths:
  - "crates/orbit-types/src/workflow/executor_def.rs"
related_features: [executors]
related_artifacts: [ORB-00384, ORB-10395]
---

# Spec: External Executor Protocol v1 — RETIRED

> **Status: Retired ([ORB-10395], 2026-07-26).** The External Executor Protocol
> is **not** a supported Orbit surface. `ExecutorType::External`, the
> `ExternalExecutor` implementation, and the shared `direct_agent` subprocess
> transport it rode on were deleted together with the rest of the v1 executor
> stack — the `ActivityExecutor` trait, `ActivityExecutorRegistry`, and the v1
> `ExecutionContext` — when v2 dispatch (`orbit-engine::activity_job`) became the
> only execution path. Nothing in Orbit spawns an `executor_type: external` def
> any more; such a def parses and stores, but is inert.
>
> The `external.example.yaml` template and the conformance fixture referenced
> below were deleted in the same change. **Do not implement against this
> document.** It is retained only so an operator who finds an old `external` def
> can recognize what it was. A future out-of-process extension point would be a
> new, separately decided contract on the v2 dispatch path — see
> [4_decisions.md](../4_decisions.md) §[External Executor Protocol for dynamic out-of-process executor registration (retired)](../4_decisions.md#external-executor-protocol-for-dynamic-out-of-process-executor-registration-retired) for the original rationale and
> its retirement note.

## Historical contract (no longer implemented)

The External Executor Protocol lets an operator register a homegrown executor —
a binary or script — without forking or recompiling Orbit. The operator declares
an `executor_type: external` executor def pointing at a `command`; at run time
Orbit spawns that command, writes a UTF-8 JSON **request envelope** to its
**stdin**, and maps the process **exit code** to the activity outcome. It is the
documented, config-only path for the common "wrapper around an internal CLI or
service" case. See [4_decisions.md](../4_decisions.md) §[External Executor Protocol for dynamic out-of-process executor registration (retired)](../4_decisions.md#external-executor-protocol-for-dynamic-out-of-process-executor-registration-retired) for rationale.

## Why This Exists

`ExecutorType` is a sealed enum and the registry's `load_from_defs` is a closed
match, so before v1 the only way to add an executor was to fork the `internal`-tier
`orbit-engine` crate. The `direct_agent` executor already implemented a generic
out-of-process transport (spawn a command, write a prompt envelope to stdin, map
the result to an outcome), but that transport was undocumented and reachable only
as an agent-family `direct_agent` def. v1 promotes that transport into a named,
documented contract — `external` — that carries **no** agent-family `model_pair`
semantics, so a non-agent homegrown executor is a first-class citizen.

## Registration

An external executor was an `Executor` resource. The copy-paste template that
used to ship at `crates/orbit-core/assets/executors/external.example.yaml` was
deleted in [ORB-10395]; the shape it declared was:

```yaml
schemaVersion: 2
kind: Executor
metadata:
  name: acme-harness        # the registry key; must match the file/metadata name
spec:
  executor_type: external   # selects the External Executor Protocol transport
  command: /opt/acme/bin/harness   # REQUIRED — the binary/script to spawn
  args:                     # operator-fixed args, passed before any runtime args
    - run
    - --json
  env:                      # injected into the subprocess environment
    ACME_PROFILE: ci
  model_flag: --model       # OPTIONAL — see "Runtime model" below
  timeout_seconds: 1800     # OPTIONAL — wall-clock budget (see "Result")
```

Invariants:

- **`command` is required.** A def with `executor_type: external` and no `command`
  is **skipped** at registration (logged as a warning), never registered.
- The def `name` is the registry key. `metadata.name`, the file stem, and the
  registry key must agree (enforced by the executor def store).
- Unknown spec fields are tolerated (additive evolution); `executor_type: external`
  parses on stores that predate v1's variant because the wire enum is open at the
  serde layer.

## Request envelope (stdin)

Orbit writes a single UTF-8 JSON object to the subprocess stdin and then closes
the stream. The shape is the shared execution envelope (`schemaVersion: 1`):

| Field | Presence | Meaning |
|-------|----------|---------|
| `schemaVersion` | always `1` | Envelope version. |
| `activity` | always | The activity definition (id, spec_type, schemas, spec_config). |
| `input` | always | The activity input payload. |
| `skills` | always (may be `[]`) | Resolved skill refs: `{id, content_hash, content, meta?}`. |
| `memory` | always | Memory context block. |
| `job` | when present | Owning job summary (id, state, steps). |
| `task` | when present | Resolved task detail for the input's task id. |

The executor **MUST drain stdin to EOF.** If it exits without reading, Orbit's
stdin writer observes a broken pipe and reports the attempt as an invocation
failure regardless of the exit code. Drain first, then do work.

## Result (exit code + streams)

v1 result semantics are **exit-code based**. Orbit maps the terminated process to
an `AttemptOutcome`:

| Process outcome | Activity state | Notes |
|-----------------|----------------|-------|
| exit `0` | `Success` | |
| non-zero exit | `Failed` | `error_code = AGENT_INVOCATION_FAILED`; trimmed **stderr** becomes the failure message (falling back to `"… exit code Some(N)"`). |
| killed by signal | `Cancelled` | |
| exceeded `timeout_seconds` (wall clock) | `Timeout` | `error_code = AGENT_TIMEOUT`. |

**stdout is captured as audit data but is NOT parsed into workflow state in v1.**
A structured stdout result envelope is a reserved, forward-compatible extension
point; do not depend on Orbit reading stdout in v1. Signal all workflow state
through the exit code, and human-/log-facing diagnostics through stderr.

A "protocol violation" in v1 — malformed behavior, an internal error, refusing to
honor the request — is surfaced the same way as any non-zero exit: the executor
exits non-zero and Orbit records a `Failed` outcome.

## Environment

The subprocess environment is assembled from the host allowlist/inherit policy
plus, in order, these injected vars (operator `env:` and the step's `env_set`
apply last and win on conflict):

- `ORBIT_AGENT_NAME` — the executor def name.
- `ORBIT_AGENT_MODEL` — the runtime model, when one is set.
- `ORBIT_ACTIVITY_ID`, and `ORBIT_ACTIVITY_TOOLS` / `ORBIT_PROC_ALLOWED_PROGRAMS`
  when the activity declares tools / allowed programs.
- Orbit run/state vars when running inside a job.

**Runtime model.** If the def sets `model_flag` and the step carries a runtime
model, Orbit appends `[model_flag, model]` **after** the operator `args`. With
either absent, nothing is appended — encode any fixed model selection directly in
`args`. Unlike `direct_agent`, `external` has no `model_pair_override`: it does not
canonicalize a strong/weak agent model pair for audit attribution.

## Sandbox & trust

- **Tier 1 runs the subprocess unsandboxed (`NoSandbox`)** — identical to the
  historical `direct_agent` registry transport. The registry-path execution
  context carries no `FsProfile`, so the def's `sandbox` / `allow_fallback` fields
  are **inert** for `external` in v1. Real `FsProfile`→OS-sandbox enforcement for
  `external` is deferred to Tier 2, which needs the richer V2 activity context
  (see [4_decisions.md](../4_decisions.md) §[External Executor Protocol for dynamic out-of-process executor registration (retired)](../4_decisions.md#external-executor-protocol-for-dynamic-out-of-process-executor-registration-retired)).
- **Registering an external executor is arbitrary code execution** with the
  runner's privileges. Treat executor defs as trusted configuration. Allowlisting
  or signature-gating registration in untrusted contexts is a follow-up, not part
  of v1.

## Relationship to `direct_agent`

`external` and `direct_agent` share one subprocess transport
(`run_subprocess_executor`): identical command/args/env produce a byte-identical
`ExecRequest`. They differ in identity and intent — `external` is a documented,
versioned contract for non-agent executors and omits agent-family `model_pair`
semantics, whereas `direct_agent` remains the agent-family path.

## Versioning

The `schemaVersion: 1` request envelope and the exit-code result semantics above
are the stable surface of v1. Future capability must be **additive** — new
optional envelope fields, or a new explicit envelope version — never a
breaking reinterpretation of an existing field. A conformance fixture formerly lived at
`crates/orbit-engine/src/executor/tests/external.rs`; it was deleted with the
transport in [ORB-10395].

## Task References

- **[ORB-00384]** — defined External Executor Protocol v1: added `ExecutorType::External`,
  registered a generic external-process executor, wrote this spec, and shipped the
  conformance fixture.
- **[ORB-10395]** — retired the protocol: deleted `ExternalExecutor`, the shared
  `direct_agent` subprocess transport, the executor registry, the example def
  template, and the conformance fixture along with the rest of the v1 executor
  stack. `ExecutorType::External` survives only so pre-existing defs still
  deserialize; dropping the wire variant is a separate release decision.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
