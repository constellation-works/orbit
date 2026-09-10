---
summary: "Policy & Sandboxing — Design"
type: design
title: "Policy & Sandboxing — Design"
owner: claude
last_updated: 2026-09-10
last_validated: 2026-09-10
status: Draft
feature: policy-sandbox
doc_role: design
tags: ["policy-sandbox"]
---

# Policy & Sandboxing — Design

This document describes Orbit's shipped policy and sandboxing implementation: v2 `PolicyDef`, profile resolution, last-match-wins path evaluation, activity/job `fsProfile` binding, macOS and Linux CLI sandbox wrapping, and `orbit-exec` supervision. See [1_overview.md](./1_overview.md) for purpose and [3_vision.md](./3_vision.md) for forward-looking gaps.

---

## 1. Policy Schema

`PolicyDef` in `crates/orbit-common/src/types/policy_def.rs` is v2-only. `crates/orbit-common/src/types/resource.rs` rejects schema v1 with a migration message that names `spec.denyRead`, `spec.denyModify`, and `spec.fsProfiles`.

A valid policy declares `name`, optional `description`, global `denyRead` / `denyModify`, and `fsProfiles` mapping names to `FsProfile { read, modify }`. The policy name must also pass the centralized resource-name validator in `crates/orbit-common/src/types/resource.rs`: it is a non-empty single file stem, not a hidden dot name, and contains no separators, traversal markers, drive-prefix characters, extension dots, or control characters ([T20260509-28]). File-backed stores validate before constructing `<name>.yaml` paths.

`PolicyDef::validate` enforces:

1. The policy name is a safe resource file stem.
2. Every profile name is non-empty.
3. Every positive `modify` rule is covered by a positive `read` rule in the same profile.
4. Profile rules do not exactly duplicate global deny entries.
5. `denyRead` never contains exceptions. A `denyModify` exception uses `!<path>`, names an exact path or `<path>/**` subtree, and is strictly contained by an earlier deny in the same policy.

`PolicyDef::merged(global, workspace)` lets workspace `fsProfiles` overwrite globals by name while global denies accumulate. A workspace may repeat or narrow a host `denyModify` exception, but cannot introduce an exception outside the host exception surface. Workspace denies are appended after host exceptions and therefore can narrow them. The merged policy is revalidated.

The shipped default expresses the versioned Orbit boundary as an ordered `.orbit/**` deny followed by exceptions for `.orbit/auto_tasks/**`, `.orbit/routines/**`, `.orbit/config.toml`, and `.orbit/resources/**`. Checkout-local `.orbit/config.yaml` is ignored runtime identity rather than repository configuration; it stays under the deny and therefore cannot become a managed-worktree sandbox anchor ([ORB-11376]). The broad deny continues to cover `.orbit/state/**`, task/learning/ADR/friction stores, databases, locks, and any future or misspelled `.orbit` path. Task `context_files` remain planning and conflict selectors; policy resolution does not convert them into filesystem grants ([ORB-10560]), and anchor materialization does not consult them at all ([ORB-10602]).

---

## 2. Profile Resolution

`PolicyDef::effective_profile(profile_name)` returns a `ResolvedFsProfile { name, read, modify }` after applying three transformations:

1. **Lookup.** Use the named profile. If the missing name is `unrestricted`, synthesize `read: ["./**"]` and `modify: ["./**"]`; other missing profiles return `OrbitError::InvalidInput`.
2. **Normalization.** Trim, convert backslashes, strip leading `./`, reject absolute, `~`, and parent-traversal rules, then compile the narrow glob syntax to regex.
3. **Deny injection.** Append `denyRead` to `read` as negated rules. Walk `denyModify` in order: ordinary entries append as negated rules, while `!<path>` entries are host exceptions. An exception is intersected with the selected profile, so an empty/read-only profile gains nothing and profile negative rules still narrow the result.

The implicit `unrestricted` profile appears only when an activity omitted `fsProfile:` and the policy did not define `unrestricted`. A real profile with that name shadows the fallback.

---

## 3. Path Evaluation

`PolicyDef::check_path(profile, op, path)` returns an `FsCheckResult { allowed, matched_rule }`. The algorithm:

1. Resolve the profile (via §2).
2. Pick the rule list by operation (`read` or `modify`).
3. If the list is empty, deny with `matched_rule = "[]"`.
4. Walk rules in order and record the most recent match against the normalized workspace-relative path. Later matches override earlier ones.
5. Use the last match's negation flag. If no rule matched but a positive rule exists, deny with `<no matching rule>`; if only negated rules exist, deny with `[]`.

Path normalization (`normalize_path`) trims, flips slashes, strips `./` prefixes, and rejects absolute paths, `~`-anchored paths, and parent-directory traversal anywhere in the component list ([T20260509-27]). Callers canonicalize first (via `orbit_policy::resolve_symlinks` / `PolicyEngine::check_resolved`) and then express the path workspace-relative.

The glob translator supports `*`, `**`, `?`, and `<prefix>/**`. It is intentionally narrower than POSIX glob syntax.

---

## 4. PolicyEngine Facade

`crates/orbit-policy/src/lib.rs` re-exports `PolicyEngine`, `FsPolicyEvaluation`, and `PolicyDecision`. `PolicyEngine` wraps a validated `PolicyDef` and exposes:

```
PolicyEngine::check(profile, operation, path) -> FsPolicyEvaluation
```

`FsPolicyEvaluation` carries `{ profile, operation, path, allowed, matched_rule }`. `evaluator.rs` currently passes through to `PolicyDef::check_path`; the indirection leaves room for caching or layered evaluators later.

`PolicyDecision` (`crates/orbit-common/src/types/policy_decision.rs`) is a separate `Allow | Deny { reason }` enum for broader policy/RBAC callers. `PolicyEngine::check` does not produce it; fs callers use `FsPolicyEvaluation`.

---

## 5. Tool-Layer Enforcement (retired)

The in-process `fs.*` builtins and their private helper `enforce_fs_policy` were retired in [ORB-10828] and [ORB-10833]. Nothing in `ToolRegistry::register_builtins()` registers an `fs.*` tool, and the CLI agent path never handed the registry to the subprocess anyway.

What remains:

- `FsCallEvent` / `FsAuditLogger` on `ToolContext` (`crates/orbit-tools/src/lib.rs`). The v2 dispatcher still wires `v2_fs_audit_logger`, which would convert an emitted `FsCallEvent` into a `V2AuditEvent` filesystem entry. No shipped builtin emits those events.
- Historical audit/import fixtures that name retired `fs.*` tools. Those strings stay parseable; a removed tool name is not a deserialization error.
- `ctx.fs_profile` / `ctx.policy_engine`, which the CLI sandbox compiler still uses to compile OS write confinement.

**Scope.** Agent dispatch spawns Claude Code, Codex CLI, Gemini, or another harness via `cli_runner.rs`, emits `tool_allowlist.harness_delegated`, and trusts that harness for tool allowlists — the engine-driven loop that enforced allowlists in-process was retired in [ORB-10801]. For task-backed dispatch, [ORB-11069] composes the effective allowlist as the deduplicated union of the activity baseline and exact `task.required_tools` before launch. Those requirements are normalized and fixed when the task is created; existing-task updates cannot change that authority. This composition changes only allowlist membership: caller role, registered host capability, tool-specific policy, filesystem policy, subprocess allowlists, and external authentication still run and may deny the tool independently. On macOS, executors declaring `sandbox: macos-sandbox-exec` also get the OS-level wrapper in §7, so `fsProfile:` can still narrow CLI filesystem writes.

On Linux, shipped agent executors declare `linux-bwrap`. The wrapper enforces writes from the resolved `modify` policy but deliberately records reads as `read_delegated`; this is not general read-allowlist parity.

---

## 6. Activity / Job fsProfile Binding

The `fsProfile:` field on an activity flows through `crates/orbit-engine/src/activity_job/`:

- `dispatcher.rs` carries `fs_profile: Option<&str>` on `DispatchInput` and threads it into `run_activity_job_dispatch`, `run_loop_step_dispatch`, and `run_agent_loop_via_driver`.
- `job_executor.rs` reads `t.fs_profile.as_deref()` from the activity spec at the call site of every step type.
- `agent_loop_driver.rs` invokes `host.tool_context_for_activity(fs_profile, audit_logger)` to construct the `ToolContext` the CLI path and remaining in-process tools read from.

`crates/orbit-core/src/runtime/v2_host/mod.rs::tool_context_for_activity` is the single materialization point:

```
fs_profile: Some(fs_profile.unwrap_or(UNRESTRICTED_FS_PROFILE).to_string())
```

This is the implicit-`unrestricted` rule from §2.2 in code form. Every v2 dispatcher path that constructs a `ToolContext` reaches this line, so omitting `fsProfile:` means "unrestricted within policy," not "no policy."

Legacy pipeline contexts are different. `crates/orbit-core/src/runtime/tool_exec.rs` fills a missing profile from `ORBIT_ACTIVITY_FS_PROFILE`; if the variable is unset, `ctx.fs_profile` stays `None`. That used to bypass the retired in-process helper; the live CLI sandbox still needs an explicit profile (see §9).

---

## 7. Sandbox / Exec Primitives

`orbit-exec` is the process-spawn layer. The public surface is in `crates/orbit-exec/src/lib.rs`:

- `ExecRequest { program, args, current_dir, timeout_ms, stdin_mode, environment_mode, debug }`.
- `EnvironmentMode::Inherit` or `ClearAndSet(Vec<(String, String)>)`; debug output redacts sensitive env values.
- `StdinMode::Inherit` / `Null` / `Bytes(Vec<u8>)`.
- `Sandbox::validate(req) -> Result<()>`; the default `NoSandbox` always returns `Ok`.
- `Sandbox::spawn(req) -> Result<Child>`; the default creates an unconfined child. A strategy that confines the process overrides this seam.
- `run_process(req, sandbox) -> ExecutionResult`.

`run_process` calls `sandbox.validate`, then `sandbox.spawn`, then `supervision::wait_with_optional_timeout`. Spawn applies the requested environment, pipes stdout/stderr, and on Unix calls `command.process_group(0)` so cleanup can kill orphan subprocesses.

Activity-scoped `proc.spawn` supplies `ActivityFsSandbox` at this seam. The runtime passes the child environment already resolved from `[execution.env]`, the owning `fsProfile`, workspace root, and program allowlist into `ToolContext`; CLI-backed agents receive the same values in their trusted `ORBIT_*` envelope for nested `orbit tool run` calls. Missing activity policy data fails closed. `validate` resolves existing path arguments symlink-safely and rejects reads denied by the profile before creating the process, and `spawn` confines the child itself. [ORB-11031]

### 7.3 Linux Landlock read boundary for activity-scoped `proc.spawn`

Argv inspection cannot be the read boundary: `bash`, `sh`, `python3`, and `git` are all on shipped activity allowlists, and every one interprets text handed to it on the command line. `git -c alias.x='!cat <path>' x` reaches any same-user file without a single path-shaped argument. `crates/orbit-exec/src/linux_landlock/` therefore compiles the resolved profile into a Landlock ruleset and applies it in the child between `fork` and `exec`, so the child and every descendant inherit it. The request-time argument check remains as an early, explainable deny; it is no longer the boundary. [ORB-11514]

The ruleset handles `EXECUTE | READ_FILE | READ_DIR | REFER`. Writes stay with the Bubblewrap mount namespace in §7.1 — one write answer, in one place. `REFER` governs moving a file between directories, and Landlock refuses a move that would give a file *more* access at its destination; without handling it, a child could relocate a denied file into a fully readable sibling directory and read it there.

Grants come from three places:

- **Workspace.** The resolved `read` rules are compiled once (`CompiledFsRules`, §3) and the tree is walked once. A directory is granted list-only — with its allowed children granted individually, so the denied path keeps no readable ancestor — when it holds a denied path *or* when an exclusion can still name a new path beneath it. The second condition is what makes the grant independent of timing; see the dynamic-name contract below. [ORB-11978]
- **Host runtime, resolver, and trust.** A fixed table: system binaries and libraries, the loader and its cache, the character devices a process opens on startup, the world-readable resolver files (`/etc/hosts`, `/etc/nsswitch.conf`, `/etc/resolv.conf`, `/etc/passwd`, …), and the CA stores. `/etc` is never granted as a directory, and neither is `/proc` — a tree-wide `/proc` grant would expose any same-user process's `environ`, including the launching Orbit process's credentials.
- **Tool state.** The directories on the child's own `PATH`, plus one directory per tool named by an environment variable the operator admitted into the child environment (`CARGO_HOME`, `RUSTUP_HOME`, `GH_CONFIG_DIR`, `TMPDIR`, `ORBIT_ROOT`, …) or by that tool's documented default under `$HOME`. `$HOME` itself is refused, as is any variable naming an ancestor of it, and publish tokens such as `$CARGO_HOME/credentials.toml` are carved back out.

Availability is fail-closed. `REFER` arrived in Landlock ABI 2, so a kernel below that — or any non-Linux host — makes activity-scoped `proc.spawn` return a capability error naming the requirement. There is no unconfined fallback.

#### Dynamic denied names

Landlock rules bind to inodes, so a ruleset cannot single out a *name* that does not exist yet. Compiling the carve-out from the denied paths a walk happens to find therefore made the boundary depend on timing: the same profile withheld a secret that already existed and disclosed the identical secret written a moment after the ruleset was compiled, to the child or to any descendant it spawned. F2026-09-054 measured that on Linux 6.8.0-139-generic / Landlock ABI 4, along with the two repairs that do not work — granting files individually blocks a legitimate generated file from being read back, and a post-run scan cannot recall bytes that already left. [ORB-11978]

The compiler asks the exclusion **rules** what they can name instead of asking the tree what it currently holds. `GlobReach` (`crates/orbit-types/src/policy/glob.rs`) answers, per directory, whether a rule can name a path strictly beneath it, and every such directory is granted list-only. A name created there afterwards then has no readable ancestor, exactly as if it had been present all along, and the ruleset's shape no longer depends on when the write happened.

That question has an answer only when the rule's own segments bound it:

- **Bounded** — `secrets/**`, `.env`, `config/*.key`. The rule names the directories it can reach (`*` and `?` cannot cross a separator), so the carve-out costs exactly those directories and every other directory keeps its whole-tree grant. This is enforced.
- **Unbounded** — `**/.env`, `**/*.env`, `a/**/b.key`. A `**` that crosses directories can name a path beneath *every* directory in the workspace, including directories that do not exist yet. Carving that out means granting no directory as a tree at all, which withdraws read access from every file the run produces itself: a compiler cannot read back the object it just wrote. Such a rule is **not enforced at the process boundary and not silently dropped**. It is returned on `LandlockReadBoundary::unenforced_exclusions`, logged once per spawn under `orbit.sandbox.landlock`, and keeps its full effect in the request-time policy check, which decides a concrete path and needs no ruleset.

The shipped default policy's `denyRead` (`**/.env`, `**/.env.*`, `**/*.env`, `**/*.env.*`) is entirely in the unbounded class, so a profile resolved from it reports all four rules as unenforced at the process boundary and keeps its previous grant shape. An operator who needs a `denyRead` rule held by the kernel against names that do not exist yet must write it in a bounded form — `secrets/**` rather than `**/secrets/**`. Closing the unbounded case needs a name-enforcing backend, which Landlock is not; no snapshot, inode census, or post-run guard substitutes for one, and none is reported as if it did.

**Acquisition, not naming.** The boundary decides which inodes a child may take bytes from, at the moment it takes them. Everything downstream of that follows and is covered by tests:

- A denied path stays unreadable for the child's whole life whether it existed at spawn or appeared afterwards, including after a rename within its directory, and it cannot be moved or hard-linked into a readable directory — `REFER` refuses a relocation that would gain access.
- An inode the ruleset already granted keeps that grant through a rename or hard link into a denied name. The child could read those bytes before it renamed anything, so the name change withdraws nothing. What stays closed is the direction that matters: a *new* inode arriving at a denied name gets no readable ancestor.
- A symlink is resolved where it points, so aliasing a denied path from a readable directory grants nothing.
- An open descriptor, an active mapping, and bytes already in the child's memory are beyond recall; no later filesystem rule revokes them.
- Creating and removing a denied name still work. This ruleset governs reads; writes stay with the mount namespace in §7.1.

**Other known limits.** SSH-authenticated `git` does not work through a scoped spawn, because `~/.ssh` is not granted (use HTTPS or `gh`). A toolchain whose runtime files live outside the `bin` directory on `PATH` — an `nvm`-style install — needs its tree named by the tool's own environment variable.

**Evidence.** `crates/orbit-exec/tests/linux_landlock.rs` exercises the real kernel: a concurrent writer creates a secret in a directory that did not exist at spawn and an indirect descendant cannot return it, while a file the run generates stays readable. It runs only where the host offers Landlock ABI 2 or later and reports a skip otherwise; `crates/orbit-exec/src/linux_landlock/tests/` covers grant compilation deterministically on any platform.

`ExecutionResult { success, stdout, stderr, exit_code, duration_ms, output }` is defined in `orbit-common`. Captured bytes use `String::from_utf8_lossy`, so non-UTF-8 output becomes replacement characters instead of failing the call.

The `Sandbox` trait remains the seam for generic `run_process` callers, but CLI-backed `agent_loop` invocations use a separate executor wrapper when the executor declares `sandbox: macos-sandbox-exec` ([T20260427-51]). The v2 host resolves the activity `fsProfile`; the engine converts workspace-relative rules to absolute roots and compiles SBPL before spawning the provider CLI.

Executor resources also accept `spec.sandbox: off` as a persistent operator
opt-out. It survives ordinary `orbit init` (without `--force`), non-overwriting
seeding, and normal resource sync, unlike omitted/null values on legacy Linux
defaults, which migrate to `linux-bwrap`. `orbit init --force` may still reset
shipped executor defaults, including sandbox.
The host carries the explicit off descriptor to the runner without resolving
filesystem grants; preparation chooses no wrapper and performs no capability
probe. The runner neutralizes supported provider-inner sandbox flags and audits
`sandbox_backend: off` with unrestricted read/write enforcement. Bare fallback
and unspecified settings retain their existing provider delegation behavior.
Orbit tool authorization and policy checks remain active. See the
[operator instructions](../../runbooks/linux-sandbox.md#explicitly-disable-worker-sandboxing)
for the resource path and introspection commands.

Compatibility is directional: new readers accept existing schema-version-2
resources, but old closed-enum readers reject `off`. Updating a binary on disk
does not replace persistent MCP servers or active drain/workflow runners.
Shared executor files must keep their prior values until those readers and
other runtime-opening processes have restarted on the new build; rollback
must restore the old concrete values before restarting old readers. The
runbook specifies the staged rollout and authoritative-MCP verification order.

The macOS wrapper resolves `sandbox-exec` from trusted absolute locations only, currently `/usr/bin/sandbox-exec`; it does not consult `PATH` for either availability checks or process spawn. If the trusted binary is missing, the runner fails closed unless the executor declares `allow_fallback: true`, and the error names the trusted location that was probed ([T20260509-30]).

The wrapper also prepares Codex's TLS trust input before a sandboxed spawn. Its
system-Keychain denies prevent Codex's rustls WebSocket transport from
completing native-root discovery, so the child environment admits explicit
non-empty `CODEX_CA_CERTIFICATE` and `SSL_CERT_FILE` values for this one provider and
backend. `CODEX_CA_CERTIFICATE` has precedence, followed by `SSL_CERT_FILE`; if
neither is present Orbit supplies macOS's public `/etc/ssl/cert.pem` through
`CODEX_CA_CERTIFICATE`. The selected path must be a readable file or dispatch
fails permanently with an actionable path-specific error. This changes no TLS
verification setting and no SBPL credential carve-out: keychain and other
private-directory denies remain intact, as do later activity-authored
`denyRead` clauses. Bare Codex, other providers, and Linux retain their prior
environment and spawn behavior. [ORB-11406]

The compiled macOS profile denies by default, allows broad reads required by agent CLIs and system libraries, allows process/signal/ipc/network/sysctl/iokit operations, and allows writes to:

- scratch/cache roots (`/tmp`, `/private/tmp`, `/private/var/folders`, `/dev`, `$HOME/Library/Caches`)
- `$HOME/.orbit/state/logs` for early inherited Orbit subprocess logging before runtime root resolution
- provider state dirs: Codex (`$CODEX_HOME` or `$HOME/.codex`), Claude (`$CLAUDE_CONFIG_DIR` or `$HOME/.claude`), Gemini (`$HOME/.gemini`), and Grok (`$HOME/.grok`)
- Claude `$HOME/.claude.json` sibling files (`.claude.json`, `.claude.json.lock`, atomic-write `.claude.json.tmp.<pid>.<ms_ts>`) when `CLAUDE_CONFIG_DIR` is unset, since these live at the home root rather than under `$HOME/.claude/` ([T20260508-13])
- positive `modify` roots from the resolved profile
- Codex side-write roots from runtime provider config, appended after policy denies so workflow state remains writable under the outer sandbox
- narrow child Orbit runtime roots appended by the v2 host after policy denies: global logs, global audit, global `orbit.db*`, global tasks, global `cache/**` (language-neutral host cache seam for toolchain artifacts shared across worktrees; not a shared Cargo target directory) [ORB-11259], workspace `.orbit/tasks/**`, workspace `.orbit/frictions/**`, workspace audit/logs, and workspace semantic DB sidecars

The child Orbit runtime roots are deliberately narrower than the workspace `.orbit` tree. They cover stores used by currently activity-exposed Orbit write tools: task/review/artifact writes under `.orbit/tasks/**`, friction reporting under `.orbit/frictions/**`, and startup/runtime audit (workspace and global `{root}/state/audit/**`), log, semantic-index, and global database writes. Nested `orbit.*` writes initialize the global audit store before the tool action, so omitting `{global}/state/audit/**` surfaces as `file-write-create ~/.orbit/state/audit` under `sandbox-exec` ([ORB-11055]). The managed CLI runner supplies that global root through `ORBIT_REGISTRY_ROOT`, which is trusted only together with managed-run provenance and never participates in workspace root resolution. Nested tool calls receive the logical workspace as `ORBIT_WORKSPACE` on the same envelope so they do not infer durable ownership from the linked-worktree cwd ([ORB-11117]). It deliberately does not reuse `ORBIT_ROOT`: that remains the operator's explicit pinned-data-root contract, while a managed linked-worktree child continues to resolve shared/local workspace roots through git and the registered checkout. Consequently global registry bootstrap cannot create workspace-only `state/job-runs`, diagnostics, scoreboard, worktrees, or knowledge directories ([ORB-11066]). Generic agent-callable state writes were removed in [ORB-10738]; graph write roots and every unlisted or future store remain outside this inventory.

`agent_implement` also exposes `orbit.adr.add` and `orbit.adr.update` ([ORB-10596]). On Linux, only the active managed worktree's `.orbit/adrs/proposed` and `.orbit/adrs/.locks` directories are bind-mounted writable after the enclosing worktree `.orbit/**` read-only mount; Accepted/Superseded ADRs and learning, task, state, and unknown local stores remain read-only. Allocation still uses the workspace-shared semantic database and `.id_alloc.lock`, so simultaneous worktrees serialize ID selection while each Proposed body lands under `<job-worktree>/.orbit/adrs/proposed/<id>/`. The allocator records that worktree-relative body path, allowing an orchestrator runtime to resolve and search it as a federated artifact while the worktree is live. macOS already re-allows the active job worktree as a whole after the policy deny, so this change adds no macOS SBPL allowance and changes no policy YAML.

Creation remains Proposed-only. In a managed-run context, `orbit.adr.update` may correct the title, body, and metadata of a Proposed record, but it cannot transition lifecycle status or modify an Accepted record. Acceptance and historical correction remain separate unmanaged human/orchestrator actions. A hub-side allocator or a second pending-decision queue was rejected: the existing shared allocator and federated artifact resolution already provide collision safety and discovery, while another protocol would duplicate allocation and introduce a second promotion lifecycle.

Negated `read` / `modify` rules become explicit SBPL denies in resolved order. Explicit host-policy exceptions and host-owned runtime roots appear after the enclosing deny, preserving last-match-wins without opening unrelated siblings. Simple path and `/**` subtree denials compile to `subpath`; non-subpath globs such as `**/*.env` compile to `regex`.

### 7.1 Linux Bubblewrap backend

On Linux, `ExecutorSandboxKind::LinuxBwrap` resolves only `/usr/bin/bwrap` and runs a real capability probe using the same private user, PID, IPC, and UTS namespaces plus mount setup required by provider execution. Absence or probe failure is permanent and fail-closed unless the executor explicitly sets `allow_fallback: true`. The availability decision happens before provider argv construction: an active outer wrapper neutralizes provider-native sandbox flags, while a bare fallback preserves them.

The deterministic argv starts with a read-only bind of `/`, explicitly retains the host network namespace, and applies canonical `modify` mounts in policy order. Broad positive roots are mounted before denials. A positive exact/subtree root is mounted after an earlier deny only when it is strictly nested beneath that denied root; equal or ancestor positives cannot mask the protection. This implements versioned-config and trusted runtime-store re-allows while unknown `.orbit` children remain read-only. A re-allowed file or directory must already exist because Bubblewrap cannot bind-mount a nonexistent child beneath a read-only parent; §7.1.1 covers how absent anchors are materialized, and a narrow re-allow that still cannot be mounted is returned on the plan's dropped-grant list rather than discarded. `/dev`, `/proc`, and `/tmp` are replaced with private minimal mounts, so `/tmp` is not a cross-worktree cache. The wrapper creates a fresh session, and parent-death cleanup remains enabled. Write-capable (implementer/unrestricted) Linux profiles also receive the host global `cache/` directory as a language-neutral extra write root so toolchain caches such as sccache can be shared without a mutable shared `CARGO_TARGET_DIR`; reviewer profiles do not. Managed worktrees additionally bind the activity cwd at `/tmp/orbit-workspace` and `<cwd>/target` at `/tmp/orbit-build` (created at spawn if absent) so compiler caches that key on absolute paths can hit across worktrees; those mounts live on the sandbox's private `/tmp` tmpfs and do not share a Cargo target directory. [ORB-11259]

The ADR authoring exception follows that same ordering: trusted host setup ensures only `<active-worktree>/.orbit/adrs/{proposed,.locks}` exists, then mounts those exact directories writable after the local `.orbit/**` deny. The ADR parent and its Accepted/Superseded lifecycle directories remain read-only. This does not add a policy exception, does not re-allow the shared workspace ADR tree, and does not expose any sibling under the worktree-local `.orbit` directory.

#### 7.1.1 Write-grant anchors are derived from the effective profile at each spawn

`spawn_linux_bwrap` prepares mount anchors immediately before compiling argv, from the same `ResolvedFsProfile` that compiles it. The grant set is every positive exact/subtree `modify` rule that is a narrow re-allow beneath an earlier deny and remains writable at its anchor after the full ordered rule list is evaluated. A later deny covering the anchor therefore prevents materialization, while a narrower deny below a writable subtree leaves the remaining subtree grant intact. Broad writable roots are excluded; they are host-owned and must already exist, so an absent one still fails closed at compile.

The policy grammar is the explicit anchor-type contract: an exact rule denotes a file, while `<root>/**` denotes a directory subtree. Filename punctuation is never type evidence, so an extensionless exact rule materializes a file and a dotted subtree root materializes a directory without any hardcoded path inventory. An existing target whose filesystem type contradicts its rule fails closed.

Creation is confined to the canonical managed worktree, which is trusted and disposable. Every worktree-owned component is checked for symlinks before both existing- and absent-target handling, the resolved existing anchor (or newly created parent) must remain inside the canonical root, and files use create-new semantics. Anchors outside that root are the host's to create and are reported, not invented. Creating an anchor grants nothing new — the final effective profile already decided the path is writable — so this is materialization, not policy.

Two consequences follow from deriving at spawn rather than during `worktree_setup`. The grant set matches the profile the kernel will enforce, including the host-appended run roots that setup never saw; and it is recomputed for each provider launch, so a run whose needs grow does not depend on a snapshot taken before it started. After a failed Bubblewrap child, the executor inspects EROFS stderr that names an attempted path and evaluates that path against the same effective profile, replacing a generic nonzero-exit message with an Orbit-owned missing-grant or shadowing-deny diagnostic. Children that omit the path retain the generic failure. [ORB-10602] [ORB-10607] [Derive Linux sandbox write-grant anchors from the effective profile at each spawn](./4_decisions.md#derive-linux-sandbox-write-grant-anchors-from-the-effective-profile-at-each-spawn-1)

The policy syntax remains schema v2. Existing policies without `denyModify` exceptions retain their prior behavior. After installing a binary that carries a changed shipped default, `orbit init` refreshes the machine-global policy assets without changing executor sandbox selection or `allow_fallback`; `--force` is unnecessary and would reset the global root. Workspace policy can then narrow the refreshed host boundary but cannot expand its exception surface.

Existing matches of non-subtree negative globs are mounted read-only before spawn. Because mount namespaces cannot reject a matching filename created later, direct invocations with an overlapping non-subtree deny fail closed. An Orbit-managed single-writer worktree may run with snapshot expansion, followed by a post-run scan that rejects any newly-created forbidden match before downstream commit. Audit metadata records the effective backend, trusted wrapper, probe outcome, redacted effective argv, `write_enforced` or `write_delegated`, and the honest `read_delegated` boundary. [ORB-10552] [Use Bubblewrap for shipped Linux CLI write confinement](./4_decisions.md#use-bubblewrap-for-shipped-linux-cli-write-confinement)

Bubblewrap's private PID namespace also establishes a liveness-authority boundary. A nested
`orbit` command receiving both truthy `ORBIT_MANAGED_RUN_CONTEXT` and a non-blank
`ORBIT_RUN_ID` is a managed child, not an authority for its host pipeline worker. On runtime
open it skips only the opportunistic orphan scan: the host worker can be alive while invisible
inside that namespace. Top-level runtime opens and explicit host recovery surfaces retain their
normal reconciliation behavior. Do not weaken the PID namespace, enable bare fallback, or infer
that an invisible host worker is dead from inside a sandboxed child. [ORB-10557]

### 7.2 Linux host readiness on Ubuntu 24.04

Ubuntu 24.04 (Noble) enables AppArmor restriction of unprivileged user namespaces through
`kernel.apparmor_restrict_unprivileged_userns`. When `/usr/bin/bwrap` starts Orbit's probe,
Bubblewrap creates the private user namespace and configures its UID map. If AppArmor has no
narrow profile granting that operation to Bubblewrap, the kernel rejects the setup and bwrap
reports `bwrap: setting up uid map: Permission denied`. A present executable is therefore not
enough; the real namespace-and-mount probe in `probe_bwrap` must succeed.

The supported Noble remediation is the distro `apparmor-profiles` package's
`bwrap-userns-restrict` profile. The package provides it under
`/usr/share/apparmor/extra-profiles/`; copy it into `/etc/apparmor.d/` and load that copy with
`apparmor_parser -r`. Verify both that the profile is visible to AppArmor and that the exact
probe shape used by Orbit exits successfully:

```bash
sudo apt-get update
sudo apt-get install --yes bubblewrap apparmor-profiles
test -x /usr/bin/bwrap
test -f /usr/share/apparmor/extra-profiles/bwrap-userns-restrict
sudo install -m 0644 \
  /usr/share/apparmor/extra-profiles/bwrap-userns-restrict \
  /etc/apparmor.d/bwrap-userns-restrict
test -f /etc/apparmor.d/bwrap-userns-restrict
sudo apparmor_parser -r /etc/apparmor.d/bwrap-userns-restrict
grep -Fq 'bwrap-userns-restrict' /sys/kernel/security/apparmor/profiles
/usr/bin/bwrap --die-with-parent --new-session --unshare-all --share-net \
  --ro-bind / / -- /bin/true
```

To roll back only this host remediation, unload the packaged profile with
`sudo apparmor_parser -R /etc/apparmor.d/bwrap-userns-restrict`, then remove only the copied
`/etc/apparmor.d/bwrap-userns-restrict` file; leave the package-managed source under
`/usr/share/apparmor/extra-profiles/` intact. The probe should then fail again on a host where the
global restriction is active, and repeating the copy-and-load sequence restores the remediation.
Do not disable
`kernel.apparmor_restrict_unprivileged_userns` globally or install a broad unconfined profile:
those changes expand the user-namespace attack surface beyond Bubblewrap. Do not set
`allow_fallback: true` to hide a failed probe, because that bypasses the fail-closed OS boundary
and runs the provider without `linux-bwrap`.

This subsection records the shipped Linux behavior from [ORB-10552] and the Ubuntu host rescue
context from [ORB-10553]. No sandbox design decision changed, and this operational remediation
requires no new ADR.

---

### Git integrity and host recovery

The Linux host appends non-overridable Git write denials after provider and
runtime convenience grants. It discovers the registered and active checkout's
`.git` entry, its real gitdir and `commondir`. Those directories include refs,
rebase state and host recovery payloads at
`<git-common-dir>/orbit/worktree-recovery/<run-id>/`. Git inspection stays
readable; source files remain writable according to the activity profile.
Metadata paths containing symlinks, symlink entries inside metadata, and
special files or hard-linked metadata files fail closed before launch: a read-only mount cannot
protect a writable alias of the same inode. This deliberately does not support
local clones whose metadata is hard-linked into another repository.

The compiler pins writable ancestor entries of existing denied paths as mount
points so they cannot be renamed aside. It replays the ordered policy overlays
at both stable workspace and build aliases, including clipping a containing
deny to an alias root. A build directory redirected into Git metadata therefore
cannot create a writable metadata mount. These are per-child namespace mounts;
they do not change the host's filesystem permissions.

For an admitted stopped rebase, the host retains the original Git pointer,
open metadata-directory handles and hashes of all rebase instruction files in
memory. Before staging it rejects a different gitdir/common directory, replaced
directory inodes or changed recovery instructions, then checks the existing
commit/index/conflict set and live ownership/authorization. Scratch copies are
permitted as readable data but cannot substitute for this host checkpoint.
Only the host stages the authorized conflict paths and continues the rebase.

This protects the live invocation's in-memory checkpoint and Git destinations.
Durable recovery certificates also live in `job_runs.pipeline_state_json` in
`<global-root>/orbit.db`. The existing child-runtime grants allow that database
and its sidecars for nested Orbit tools. They do not provide a host-only raw
filesystem boundary for durable recovery certificates; protecting that store
requires separating host writes from leaf tool execution. Git mount tests do
not establish database integrity.

The required live integrity fixture is explicit and fails on namespace denial:

```sh
cargo test -p orbit-exec --test linux_sandbox kernel_git_metadata_integrity_through_original_and_build_aliases -- --ignored --exact --nocapture
```

Run it on an authorized Linux host where `/usr/bin/bwrap` can create user and
mount namespaces with the shipped probe flags. Deterministic compilation and
host-recovery fixtures do not establish kernel confinement; a nested runner's
namespace denial leaves this gate incomplete until host execution succeeds.

---

## 8. Process Supervision

`crates/orbit-exec/src/supervision/wait.rs::wait_with_optional_timeout` drains stdout/stderr in background threads, writes stdin bytes when requested, installs Unix SIGINT/SIGTERM handling, and polls `child.wait_timeout` every `WAIT_POLL_INTERVAL = 100ms`. Clean exits still call `kill_process_group(child.id())` to reap orphans. Parent signals terminate the group and report `exit_code = Some(128 + signal)` with annotated stderr; deadlines terminate with SIGTERM and append `process timed out`.

`crates/orbit-exec/src/supervision/cleanup.rs` is the termination layer. The escalation policy:

1. Send `SIGTERM` (or the supplied signal) to the entire process group via `killpg`.
2. Poll `process_group_is_alive(pid)` for up to `TERMINATION_GRACE_PERIOD = 5 seconds`.
3. If the group is gone, return success.
4. Otherwise send `SIGKILL` to the group, then call `child.kill()` and `child.wait()` to reap.

`process_group_is_alive` uses `killpg(pid, 0)`, treats `ESRCH` as "all gone," and treats other errno values as "still alive" so cleanup errs toward SIGKILL.

`SignalHandlerGuard` is RAII and refcounted: the first live waiter installs SIGINT/SIGTERM handlers and snapshots the previous `sigaction` structs; the last drop restores them and re-raises a captured signal so a long-running server's original handler (tokio `ctrl_c` / SIGTERM, or SIG_DFL) still runs. `SIG_IGN` is not re-raised. When the previous disposition is SIG_DFL, the process stderr is annotated with `process interrupted by signal SIG…` before `raise`, because the wait result is discarded as the process terminates. A process-wide mutex covers only that install/drop critical section — never `raise` — so concurrent `run_process` waits overlap. Each waiter registers its child's pgid in a lock-free table and snapshots a signal generation counter. The handler is async-signal-safe: it stores the signal, increments the generation, records a pending forward, and `killpg`s every registered group. Waiters that miss a slot still observe the generation counter on the next poll and run the ordinary termination path.

Non-Unix builds use a fallback `terminate_process_group` that just calls `child.kill().ok(); child.wait().ok();` — process-group semantics do not apply on Windows, so orphan reaping is best-effort.

---

## 9. Test surfaces

Risk-weighted regression tests sit beside the implementations they guard
([T20260509-7]):

- `crates/orbit-policy/src/engine.rs#tests` — `PolicyEngine::check` boundary
  semantics: positive read-rule matches return `allowed=true` with the rule
  recorded in `matched_rule`; modify paths outside any positive rule resolve
  to `allowed=false`; ordinary global `denyRead` / `denyModify` rules override
  profile-level positive rules under last-match-wins; an unknown profile name
  errors structurally (with the documented `unrestricted` exception); and the
  `matched_rule` field is populated for audit attribution. Traversal inputs
  such as `../secret.txt`, `src/../secret.txt`, and their backslash-normalized
  equivalents are rejected as `OrbitError::InvalidInput` for both read and
  modify checks ([T20260509-27]). The same surface proves host modify
  exceptions intersect profile authority, workspace exceptions cannot exceed
  the host surface, and later workspace denies still win ([ORB-10560]).
- `crates/orbit-exec/src/linux_landlock/tests/` and
  `crates/orbit-exec/tests/linux_landlock.rs` — grant compilation decides the
  workspace and host tables without applying a ruleset, while the integration
  suite applies the real ruleset to real children: a host sentinel and a
  `denyRead` match are withheld from a `git` shell alias, a denied file survives
  neither an in-place rename nor a move into a readable directory, a generated
  file stays readable, another process's `environ` is not, declared tool state
  is readable while its publish token is not, and `git` / `rg` / `cargo` /
  `make` / `gh` still run. `crates/orbit-tools/tests/proc_spawn_lockdown.rs`
  reproduces the same alias bypass through the tool itself and pins the
  request-time `/etc` denial ([ORB-11514]).
- `crates/orbit-exec/src/macos_sandbox/compile.rs#tests` and
  `crates/orbit-exec/src/macos_sandbox/tests/provider_dirs.rs` — trusted wrapper
  resolution ignores `PATH`, including a macOS runtime test that places a fake
  `sandbox-exec` earlier on `PATH` and verifies the fake wrapper is not
  executed ([T20260509-30]). SBPL compilation tests
  cover `denyRead` / `denyModify` clause emission (`subpath` for simple
  rules, `regex` for non-trivial globs) and resolved deny/re-allow ordering
  under last-match-wins. macOS-gated runtime tests
  (`compiled_profile_denies_reads_to_negated_read_path` and
  `compiled_profile_for_realistic_agent_loop_profile_allows_repo_writes_denies_dotenv`)
  exercise an `agent_loop`-shaped profile end-to-end against the kernel
  sandbox. The Codex CA regression additionally runs a real macOS wrapper,
  loads one certificate from the injected public PEM bundle, and proves
  synthetic Keychain material stays unreadable; platform-neutral fixtures pin
  explicit-variable precedence, missing-path errors, and the exact provider /
  backend environment boundary ([ORB-11406]).
- `crates/orbit-store/src/file/policy_def_store/` — policy resource
  name tests reject traversal-shaped names such as `../x` before path
  construction and assert no file is written outside the policy store
  ([T20260509-28]).

macOS runtime tests skip where `sandbox-exec` cannot apply. Linux Bubblewrap
tests compile argv and exercise fail-closed/fallback behavior on every host;
kernel tests probe real `/usr/bin/bwrap` and skip with its concrete capability
failure when user or mount namespaces are unavailable. The Linux argv and
kernel cases also cover writable versioned `.orbit` paths versus protected
state, record, database/lock, and unknown paths ([ORB-10560]). Runtime-host
tests pin the managed-worktree ADR mount as the sole local record-store
exception, tool-host tests prove executors can refine Proposed ADRs but cannot
accept or rewrite Accepted records, and the SQLite allocator race test launches
two child processes with distinct worktree roots against one database/lock and
asserts 100 collision-free dense IDs per artifact kind ([ORB-10596]).

---

## 10. Concerns & Honest Limitations

1. **CLI read policy is delegated.** Both shipped OS wrappers confine writes, but Linux Bubblewrap intentionally keeps the host read surface available and neither backend replaces harness enforcement of arbitrary read globs.
2. **CLI tool allowlists are delegated.** The OS wrappers narrow writes, but Orbit still trusts Claude/Codex/Gemini/Grok harnesses for declared `tools:`.
3. **Provider state directories are trusted write roots.** `$HOME/.orbit` plus Codex, Claude, and Gemini state dirs are outside the activity workspace and emitted unconditionally.
4. **Codex side-root appends are config-coupled.** If Codex is configured without the workspace-write side roots, inherited Orbit subprocesses can hit `.orbit` write denials.
5. **macOS provenance syscall allowances are private.** `vnguard` and `Sandbox`/67 mirror current Codex startup needs and may require review after OS changes.
6. **Legacy contexts can leave `fs_profile = None`.** Non-activity callers retain that compatibility shape. Activity-scoped `proc.spawn` rejects a missing profile, and CLI-backed activities export `ORBIT_ACTIVITY_FS_PROFILE` so nested tool calls reconstruct the owning profile.
7. **No in-process `fs.*` enforcement remains.** A revived harness would need to rebuild the retired helper (or move enforcement below the tool layer) rather than rely on leftover builtins.
8. **Generic exec path admission has two layers.** Activity-scoped `proc.spawn` validates existing path arguments against the owning activity's resolved `fsProfile` before launch, and on Linux confines the child itself with Landlock (§7.3); CLI-agent OS wrappers remain the kernel-level boundary for `backend: cli` agents. The program allowlist is mandatory rather than opt-in: asset load rejects an activity whose `tools` cover `proc.spawn` while `proc_allowed_programs` is absent, and the v2 activity tool context always marks `proc.spawn` activity-scoped, so a missing list denies every program instead of degrading to allow-all ([ORB-10959], [ORB-11031]).
9. **Symlink semantics are implicit.** `workspace_relative_path` follows symlinks and rejects out-of-workspace targets, but no spec states that invariant.
10. **Glob syntax is narrow.** Character classes, brace expansion, and POSIX bracket expressions are unsupported.
11. **Policy result shapes are parallel.** `PolicyDecision` and `FsPolicyEvaluation` have no bridge for future non-fs evaluators.
12. **Empty rule sets are safe but opaque.** A profile with only deny rules reports `matched_rule = "[]"`, not the matching deny rule.
13. **Signal handling is process-global.** SIGINT/SIGTERM dispositions are shared across concurrent waits (refcounted install, lock-free pgid fan-out). A waiter that cannot claim a pgid slot still terminates from the generation counter within one poll interval.
14. **`denyRead` binds to inodes that exist at spawn.** The Landlock carve-out in §7.3 cannot name a file that does not exist yet, so a deny-matching file created under an already-granted directory after the ruleset is compiled is readable by that child. Relocation and rename of an existing denied file are closed; concurrent third-party creation is not.
15. **Workspace canonicalization errors collapse to denial.** A missing workspace root can surface as `PolicyDenied("path is outside workspace")` rather than a clearer root-missing error.

---

## Task References

- **[T20260416-0728]** — Align policy contract with runtime enforcement; established v2 schema and effective-profile resolution.
- **[T20260417-0550]** — Decompose `orbit-exec` supervision modules.
- **[T20260417-0557]** — Harden Orbit path boundaries and dependency advisories.
- **[T20260417-0558-4]** / **[T20260417-0558-5]** — Harden `orbit-exec` supervision (signal-pipe handler and process-group reaping).
- **[T20260419-0503]** — Enforce `fsProfiles` across runtime and CLI; introduced the `tool_context_for_activity` materialization.
- **[T20260328-221810]** — Agent subprocess termination on Ctrl+C / job-run cancel; predecessor of the current signal-pipe design.
- **[T20260426-0605]** — Auditability design folder cross-linked from §5.
- **[T20260426-0622]** — Add this policy & sandboxing design folder and document the current contract.
- **[T20260427-51]** — Wrap cli-backend agent invocations in `sandbox-exec` on macOS.
- **[T20260428-10]** — Allow Codex CLI state writes under the macOS sandbox.
- **[T20260428-14]** — Extend the macOS sandbox state-dir allowance to Claude (`~/.claude` / `$CLAUDE_CONFIG_DIR`) and Gemini (`~/.gemini`), and document why side-write roots remain Codex-only.
- **[T20260430-23]** — Shorten the policy sandbox design docs while preserving the shipped contract and ADR history.
- **[T20260508-13]** — Allow Claude's `$HOME/.claude.json` sibling files (`.json`, `.lock`, atomic-write `.tmp.<pid>.<ms_ts>`) under the macOS sandbox.
- **[T20260509-7]** — Add `PolicyEngine::check` boundary tests and macOS sandbox `denyRead` / realistic agent-loop profile tests.
- **[T20260509-28]** — Validate policy and executor resource names as safe file stems before file-store path construction.
- **[T20260509-30]** — Resolve `sandbox-exec` from trusted absolute locations and keep availability errors fail-closed and explicit.
- **[ORB-00129]** — Re-allow narrow workspace child Orbit runtime stores for activity-exposed learning, friction, and job-run state tools without removing the default workspace `.orbit/**` deny.
- **[ORB-10552]** — Ship fail-closed Linux Bubblewrap write confinement without claiming read-policy parity.
- **[ORB-10553]** — Rescue the Ubuntu host prerequisite for the shipped Linux Bubblewrap probe.
- **[ORB-10560]** — Add host-policy modify exceptions for the explicit versioned `.orbit` surface while preserving protected stores and unknown-path denial.
- **[ORB-10573]** — Materialize only exact missing versioned-config anchors gated by both task scope and the effective host policy/profile before Linux provider launch.
- **[ORB-10602]** — Replace that table-and-selector gate with per-spawn derivation from the effective profile, and surface every unmountable grant against its path and rule.
- **[ORB-10596]** — Allow executor-authored Proposed ADRs through one narrow managed-worktree mount while preserving global allocation, federated discovery, and separate acceptance.
- **[ORB-11376]** — Remove checkout-local runtime identity from the managed-agent write exception so absent identities cannot be published as empty anchors.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
