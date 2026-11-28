---
title: Script Steps — Overview
owner: claude
created_date: 2026-10-27
updated_date: 2026-11-28
last_validated: 2026-11-21
status: Draft
stage: building
next: "File the explicit script_grant raise surface for orbit-office proposals (phase 4 part; unfiled)"
feature: script-steps
doc_role: overview
type: design
summary: "A job step that runs a workspace script by name inside the executor sandbox, authorized by an operator grant bound to the script's content digest, with JSON stdin/stdout, declared files, audit and the job retry policy"
tags: [script-steps, activity-job, routines, sandbox, grants, policy-sandbox]
---

# Script Steps — Overview

Status: building; phases 1 to 5 (Phasing) have landed. Phase 1 ([ORB-19983]) added the `Script`
manifest and its loader, `target: script:` parsing, the `script_grant` table and
witness, `orbit script grant|revoke|list|show`, the governed rows and doctor
findings. Phase 2 ([ORB-19985]) runs a granted step on Linux under
`linux-bwrap-script-v1` (no network, `env_pass` or `files.inputs` yet), with the
`script.execution` audit, `orbit run show` and `orbit script history`. [ORB-20866]
added declared output files (`files.outputs`, §4 Files out) from phase 3, and
[ORB-21297] added phase 3's Linux part: declared input files (`files.inputs`, §4
Files in) and the `attach_script_files` action that attaches a step's output files
to a task. [ORB-21298] completed phase 3: the `macos-sandbox-script-v1` profile, so
macOS runs script steps (with no network), `RLIMIT_CPU` and `RLIMIT_NOFILE` on both
platforms, the grant-time and pre-spawn root probes with TCC reporting, and the
nested-sandbox refusal on macOS. Platforms other than Linux and macOS refuse with
`script_sandbox_unavailable`. Phase 4
([ORB-21299]) raises a `script_grant` decision request when a step is refused for a
missing, stale or insufficient grant, applies a human or registered-channel answer as a
digest-bound grant, and adds the dashboard Scripts view with its operator-only grant
action. Phase 5 ([ORB-21301]) honours `network: https` and `network: any` grants and
`env_pass` on Linux, and redacts each passed value from everything the run keeps.
This doc is the design deliverable of [ORB-19294]. It reverses the removal recorded in
[The v2 shell activity surface is removed, not sandboxed](../activity-job/4_decisions.md#the-v2-shell-activity-surface-is-removed-not-sandboxed)
([ORB-00374]) by taking the alternative that decision recorded and set aside:
"retrofit the sandbox/policy/cwd pipeline". It also narrows the reach of
[Routine targets are catalog references only](../routines/4_decisions.md#routine-targets-are-catalog-references-only--no-inline-command-payloads)
([ORB-10021]):
routines still name only `job:<name>`, and a job may now contain a step that names a
granted script. Phase 2 recorded the superseding decision,
[Granted script steps run under the script profile](../activity-job/4_decisions.md#granted-script-steps-run-under-the-script-profile).

## 1. Motivation

Bearing: Orbit already has these pieces, read from source at `a5b5408ea` (the `local_shell`
bullet was rechecked at `d10fae79b`).

- **Job steps name catalog targets by prefix.** A job step's `target:` is a
  namespace-prefixed reference. At that commit only `activity:<name>` resolved
  (`ACTIVITY_REF_PREFIX` in `crates/orbit-engine/src/activity_job/catalog/mod.rs`
  and `resolve_ref` in `crates/orbit-engine/src/activity_job/catalog/resolve.rs`),
  and any other prefix was a load error (`ResolveError::UnknownRefKind`); phase 1
  ([ORB-19983]) added `script:` resolution alongside it. `TargetRef`
  (`crates/orbit-types/src/workflow/activity_job/job_v2.rs`) was made prefixed so that
  "future kinds land without a breaking shape change". A step already carries
  `timeout_seconds`, `retry` (`RetrySpec`) and `when`.
- **Activity types are `agent_loop` and `deterministic`.** `ActivityV2Spec`
  (`crates/orbit-types/src/workflow/activity_job/activity_v2.rs`) has no third variant.
  Activities declare `input_schema_json` and `output_schema_json`.
- **A `local_shell` action already runs a direct child.** A `deterministic` activity with
  `action: local_shell` ([ORB-11294]; `crates/orbit-engine/src/executor/automation/shell.rs`)
  runs one child process with no shell, in one of two declared forms: `command` with
  `args` (an `execve`), or `shell` with a non-empty inline `script`, run as
  `<shell> -c <script>`. Setting both `command` and `shell` is refused, and so is `args`
  with `shell`. The program comes from the activity config or, when the activity names
  none, from the selected executor definition's `command`; with neither, the step fails.
  The definition's `args` are used when the activity names no program, and otherwise
  prefix the `args` of a `command` activity. Neither source is step input: templated input
  never supplies the program or argv. The executor defaults to `local-shell` when
  `config.executor` is absent, and its shipped
  definition declares no `sandbox`, so a step runs bare under the process supervisor. Its
  `cwd` check validates only the child's initial working directory: the directory must
  resolve inside the workspace root. Filesystem access after launch is limited only by an
  effective OS sandbox or by restrictions the Orbit process inherited. Kernel confinement is
  opt-in, through a `sandbox:` on the executor definition, which wraps every activity that uses
  it. An activity's optional `fsProfile` selects the named policy profile; without one, the
  built-in `unrestricted` profile applies (`crates/orbit-core/assets/executors/local-shell.yaml`).
  On macOS, Seatbelt applies the profile's read and modify rules, including policy denies. On
  Linux, Bubblewrap enforces modify rules only: profile read rules, including `denyRead`, are not
  enforced, so host reads stay available except for fixed credential and runtime-directory masks
  and explicit caller-provided masks. A nonzero exit fails the step
  (`LocalShellFailed`) unless `allow_nonzero_exit` is set.
- **The non-success outcome survives.** `StepOutcome { success, output, message }`
  (`crates/orbit-engine/src/activity_job/job_executor/exec_ctx.rs`) still carries
  `success`, and the retry loop treats `Ok(success = false)` as retryable
  (`crates/orbit-engine/src/activity_job/job_executor/step.rs`). The CLI agent-loop
  leaf now reaches it for provider timeouts and nonzero exits, so the claim in the
  ORB-00374 record that no leaf produces it is no longer true. A step's recorded
  output is wrapped as `steps.<id>.output` (`wrap_step_output`).
- **A sandbox that runs untrusted code with nothing granted by default.** The plugin
  build profile (`crates/orbit-exec/src/build_sandbox/**`, design in
  [plugins §3 install-time build](../plugins/specs/install-time-build.md)) runs a child
  under Bubblewrap on Linux (`linux-bwrap-build-v1`: constructed root, every namespace
  unshared, fresh `/proc`, minimal `/dev`) and a deny-default SBPL profile on macOS
  (`macos-sandbox-build-v1`, no network). Reads are denied by default, the credential
  denies always win, one capped log (`PLUGIN_BUILD_LOG_CAP_BYTES`, 1 MiB) covers stdout
  and stderr, and a wall-clock and directory-size bound kill the process group. A host
  that cannot apply the profile refuses: there is no weaker fallback. A networked
  `fetch` phase exists on Linux only (Landlock, outbound TCP 443). macOS refuses it
  because a descendant that calls `setsid` escapes the process-group kill
  (`BUILD_FETCH_PHASE_SUPPORTED`).
- **Worker containment.** Linux worker containment defaults to `warn`. A worker runs
  in a transient systemd scope with configured `machine.worker_*` limits when the
  user manager is reachable. If it is unavailable, `warn` launches the worker in the
  caller's cgroup with a warning; `off` also launches without a worker scope (with a
  warning on Linux), while `strict` refuses an unavailable scope
  (`crates/orbit-core/src/application/job/pipeline/worker/scope.rs`, [ORB-12903]).
  `machine.worker_cpu_quota` is optional, and systemd may accept `CPUQuota=` without
  enforcing it when the CPU controller is not delegated. Under `warn` or `off`, a
  script may therefore run without worker-scope resource limits. macOS has no cgroup,
  so workers there run uncontained; a tracked-tree watchdog was filed as [ORB-19251] and has since landed.
- **Operator grants with a tamper-evident witness.** Plugin grants are host-local
  (SQLite `plugin_store`) and checked against a witness file under
  `~/.orbit/plugins/.grants/<ns>.json` that no agent or plugin profile can write
  ([plugins §3](../plugins/2_design.md#3-lifecycle-and-state)). Declared programs are
  resolved to canonical paths at consent, and a changed resolution is a re-consent
  event ([plugins §4.3](../plugins/2_design.md#43-sandboxing)). A tool whose catalog row
  has no Agent capability is operator-only (`classify_agent_tool_grant`,
  `crates/orbit-common/src/governance/authorization/grants.rs`).
- **Human answers through registered channels.** A decision request can be answered
  only through the human funnel, or by a registered channel's plugin calling
  `orbit.decision.submit`. Registration is operator-only host config
  ([human decision channel §3](../human-decision-channel/2_design.md#3-delegated-human-authority)).
- **Child environments.** Every Orbit child environment is built over a cleared env
  through `allowlisted_child_env` (`orbit_common::security::child_env`), and plugin
  `env_pass` refuses `ORBIT_*` names.

### The gap

Daniel wants routines to run scripts, and wants a job to mix script steps with agent
and deterministic steps so that one job can be a whole pipeline (2026-10-27). Example:
an agent step builds something, `package.sh` packages it, and a deterministic step
uploads the result, with outputs flowing from step to step.

Two routes run a script from a job today. The first is `local_shell` (Bearing): a
`deterministic` step runs a program or an inline script that the workspace YAML names,
bare unless the operator opts into a sandbox, with no consent tied to the script's
content. The second takes four pieces for one script: a plugin that wraps the script as
a tool, a `deterministic` activity whose `action: plugin.tool_call` calls that tool, a
job around the activity, and a routine that targets the job. The plugin also brings
the wrong boundary. orbit-office already runs approved scripts this way
(`office.script-run`, relayed on this task from [ORB-19206]). Its own spec admits that
the script runs with the plugin's grants, including the plugin's `any` network grant,
because Orbit gives a plugin's child no narrower sandbox than the plugin's.

ORB-00374 removed `type: shell` for three reasons, and all three still bind:

1. **No OS sandbox and no cwd confinement.** `run_shell` called `Command::new(program)`
   with the worker's full authority.
2. **A self-asserted allowlist.** `program ∈ allowed_programs`, with both fields in the
   same workspace YAML ([ORB-00363]). Whoever could write the YAML could widen the list.
3. **Arbitrary command text on a timer.** A routine `run: {type: shell, command: …}`
   would have reopened the surface unattended.

The same three risks apply to `local_shell`, which restored the inline form under
[ORB-11294]. Its shipped definition declares no sandbox, so its `cwd` check validates only
the child's initial working directory. Filesystem access after launch is limited only by an
effective OS sandbox or by restrictions the Orbit process inherited. Its program is named
in the workspace YAML with no allowlist.
A routine can run a job that holds a `local_shell` step. This design leaves
`local_shell` as it is (see [Relation to `local_shell`](./2_design.md#relation-to-local_shell)).

Three things exist now that did not when the shell surface was removed:

- **A no-default-grant sandbox in-tree.** The build sandbox above shipped for plugin
  builds. It is the profile a script needs: nothing readable but what is named, one
  writable directory, no network, a capped log and a kill bound.
- **Per-run resource containment.** A worker scope can apply memory and task limits
  to a Linux script only when that scope was created. With the default `warn` mode,
  an unavailable worker scope falls back to the caller's cgroup; `off` skips the
  worker scope and logs a warning on Linux. If neither the script nor its worker has
  a scope, a runaway script can exhaust resources in the caller's cgroup and affect
  the host. A CPU quota is optional and may be accepted without enforcement when
  systemd lacks CPU-controller delegation.
- **A consent model whose authority lives outside the workspace.** Plugin grants
  showed how: host-local, digest-witnessed, operator-only to write, re-consent on a
  changed request.

### Intended outcome

A job step can run a script that the workspace names, as a program with a list of
arguments and no shell, inside the executor sandbox. It runs only while the operator's
grant for that script name matches the script's current content digest. It reads
JSON on stdin, writes JSON on stdout that later steps read as `steps.<id>.output`, can
take and return declared files, and fails or retries like any other step. Every run,
and every refusal, is recorded on the job run.

Routines, condition triggers and plugins keep their contracts. Routine targets stay
`job:<name>`. No YAML field, workspace setting or agent can grant a script, widen a
grant or keep a grant valid across a content change.

### Out of scope

- **Inline shell text in script steps.** No `command:`, no `sh -c`, no expression
  evaluated as a command in the manifest or the `script:` target (§1). `local_shell` keeps
  its inline form; see [Relation to `local_shell`](./2_design.md#relation-to-local_shell).
- **Unsandboxed scripts.** There is no `unsandboxed` grant, and a host without the
  profile refuses (§3, §7).
- **Scripts calling Orbit.** A script has no `orbit_tools`, no callbacks and no task
  writes. Later steps write (§6).
- **Agent-chosen scripts.** An agent can't name a script to run, because script steps
  live in job definitions, and it can't grant one.
- **Replacing plugins.** Reusable tools still ship as plugins (§6).
- **Routine `script:` targets in v1.** Wrap the script in a job (§1, §6).
- **Cross-host grants.** A grant is consent on one host (§7).

### Acceptance (feature level)

- A job step `target: script:<name>` whose manifest is missing, whose `entry` escapes the
  workspace, or which uses inline-code interpreter flags fails to load, and the error
  names the file; unrelated valid scripts remain available ([ORB-23140]). `type: shell`
  and routine `target: script:` stay load errors.
- With no grant, a stale grant or a request beyond the grant, the step refuses before
  spawn with a non-retryable typed error recorded on the run, and nothing executes.
- Editing a granted script's manifest or entry, in the workspace or in a run worktree,
  makes the next run refuse with `grant_stale` and both digests, until an operator
  re-grants.
- An agent session and a plugin callback can't call `orbit.script.grant` or
  `orbit.script.revoke`, and can't write the grant witness. A forged grant row without
  a matching witness is refused.
- On Linux a script sees only the mount table of §3. It can't reach the network under
  `none`, can't write outside the cwd worktree, the scratch directory and its granted
  roots, can't read `.orbit/` or the credential paths, and leaves no process running
  after its timeout.
- On macOS the same filesystem and network denials hold, and the doc's gaps are
  reported in `orbit doctor`. A network grant is refused, and `strict` containment
  refuses the step.
- A script's stdin is its validated input. A valid stdout JSON becomes
  `steps.<id>.output`, and invalid output fails without retry. A nonzero exit is
  `Ok(success = false)` and follows the step's `retry`.
- Declared output files within the caps reach `steps.<id>.files` with their digests.
  Over-cap output fails the step and returns no files.
- Each attempt records the script name, digest, grant ID, argv, exit code, duration and
  capped log on the job run.
- A pull follower refuses a script step unless its own grant was made with
  `--follower`. A host without the profile refuses with `script_sandbox_unavailable`,
  and so does a nested-sandbox start.
- An older binary meeting `target: script:` reports a load error and runs nothing.
- `local_shell` is unchanged. An activity that uses it loads and dispatches as it does
  today, and a nonzero exit still fails the step unless `allow_nonzero_exit` is set.

The mechanisms, walkthroughs and open questions are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## 2. Core Concepts

- **Script step.** A job step whose `target: script:<name>` runs a workspace-named script as a program with an argument list and no shell, inside the script sandbox.
- **Script manifest.** The workspace file naming a script's entry, declared input and output files, and the network and environment it asks for.
- **Script grant.** The operator's host-local, digest-witnessed consent for one script name; it holds only while it matches the script's current content digest, and no YAML field or agent can create or widen one.
- **Script sandbox.** The `linux-bwrap-script-v1` and `macos-sandbox-script-v1` profiles: nothing readable but what is named, one writable directory, a capped log and a kill bound, with no network unless the grant says so.
- **I/O contract.** JSON on stdin, JSON on stdout read by later steps as `steps.<id>.output`, and declared files exposed as `steps.<id>.files` with their digests.
- **`local_shell`.** The existing deterministic action that runs a direct child; it stays as it is and does not use the grant model.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Why the removal of the shell surface is reversed | [2_design.md](./2_design.md#why-orb-00374-is-reversed) | [ORB-00374] |
| Relation to `local_shell` | [2_design.md](./2_design.md#relation-to-local_shell) | [ORB-11294] |
| Reference form and manifest loading | [2_design.md §1](./2_design.md#1-reference-form) | [ORB-19983], [ORB-23140] |
| Authority outside the workspace YAML | [2_design.md §2](./2_design.md#2-authority-outside-the-workspace-yaml) | [ORB-19983], [ORB-21299] |
| Sandbox and capabilities | [2_design.md §3](./2_design.md#3-sandbox-and-capabilities) | [ORB-19985], [ORB-21298], [ORB-21301] |
| I/O contract and files | [2_design.md §4](./2_design.md#4-io-contract) | [ORB-20866], [ORB-21297] |
| Audit and composition | [2_design.md §5–6](./2_design.md#5-audit) | [ORB-19985] |
| Placement and compatibility | [2_design.md §7–8](./2_design.md#7-placement) | [ORB-10021] |
| orbit-office requirements | [2_design.md §9](./2_design.md#9-orbit-office-requirements) | [ORB-19206] |
| Standing rules | [4_decisions.md](./4_decisions.md) | — |

## Task References

- [ORB-19294] — designs script steps (this doc).
- [ORB-19983] — phase 1: added script manifests, `target: script:` parsing, the grant store and `orbit script grant|revoke|list|show`; nothing executes.
- [ORB-19985] — phase 2: Linux execution under `linux-bwrap-script-v1`, `script.execution` audit and `orbit script history`.
- [ORB-23140] — confined invalid script manifests to jobs that reference them.
- [ORB-21299] — phase 4: `script_grant` decision requests raised by refused steps, human and channel answers that write digest-bound grants, and the dashboard Scripts view.
- [ORB-20866] — phase 3 part: declared output files kept as run-owned, digest-verified sets exposed as `steps.<id>.files`, with grant output caps.
- [ORB-21297] — phase 3 part (Linux): declared `files.inputs` bound read-only at `in/<as>` and recorded with their SHA-256, and the `attach_script_files` action.
- [ORB-21298] — phase 3 part (macOS): the `macos-sandbox-script-v1` profile, `RLIMIT_CPU` and `RLIMIT_NOFILE` on both platforms, grant-time and pre-spawn root probes with TCC reporting, nested-sandbox refusal, `strict` refusing on macOS, and the doctor `script-sandbox` row.
- [ORB-21301] — phase 5 (Linux): `network: https` and `any` grants, `env_pass`, and redaction of passed values.
- [ORB-00374] — removed the v2 `shell` activity instead of sandboxing it; this doc takes its retrofit alternative.
- [ORB-00363] — found that the shell allowlist came from the YAML it guarded.
- [ORB-11294] — restored `local_shell` as a deterministic action; the Bearing and
  [Relation to `local_shell`](./2_design.md#relation-to-local_shell) inventory it.
- [ORB-10021] — made routine targets catalog references only.
- [ORB-12903] — added worker containment scopes.
- [ORB-19251] — macOS worker containment by tracked process tree.
- [ORB-19206] — orbit-office orchestration that relayed the five script-runner requirements in §9.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
