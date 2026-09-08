---
summary: "Policy & Sandboxing — Decisions"
type: design
title: "Policy & Sandboxing — Decisions"
owner: claude
last_updated: 2026-09-08
status: Draft
feature: policy-sandbox
doc_role: decisions
tags: ["policy-sandbox"]
last_validated: 2026-09-08
---

# Policy & Sandboxing — Decisions

This file preserves the feature's decision reasoning. New entries follow the template in [../CONVENTIONS.md](../CONVENTIONS.md) and cite the task that made the decision real.

---

## Dedicated policy & sandboxing design ownership

**Recorded:** 2026-05-11 02:06:39.393746Z · [T20260426-0622]

### Context
Policy and sandboxing semantics were spread across `orbit-policy`, `orbit-exec`, the `PolicyDef` schema in `orbit-common`, the activity dispatcher, and the v2 host. There was no canonical place to record invariants, the `unrestricted` fallback, or the supervision contract.

### Decision
Create `docs/design/policy-sandbox/` as the canonical design folder, with claude as owner. Auditability owns the recording of denials; this folder owns the *semantics* of allow/deny and the *contract* for how spawned processes are supervised.

### Consequences
- Policy and sandboxing decisions now have one decision log, one glossary, and a feature-owned spec to cite.
- Cost: this folder cross-links into auditability and activity-job, so when those folders change their cross-references must be kept in sync rather than this folder absorbing them.

## Policy schema is v2-only with named profiles plus global denies

**Recorded:** 2026-05-11 02:06:39.394869Z · [T20260416-0728]

### Context
An earlier policy schema (v1) used a different shape for allow/deny rules. Supporting both shapes in the runtime caused interpretation drift between the loader, the merger, and the evaluator.

### Decision
Reject `schemaVersion: 1` at load time with an explicit migration message. v2 declares `denyRead`, `denyModify`, and `fsProfiles` and is the only accepted shape. Workspace policies override globals by profile name; global denies accumulate.

### Consequences
- Schema parsing has one supported branch, and profile authoring is uniformly `{ read, modify }` with global denies.
- Cost: existing v1 policy files require a manual migration; there is no automatic upgrader.

## Implicit `unrestricted` profile materializes when an activity omits `fsProfile:`

**Recorded:** 2026-05-11 02:06:39.396202Z · [T20260419-0503]

### Context
Activities can omit `fsProfile:`. A naive design would either reject the activity at load or run it without policy enforcement. Both are wrong: rejection breaks the common case, and unguarded execution means audit blindness.

### Decision
When an activity omits `fsProfile:`, the v2 host substitutes the constant `UNRESTRICTED_FS_PROFILE` ("unrestricted") at `tool_context_for_activity`. If the policy does not define a profile of that name, the resolver synthesizes `read: ["./**"]` and `modify: ["./**"]`. Global `denyRead` / `denyModify` rules still apply because they are injected after profile resolution.

### Consequences
- "Unrestricted" remains auditable and narrowed by global denies, while policy authors can shadow it with a real profile.
- Cost: the word "unrestricted" carries different meaning depending on whether the policy defines a profile of that name, which is a learnable but real source of confusion.

## Deny rules inject as negated profile rules with last-match-wins evaluation

**Recorded:** 2026-05-11 02:06:39.397360Z · [T20260416-0728]

### Context
A separate "deny pass" before profile evaluation is the obvious shape, but it makes precedence ambiguous when a profile rule and a deny rule both match. Multiple Orbit features (workspace overrides, profile narrowing, denyModify-also-implies-denyRead-for-modify validation) need a single evaluation order.

### Decision
`effective_profile` appends every entry of `denyRead` to the profile's `read` list as `!<rule>` and every entry of `denyModify` to the profile's `modify` list as `!<rule>`. `check_path` walks the resolved list in order and the **last match wins**. There is no separate deny pass.

### Consequences
- Profile rules and deny rules are evaluated in one deterministic pass; appended denies win over earlier positive matches.
- Cost: a profile author cannot re-allow a globally denied path by ordering, which is the intended safety property but surprises authors who expect a simple allowlist with overrides.

## Modify rules must be covered by a read rule in the same profile

**Recorded:** 2026-05-11 02:06:39.398601Z · [T20260416-0728]

### Context
A profile that grants `modify: ["./build/**"]` without granting `read: ["./build/**"]` is technically valid but produces a confusing operational story: a tool may be allowed to write a file it cannot read, breaking the standard read-modify-write pattern.

### Decision
`PolicyDef::validate` rejects any profile whose positive `modify` rule is not covered by a positive `read` rule in the same profile. "Covered" is checked structurally (`rule_covers_path_rule`): exact match, `**`, or a `<prefix>/**` rule that prefixes the modify rule.

### Consequences
- Modify rules require corresponding read coverage, so read-modify-write audit stories stay consistent.
- Cost: profile authors who *only* want to allow append-style writes cannot express that without granting a read rule. There is no "write-only" profile shape today.

## Tool layer is the policy enforcement point for HTTP-backed activities

**Recorded:** 2026-05-11 02:06:39.399820Z · [T20260419-0503]
**Status:** Superseded by [Retire the remaining unregistered fs builtins and their policy helpers](#retire-the-remaining-unregistered-fs-builtins-and-their-policy-helpers) ([ORB-10833])

### Context
Policy enforcement could plausibly live at the syscall layer, the fs trait layer, the tool layer, or the activity layer. Each placement has different trust and coverage tradeoffs.

### Decision
Enforcement lives in `orbit-tools::builtin::fs::enforce_fs_policy`. Every fs builtin calls it before the underlying read or modify, and emits `FsCallEvent` through `FsAuditLogger`. The `Sandbox` trait in `orbit-exec` does not consult the policy engine; exec is supervised but not policy-gated. This applies only to `backend: http` activities — `backend: cli` runs spawn an external CLI agent and emit a `tool_allowlist.harness_delegated` event in lieu of enforcement.

### Consequences
- HTTP-backed fs decisions have one auditable helper, but tool authors must route work through it.
- Cost: CLI-backed activities still bypass this helper, and HTTP tools that skip it are also unguarded. Current macOS executors can narrow CLI filesystem writes with `sandbox-exec`, but closing the general gap likely requires a `PolicyAwareFs` trait, broader OS sandboxes, or both.

## Retire the remaining unregistered fs builtins and their policy helpers

**Recorded:** 2026-08-15 · [ORB-10833]
**Status:** Accepted
**Supersedes:** [Tool layer is the policy enforcement point for HTTP-backed activities](#tool-layer-is-the-policy-enforcement-point-for-http-backed-activities)

### Context
[ORB-10828] removed the last registered filesystem builtins (`fs.read`, `fs.delete`). The rest of the family (`write`, `patch`, `ls`, `copy`, `move`, `mkdir`, `create`) stayed on disk behind a no-op `fs::register` so that PR could stay scoped. The private policy helpers (`check_read_policy`, `check_modify_policy`, `check_workspace_boundary`, `enforce_fs_policy`) existed only for those unregistered tools. The live agent path is CLI-only and never hands a `ToolRegistry` to the subprocess.

### Decision
Delete the remaining unregistered `fs.*` implementations and the private policy helpers with them. Do not keep the helpers as a future-harness library. Keep public `FsCallEvent` / `FsAuditLogger` / `fs_profile` plumbing on `ToolContext` because the engine still constructs it and historical audit rows still name retired tools. A revived in-process harness is a clean revert, not leftover dead code.

### Consequences
- `ToolRegistry::register_builtins()` registers no `fs.*` tools. Activity and skill text must name provider-native filesystem tools, not Orbit `fs.*` builtins.
- Historical store/audit fixtures that mention retired tool names continue to parse; unknown tool names are not a deserialization error.
- Cost: a future in-process filesystem harness must rebuild the retired helper (or move enforcement below the tool layer). The rejected alternative was leaving `enforce_fs_policy` compiled but unused behind `#![allow(dead_code)]`.

## Children spawn as process-group leaders so orphan subprocesses are reapable

**Recorded:** 2026-05-11 02:06:39.400978Z · [T20260417-0558-4], [T20260328-221810]

### Context
Naive subprocess code on Unix leaves orphan grandchildren holding open pipe write ends, which causes the parent's `wait_with_output` to hang when the orphan never exits. Earlier versions of orbit-exec hit this exact failure when an agent's tool spawned long-lived helpers.

### Decision
On Unix, every spawned child calls `command.process_group(0)` so the child becomes a process-group leader (PGID = PID). The supervision layer kills the entire group via `killpg` when the child exits, when the parent receives SIGINT/SIGTERM, or when the deadline expires.

### Consequences
- Orphan subprocesses are reaped, and signal handling can target the whole tree with one syscall.
- Cost: tools that intentionally fork detached helpers (e.g., long-running daemons) cannot do so under orbit-exec without explicitly creating their own process group inside the child.

## SIGTERM with 5-second grace, then SIGKILL for the whole group

**Recorded:** 2026-05-11 02:06:39.402116Z · [T20260417-0558-4]

### Context
A timed-out or interrupted child needs a chance to flush state before being killed, but the supervisor cannot wait indefinitely. The escalation policy needs a single, predictable shape.

### Decision
`terminate_process_group` sends `SIGTERM` (or the supplied signal) to the group, polls `process_group_is_alive` for `TERMINATION_GRACE_PERIOD = 5 seconds`, and on expiry sends `SIGKILL` to the group plus a direct `child.kill()`/`child.wait()`. stderr is annotated with `process timed out` (deadline path) or `process interrupted by signal SIG…` (parent-signal path).

### Consequences
- Termination is deterministic, and annotated stderr distinguishes timeout, signal, and clean-exit paths.
- Cost: the 5-second constant is global. Activities that need a longer drain (database flush, large I/O cleanup) cannot extend it without code changes.

## Signal handler installation is process-global and refcounted

**Recorded:** 2026-05-11 02:06:39.403286Z · [T20260417-0558-5]
**Updated:** 2026-09-08 · [ORB-11697]

### Context
Installing parent-side SIGINT/SIGTERM handlers is a process-global operation. Two concurrent `run_process` calls cannot install independent handlers without races, and a panicking call must restore the prior handler so the orbit process itself remains interruptible. Holding the install mutex for each child's entire lifetime also silently serialized every supervised subprocess in the daemon (parallel job branches, MCP `proc.spawn`, web-server `gh` calls).

### Decision
`SignalHandlerGuard::install` refcounts a process-wide handler. The first live waiter installs SIGINT/SIGTERM and snapshots the previous `sigaction` structs; the last drop restores them and re-raises a captured signal (except `SIG_IGN`) so daemons still shut down. The mutex is held only for that install/drop critical section, never across `raise`. Each waiter registers its child's pgid in a lock-free table and snapshots a signal generation counter. The handler is async-signal-safe: atomic store of the signal, generation, and pending-forward, then `killpg` to every registered group. Waiters poll the generation counter and still run `terminate_process_group` so a missed slot or a race with unregister cannot leave a child running.

### Consequences
- Concurrent `run_process` / `supervise_child` waits overlap. Ctrl-C terminates every live child process group, not only the waiter that would have held an exclusive mutex.
- The captured SIGINT/SIGTERM is re-raised after restore, so a daemon that installed tokio `ctrl_c` / SIGTERM (or that still has SIG_DFL) actually shuts down instead of swallowing the signal.
- Panics still restore prior handlers via the last Drop.
- Cost: at most `MAX_LIVE_PROCESS_GROUPS` children get immediate handler-side `killpg`; additional waiters terminate on the next 100 ms poll instead. The table is not a semaphore.

## `NoSandbox` is the default `Sandbox` impl; real isolation is deferred

**Recorded:** 2026-05-11 02:06:39.404695Z · [T20260417-0550]

### Context
The `Sandbox` trait is the seam where kernel-level or container-level isolation would attach to `orbit-exec`. The trait shipped with the supervision rework, but no real impl is registered.

### Decision
Ship `NoSandbox` as the default and only implementation. Defer kernel-level isolation (bubblewrap, sandbox-exec, container, seccomp) until policy enforcement at the tool layer is judged insufficient and the platform-coverage cost is understood. The trait surface is stable so a future impl can attach without changing the runner.

### Consequences
- The trait surface is stable for future isolation, while today's generic runner stays explicit about relying on CLI OS wrappers plus remaining in-process tool policy (`proc.spawn`).
- Cost: a future in-process filesystem tool has no leftover helper and no generic exec-level isolation backstop. This is the structural reason §1.1 of [3_vision.md](./3_vision.md) lists real sandboxing as the top open question.

## `sandbox-exec` wraps cli-backend agent invocations on macOS

**Recorded:** 2026-05-11 02:06:39.406378Z · [T20260427-51]

### Context
[Tool layer is the policy enforcement point for HTTP-backed activities](#tool-layer-is-the-policy-enforcement-point-for-http-backed-activities) left CLI backends outside Orbit's tool-layer enforcement: the harness emits `tool_allowlist.harness_delegated`, but Claude/Codex/Gemini built-in tools run with the orbit process's filesystem rights. Provider-native sandboxes were inconsistent (`codex --sandbox`, `gemini -s`, no Claude equivalent), leaving `fsProfile` unenforced for some CLI runs.

### Decision
Add `orbit-exec::macos_sandbox` as the declarative seam: compile a `ResolvedFsProfile` to SBPL and wrap Claude, Codex, and Gemini invocations with `sandbox-exec -f <profile>` when executor YAML declares `spec.sandbox: macos-sandbox-exec`. When Orbit owns the outer sandbox, neutralize provider-native sandbox flags so there is one filesystem authority. Resolve descriptors in `V2RuntimeHost::resolve_executor_sandbox` and compile SBPL in orbit-engine near the spawn site.

### Consequences
- All three providers share `FsProfile` compiled to SBPL as the macOS filesystem authority, giving Claude OS-enforced narrowing too.
- `allow_fallback` can degrade gracefully, but the safe default is fail-closed; Linux, Docker, network restriction, and activity-level overrides stay out of scope for v1.
- Cost: SBPL writes are static text; complex `denyRead` / `denyModify` rule combinations don't always translate cleanly. Simple subtree denials use `subpath`; non-subpath deny globs use SBPL `regex` to avoid over-denying the containing directory. Activities that need precise allow-side glob semantics under sandbox should declare profiles with explicit subpath roots.

## Codex state and side roots are narrow sandbox write allowances

**Recorded:** 2026-05-11 02:06:39.407865Z · [T20260428-10]

### Context
Codex-backed `agent_implement` reached startup under `sandbox-exec` but failed with `Operation not permitted`: the profile allowed worktree, temp/cache, and `$HOME/.orbit` writes but not Codex state. After that, workflow state still failed because policy denied workspace `.orbit/**` after Orbit passed the same root via Codex `--add-dir`, and `**/*.env` over-denied when compiled as a containing-directory `subpath`.

### Decision
Keep `sandbox-exec` authoritative and add narrow Codex allowances: `$CODEX_HOME` or `$HOME/.codex`, plus Codex side-write roots from runtime provider config appended after policy-derived denials. Compile non-subpath deny globs such as `**/*.env` as SBPL `regex` clauses. Do not grant broad `$HOME` writes or disable the outer sandbox.

### Consequences
- Codex-backed `backend: cli` runs can initialize under the macOS sandbox while project writes stay constrained by the resolved `fsProfile`.
- `CODEX_HOME` relocates state, and inherited Orbit subprocesses can persist workflow state through the same side roots Codex receives.
- Cost: the Codex state directory and provider side roots are trusted writable state outside ordinary project-content policy, similar to the existing `$HOME/.orbit` allowance for inherited Orbit subprocesses.

## Per-provider state-dir allowances are emitted unconditionally for every supported CLI

**Recorded:** 2026-05-11 02:06:39.409178Z · [T20260428-14]

### Context
[Codex state and side roots are narrow sandbox write allowances](#codex-state-and-side-roots-are-narrow-sandbox-write-allowances) unblocked Codex state writes, but Claude writes startup state under `$HOME/.claude` or `$CLAUDE_CONFIG_DIR`, and Gemini writes under `$HOME/.gemini`. SBPL compilation receives `ResolvedFsProfile` plus host env, not the active provider, so provider-conditional allow clauses would require new plumbing.

### Decision
Emit state-dir allows for all supported CLI providers on every macOS sandbox profile: `$CODEX_HOME` / `$HOME/.codex`, `$CLAUDE_CONFIG_DIR` / `$HOME/.claude`, and `$HOME/.gemini`. Keep `append_provider_side_write_roots` Codex-only because Claude and Gemini have no `--add-dir` equivalent; document that a future provider with such a surface should generalize the branch.

### Consequences
- Claude and Gemini reach past CLI startup under `macos-sandbox-exec` with the same state-dir defense story as Codex.
- Emitting all three narrow state-dir allowances avoids provider plumbing; Codex side roots remain a separate branch until another provider ships an equivalent surface.
- Cost: every macOS sandbox profile carries three state-dir allow clauses regardless of which provider runs. If a future provider's state dir overlaps with another sensitive root, this design needs revisiting.

## Claude state surface includes `$HOME/.claude.json` siblings, not just `$HOME/.claude/`

**Recorded:** 2026-05-11 02:06:39.410317Z · [T20260508-13]

### Context
[Per-provider state-dir allowances are emitted unconditionally for every supported CLI](#per-provider-state-dir-allowances-are-emitted-unconditionally-for-every-supported-cli) modeled Claude's state surface as the `$HOME/.claude/` directory (or `$CLAUDE_CONFIG_DIR` when set) and emitted a single `(allow file-write* (subpath ...))` clause per provider state dir. In practice, Claude Code persists its main settings to `$HOME/.claude.json` — a sibling *file* at the home root, with `.lock` and atomic-write `.tmp.<pid>.<ms_ts>` companions. SBPL `subpath` only matches the named directory and everything strictly below, so `.claude.json` (a sibling, not a child) was denied at the kernel. Symptom: every Claude invocation under `macos-sandbox-exec` lost the ability to update its state, and tool calls that wait on the state-file lock hung silently. Codex/Gemini were unaffected because all of their state lives under their state directories.

The override case is clean: when `CLAUDE_CONFIG_DIR` is set, Claude writes `<override>/.claude.json` and its siblings inside the override directory, already covered by the existing `(subpath "$CLAUDE_CONFIG_DIR")` clause.

### Decision
When the SBPL profile is compiled with `CLAUDE_CONFIG_DIR` unset and `HOME` resolved, additionally emit:

- `(allow file-write* (literal "$HOME/.claude.json"))`
- `(allow file-write* (literal "$HOME/.claude.json.lock"))`
- `(allow file-write* (regex "^$HOME/\.claude\.json\.tmp\.[0-9]+\.[0-9]+$"))`

Use `literal` for the canonical and lock files (predictable names) and `regex` for the tmp pattern. The home prefix in the regex is escaped with the existing `push_regex_escaped` helper so symlink-free home paths containing regex meta characters do not widen the allow.

### Consequences
- Claude under `macos-sandbox-exec` can persist settings and acquire its lockfile; tool calls that depend on a freshly-updated state file no longer hang.
- The `CLAUDE_CONFIG_DIR` branch is unchanged — the existing subpath clause already covers the JSON file inside the override.
- Cost: three additional clauses on every macOS sandbox profile when `HOME` resolves and `CLAUDE_CONFIG_DIR` is unset. Symmetric to the [Per-provider state-dir allowances are emitted unconditionally for every supported CLI](#per-provider-state-dir-allowances-are-emitted-unconditionally-for-every-supported-cli) trade-off; provider plumbing is avoided.
- This ADR amends [Per-provider state-dir allowances are emitted unconditionally for every supported CLI](#per-provider-state-dir-allowances-are-emitted-unconditionally-for-every-supported-cli) rather than replacing it: the per-provider state-dir clauses still emit unconditionally; the new clauses are scoped to the HOME-fallback branch only.

## macOS sandbox wrapper resolves from trusted absolute locations

**Recorded:** 2026-05-11 02:06:39.411675Z · [T20260509-30]

### Context
The macOS CLI wrapper previously spawned `sandbox-exec` by bare name and checked availability by walking `PATH`. A writable or config-influenced `PATH` could point Orbit at an attacker-controlled wrapper while Orbit still believed kernel sandbox enforcement was active.

### Decision
Resolve the wrapper only from trusted absolute locations, currently `/usr/bin/sandbox-exec`, and use the same trusted resolver for availability checks, audit argv, and process spawn. Missing trusted binaries fail closed unless the executor explicitly allows fallback, and the error names the trusted location that was probed.

### Consequences
- Fake `sandbox-exec` binaries earlier on `PATH` are ignored, so the sandbox boundary no longer depends on inherited environment ordering.
- Availability messages describe the trusted absolute location instead of implying arbitrary `PATH` lookup.
- Cost: the implementation is intentionally macOS-location-specific; if Apple moves or removes the binary, Orbit must update the trusted location list or add a new backend rather than silently accepting a user-supplied replacement.

---

## Task References

- **[T20260328-221810]** — Subprocess termination on Ctrl+C / job cancel; predecessor of the current process-group design.
- **[T20260416-0728]** — Aligned the policy contract with runtime enforcement; v2 schema and effective-profile resolution land here.
- **[T20260417-0550]** — Decomposed `orbit-exec` supervision modules.
- **[T20260417-0558-4]** / **[T20260417-0558-5]** — Hardened `orbit-exec` supervision (process-group reaping, signal-pipe handler).
- **[T20260419-0503]** — Enforced `fsProfiles` across runtime and CLI; introduced `tool_context_for_activity`.
- **[T20260426-0622]** — Add this design folder and record the initial ADR set.
- **[T20260427-51]** — Wrap cli-backend agent invocations in `sandbox-exec` on macOS with inner-flag neutralization for codex/gemini.
- **[T20260428-10]** — Allow Codex CLI state writes under the macOS sandbox.
- **[T20260428-14]** — Extend the macOS sandbox state-dir allowance to Claude and Gemini, and document why side-write roots remain Codex-only.
- **[T20260430-23]** — Shorten the policy sandbox design docs while preserving the shipped contract and ADR history.
- **[T20260508-13]** — Add `$HOME/.claude.json{,.lock,.tmp.<pid>.<ms_ts>}` sibling allows to the macOS sandbox profile so Claude can persist its main settings file.
- **[T20260509-30]** — Resolve `sandbox-exec` from trusted absolute locations rather than inherited `PATH`.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

## Use Bubblewrap for shipped Linux CLI write confinement

**Recorded:** 2026-08-01 23:26:11.357573Z · [ORB-10552]
**Paths:** `crates/orbit-exec/src/linux_sandbox.rs`, `crates/orbit-engine/src/activity_job/cli_runner/**/*.rs`, `crates/orbit-core/src/runtime/v2_host/sandbox.rs`

### Context
Linux CLI agents previously ran with the worker account's ambient filesystem rights. The real alternatives were a Bubblewrap mount-namespace boundary, a Landlock allowlist layer, or continued delegation to provider-native sandboxes; Bubblewrap closes the highest-value write gap at the existing executor wrapper seam without turning Orbit into a container runtime.

### Decision
Shipped Linux agent executors use the concrete `linux-bwrap` backend. Orbit resolves only `/usr/bin/bwrap`, capability-probes the namespaces and mounts it needs, mounts the host root read-only, rebinds canonical policy and runtime roots writable in rule order, retains the host network namespace, and fails closed unless `allow_fallback` explicitly permits bare execution. This backend claims `write_enforced` and `read_delegated`; read-policy parity remains deferred.

### Consequences
- Linux CLI agents gain kernel-backed write confinement without changing the generic `run_process + NoSandbox` contract or the macOS backend.
- Non-subtree deny globs are snapshot-expanded; managed single-writer worktrees receive a post-run new-match check, while direct overlapping invocations fail closed.
- Provider-native sandbox flags are neutralized only after the outer wrapper passes its capability probe, so bare fallback retains the provider boundary.
- Cost: Bubblewrap availability depends on `/usr/bin/bwrap` plus host user-namespace policy, and write confinement deliberately leaves the broad host read surface and provider network access delegated for a later decision.

## Sandbox availability is a host precondition, not a runtime fallback

**Recorded:** 2026-08-08 19:13:44.348233Z
**Paths:** `crates/orbit-exec/src/linux_sandbox.rs`, `crates/orbit-engine/src/activity_job/cli_runner/spawn.rs`, `crates/orbit-core/assets/executors/**`, `docs/runbooks/**`

### Context

The Linux sandbox executor launches agent processes under a user-namespace sandbox. On hosts where unprivileged user namespaces are restricted, the sandbox helper cannot establish its uid map and every dispatch fails at spawn with a permission error that reads, to anyone above the spawn layer, like an executor defect.

A per-dispatch preflight now exists — it actually executes the sandbox helper rather than merely checking that the binary is installed — and dispatch fails closed with a remediation hint when the probe fails. Executors may declare an opt-in that permits running without the sandbox, and that opt-in defaults off.

What is still unrecorded is the stance behind all of this. Two questions have no written answer:

1. When the host cannot provide the sandbox, is the correct outcome to refuse, or to degrade?
2. Is making the host capable an operator responsibility, or is it Orbit's job to work anywhere?

Without an answer, each incident is re-litigated from scratch. The most recent occurrence appears to have been cleared out of band, with no record of what was changed or by whom — which is the concrete cost of having no stated position. A related gap follows from the same silence: the opt-in exists as a declarable field with no documented operator procedure telling anyone when it is legitimate to set it, so its only realistic uses are panic and cargo-culting.

The alternatives considered were: making the sandbox best-effort with automatic fallback to an unsandboxed run; attempting to detect and work around restricted namespaces inside the sandbox helper by emitting a different argument shape; and treating sandbox availability as a host precondition.

### Decision

Treat sandbox availability as a host precondition. Orbit verifies it and refuses; it does not silently degrade.

Concretely: the preflight stays fail-closed and stays per-dispatch. When it fails, the failure is permanent and names the host condition rather than the symptom. Running without the sandbox remains possible only through the explicit per-executor opt-in, which stays defaulted off and is documented as an operator decision with a stated risk, not a troubleshooting step. Orbit does not attempt to detect restricted namespace configurations and emit an alternative argument shape to work around them.

Making a host capable of running the sandbox is an operator responsibility, and the supported remedies belong in the runbook rather than in code.

### Consequences

- A host that cannot sandbox fails loudly and early, at a layer that knows why, instead of producing spawn errors that read as executor or agent defects.
- The opt-in acquires a meaning: it marks a deliberate, recorded acceptance of running unsandboxed on a specific executor, rather than an undocumented escape hatch.
- Out-of-band host changes stop being invisible, because the runbook names what a correct host looks like and the preflight asserts it.
- Cost: an operator whose host cannot be reconfigured — a managed or hardened environment where unprivileged user namespaces are unavailable and cannot be enabled — has no supported path except opting individual executors out of sandboxing entirely. This decision deliberately refuses the middle ground of automatic degradation, which means that operator carries a coarser, more explicit risk than a fallback would have given them.
- Cost: the per-dispatch probe executes the sandbox helper on every dispatch. That is a real cost paid on every run to catch a condition that changes rarely, accepted because a stale cached answer is worse than the probe.

## Derive Linux sandbox write-grant anchors from the effective profile at each spawn

**Recorded:** 2026-08-09 03:42:52.076176Z · [ORB-10602]
**Paths:** `crates/orbit-exec/src/linux_sandbox.rs`, `crates/orbit-engine/src/activity_job/cli_runner/spawn.rs`

### Context

Bubblewrap cannot bind-mount a path that does not exist, so a policy re-allow beneath the read-only `.orbit` mount only takes effect if its anchor is already on disk. Three separate mechanisms handled that, and all three were wrong in the same direction.

Materialization ran once, in `worktree_setup`, over a snapshot of the task's `context_files`, using the un-absolutized policy profile — a profile that omits the host-appended run roots the spawn actually enforces. The set could therefore neither match the kernel's view nor grow during the run.

Which targets were eligible was gated on a hardcoded `[(path, kind); 5]` table matched by exact tuple. The table duplicated the shipped policy's `denyModify` exception list, so the two could drift; and because membership included the filesystem kind, a grant naming a *file* beneath a directory in the table missed the match entirely. That case was reported roughly half an hour after the table shipped.

Finally, `compile_linux_bwrap_argv` returned nothing for a positive rule whose path was absent. The grant silently stayed under the read-only bind, and the agent received an unattributable `Read-only file system` mid-turn.

### Decision

Derive the write-grant set from the effective `ResolvedFsProfile` — every positive exact/subtree rule that is a narrow re-allow beneath an earlier deny — and materialize absent anchors in `spawn_linux_bwrap`, immediately before compiling argv from that same profile.

Anchor shape is derived from evidence in order: what is on disk, then the rule's syntax (`<root>/**` is necessarily a directory), then whether another rule nests beneath the anchor, then whether the leaf carries a file extension. Directory is the residual default, since a directory anchor still admits files created inside it while a file anchor admits nothing.

Creation is confined to the managed worktree: every component that root owns is symlink-checked, files use create-new semantics, and an existing anchor of the wrong type fails closed naming its path and rule. Anchors outside that root are host-owned and are reported rather than invented. `LinuxBwrapPlan` carries a `dropped_grants` list, and a grant left unmountable inside a managed worktree is a permanent spawn failure naming the path and the granting rule.

## Rejected alternatives

**Keep materialization in `worktree_setup` and widen the table.** The table is the defect, not its length: it duplicates the policy's exception list, and any table keyed by kind reproduces the file-under-granted-directory miss. Setup also cannot see the host-appended roots, so its grant set is structurally unable to match the enforced one.

**Re-derive grants continuously during the run.** A live child's mount namespace is fixed at `bwrap` exec; there is no way to add a mount to a running provider without re-spawning it. Per-spawn derivation is the finest granularity the backend actually supports, so the chosen point is not a compromise between fast and correct — it is the only place the question can be answered.

**Fail the run at setup when a task's context selector names a path policy denies.** Rejected: `context_files` are read references at least as often as write targets, so this fails runs that merely cite a policy or resources file as reading context. `linux_bwrap_write_grant_diagnostic` provides the attributable path-and-rule explanation without converting a read reference into a refusal.

### Consequences

- A granted path that does not exist yet is usable by the sandboxed process, and file-versus-directory no longer decides whether a grant takes effect.
- Adding a versioned `.orbit` path is now a one-line policy change; no Rust inventory tracks it.
- The grant set is recomputed per spawn from the enforced profile, so it cannot drift from the kernel's view.
- A denial is attributable before the provider starts, against a path and a rule, instead of as an EROFS inside an agent turn.
- Cost: an exact *file* grant that is absent, untracked, and un-ignored leaves an empty anchor in the worktree that `git add -A` would stage. Checkout-local `.orbit/config.yaml` is no longer such a grant after [ORB-11376]; repository-versioned exact grants must still account for this behavior.
- Cost: anchor shape for an absent exact rule is inferred, not declared. Spelling a directory grant as `<path>/**` remains the way to state it unambiguously.
- Read-only git metadata for linked worktrees is unchanged and still out of scope.

## Derive Linux sandbox write-grant anchors from the effective profile at each spawn

**Recorded:** 2026-08-08 20:36:47.656731Z · [ORB-10602], [ORB-10607]
**Paths:** `crates/orbit-exec/src/linux_sandbox.rs`, `crates/orbit-engine/src/activity_job/cli_runner/**/*.rs`, `docs/design/policy-sandbox/**`

### Context
Bubblewrap can only re-bind an exception beneath a read-only parent when the exception anchor exists. The prior hardcoded path/type inventory drifted from effective policy, while preparing every apparent re-allow would materialize paths shadowed by later workspace denies and filename-shape inference could not distinguish dotted directories from extensionless files.

### Decision
Derive Linux write-grant candidates at every provider spawn from the same ordered ResolvedFsProfile used to compile Bubblewrap argv. Materialize only narrow re-allows whose anchor is writable under the final last-match-wins decision; exact rules denote file anchors and `<root>/**` rules denote directory anchors. Confine creation to the canonical managed worktree, reject symlinks in every worktree-owned component and canonical escapes, and translate child-reported EROFS paths into Orbit policy diagnostics after failed invocations.

### Consequences
- Policy evaluation, anchor preparation, mount compilation, and denial diagnostics share one effective rule sequence without a hardcoded path inventory.
- Later workspace denies prevent materialization, while narrower denies below a writable subtree preserve the remaining grant.
- Cost: policy authors must use exact syntax for file anchors and `<root>/**` syntax for directory anchors; an existing target whose filesystem type contradicts that syntax fails closed.
- Cost: post-invocation attribution depends on the child including the attempted path in its EROFS stderr; failures that omit the path retain the generic nonzero-exit diagnostic.

## Keep checkout identity outside managed-agent write grants

**Recorded:** 2026-09-06 · [ORB-11376]
**Paths:** `crates/orbit-core/assets/policies/default.yaml`, `crates/orbit-cli/src/command/workspace/init.rs`, `docs/runbooks/state-and-backup.md`

### Context

The Linux pre-spawn writer derives every narrow re-allow from the effective policy and creates an absent exact file as a zero-byte Bubblewrap mount anchor. The default policy still re-allowed `.orbit/config.yaml` even though workspace initialization ignores that checkout-local identity rather than versioning it. A worker branch created before the initializer's `.gitignore` landed therefore retained the empty anchor, and its delivery commit introduced a tracked zero-byte identity. Resumed local delivery exposed and merged that already-created artifact; no evidence identifies resume as the writer.

### Decision

Keep `.orbit/config.yaml` beneath the default `.orbit/**` agent-write deny. The initializer remains the authoritative writer. `workspace init --force` may restore a missing or malformed identity only after the global registry supplies both the requested logical workspace and the exact checkout path/data-root binding. It archives malformed bytes under `.orbit/state/recovery/workspace-identity/` before atomically restoring the registered ID. A parseable different workspace ID remains an ownership conflict and is never replaced through this recovery.

### Rejected alternatives

**Delete empty anchors just before commit.** Emptiness is not evidence that a repository file is disposable, and commit-time cleanup would duplicate policy semantics after the writer already ran.

**Relax identity mismatch validation.** A parseable different ID is a competing ownership claim, not corruption. Replacing it would turn `--force` into a registry bypass.

**Require operators to hand-create the YAML.** Manual reconstruction loses the supported ownership proof and makes it easier to bind the child to a parent or invented workspace.

### Consequences

- Managed agents cannot create or modify checkout identity, and Linux launch has no identity anchor to leak into delivery.
- Exact registered checkouts have a bounded recovery command; corrupt bytes remain available as local evidence.
- Workspace identity changes remain an operator-owned initializer action rather than ordinary repository editing.

## Task References

- **[T20260328-221810]** — Subprocess termination on Ctrl+C / job cancel; predecessor of the current process-group design.
- **[T20260416-0728]** — Aligned the policy contract with runtime enforcement; v2 schema and effective-profile resolution land here.
- **[T20260417-0550]** — Decomposed `orbit-exec` supervision modules.
- **[T20260417-0558-4]** / **[T20260417-0558-5]** — Hardened `orbit-exec` supervision (process-group reaping, signal-pipe handler).
- **[T20260419-0503]** — Enforced `fsProfiles` across runtime and CLI; introduced `tool_context_for_activity`.
- **[T20260426-0622]** — Add this design folder and record the initial ADR set.
- **[T20260427-51]** — Wrap cli-backend agent invocations in `sandbox-exec` on macOS with inner-flag neutralization for codex/gemini.
- **[T20260428-10]** — Allow Codex CLI state writes under the macOS sandbox.
- **[T20260428-14]** — Extend the macOS sandbox state-dir allowance to Claude and Gemini, and document why side-write roots remain Codex-only.
- **[T20260430-23]** — Shorten the policy sandbox design docs while preserving the shipped contract and ADR history.
- **[T20260508-13]** — Add `$HOME/.claude.json{,.lock,.tmp.<pid>.<ms_ts>}` sibling allows to the macOS sandbox profile so Claude can persist its main settings file.
- **[T20260509-30]** — Resolve `sandbox-exec` from trusted absolute locations rather than inherited `PATH`.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.

## Task References

- **[T20260328-221810]** — Subprocess termination on Ctrl+C / job cancel; predecessor of the current process-group design.
- **[T20260416-0728]** — Aligned the policy contract with runtime enforcement; v2 schema and effective-profile resolution land here.
- **[T20260417-0550]** — Decomposed `orbit-exec` supervision modules.
- **[T20260417-0558-4]** / **[T20260417-0558-5]** — Hardened `orbit-exec` supervision (process-group reaping, signal-pipe handler).
- **[T20260419-0503]** — Enforced `fsProfiles` across runtime and CLI; introduced `tool_context_for_activity`.
- **[T20260426-0622]** — Add this design folder and record the initial ADR set.
- **[T20260427-51]** — Wrap cli-backend agent invocations in `sandbox-exec` on macOS with inner-flag neutralization for codex/gemini.
- **[T20260428-10]** — Allow Codex CLI state writes under the macOS sandbox.
- **[T20260428-14]** — Extend the macOS sandbox state-dir allowance to Claude and Gemini, and document why side-write roots remain Codex-only.
- **[T20260430-23]** — Shorten the policy sandbox design docs while preserving the shipped contract and ADR history.
- **[T20260508-13]** — Add `$HOME/.claude.json{,.lock,.tmp.<pid>.<ms_ts>}` sibling allows to the macOS sandbox profile so Claude can persist its main settings file.
- **[T20260509-30]** — Resolve `sandbox-exec` from trusted absolute locations rather than inherited `PATH`.
- **[ORB-00048]** — Extend the unconditional provider state-dir allowance set to include Grok's `$HOME/.grok` state directory while hardening fourth-family scoreboards and analytics.
- **[ORB-10552]** — Implement fail-closed Linux Bubblewrap write confinement and preserve the explicit read-policy limitation.
- **[ORB-10560]** — Amend global deny resolution with profile-intersected host modify exceptions for versioned `.orbit` configuration.
- **[ORB-10573]** — Amend Linux delivery with trusted, two-gate preparation of missing versioned-config mount anchors.
- **[ORB-10602]** — Derive write-grant anchors from the effective profile at each spawn; remove the hardcoded target inventory and the context-file materialization gate. [Derive Linux sandbox write-grant anchors from the effective profile at each spawn](#derive-linux-sandbox-write-grant-anchors-from-the-effective-profile-at-each-spawn-1)
- **[ORB-10607]** — Enforce final-policy materialization, canonical/symlink containment, rule-derived anchor types, and production failed-write attribution. [Derive Linux sandbox write-grant anchors from the effective profile at each spawn](#derive-linux-sandbox-write-grant-anchors-from-the-effective-profile-at-each-spawn-1)
- **[ORB-10833]** — Retire the remaining unregistered `fs.*` builtins and their private policy helpers. [Retire the remaining unregistered fs builtins and their policy helpers](#retire-the-remaining-unregistered-fs-builtins-and-their-policy-helpers)
- **[ORB-11376]** — Protect checkout-local runtime identity from managed-agent writes and add exact-registration recovery. [Keep checkout identity outside managed-agent write grants](#keep-checkout-identity-outside-managed-agent-write-grants)

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
