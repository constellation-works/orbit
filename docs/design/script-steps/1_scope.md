---
title: "Scope: Script steps"
owner: claude
last_updated: 2026-10-30
last_validated: 2026-10-30
status: Draft
stage: building
next: "Phases 1-2 landed ([ORB-19983], [ORB-19985]); phase 3 (files, macOS profile) is not yet filed"
feature: script-steps
doc_role: scope
type: design
summary: "Scope: a job step that runs a workspace script by name inside the executor sandbox, authorized by an operator grant bound to the script's content digest, with JSON stdin/stdout, declared files, audit and the job retry policy"
tags: [script-steps, activity-job, routines, sandbox, grants, policy-sandbox]
---

# Scope: Script steps

Status: proposed; phases 1 and 2 (Phasing) have landed. Phase 1 ([ORB-19983]) added the `Script`
manifest and its loader, `target: script:` parsing, the `script_grant` table and
witness, `orbit script grant|revoke|list|show`, the governed rows and doctor
findings. Phase 2 ([ORB-19985]) runs a granted step on Linux under
`linux-bwrap-script-v1` (no network, `env_pass` or `files.inputs` yet), with the
`script.execution` audit, `orbit run show` and `orbit script history`; other hosts
refuse with `script_sandbox_unavailable`. This doc is the design deliverable of [ORB-19294]. It reverses the removal recorded in
[The v2 shell activity surface is removed, not sandboxed](../activity-job/4_decisions.md#the-v2-shell-activity-surface-is-removed-not-sandboxed)
([ORB-00374]) by taking the alternative that decision recorded and set aside:
"retrofit the sandbox/policy/cwd pipeline". It also narrows the reach of
[Routine targets are catalog references only](../routines/4_decisions.md#routine-targets-are-catalog-references-only--no-inline-command-payloads)
([ORB-10021]):
routines still name only `job:<name>`, and a job may now contain a step that names a
granted script. Phase 2 recorded the superseding decision,
[Granted script steps run under the script profile](../activity-job/4_decisions.md#granted-script-steps-run-under-the-script-profile).

Bearing: Orbit already has these pieces, read from source at `a5b5408ea` (the `local_shell`
bullet was rechecked at `d10fae79b`).

- **Job steps name catalog targets by prefix.** A job step's `target:` is a
  namespace-prefixed reference. At that commit only `activity:<name>` resolved
  (`ACTIVITY_REF_PREFIX` and `resolve_ref`, `crates/orbit-engine/src/activity_job/catalog.rs`),
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
  definition declares no `sandbox`, so a step runs bare under the process supervisor and
  is confined only by a `cwd` check: the
  directory must resolve inside the workspace root. Kernel confinement is opt-in, through
  a `sandbox:` on the executor definition and an `fsProfile` on the activity
  (`crates/orbit-core/assets/executors/local-shell.yaml`). A nonzero exit fails the step
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
  [plugins §3 install-time build](../plugins/3_install_time_build.md)) runs a child
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
  so workers there run uncontained; a tracked-tree watchdog is filed as [ORB-19251].
- **Operator grants with a tamper-evident witness.** Plugin grants are host-local
  (SQLite `plugin_store`) and checked against a witness file under
  `~/.orbit/plugins/.grants/<ns>.json` that no agent or plugin profile can write
  ([plugins §3](../plugins/1_scope.md#3-lifecycle-and-state)). Declared programs are
  resolved to canonical paths at consent, and a changed resolution is a re-consent
  event ([plugins §4.3](../plugins/1_scope.md#43-sandboxing)). A tool whose catalog row
  has no Agent capability is operator-only (`classify_agent_tool_grant`,
  `crates/orbit-common/src/governance/authorization/grants.rs`).
- **Human answers through registered channels.** A decision request can be answered
  only through the human funnel, or by a registered channel's plugin calling
  `orbit.decision.submit`. Registration is operator-only host config
  ([human decision channel §3](../human-decision-channel/2_design.md#3-delegated-human-authority)).
- **Child environments.** Every Orbit child environment is built over a cleared env
  through `allowlisted_child_env` (`orbit_common::security::child_env`), and plugin
  `env_pass` refuses `ORBIT_*` names.

## Problem

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
[ORB-11294]. Its shipped definition declares no sandbox, so its `cwd` check is the only
confinement by default. Its program is named in the workspace YAML with no allowlist.
A routine can run a job that holds a `local_shell` step. This design leaves
`local_shell` as it is (see [Relation to `local_shell`](#relation-to-local_shell)).

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

## Goal

A job step can run a script that the workspace names, as a program with a list of
arguments and no shell, inside the executor sandbox. It runs only while the operator's
grant for that script name matches the script's current content digest. It reads
JSON on stdin, writes JSON on stdout that later steps read as `steps.<id>.output`, can
take and return declared files, and fails or retries like any other step. Every run,
and every refusal, is recorded on the job run.

Routines, condition triggers and plugins keep their contracts. Routine targets stay
`job:<name>`. No YAML field, workspace setting or agent can grant a script, widen a
grant or keep a grant valid across a content change.

## Shape

The task's items map to sections as follows: item 1 is §1, items 2–3 are §2–§3,
items 4–8 are §4–§8, and item 9 is Phasing and Open questions. The orbit-office
requirements relayed on the task are answered in §9.

### Why ORB-00374 is reversed

ORB-00374 chose removal over retrofit because retrofitting needed a sandbox, a policy
and a cwd pipeline that did not exist for a non-agent child. Each objection now has an
answer that does not depend on the workspace YAML:

| ORB-00374 objection | Answer here |
|---|---|
| No OS sandbox | A script profile derived from the build sandbox: Bubblewrap on Linux, deny-default SBPL on macOS, no unsandboxed fallback (§3) |
| No cwd confinement | The cwd is the run worktree, or the workspace root mounted read-only. Nothing outside the mount table exists for the child (§3) |
| No policy consultation | Each run checks a host-local operator grant: script name, content digest and capability set (§2) |
| The allowlist lived in the same YAML | The YAML only *requests* capabilities. The grant, written by the operator outside the workspace, is the authority, and a request beyond it refuses (§2) |
| Arbitrary command text | No inline text in the script manifest or the `script:` target. A step names a script, and the program and arguments are an argv list with no shell (§1). `local_shell` keeps its inline form; see [Relation to `local_shell`](#relation-to-local_shell) |
| The `Ok(success = false)` path lost its coverage | A script's nonzero exit is that outcome. Phase 2 restores the audit-message coverage ORB-00374 removed (§4) |

What stays from ORB-00374: `type: shell` stays a load error. The script surface takes no
inline command text: its manifest has no text field, and a routine target names only
`job:<name>`. Inline command text in an activity config is accepted only by the
`local_shell` action, which this design leaves as it is.

### Relation to `local_shell`

`local_shell` stays supported, and this design neither removes nor rejects it. The table
compares the two routes for one script.

| | `local_shell` (existing) | Script step (proposed) |
|---|---|---|
| Program | `command` + `args`, or `shell` + inline `script`, in the activity config; or the selected executor definition's `command` when the activity names none | A manifest and an entry file, with no inline text (§1) |
| Authority | The activity YAML, or the executor definition it selects, that names the program; no digest and no operator check | An operator grant, host-local, bound to the manifest and entry digest (§2) |
| Content change | Not checked. Neither the YAML nor the file it runs is digested | Voids the grant; the next run refuses with `grant_stale` (§2) |
| Sandbox | None by default. A `sandbox:` on the executor definition and an `fsProfile` on the activity opt in | The script profile, default-deny, with no unsandboxed fallback (§3) |
| Network | The host's, by default | `none` by default; a grant can widen it (§3) |
| Files and cwd | `cwd` must resolve inside the workspace root | Worktree or read-only root, with `.orbit/` masked; declared inputs and outputs (§3, §4) |
| Environment | An allowlisted baseline, plus the executor's and the activity's `env` | Cleared; fixed values plus the grant's `env_pass` names (§3) |
| Output | `success`, `exit_code`, `stdout`, `stderr`, `timed_out`, `argv` and `cwd` | Stdout JSON validated against a schema, plus declared files (§4) |
| Nonzero exit | Fails the step (`LocalShellFailed`) unless `allow_nonzero_exit` is set | `Ok(success = false)` with `{exit_code, signal}` (§4) |
| Audit | The step's output; on failure, a checkpointed diagnostic | A `script_execution` entry with digest, grant ID and provenance (§5) |

**Relationship.** The script step is a second route, for an operator who must consent to
exact bytes and wants the script profile. It does not replace `local_shell`. The inline
`shell` + `script` form is the one inline command text that the no-inline rule in §1 does
not cover. Changing `local_shell`'s shipped posture (its default sandbox, its nonzero-exit
mapping, or whether a routine's job may use it) changes a shipped action, so it is a
separate decision (open question 8). Removing or rejecting `local_shell` is a compatibility
decision of its own, and this design does not assume it has been made.

**Why.** The script route adds what the table's right column shows: consent bound to the
bytes a reviewer approved, and a default-deny sandbox with no unsandboxed fallback. It is
built beside `local_shell` because changing a shipped action in place is a separate
decision.

### 1. Reference form

**Decision.** A job step names a script as `target: script:<name>`, beside
`target: activity:<name>`. A script is a manifest at
`.orbit/resources/scripts/<name>.yaml` in the workspace, beside the workspace catalog's
`resources/jobs` and `resources/activities`. Its `entry` names the program file by a
workspace-relative path, either inside `.orbit/resources/scripts/` or anywhere in the
repository. The manifest is the catalog entry; the entry file is the code. Scripts are
workspace-only: the global root's `resources/` holds no scripts, so no script reaches
every workspace on a host.

```yaml
# .orbit/resources/scripts/package.yaml
schemaVersion: 1
kind: Script
metadata:
  name: package
spec:
  description: Package the built site into a tarball and report its digest.
  entry: tools/package.sh            # workspace-relative; resolved physically
  interpreter: [bash]                  # optional; see Interpreters
  args: ["--out", "out/", "--label={{ input.label }}"]
  input_schema_json: {type: object, required: [dist_dir], properties: {dist_dir: {type: string}, label: {type: string}}}
  output_schema_json: {type: object, required: [tarball, sha256], properties: {tarball: {type: string}, sha256: {type: string}}}
  files:
    inputs: [{from: "{{ input.dist_dir }}", as: dist}]
    outputs: {max_files: 1, max_file_bytes: 1048576}
  requests:                            # asks only; the grant decides (§2)
    network: none
    write_roots: []
    env_pass: []
    timeout_seconds: 300
```

The rules:

- **No inline code in the manifest.** The manifest has no `command`, `script` or `run`
  text field, and `target: script:<name>` names a manifest and nothing else. This rule
  covers the script surface only; `local_shell`'s inline `script` is outside it.
  `entry` must resolve physically (after symlinks) to a regular file beneath the
  workspace root, outside `.git/` and `.orbit/state/`. A path that escapes is a load
  error naming the manifest.
- **argv, never a shell.** Orbit `execve`s `[launcher…, <entry snapshot>, args…]`
  directly. No string is ever handed to `sh -c`.
- **Templated arguments render to exactly one argv element each.** `{{ … }}` takes the
  step's input, the same template context as other steps, and must render to a
  scalar. A value is never split on whitespace and never globbed. A rendered value
  that starts with `-` is refused unless the template has a literal prefix
  (`--label={{ input.label }}`), so input can't become an option.
- **Interpreters.** Three forms, all resolved to canonical paths at grant time,
  as plugin `requires.programs` are:
  - an `interpreter:` list in the manifest (`[bash]`, `[python3]`, or a launcher with
    fixed arguments such as `[uv, run, --offline, --locked, --no-config, --no-project,
    --python, "3.12", --script]`);
  - a `#!` line, honored only as `#!/abs/path [one-arg]` or
    `#!/usr/bin/env <name>`, and treated as if written in `interpreter:`;
  - neither, for a native executable.

  The first element is searched on the granting operator's `PATH` and recorded in the
  grant. At run time Orbit execs that recorded path itself; the kernel's `#!`
  handling and the caller's `PATH` are never used. Interpreter arguments are literal.
  The known inline-code flags (`-c` for `sh`, `bash`, `zsh`, `dash` and `python*`, `-e`
  for `perl`, `ruby` and `node`, `-E` for `perl`, and `-p`, `--eval` and `--print` for
  `node`), including bundled spellings such as `bash -ec`, are refused, so an
  interpreter list can't smuggle code past the digest.
- **Routines don't name scripts.** A routine `target: script:<name>` is rejected at
  parse time with guidance to wrap the script in a one-step job, the treatment
  `activity:<name>` already gets.

**Why.** A named manifest gives the loader, `orbit script list`, the dashboard and the
grant one stable key per script, and a place for the I/O contract and capability
requests. Letting `entry` live anywhere in the repository means the `tools/package.sh`
that CI and humans already run is the same file Orbit runs. The digest (§2) covers
both files, so the split costs no authority.

**Rejected alternatives.**

- **A `type: script` activity variant.** It would put a third spelling of "run this"
  into `ActivityV2Spec`, and an activity file holding `entry` plus its schemas plus its
  requests is the manifest under another name. Activities would also be reachable from
  inline job specs, which this design keeps out. A script step is a step target, not
  an activity type.
- **Only the catalog directory** (the brief's `.orbit/scripts/`, here
  `.orbit/resources/scripts/`). It forces a second copy of every script CI already runs,
  and the two drift.
- **Only a repository path, with no manifest.** There is nowhere to put the schemas
  and requests, and no catalog to list or grant.
- **Inline `command:` text in the manifest, with a strict parser.** Any text field there
  is a place where the grant can't see the code. `local_shell` already takes inline
  `shell` + `script` in an activity; this design doesn't copy that form into the script
  surface.

### 2. Authority outside the workspace YAML

**Decision.** A script runs only under a current **script grant**: an operator's
host-local consent to one workspace's script name at one content digest, with an
explicit capability set. The manifest's `requests` are what the script asks for. The
grant is what it gets, and a request beyond the grant refuses the step. Nothing in the
workspace can create, widen or refresh a grant.

**The digest.** `sha256("orbit.script.grant.v1\n" ‖ manifest bytes ‖ "\n" ‖ entry
bytes)`, over the bytes Orbit is about to execute. Any edit to the manifest (an
argument, a schema, a request) or to the entry file changes it. The recorded
interpreter paths are part of the grant, not the digest. A changed resolution is a
re-grant event, as with plugin programs.

**The grant record.** The grant lives in host-local SQLite (`script_grant`, next to
`plugin_store`), with a witness at
`~/.orbit/scripts/.grants/<workspace_id>/<name>.json`. The witness also holds a copy
of the granted manifest and entry bytes, so a re-grant can show the exact diff. The
record carries:

| Field | Meaning |
|---|---|
| `grant_id` | `sg-<hash>`, cited in audit |
| `workspace_id`, `name`, `digest` | What is granted |
| `interpreter_paths` | Canonical paths resolved at grant |
| `network` | `none` (default), `https` or `any` (§3) |
| `read_roots`, `write_roots` | Extra absolute paths beyond the defaults in §3 |
| `env_pass` | Variable names copied into the child env; `ORBIT_*` refused |
| `limits` | `timeout_max_seconds`, `memory_max`, `tasks_max`, `scratch_max_bytes`, output-file caps; each at or below the host's `machine.worker_*` and `machine.script_*` ceilings (Configuration) |
| `host_role` | `owner` (default) or `follower` (§7) |
| `grantor`, `granted_at`, `provenance` | Human label, time, and `cli`, `dashboard` or `decision-channel <name>` |
| `expires_at` | Optional; none by default |

The loader refuses a row whose witness integrity value doesn't match, as the plugin
loader does. `orbit_tools` makes `orbit.db` writable but never `scripts/.grants/`, so a
forged row has no matching witness.

**Who can grant.** Only a human, through three routes:

1. **CLI.** `orbit script grant <name> [--network https] [--write-root PATH]…
   [--env-pass NAME]… [--follower]`. It prints the digest, the diff against the last
   granted bytes, the requested capabilities against the ones being granted, and the
   resolved interpreter paths. `orbit script revoke <name>` removes a grant.
2. **Dashboard.** An operator-session action with the same diff and capability view.
3. **A decision request** (phase 4, §9.1). A `script_grant` request binds workspace,
   name, digest and capability set. Anyone may raise one, including an agent or a
   plugin. Only the human funnel or a registered decision channel's
   `orbit.decision.submit` can answer it. The request is superseded when the digest
   changes.

`orbit.script.grant` and `orbit.script.revoke` are governed operations whose catalog
rows have no Agent capability, so `classify_agent_tool_grant` makes them operator-only.
Both are refused from an agent envelope and from a plugin callback session.

**How agents are kept from editing granted scripts or their grants.**

- **Edits are allowed and inert.** An agent can still change a script: it is a file in
  a repository it works in, and a PR that changes `tools/package.sh` is ordinary
  work. The change voids the grant. The next run snapshots the bytes, computes the
  digest, finds no match and refuses with `grant_stale`, naming the granted and
  current digests. Nothing runs until an operator re-grants after seeing the diff.
- **Grants are out of reach.** The grant store and witness sit under the global root,
  where no agent fs profile and no plugin `orbit_tools` inventory grants writes. The
  grant tools are operator-only, and a decision answer needs a human.
- **No time-of-check gap.** Before checking the grant, Orbit copies the manifest and
  entry into the step's private scratch directory. It digests and executes that copy,
  mounted read-only. A write to the original after the check changes nothing for this
  run.
- **Visible staleness.** `orbit doctor`, `orbit script list` and the dashboard show
  each script as `granted`, `stale` (with the commit that changed it, when the
  checkout is Git), `ungranted` or `revoked`. A stale granted script is a doctor
  warning.

**Why.** It is the plugin consent model applied to a smaller unit. Binding the digest
makes a grant mean "these bytes", which is what a reviewer actually approved. Keeping
grants host-local means a workspace pushed to another host carries no authority with
it.

**Rejected alternatives.**

- **An allowlist in workspace config (`scripts.allowed: [...]`).** This is exactly the
  ORB-00363 tautology.
- **A grant by name only.** A later edit would inherit the authority, so an agent
  could swap the code under an approved name.
- **A grant by Git commit or signed tag.** Many workspaces aren't Git checkouts, a
  commit approves far more than one script, and orbit-office stores scripts outside
  Git.
- **Making granted scripts unwritable to agents (a read-only mask in agent
  profiles).** It doesn't cover a human or CI editing a script, it blocks legitimate
  PRs to scripts, and the digest already makes edits harmless.
- **Agent-grantable scripts with operator review later.** The point of the grant is
  that no agent session holds the authority to run new code unattended.

### 3. Sandbox and capabilities

**Decision.** A script runs under a new **script profile** in `orbit-exec`, built from
the build sandbox's construction and supervision: `linux-bwrap-script-v1` and
`macos-sandbox-script-v1`. A host that can't apply the profile refuses the step. There
is no unsandboxed fallback and no `unsandboxed` grant in v1.

**Filesystem (both platforms).**

| Path | Access | Notes |
|---|---|---|
| Host runtime table (`/usr`, loader, `/etc` resolver, CA, `localtime`, minimal `passwd`) | read | Same table as the build profile |
| Recorded interpreter paths | read, execute | From the grant, never the caller's `PATH` |
| Script snapshot (manifest and entry) | read | In the step's scratch directory; the `entry` argument |
| Run worktree (when the job has one) | read, write | The cwd |
| Workspace root (when the job has no worktree) | read only | The cwd; write only if the grant lists it in `write_roots` |
| `.git` of the cwd tree | read only | So `git describe` and `git archive` work |
| `.orbit/` of the workspace | none | Masked: no store, no config, no task bundles |
| Step scratch: `home/`, `tmp/`, `in/`, `out/` | `in/` read, the rest read-write | `$HOME` and `$TMPDIR` point here; size-bounded |
| Grant `read_roots` / `write_roots` | as granted | Absolute paths, canonicalized at grant, created before spawn as plugin write roots are |
| Credential paths (`~/.ssh`, `~/.config/gh`, …) and the global root | none | The credential denies win over every grant |

**Network.** `none` by default. On Linux the network namespace is unshared. On macOS
the profile uses `(deny network*)`.

- **`https`** (Linux only). The host network is shared under a Landlock ruleset that
  allows outbound TCP to port 443 only. It is the build profile's fetch mode, and
  needs Landlock ABI 4; an older kernel refuses.
- **`any`** (Linux only). The network is shared with no rule. The grant command warns.
- **macOS refuses both in v1.** A descendant that calls `setsid` escapes the
  process-group kill and would keep its network after the step ends. That is the same
  reason the build sandbox refuses `fetch` there (`BUILD_FETCH_PHASE_SUPPORTED`).

**Environment.** The env is cleared and composed through `allowlisted_child_env`:

- **Fixed values:** `PATH` (the recorded interpreter directories, then `/usr/bin:/bin`),
  `HOME`, `TMPDIR`, `LANG=C.UTF-8`, `TZ`.
- **Step context:** `ORBIT_SCRIPT_NAME`, `ORBIT_SCRIPT_DIGEST`, `ORBIT_SCRIPT_RUN_ID`,
  `ORBIT_SCRIPT_STEP_ID`, `ORBIT_SCRIPT_IN` and `ORBIT_SCRIPT_OUT`.
- **Granted names:** each `env_pass` name, copied when the job's process has it. A
  missing one is unset, never an empty string, and is recorded.

No Orbit credential, claim token or provider key is passed. The script can't call
`orbit` tools, because no `orbit_tools` inventory is mounted. Writes back to Orbit
happen in a later deterministic step that reads the script's output.

**Secrets.** They reach a script only through the grant's `env_pass` list, read from
the environment of the process that runs the job. The captured log and any recorded
argv go through the shared redaction, with each passed value registered as a literal
to redact. A secret file, such as a backup tool's credentials, is reachable only when
it is listed in `read_roots` and isn't under a credential-deny path. A dedicated
`secret_files` grant is an open question.

**Resources.**

- **Timeout.** The step's `timeout_seconds` (existing field), defaulting to the
  manifest request or 600 s, and capped by the grant's `timeout_max_seconds` (default
  3600). On expiry Orbit sends `SIGTERM`, waits 10 s, then sends `SIGKILL`. The step
  fails with `script_timeout`.
- **Linux.** The script is a descendant of the job's worker and inherits its cgroup;
  worker limits apply only when the worker actually has a scope. The default
  `machine.worker_containment = "warn"` runs a worker in the caller's cgroup with a
  warning when its scope is unavailable, and `off` runs it without a worker scope
  (with a warning on Linux).
  When systemd can create it, the script also gets its own transient scope,
  `orbit-script-<run>-<step>.scope`, with `MemoryMax`, `TasksMax` and `CPUQuota` from
  the grant, each capped at the machine value. If that per-script scope is unavailable,
  `warn` or `off` permits the step to continue in the worker's cgroup; `strict` refuses
  the step. A worker with an unavailable scope is likewise refused under `strict`.
  Thus a script may run with neither scope under the default mode and can exhaust
  host resources. `CPUQuota` is optional and systemd enforces it only where the user
  manager delegates the CPU controller; elsewhere it may be accepted but not enforced.
  Bubblewrap's `--unshare-pid` and `--die-with-parent` mean that killing the
  namespace's init kills every descendant, so on Linux nothing outlives the step.
- **Scratch size.** The scratch directory is size-bounded (`scratch_max_bytes`,
  default 1 GiB), polled as the build directory is. A breach kills the step.

**macOS gaps, stated.**

- **No memory or task limits.** There is no cgroup. A script inherits whatever the
  worker has, which today is nothing; [ORB-19251]'s tracked-tree watchdog would cover
  the script's tree too. `RLIMIT_CPU` and `RLIMIT_NOFILE` are set, but `RLIMIT_NPROC`
  is per user and isn't used. `strict` containment refuses a script step on macOS
  until ORB-19251 decides whether the watchdog counts as containment.
- **Timeout escape.** Without a pid namespace, a descendant that calls `setsid`
  escapes the process-group kill. It stays under the Seatbelt profile, confined to the
  granted paths and with no network, but it can keep writing there after the step
  ends.
- **No network** (above).
- **Privacy permissions (TCC).** These apply on top of the profile (§9.5).

**Why.** Starting from the build profile means the script inherits a boundary that
already denies reads by default, and it reuses code that has been reviewed and is
running. The worktree-or-read-only-root rule is the cwd confinement ORB-00374 lacked.
An unattended routine never writes into a primary checkout that other runs read,
unless the operator listed it.

**Rejected alternatives.**

- **The agent profile** (host readable, writes confined). A script would read every
  repository and file the host user can, which is far more than a chore needs.
- **The plugin backend's Landlock-only boundary.** It has no mount namespace and no
  pid namespace, so it gives no kill-everything timeout and only a partial read
  denial. It is the gap orbit-office reported.
- **Running the script in the worker's own sandbox, if any.** Workers aren't
  sandboxed, and an agent's sandbox is wider than a script needs.
- **An `unsandboxed` grant as plugins have.** That reopens the ORB-00374 surface behind
  a checkbox. Open question 6 tracks whether a host without Bubblewrap ever needs one.

### 4. I/O contract

**Decision.**

- **stdin.** One JSON document, the step's rendered input object, validated against
  the manifest's `input_schema_json` before spawn, then EOF. Templated `args` read the
  same object.
- **stdout.** Read up to 1 MiB. On exit 0 it is parsed as JSON and validated against
  `output_schema_json`. It becomes `steps.<id>.output`. Empty stdout with no schema
  gives `{}`.
- **The log.** stdout and stderr are interleaved into one capped log (1 MiB, head and
  tail kept with a truncation marker, the build log's cap), redacted, and stored as a
  run artifact.
- **Outcomes.**

| Result | Step outcome | Retry |
|---|---|---|
| Exit 0, valid output | `Ok(success = true)` | — |
| Exit 0, stdout not JSON, over the cap, or failing `output_schema_json` | `Err(script_output_invalid)` | No (validation bypasses retry, as `RetrySpec` documents) |
| Nonzero exit or killed by a signal | `Ok(success = false)`; `message` names the exit code or signal and the last stderr lines; `output` is `{exit_code, signal}` | Yes, per the step's `retry` |
| `script_timeout`, scope memory or task breach | `Ok(success = false)` with the code | Yes |
| Refusals: `grant_missing`, `grant_stale`, `grant_insufficient`, `interpreter_changed`, `script_sandbox_unavailable`, `script_root_unavailable`, `script_follower_refused` | `Err`, non-retryable | No; another attempt meets the same refusal |

  The nonzero-exit row restores the `Ok(success = false)` leaf that ORB-00374 removed.
  The job executor still supports it: `StepOutcome.success`, parallel, fan-out and
  loop aggregation, and the retry loop are unchanged. Phase 2 adds the coverage of the
  audit-message path whose loss that decision recorded as a cost.
- **Files in.** `files.inputs` lists `{from, as}`. `from` is templated and must
  resolve physically inside the cwd tree or a granted `read_root`. Each one is
  bind-mounted (Linux) or copied (macOS) read-only at `in/<as>`. A missing input
  refuses the step before spawn.
- **Files out.** After exit 0, Orbit collects regular files at the top level of
  `out/`; symlinks and subdirectories are refused. The default caps are 8 files of at
  most 1 MiB each, matching the task-artifact limit. The grant may raise them, up to
  64 files of 64 MiB each. Files go to the run's artifact store under
  `script/<step_id>/out/` and are exposed as `steps.<id>.files`: `[{name, path,
  bytes, sha256}]`. Over-cap output fails the step (`script_outputs_over_cap`). It is
  all or nothing, so a later step never sees part of a set.

`steps.<id>.files` is a second key beside `output` in the step map. It is persisted in
the step record, so resume reseeds it the way it reseeds output.

**Why.** JSON in and out is the shape every other step already speaks, so templates,
`when:` guards and later steps need nothing new. Declared files cover what JSON can't,
such as a tarball or a chart, without giving later steps the script's scratch
directory.

**Rejected alternatives.**

- **Scrape key-value pairs from stdout.** Untyped, and there is no schema.
- **Pass input only as arguments.** That hits argv length limits, and quoting
  structured data invites injection.
- **Let later steps read the script's scratch directory.** It exposes everything the
  script left behind and couples steps to a layout. Declared, capped outputs are an
  interface.
- **Treat a nonzero exit as `Err`, as `local_shell` does.** `local_shell` fails the step
  with `LocalShellFailed` unless `allow_nonzero_exit` is set, and checkpoints the
  diagnostic. The retry loop retries `Err` and `Ok(success = false)` alike, and both reach
  `recovery_activity`, so the choice is the step's outcome shape, not whether retry
  applies. A script's nonzero exit is an ordinary result of a chore, not an executor
  fault, so the script step returns `Ok(success = false)` with `{exit_code, signal}` as
  its output. `local_shell` with `allow_nonzero_exit: true` returns the exit status as
  output, but the job executor sees that as a successful step.

### 5. Audit

**Decision.** Each attempt of a script step records a `script_execution` entry on the
job run's step:

- **Identity:** script name, entry path, digest, grant ID, grantor and grant
  provenance.
- **What ran:** the recorded interpreter paths, the rendered argv (redacted), the cwd
  mode (worktree or read-only root), the effective capability set, and the profile ID
  and platform.
- **What happened:** exit code or signal, duration, `timed_out`, resource-breach
  evidence (the scope's `memory.events` and `pids.events` on Linux), stdout bytes, the
  capped log's artifact path, and the output files with their `sha256`.

Refusals record an entry too, with the refusal code. For `grant_stale` it holds the
granted and current digests. Grant, revoke and re-grant write governed audit rows with
the diff digest. `orbit run show`, the dashboard run view and `orbit script history
<name>` read the entries.

**Why.** An operator answering "what ran last night, and was it what I approved?"
needs the digest and grant ID on the run itself, not only in a host log.

**Rejected alternative.** Logging only to the host service log. It isn't attached to
the run, isn't kept with the run and can't be read from another host's dashboard.

### 6. Composition

**Decision.**

- **Routines.** These target `job:<name>` as today. A job with script steps is an
  ordinary catalog job, so the routine design and the catalog-only decision hold
  unchanged: the scheduler adds no execution surface, and the job's steps carry their
  own authority.
- **Condition triggers** ([ORB-19288] designed them). Their phase 2 actions are routine job runs
  ([condition triggers §6](../condition-triggers/1_scope.md#6-actions)). A condition
  whose routine runs a job with a script step passes its `subject` as job input, and
  the script reads it on stdin. A script step that needs the operator gate a release
  job needs declares it on the job (`authority: operator`,
  [condition triggers §7](../condition-triggers/1_scope.md#7-authority-for-high-authority-actions)),
  and both grants apply: the condition's grant to fire, and the script grant to run.
- **Agent and deterministic steps.** These mix freely. A later step reads
  `steps.<script>.output` and `steps.<script>.files`, and a script reads earlier
  outputs through its templated input.
- **Plugins.**

| | Plugin tool | Script step |
|---|---|---|
| What it is | A reusable, versioned tool shipped across workspaces | A workspace-local chore |
| Installed by | `orbit plugin add`, from a pinned source | A file in the workspace |
| Authority | A host grant per plugin namespace, covering every tool | A host grant per script, at one digest |
| Called by | Agents (through the broker), deterministic `plugin.tool_call`, MCP | A job step only |
| Sandbox | Landlock or Seatbelt with the plugin's grants | The script profile (§3) |
| Talks to Orbit | Through `orbit_tools` callbacks | Never; later steps do |

A plugin that only wraps one script, as `office.script-run` does, should become a
script step once phase 4 ships.

**Why.** It keeps one execution surface per kind of code. A plugin answers "share
this tool", and a script step answers "run this chore here".

**Rejected alternatives.**

- **`target: script:<name>` on routines.** It is sugar for a one-step job, and it would
  reopen the catalog-only decision for a convenience. Open question 1 tracks it.
- **Scripts calling `orbit`.** That would need an `orbit_tools` inventory and callback
  authority inside a sandbox meant to hold nothing.

### 7. Placement

**Decision.** A script step runs on the host that runs its job, inside that job's
worker. Grants are host-local, so a script runs only where an operator granted it
there. A host without a grant refuses with `grant_missing`.

- **Pull followers.** A pull follower holds replica checkouts and owns nothing. It
  refuses a script step (`script_follower_refused`) unless its own grant was made with
  `--follower` (`host_role: follower`). This covers a claimed leaf too: a job that a
  follower claimed and runs from the owner's task meets the follower's grants, not the
  owner's.
- **Hosts without the sandbox.** Platforms other than Linux and macOS, Linux without
  usable unprivileged user namespaces or Bubblewrap, or a Landlock ABI below 4 for an
  `https` grant: the step refuses with `script_sandbox_unavailable`. `orbit doctor`
  reports a `script sandbox` row (available, or the reason), and `orbit script grant`
  warns on such a host.
- **Nested sandboxes.** Orbit probes the profile once before spawn. Seatbelt can't
  nest (exit 71, `sandbox_apply: Operation not permitted`, as in [ORB-19224]), and
  Bubblewrap inside another user namespace may be refused. Either way the step refuses
  with `script_sandbox_unavailable`. It never runs under the outer profile instead.

**Why.** A grant is consent on a host, for that host's files and network. Carrying it
to another machine would run code where nobody approved it.

**Rejected alternatives.**

- **Replicating grants with the workspace.** Authority would travel through the
  channel the workspace controls.
- **A `hosts:` pin in the manifest.** Routines dropped host pins
  ([routines decisions](../routines/4_decisions.md)). Placement follows the job, and the
  grant gates it.

### 8. Compatibility

**Decision.**

- **Older binaries.** One that meets a job step `target: script:<name>` fails that
  job's load with `ResolveError::UnknownRefKind`, as `type: shell` fails today. A
  routine targeting the job reports a per-file load error and fires nothing. An older
  binary ignores `.orbit/resources/scripts/`.
- **Persisted runs.** `script_execution` and `steps.<id>.files` are additive, optional
  fields on the persisted step record, and readers tolerate their absence. An older
  binary can't resume a run with a script step, because the job no longer loads, so the
  run stays unresumable on that binary instead of running the step.
- **Grants.** `script_grant` is a new host table added by an additive migration inside
  the release-gated compatibility window ([ORB-15732]). Old binaries never read it. A
  witness with an unknown `orbit.script.grant.v<N>` version refuses.
- **`type: shell`** stays a load error (§1). **Inline text** stays out of the script
  manifest and target. `local_shell`'s `shell` + `script` form is unchanged; removing or
  rejecting it would be a separate compatibility decision.

**Why.** Fail closed on the old side, add on the new side. A script must never run on
a binary that can't enforce its grant.

### Configuration

| Key | Default | Meaning |
|---|---|---|
| `machine.script_timeout_max_seconds` | `3600` | Ceiling on any grant's `timeout_max_seconds` on this host |
| `machine.script_scratch_max_bytes` | `1GiB` | Ceiling on any grant's scratch size |
| `machine.script_output_files_max` | `64` × `64MiB` | Ceiling on any grant's output-file caps |
| `machine.worker_containment` | (existing; defaults to `warn`) | `warn` permits uncontained worker/script fallbacks, with a Linux warning when a scope is unavailable (macOS launches uncontained without a warning); `off` skips worker scopes with a Linux warning; `strict` refuses a worker or script step whose required scope can't be created, and every script step on macOS (§3) |

Host keys, not workspace keys. Each grant's limits must sit at or below them.

### Surfaces

- **CLI.** `orbit script list` shows name, entry, digest, grant state, last run and
  outcome. `orbit script show <name>` adds the manifest, the grant and its diff
  against the current bytes. `orbit script grant`, `orbit script revoke` and
  `orbit script history <name>` round it out.
- **Doctor.** A `script sandbox` row, plus one finding per `stale` or `revoked` script
  that a job still references.
- **Dashboard.** A Scripts view in the Automation area with grant state and diffs, and
  the operator-only grant and revoke actions. The run view shows `script_execution`.
- **MCP.** `orbit.script.list` and `orbit.script.show` are read-only and grantable to
  agents. Grant and revoke are operator-only (§2).

### 9. orbit-office requirements

The orbit-office orchestration ([ORB-19206]) asked for five decisions so that
`office.script-run` can become a thin wrapper over a script step, or be retired.

1. **A grant path besides an operator command.** *Decided, phase 4.* The office
   registers a decision channel once, from the operator CLI
   (`orbit decision channel register office --human <name> --plugin office`). An
   office proposal raises a `script_grant` request bound to name, digest and
   capability set. When the user accepts it in the office app, the office plugin calls
   `orbit.decision.submit`, and core writes the grant with the channel's human as
   grantor and `decision-channel office` as provenance. This is the digest-bound
   consent STD-05 §R21 describes. It stays out of an agent's reach the way every
   channel answer does:
   - registration is operator-only;
   - the submit tool isn't callable from an agent session;
   - core verifies the binding, so a changed digest supersedes the request.
2. **Script roots in a plugin's store.** *Partly decided.* Office scripts under the
   workspace's `library/` are already in reach: a manifest's `entry` may be any
   workspace-relative path, and the digest is computed by Orbit over the bytes it
   snapshots and runs, never taken from the plugin. A plugin-managed store outside the
   workspace (`{{plugin_state}}`) and artifact references are deferred to open
   question 2. What is missing is a contract for a plugin to contribute manifests whose
   entry lives in its own state, and for Orbit to read those bytes without the plugin's
   say-so.
3. **A launcher and a fetch-at-grant cache.** *Decided for the launcher and the
   read-only cache; deferred for the fetch.*
   - **Launcher.** An `interpreter:` list with fixed arguments (§1) takes the office's
     `uv run --offline --locked … --script` line as written. `uv` is resolved and
     recorded at grant.
   - **Cache.** The dependency cache is a grant `read_root` (for example
     `plugin_state/uv-cache`), mounted read-only, and `--offline --locked` keeps the
     run from touching the network.
   - **Fetch at grant time.** This would be a grant-time `prepare` phase under the
     build profile's fetch mode, Linux only, because macOS refuses the networked phase
     for the reason in §3. Office installs are usually a single Mac, so until open
     question 3 settles it, the office keeps resolving dependencies itself at approval
     under its plugin grants, and the script step only reads the result.
4. **File inputs and file outputs.** *Decided* (§4). Declared inputs are mounted
   read-only at `in/<as>`, from the cwd tree or a granted `read_root` such as
   `library/`. `out/` comes back as `steps.<id>.files`, with the office's caps (8 files
   of 1 MiB, log 1 MiB) as the defaults. A later deterministic step attaches the files
   to the review task it files, through a new deterministic action that attaches
   `steps.<id>.files` as task artifacts (phase 3). The office keeps proposal
   approval, output review and pruning.
5. **File operations outside the workspace, on macOS.** *Decided.*
   - **Extra roots.** A grant lists them in `write_roots`, for example `~/Downloads`.
   - **Checked at grant.** The grant command checks each root: it must exist,
     canonicalize, sit outside the credential denies, and pass a read and write probe
     by the granting process. A failed root refuses the grant and names the path.
   - **Checked again at run.** Before spawn, the job's process repeats the probe. TCC
     attributes access to the responsible process, which is the clock's launchd agent
     or the binary it runs, not the terminal that granted. A denied or missing root
     refuses the step with `script_root_unavailable`, naming the path and, on macOS,
     the privacy pane to grant it in. Nothing runs, so there is no partial run.
   - **Later denials.** A denial the script meets after the probe is the script's
     error, a nonzero exit.
   - **Inside a worker sandbox.** The step refuses with `script_sandbox_unavailable`
     (§7) instead of nesting or weakening. A clock-started routine on the Mac isn't
     nested, so the office's case runs.

## Use cases, walked through

### A. Nightly `backup.sh` routine

```yaml
# .orbit/resources/scripts/backup.yaml
schemaVersion: 1
kind: Script
metadata: {name: backup}
spec:
  description: Snapshot the workspace's data directory into the backup volume.
  entry: .orbit/resources/scripts/backup.sh          # #!/usr/bin/env bash
  args: ["--dest", "/srv/backups/notes"]
  output_schema_json: {type: object, required: [archive, bytes], properties: {archive: {type: string}, bytes: {type: integer}}}
  requests: {network: none, write_roots: [/srv/backups/notes], timeout_seconds: 1800}

# .orbit/resources/jobs/nightly_backup.yaml (abridged)
spec:
  steps:
    - id: backup
      target: script:backup
      timeout_seconds: 1800
      retry: {max_attempts: 2}

# .orbit/routines/nightly-backup.yaml
trigger: {cron: "0 3 * * *"}
target: job:nightly_backup
```

1. The operator runs `orbit script grant backup --write-root /srv/backups/notes`. Orbit
   resolves `bash` to `/usr/bin/bash`, shows the request against the grant, writes
   `sg-…` and its witness, and probes `/srv/backups/notes`.
2. At 03:00 the clock submits `nightly_backup`, and the worker starts under the
   configured containment policy (in a scope when available, or uncontained under
   `warn`/`off`). The step snapshots `backup.yaml` and `backup.sh`, computes the
   digest, matches the grant, and checks that the requested `write_roots` lie within
   the granted ones.
3. Bubblewrap starts with no network and the workspace root mounted read-only as the
   cwd (the job has no worktree). `/srv/backups/notes` is writable, the scratch
   directory is `$HOME` and `$TMPDIR`, `.orbit/` is masked and `~/.ssh` is denied. The
   environment is fixed, and stdin is `{}`.
4. The script writes the archive and prints `{"archive": "...", "bytes": 81234567}`,
   then exits 0. Orbit validates the output and records a `script_execution` entry with
   the digest, grant ID, duration and log.
5. If the disk is full, the script exits 1: `Ok(success = false)`. The retry runs once
   more, then the run fails with "script exited 1" and the stderr tail in
   `orbit run show`.

An off-site copy needs `network: https` and a credential, which is Linux only in v1:
`--network https --env-pass RESTIC_PASSWORD --read-root ~/.config/restic`. On a Mac the
same grant is refused (§3).

### B. Agent step → `package.sh` → deterministic upload

```yaml
# .orbit/resources/jobs/site_release.yaml (abridged)
spec:                                     # runs in a task's run worktree
  steps:
    - id: build
      target: activity:build_site          # agent_loop; output {dist_dir, label}
    - id: package
      target: script:package               # §1 manifest
      default_input:
        dist_dir: "{{ steps.build.output.dist_dir }}"
        label: "{{ steps.build.output.label }}"
      retry: {max_attempts: 2}
    - id: upload
      target: activity:upload_release      # deterministic
      default_input:
        tarball: "{{ steps.package.files[0].path }}"
        sha256: "{{ steps.package.output.sha256 }}"
```

1. The agent step builds into the run worktree and returns
   `{"dist_dir": "site/dist", "label": "v42"}`.
2. The `package` step renders its input and validates it against
   `input_schema_json`. It renders `args` to
   `["--out", "out/", "--label=v42"]`, a single element with no shell, and resolves
   `files.inputs` `site/dist` inside the worktree, mounting it read-only at `in/dist`.
3. The digest matches the grant. The script runs with the worktree as its cwd
   (writable), no network, and a cleared env. It tars `in/dist` into
   `out/site-v42.tgz`, prints `{"tarball": "site-v42.tgz", "sha256": "…"}` and exits 0.
4. Orbit validates stdout and collects `out/` (one file, under the caps). It records
   `steps.package.output` and `steps.package.files`.
5. The `upload` step reads both. The upload is a deterministic action with its own
   network authority, so the script never needed a network grant.

Had the agent written a `dist_dir` of `../../etc`, the input would resolve outside the
worktree and the step would refuse before spawn. Had it returned
`label: "--delete"`, the literal `--label=` prefix keeps it a value.

### C. A script edited between runs

1. `package.sh` is granted at digest `d1`. During a later run of `site_release`, the
   agent step edits `tools/package.sh` in the run worktree. The worktree is
   writable, and the agent may have been asked to fix the build.
2. The `package` step snapshots the worktree's copy, computes `d2 ≠ d1`, and refuses
   with `grant_stale`. The refusal is non-retryable and records both digests on the
   run. Nothing ran.
3. The same holds if the edit lands through a PR and a nightly routine meets it on the
   primary checkout, or if a script with a writable root wrote to its own file in the
   last run. Every later run refuses until an operator acts.
4. `orbit doctor` and the dashboard show `package: stale` with the changing commit.
   `orbit script show package` prints the diff from the witness copy of `d1` to the
   current bytes.
5. The operator reviews the diff and runs `orbit script grant package`, which writes a
   new grant at `d2` and supersedes the old one. Or the operator reverts the change.
   An agent can do neither, because the grant tool is operator-only and the witness is
   unwritable. It can only raise a `script_grant` request (phase 4) for a human to
   answer.

A script that rewrites itself during a run changes nothing for that run (§2,
"No time-of-check gap"); the next run refuses.

## Non-goals

- **Inline shell text in script steps.** No `command:`, no `sh -c`, no expression
  evaluated as a command in the manifest or the `script:` target (§1). `local_shell` keeps
  its inline form; see [Relation to `local_shell`](#relation-to-local_shell).
- **Unsandboxed scripts.** There is no `unsandboxed` grant, and a host without the
  profile refuses (§3, §7).
- **Scripts calling Orbit.** A script has no `orbit_tools`, no callbacks and no task
  writes. Later steps write (§6).
- **Agent-chosen scripts.** An agent can't name a script to run, because script steps
  live in job definitions, and it can't grant one.
- **Replacing plugins.** Reusable tools still ship as plugins (§6).
- **Routine `script:` targets in v1.** Wrap the script in a job (§1, §6).
- **Cross-host grants.** A grant is consent on one host (§7).

## Concerns and honest limitations

- **Interpreter bytes aren't digested.** A `bash` or `python3` upgrade doesn't void a
  grant; only a changed resolved path does. Digesting interpreters would void every
  grant on each package update, and the host's own packages are the operator's trust
  base anyway.
- **Imported code isn't digested.** A Python script that imports a sibling module in
  the repository, or a shell script that `source`s another file, runs code the digest
  doesn't cover. The read-only mount means the script can read the whole cwd tree.
  Mitigation for v1: `orbit script grant` warns on a `source` or relative import it can
  detect, and the doc on writing scripts says to keep each script in a single file. A
  `digest_includes:` list is open question 4.
- **The cwd tree is readable.** A script can read every file of the worktree or
  workspace root. That is narrower than the agent profile but wider than "only its
  inputs". A `cwd: none` mode is open question 5.
- **macOS is weaker.** It has no memory or task limits, timeout descendants can escape
  through `setsid`, it has no network modes, and TCC adds a second permission system
  (§3, §9.5).
- **Secrets in env are visible to the whole tree.** Any process the script starts can
  read the passed variables. Redaction covers Orbit's logs, not what the script does
  with them.
- **Grants are per host.** Two owner hosts running the same job need two grants. That
  is deliberate (§7), but it is extra work.
- **Re-grant fatigue.** A script edited often needs frequent re-grants. The diff view
  is the only mitigation; an operator who re-grants without reading has spent the
  protection.
- **`local_shell` stays the weaker route.** A workspace that uses it runs its program bare
  unless an operator adds a sandbox, and a routine can reach that program through a job.
  This design makes the script route stronger and leaves `local_shell` as it is (open
  question 8).
- **The doc's size.** If phase 2 grows specs (the profile's mount table, the grant
  witness format), the feature graduates to the full track (CONVENTIONS.md §1).

## Phasing

1. **Catalog and grants, nothing executing.** Landed in [ORB-19983]. This phase adds:
   - the `Script` manifest, its loader and validation (`entry` resolution, argument
     templates, interpreter forms);
   - `target: script:` parsing, with a step that resolves but refuses at dispatch
     (`script_steps_unavailable`, since replaced by phase 2 execution);
   - the `script_grant` table, the witness, `orbit script grant`, `revoke`, `list` and
     `show`, and the governed rows;
   - doctor findings.

   Operators can grant and see staleness before anything runs.
2. **Linux execution.** Landed in [ORB-19985]. This phase adds:
   - the `linux-bwrap-script-v1` profile in `orbit-exec`;
   - a new `RuntimeHost` capability that Core implements for the grant check, snapshot
     and spawn;
   - stdin and stdout, the log, `Ok(success = false)` with its restored audit-message
     coverage, per-step scopes, `script.execution` audit, `orbit run show` and
     `script history`.

   It covers use cases A (without network) and C, and lands the decision record that
   supersedes ORB-00374's removal.
3. **Files and macOS.** Not yet filed. This phase adds `files.inputs` and `files.outputs` with
   `steps.<id>.files`, a deterministic action that attaches those files to a task as
   artifacts, the `macos-sandbox-script-v1` profile, `RLIMIT_*` limits, the
   root probes and TCC reporting, and nested-sandbox refusal. It covers use case B, and
   orbit-office items 4 and 5.
4. **Grant requests.** Not yet filed. This phase adds the `script_grant` decision request, answers
   from registered channels (orbit-office item 1), and the dashboard Scripts view with
   its grant action. `office.script-run` can then become a thin wrapper.
5. **Network.** Not yet filed. `https` and `any` on Linux, with `env_pass` and the redaction of passed
   values. It covers the off-site part of use case A.
6. **Later.** Each of these is filed only when needed: a grant-time `prepare` (fetch)
   phase, plugin-store roots, routine `script:` sugar, and macOS network once
   [ORB-19251] contains descendants.

## Open questions

1. **Routine `target: script:`.** Should a routine name a script directly, as sugar for
   a generated one-step job? Leaning: no. The catalog-only decision is cheap to keep,
   and a one-step job is a few lines.
2. **Plugin-store and artifact roots.** How does a plugin contribute a manifest whose
   entry lives in `{{plugin_state}}`, and does Orbit trust a plugin-reported path or
   resolve it from the plugin's install record? Artifact references would also need an
   immutable artifact identity, which task artifacts don't have today.
3. **A grant-time fetch.** Should grants run a networked `prepare` phase under the build
   profile's fetch mode, with the result cache digested into the grant? It is Linux
   only, for the same reason as §3, and macOS is where the office needs it.
4. **Covering imports.** Should the manifest list extra files (`digest_includes:`) that
   are bound into the digest and mounted alone, instead of the whole cwd tree?
5. **`cwd: none`.** Should a script that needs only its inputs get a mode where the
   cwd is the scratch directory and no workspace tree is mounted? It would be the
   tightest profile, and a fit for the office.
6. **Hosts without Bubblewrap.** Is there ever a case for a narrower, Landlock-only
   profile on Linux hosts that refuse user namespaces, or does refusing stay the
   answer?
7. **Grant expiry.** Should grants expire by default, as condition-trigger operator
   grants do (30 days)? Leaning: no for script grants, since the digest already bounds
   them, but yes when the script has network.
8. **`local_shell` posture.** Should the shipped `local-shell` definition declare a
   sandbox, should its nonzero-exit mapping change, and should a routine's job be refused
   a `local_shell` step? Each changes a shipped action or a routine's reach, so each needs
   its own decision record. This doc decides none of them.

## Acceptance (feature level)

- A job step `target: script:<name>` whose manifest is missing, whose `entry` escapes the
  workspace, or which uses inline-code interpreter flags fails to load, and the error
  names the file. `type: shell` and routine `target: script:` stay load errors.
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

## Task References

- [ORB-19294] — designs script steps (this doc).
- [ORB-19983] — phase 1: added script manifests, `target: script:` parsing, the grant store and `orbit script grant|revoke|list|show`; nothing executes.
- [ORB-19985] — phase 2: Linux execution under `linux-bwrap-script-v1`, `script.execution` audit and `orbit script history`.
- [ORB-00374] — removed the v2 `shell` activity instead of sandboxing it; this doc takes its retrofit alternative.
- [ORB-00363] — found that the shell allowlist came from the YAML it guarded.
- [ORB-11294] — restored `local_shell` as a deterministic action; the Bearing and
  [Relation to `local_shell`](#relation-to-local_shell) inventory it.
- [ORB-10021] — made routine targets catalog references only.
- [ORB-12903] — added worker containment scopes.
- [ORB-19251] — macOS worker containment by tracked process tree.
- [ORB-19224] — typed the nested Seatbelt refusal in a macOS worker.
- [ORB-15732] — recorded the 30-day, release-gated compatibility window.
- [ORB-19206] — orbit-office orchestration that relayed the five script-runner requirements in §9.
- [ORB-19288] — condition triggers, whose routine actions can run jobs with script steps.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
