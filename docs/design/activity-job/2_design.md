---
summary: "Activity / Job — Design"
type: design
title: "Activity / Job — Design"
owner: codex
last_updated: 2026-09-08
last_validated: 2026-09-08
status: Draft
feature: activity-job
doc_role: design
tags: ["activity-job"]
---

# Activity / Job — Design

This document describes the shipped Activity / Job substrate across `orbit-types`, `orbit-engine`, `orbit-core`, and `orbit-cli`: asset shape, normalization, dispatch boundaries, DAG execution, audit, and retained legacy edges. See [1_overview.md](./1_overview.md) for purpose and [3_vision.md](./3_vision.md) for open questions.

---

## 1. Asset Shape and Two-Pass Loading

Activity / Job assets are `schemaVersion: 2` YAML envelopes with:

- `kind: Activity` or `kind: Job`
- `metadata.name`
- typed `spec`

The loader in `crates/orbit-engine/src/activity_job/asset_loader.rs` reads the schema header first, then parses the full envelope into `ActivityV2` or `JobV2`; that shape arrived in [T20260418-2010]. `schemaVersion: 1` is retired after [T20260419-2156], and `kind` mismatches are structural errors, so an activity cannot dispatch as a job or vice versa.

---

## 2. Activity Surface

`ActivityV2` carries shared fields:

- `description`
- `input_schema_json`
- `output_schema_json`
- optional `fsProfile`

and then flattens one `ActivityV2Spec` variant:

- `AgentLoop(AgentLoopSpec)`
- `Deterministic(DeterministicSpec)`

The common `agent_loop` fields are:

- `instruction`
- `tools`
- `on_denial`
- optional `model`
- `max_iterations` (inert; see §5)
- `provider`
- `wall_clock_timeout_seconds`
- `require_response_envelope` (default `false`; opt in only when downstream
  templates consume structured response fields)
- `require_completion_envelope` (default `true`; opt **out** only for an
  activity whose non-completion is harmless — see §7.6a)

A former `Groundhog(GroundhogSpec)` variant (a sibling activity kind from [T20260420-0510-2]) was removed as unused in [ORB-10332]; activity specs are now only `agent_loop` and `deterministic`.

`DeterministicSpec` is just `{ action, config }`. A fourth variant, `Shell(ShellSpec)`, was removed in [ORB-00374] as a fail-closed security fix (see [The v2 shell activity surface is removed, not sandboxed](./4_decisions.md#the-v2-shell-activity-surface-is-removed-not-sandboxed)); `type: shell` now fails to deserialize at load rather than spawning an unsandboxed subprocess.

---

## 3. Job Surface

`JobV2` carries:

- `state`
- optional `default_input`
- `max_active_runs`
- `kind`
- `steps`

`JobKind` is currently `workflow` or `subroutine`, added in [T20260419-0339]. The more interesting surface is the step grammar from [T20260418-2018]:

- every step has `id`
- every step may add `when`
- every step may add `retry`
- every step chooses exactly one body

The body is one of:

- flat `TargetStep`
- named `TargetRef`
- `parallel`
- `fan_out` plus matching `fan_in`
- `loop`

`TargetStep` is the executor-facing form. It inlines an `ActivityV2Spec` plus optional `fsProfile`, `default_input`, `timeout_seconds`, and optional `session`. `TargetRef` is the authoring-facing form: `target: activity:<name>`. It is resolved away before execution.

Step-local input layering landed earlier, but the shipped job-level `default_input` behavior changed in [T20260423-0445]:

- if the caller passes `null`, the run input becomes `job.default_input`
- if both the caller input and `job.default_input` are JSON objects, Orbit performs a shallow merge and caller-supplied keys win on conflict
- if the caller input is any non-object JSON value, it replaces `job.default_input` entirely

Step-level `default_input` is still recursively template-rendered before dispatch. Support landed in [T20260413-0141], entered the v2 DAG path in [T20260418-2018], and was corrected for job-level merges in [T20260423-0445].

---

## 4. Load-Time Normalization Pipeline

orbit-core normalizes raw YAML before dispatch.

Job catalog listing uses `MergeByKey` precedence after [T20260425-0204]: `ORBIT_JOB_DIR` / `ORBIT_V2_JOB_DIR` entries first, then workspace jobs, then global seeded jobs. The first valid `metadata.name` wins, so listing can show a workspace `task_auto_pipeline` in place of the global default without making the catalog ambiguous. Duplicate names inside one directory tree remain invalid because that single layer would otherwise be ambiguous.

Job execution deliberately has a different order: explicit `ORBIT_JOB_DIR` / `ORBIT_V2_JOB_DIR` entries, then global seeded jobs, then workspace jobs only for names that are not shipped defaults. Thus an explicit environment catalog can opt in to a replacement for testing or smoke runs, but a workspace-local file cannot shadow a shipped job when Orbit resolves that job by name for execution.

Activity resolution is likewise execution-oriented, but its directory order is explicit `ORBIT_ACTIVITY_DIR` / `ORBIT_V2_CATALOG_DIR`, then global seeded activities, then workspace activities. Explicit and global directories use first-wins loading. Workspace activities may add names that are absent from the catalog, but any workspace name matching a shipped default is skipped even if the global file is missing; a workspace activity also cannot replace an earlier explicit or global entry. Duplicate names inside one activity directory tree remain invalid.

This is an intentional default-shadowing asymmetry: job *listing* is workspace-preferred, while named job execution and activity resolution keep shipped defaults authoritative over workspace resources. The split follows [L-0060] and its originating security fix [ORB-00356]: display/catalog override semantics must not make checked-in workspace YAML executable in place of a trusted shipped default. That learning is the rationale; the runtime and job catalog code are authoritative for the exact loading behavior.

### 4.1 Managed materialization and retirement

Global activity and job defaults materialized by `orbit init` are managed
resources, not merely additive examples. After [ORB-10684] / [Track bundled activity and job ownership by content digest before retirement](./4_decisions.md#track-bundled-activity-and-job-ownership-by-content-digest-before-retirement), each
resource directory carries a hidden `.orbit-managed-assets.json` manifest. Its
entry for a bundled file is the SHA-256 digest of the content Orbit last wrote;
the manifest is therefore the ownership authority for later retirement, while
the YAML filename alone never authorizes deletion.

Refresh applies the same reconciliation to activities and jobs:

- a previously managed name absent from the current embedded list is deleted
  when its bytes still match the recorded digest;
- a locally modified retired managed file is moved to
  `resources/.retired-managed/{activities,jobs}/`, preserving its content while
  keeping recursively loaded catalogs from activating it;
- an installation without a manifest adopts only exact current bundled bytes
  as managed; other YAML remains in place and `orbit init` warns with the paths
  plus the manual remedy to move or delete a stale legacy file;
- the new manifest contains only current assets whose managed ownership is
  established, so repeating refresh is idempotent;
- a needed manifest write that hits EROFS/EACCES (immutable or shared global
  root, sandboxed runner) records a warning and continues. Read-only commands
  such as `orbit task show` must not fail closed on that maintenance write.
  Malformed manifests, unexpected `asset_kind`, and non-permission write
  failures (ENOSPC, EIO) stay rejected. A writable directory whose embedded
  digest changed still refreshes the asset and writes the manifest
  ([ORB-10840]).

This means a manifest-aware upgrade cannot leave a retired managed activity
with removed tool grants—or a retired managed job with removed actions—in the
catalog. Legacy files whose history cannot be proven remain operator-owned:
Orbit will not guess from their names, and the warning is the recovery contract
for installations predating managed provenance.

#### 4.1.1 All five artifact kinds, and the standing doctor surface

After [ORB-10800] / [All five definition-artifact kinds carry managed provenance, and doctor reports it](./4_decisions.md#all-five-definition-artifact-kinds-carry-managed-provenance-and-doctor-reports-it), the same mechanism governs all five definition
artifact kinds. Skills, auto-tasks, and routines previously used an additive
seeding path that skipped existing files and retired nothing, so a default
dropped from a release stayed on disk — and dispatchable — in every existing
workspace.

Two generalizations made that possible. `reconcile_managed_assets` takes a
layout: manifest keys are `<name>.yaml` stems for the four flat catalogs, and
*relative paths* for skill directory trees, so `orbit-task/references/review.md`
is managed independently of `orbit-task/SKILL.md`. And the recorded digest is
always taken over the **rendered** document, which is what gives honest
provenance to the two kinds that substitute placeholders before writing —
routines (host id, workspace-scoped name) and skills (`ORBIT_ROOT_TOKEN`).

`orbit doctor` reports one row per kind, classifying each artifact as *faulty*
(fails to load), *deprecated* (digest proves Orbit wrote a default no longer
shipped), or *stale* (an Orbit-written copy of an older release, or an untracked
file occupying a bundled default's name). Classification reads the manifest
only, never loader precedence — precisely because precedence is *not* uniform
across kinds (§4 above): skills merge workspace-over-global while activities
keep shipped defaults authoritative. Provenance of the file Orbit wrote is the
same question for every kind.

Activity *fault* detection is the exception: it walks every production catalog
directory (env override, global managed tree, workspace `.orbit/resources/activities/`)
and applies the same `load_activity_asset` + tool-allowlist validation catalog
construction uses, so a workspace file that fails `orbit activity list` or job
dispatch cannot be reported healthy. Retired `spec.backend: http|auto` findings
name `orbit doctor --fix-retired-activity-backends`, which deletes only that
key from schemaVersion 2 agent-loop activities.

`orbit doctor --fix-stale-artifacts` retires only deprecated artifacts whose
digest still proves Orbit wrote them, preserving locally modified ones under
`.retired-managed/` rather than deleting them. Faulty and user-authored files
are reported and never touched by that flag. A workspace-authored fault is a
`Warning`; only an unloadable *shipped default* is an `Error`, which keeps the
`orbit doctor` exit code stable for existing cron and CI callers.

Direct single-activity runtime helpers:

1. Read YAML from disk.
2. Parse via `load_activity_asset(...)`.
3. Build audit sinks and run id with `system` as the v2 envelope `agent_identity`.
5. Dispatch the concrete `ActivityV2Spec`.

Job runs:

1. Read YAML from disk.
2. Parse via `load_job_asset(...)`.
3. Build the activity catalog from seeded/workspace activity directories.
4. Resolve every `target: activity:<name>` into a concrete `TargetStep` and resolve any job-level or step-level `recovery_activity` into a cached activity spec.
5. Reject retired declarations: any `session:` binding on a step.
6. Build audit sinks and run id with `system` as the v2 envelope `agent_identity`.
7. Execute the normalized `JobV2`.

The target-ref pass was added in [T20260418-2019] and `run-v2` entrypoints in [T20260418-2143]. Backend selection and the HTTP agent loop were retired in [ORB-10801].

The public CLI now executes activity assets through jobs rather than exposing a standalone `orbit activity run` subcommand. `orbit activity` is an inspection/catalog surface; `orbit job run` and workflow aliases under `orbit run` are the public execution surfaces after [T20260426-0047].

Some module comments still describe older phase ordering; the authoritative behavior is the orbit-core call path in `crates/orbit-core/src/application/job/exec.rs`.

Seeded direct shipment workflows (`task_local_pipeline` and `task_pr_pipeline`) opt into recovery activities on specific steps. Most use `step_failure_recovery`; after [ORB-11281], the PR pipeline's `sync_base` checkpoint instead uses `pr_conflict_recovery`, and [ORB-11488] extends that same typed leaf to `complete_pr`. The completion path acts only when GitHub reports `DIRTY` under retained `completion: done` authority. It verifies that the task branch still equals the published SHA, fetches and pins the PR base, and invokes the existing `git_rebase` boundary. Only actual unmerged index entries produce `RecoverableVcsConflict`; the executor retries `complete_pr` once after recovery, recognizes the recovered pinned rewrite through the original published SHA, lease-pushes the same branch, and continues polling the same PR. A second base advance, changed remote task branch, exhausted recovery episode, missing completion authority, fetch/push/auth failure, protection or review/check refusal, or any other VCS diagnostic remains untyped and cannot invoke the agent. The typed recovery envelope carries the original and target base SHAs plus conflicting paths, bypasses ordinary retry, and permits exactly one system-crew invocation followed by exactly one executor-owned retry of the same deterministic step. Activity routing has one mechanism after [ORB-10622]: a rendered `crew` input selects that crew, and an activity without one uses the run's resolved crew. [ORB-11242] adds one gate on top of that selection rather than a second mechanism: a run whose input carries `allowed_crews` refuses to dispatch any activity whose *resolved* crew is outside it. Both recovery leaves opt into `workflow.system_crew`; the executor renders that configured name into their activity input before resolution. Higher-level orchestration workflows do not enable recovery because replaying child dispatch or planning orchestration is not a safe leaf action.

[ORB-11982] narrows what that completion step is allowed to call delivered. The same `head`, `base`, and `published_head_sha` checkpoints that authorize conflict repair also pin the pull request the run may complete on, and the pin follows the one rewrite a bounded repair produces. Completion refuses a pull request whose head branch or base branch no longer matches those pins, before any merge is requested; refuses a merged state whose head is not the pinned candidate or that names no merge commit; and refuses a provider answer that reports a merge timestamp while still calling the pull request open or closed, which is the F2026-09-102 shape where a task reached `done` with its PR open. The evidence that did permit the transition — pull request, branches, merged head, and landing commit — is written to the activity output and named in the durable authorization note, so task history distinguishes an evidence-backed automatic completion from any other writer of the same transition. A protection or required-check refusal additionally reports whether the published candidate is behind the current base, which separates a red merge ref that no longer describes the proposal from a genuine failure at the current candidate and base.

Conflict recovery projects the failed target's rendered task IDs and assigned
worktree into its own input. `repo_root` defaults to that same assigned path,
and `failed_step_input` retains the candidate branch, pre-rewrite HEAD, and
other phase checkpoints. The run ID comes from the executor. This lets normal
task-context loading, worktree-pair validation, and sandbox preparation use the
same assignment as the failed step; no primary-checkout fallback is needed.
System-crew selection still enforces the original run's crew allowlist.
The dedicated conflict-recovery leaf edits conflict files only. Its managed
sandbox deliberately retains the read-only linked-worktree Git metadata
boundary: the leaf does not stage, abort, continue, or restart a rebase. After
a successful terminal invocation, the host-side worktree boundary rechecks the
live run and task owner, retry-lineage worktree checkpoint, prepared branch,
original HEAD and merge base, pinned target ref and SHA, stopped-rebase
metadata, and exact unmerged path set. It rejects incomplete or out-of-scope
file edits, stages only that authenticated set, and runs one `rebase
--continue`. A later conflict or changed/cancelled owner remains a failed
recovery; the terminal handoff keeps the original error and preserved
candidate/PR evidence. The boundary finally requires the target as an ancestor
and a remaining candidate commit. Ordinary providers still cannot move HEAD;
primary-checkout drift checks apply to recovery too.

The stopped index also contains the candidate's nonconflicting changes. Host
continuation commits those staged paths normally; their disappearance from the
dirty-path map is not unrelated editing. After continuation, tracked state must
be clean and pre-existing untracked contents must be unchanged. Only after all
boundary checks succeed does the host transactionally persist the exact recovered
HEAD, original HEAD/base, pinned target, branch, task IDs, workspace, run/step and
pre-rewrite remote lease in `PipelineState.rebase_recovery_checkpoints`. This is
recovery provenance, not a successful workflow step. Missing run storage or a
checkpoint write failure prevents recovery success.

The same failed step still executes once after recovery. Reusing a rewritten
HEAD requires an exact host checkpoint; a fresh-looking manual replacement is
insufficient. Once recovery succeeds, a subsequent step error describes the new
failure rather than reviving resolved conflicts. Failure handoff derives conflict
paths from the live stopped rebase, retains the immutable worktree base in its
preservation evidence, and does not infer unresolved paths from an old error after
Git has completed. A resumed run authenticates copied recovery evidence against
the source run and retry lineage before refreshing preparation for that exact
preserved candidate. Recovery checkpoints survive ordinary step checkpoints and
process restart, including interruption before the recovered step completes.
If execution stops after Git changes HEAD but before the host persists verified
completion, there is no authenticated completed-head record: resume fails closed
and requires inspection. It never reconstructs success from the agent's result.

#### Recovery authority is host-only

`PipelineState.rebase_recovery_checkpoints` lives in the run row of
`<global>/orbit.db`, and a managed leaf holds modify grants on that database and
its WAL/SHM sidecars so `orbit.task.*` and audit tools keep working inside the
sandbox. Those grants also cover the recovery rows, so the rows are progress
data, not authority: a leaf can write bytes that look exactly like a host
checkpoint, and authenticating them against the same store proves nothing.

After [F2026-09-063], the record a resume trusts is a separate certificate the
host writes to `<global>/state/recovery-authority/authority.db`. That root
appears in no leaf write grant, and sandbox resolution appends an explicit
subtree deny for it after every convenience grant, alongside the Git-metadata
denies. The subtree form covers the database together with its `-wal` and `-shm`
sidecars, and Bubblewrap binds the root read-only, so it is unwritable through
its own path, unreachable through a symlink alias a leaf builds in its worktree,
and unrenameable as a mountpoint. Removing the `orbit.db` grants was rejected as
the fix: it would break admitted leaf writes without making the remaining
evidence authentic.

The trusted root is established before any filesystem effect. The configured
global root must be absolute and traversal-free and must already exist; it is
resolved once, without writing, and the canonical result anchors every later
join. Aliasing in the configured root itself — a symlinked `$HOME`, a macOS
`/var` prefix — is a supported operator layout, and anyone able to redirect that
root already owns the run store the authority exists to outrank. Below the
trusted root the tree is created one component at a time, so a symlink standing
in for `state` or `recovery-authority` is refused instead of followed, and the
database and its sidecars are refused when any of them is a link.

Confinement here is by location, not by a secret. Bubblewrap mounts the host
filesystem `--ro-bind / /` and enforces only write boundaries, so a key file
would be readable by every leaf and a keyed MAC would add no authority.

Each certificate binds the run ID, step ID, canonical workspace path, recovered
`head_sha`, pinned `base_sha`, and a BLAKE3 digest over the whole checkpoint
payload, so an edit to any field — including `task_ids` and `rewritten`, which
the bound columns do not name — no longer matches. Certificates are immutable
per `(run_id, step_id)`: re-issuing identical evidence is a harmless retry, and
different evidence for an already certified step is refused rather than
replacing the accepted record. `recovered_head_checkpoint` checks the
certificate before it consults the source run, so a payload relabelled into
another run or step, or replayed after a host restart, fails at the first gate;
lineage and source-equality checks continue to apply behind it.

**Migration.** Checkpoints written before this boundary have no certificate and
are never backfilled — a row in a store the leaf can write is not evidence that
the host wrote it, so blessing old rows would import exactly the forgery the
boundary removes. `recovered_head_checkpoint` refuses an otherwise matching but
uncertified checkpoint with a typed error naming the run and step, rather than
reporting it absent. The operational consequence is bounded: an in-flight run
resumed across the upgrade redoes its rebase from the preserved candidate
instead of inheriting the rewritten HEAD, and a resume that has no other
authority (no preservation commit and no successful workflow commit) fails
closed for inspection. No durable data is discarded; the stale rows remain
readable and inert.

`step.recovery_attempted` retains a bounded, redacted `error_message` and
`failure_phase` when authorization, input preparation, crew resolution, dispatch,
or the recovery activity fails. These optional fields are absent from older
events and successful attempts. Failed attempts supplement the original step
conflict. Successful recovery supersedes that conflict; any failure of the
post-recovery step becomes the returned error and terminal handoff evidence. An
admitted attempt settles its recovery budget even when preparation fails before
launch.

After [ORB-10382], a recovery activity's structured result is **advisory only**, and its `output_schema_json` declares no `required` fields to keep it that way. A `recovery_activity` is a step attribute rather than a step, so it has no step id and no `{{ steps.<id>.output.* }}` template can consume it; `attempt_recovery_activity` gates the executor's single post-recovery attempt on dispatch success, never on a returned `recovered` field. `pr_conflict_recovery` no longer advertises such a field at all. Its host-side continuation is authorized by the executor-selected activity and authenticated checkpoints, not response JSON. Final success is established when the deterministic `git_rebase` retry verifies a clean index, no stopped rebase, pinned target-base ancestry, and the expected branch rewrite; later push/open/promote/complete checkpoints remain the normal authorities.

After [ORB-10499], that post-recovery attempt is identified as the source of the "duplicate implement invocation" reported in [F2026-07-174], and an implement invocation can now cancel itself once its task stops accepting writes. The audit trail of the reported run settles the dispatch question the friction could not: the two `implement_one` invocations were serial and deliberate, not concurrent. Attempt #1 exited 0 with `timed_out: false` and was classified `error`, `step_failure_recovery` reported success, `step.recovery_attempted` fired, and attempt #2 ran 848s to completion — after which `commit` reported `skipped_no_diff_expected`. So there is no double-dispatch bug and no retry-after-perceived-timeout policy, only the executor's bounded post-recovery attempt working as designed. Why attempt #2's work was unpersistable is a separate fact the friction conflated with the first: the task's own history shows `status_changed` and then `review_approved` landing *inside* attempt #2's window, attributable to no step of the run — `promote_no_diff` did not run until the end — and recorded against actor `unknown`, so which actor promoted it is not recoverable from the evidence and is deliberately not asserted. What both halves share is one assumption: that an implement invocation is the only actor on its task for the duration of its run. The executor assumes a step classified as failed leaves its task untouched, and the agent assumes its dispatch-time envelope stays valid until its final write. The fix keeps the re-dispatch — most failed attempts do leave the task unfinished — and instead makes the invocation able to see its situation. `agent_task_context_json` injects `status` and `terminal` into the `task` envelope (`terminal` mirroring the `update_task` write gate, where `Done` refuses every non-comment mutation and `Archived` refuses everything but a bare restore), and `agent_implement`'s instruction opens with a terminal-task precheck that stops before resolving context files and returns `success` with `skipped_reason: "task_terminal"`. Because the reported task went terminal mid-run, the dispatch-time snapshot alone is not sufficient: the contract also requires re-reading status through `orbit.task.show` at each checkpoint where the remaining work is still expensive, and treats a terminal-status write rejection as a stop rather than something to retry around. The guard is advisory in the sense of [L-0115] — the executor still gates on durable state and still pays subprocess startup — and an engine-side claim/lock refusing the re-dispatch outright was rejected both as hard-coding a task-lifecycle judgment into the generic step executor and as ineffective here, since the task was still `in-progress` when attempt #2 was dispatched.

After [ORB-10306], retry classification and recovery eligibility are separate decisions. `WorktreeIntegrity` still bypasses ordinary retry and backoff, but an explicitly configured step- or job-level recovery activity receives its complete rendered diagnostic and may run exactly once before the executor's single post-recovery attempt. Without configured recovery—or when recovery or that post-recovery attempt fails—the original integrity error is preserved and the executor performs no automatic checkout reconciliation. Other non-retryable error classes remain ineligible for recovery.

After [ORB-10232], `task_pr_pipeline` exposes the PR handoff as ordered durable checkpoints: `commit` (`git_commit`), `prepare_branch` (`pr_prepare`), `sync_base` (`git_rebase`), `push` (`git_push`), `pr_open`, and `promote_tasks` (`pr_promote`). The no-diff-expected path skips the remote phases and uses its own promotion checkpoint. Each performed activity returns a `phase` and `decision`, so persisted step output distinguishes work performed on the first attempt from a current/skipped phase or a recovered/reused phase; job recovery audit records the recovery decision around the one retried step. `pr_open` no longer commits, rebases, pushes, or mutates task lifecycle state.

After [ORB-10380] / [Pipeline steps consume a base commit pinned at worktree setup, never a moving ref name](./4_decisions.md#pipeline-steps-consume-a-base-commit-pinned-at-worktree-setup-never-a-moving-ref-name), the base a run was created at is a pinned commit, not a name. `worktree_setup` resolves its start point once, creates the worktree at that commit, and emits `base_sha` alongside `base_ref`; both task pipelines pass `base_sha` into `commit`. After [ORB-10519] / [Workflow alone creates shipment commits while dirty failures remain recoverable](./4_decisions.md#workflow-alone-creates-shipment-commits-while-dirty-failures-remain-recoverable), `commit` rejects a ref name in `input.base_sha` and compares the resolved immutable commit directly with HEAD without traversing provider-created history. A mismatch is a typed `worktree_head_changed` failure; the provider boundary should already have rejected it. [No-diff-expected tasks bypass repository change gates](../auto-tasks/4_decisions.md#no-diff-expected-tasks-bypass-repository-change-gates)'s `no-diff-expected` carve-out remains reachable before changed-HEAD and empty-stage failures, and no failure path stages or resets another checkout. `base_ref` still flows as the moving name to `sync_base` and `pr_open`, which remain the reconciliation with a base that moved.

After [ORB-11456] / [Resumed shipment accepts only its durable Orbit preservation commit](./4_decisions.md#resumed-shipment-accepts-only-its-durable-orbit-preservation-commit), a successful terminal `pr_failure_handoff` is checkpointed separately from successful steps. Its output records the task, handoff run, reused worktree owner, original base, exact preserved HEAD, and whether the hook itself created that commit. Before a resumed implementation runs, the executor compares the checkout with the unchanged worktree `base_sha`. A moved HEAD is accepted only when the task still belongs to the checkpoint owner, the active run descends from the handoff run, the source run's immutable state contains the exact same evidence, and HEAD equals either the hook-created preservation commit or the source run's successful workflow commit recorded at that same handoff HEAD. `git_commit` repeats those checks against the active and source run states before staging. Missing evidence, an unrelated retry, changed task ownership, or any later commit fails without changing HEAD, the index, or worktree files. The original base remains the delivery base and the preservation commit remains in history, so resumed edits become one new workflow-authored commit on top of it and `pr_open` reuses the failure-handoff PR.

After [ORB-11493], a resumed `task_pr_pipeline` may replace one stale
`prepare_branch` checkpoint when its authenticated terminal handoff says
`sync_base` failed. The executor first proves the exact candidate HEAD, branch,
task owner, PR number, immutable source checkpoint, and retry lineage. It then
reruns only `prepare_branch`; implementation and commit remain successful
checkpoints and are not replayed. The descendant run's new preparation output
records `resume_refresh` with the handoff run, previous base SHA, attempt, and
limit, while the source run retains its original checkpoint unchanged. The
ordinary `git_rebase` boundary consumes the fresh pin, so a real conflict still
reaches the existing one-shot `pr_conflict_recovery` path and later steps reuse
the same branch and PR. One preparation refresh is allowed. A later
`sync_base` failure carrying an already-refreshed checkpoint stops with
`resume_checkpoint_refresh_exhausted`; changed candidate or PR identity,
changed/live ownership, unrelated lineage, inconsistent downstream success,
and absent evidence stop earlier. Cancellation admission and the explicit
completion authorization remain owned by their existing Core and `pr_complete`
gates; checkpoint refresh cannot grant either.

The provider boundary now admits only worktree-file changes from an implementation provider. Any assigned HEAD or branch movement is a typed `worktree_content_conflict` regardless of commit count, message trailers, or changed paths; Orbit does not try to prove that provider history belongs to the task. `git_commit` stages the task worktree diff, creates exactly one workflow-owned commit, and returns that SHA. It never enumerates or adopts commits above the pinned base, parses `Agent-*` trailers, or validates a commit against `context_files`.

Host Git children always carry a finite `ExecRequest.timeout_ms`. Light commands default to 30s; `fetch` 60s; `worktree add` and `rebase` 120s. Activity input may overlay those values with `git_timeout_ms` or `git_timeouts` (`default`, `fetch`, `worktree_add`, `rebase`); 0, non-integers, unknown keys, and values above 600s are rejected, and the request never uses an unbounded deadline. Hook disabling (`core.hooksPath=/dev/null`, `gc.auto=0`, `--no-verify` on commit/push) and the cleared VCS environment stay in force.

Timeout recovery is not conflict recovery and not failure-handoff recovery. A `worktree add` that times out after this attempt registered the path is removed only when the checkout is owned and nothing unique is retained; otherwise the leftover is refused with path/HEAD/branch evidence and is never admitted incomplete. Deleted tracked files, dirty files, and extra commits are not completeness failures and are not restored or deleted as a shortcut. A rebase this attempt started is aborted on timeout when provenance matches (`orig-head` / `onto` / `head-name`); a pre-existing or foreign rebase is left intact and refused diagnostically. Unmerged paths still produce `RecoverableVcsConflict` for `pr_conflict_recovery`. `pr_failure_handoff` still preserves the candidate.

After [ORB-11639] / [Worktree setup refuses unexplained stale branch reuse before publishing checkpoints](./4_decisions.md#worktree-setup-refuses-unexplained-stale-branch-reuse-before-publishing-checkpoints), `worktree_setup` validates an existing branch or registered checkout against the freshly resolved `base_sha` before admission. A successful setup still means HEAD equals `base_sha`; `base_ref` remains the moving start-point name. An unexplained mismatch fails closed with branch, tip, and requested base, leaves retained commits and dirty files untouched, and does not relabel old candidate history as a new base. Retrying setup without recovering the leftover refuses again. Authorized moved-head (epic `allow_moved_head`) and validated failure-handoff resume keep using the original setup checkpoint at commit/resume time; they are not setup-time branch-reuse escapes. Recovery is operator-led: inspect the leftover, then move the branch aside or delete it only after confirming no retained candidate is needed.

`pr_prepare` is the pre-rewrite authority boundary. It records the exact head SHA, base SHA, and observed remote task-branch SHA before `git_rebase` may rewrite history. `git_push` classifies the remote ref as missing, current, fast-forwardable, remote-ahead, or diverged. Missing and fast-forwardable refs use normal push; current refs are reused; remote-ahead refs fail closed. Divergence may use force-with-lease only when the persisted preparation SHA still exactly matches the observed remote SHA and `git_rebase` reports a performed or recovery-reused rewrite. The engine-private VCS operation emits a branch-scoped `--force-with-lease=refs/heads/<branch>:<expected-sha>`, so a concurrent remote update rejects the push instead of overwriting it. This is [PR handoff recovery follows job checkpoints and exact remote leases](./4_decisions.md#pr-handoff-recovery-follows-job-checkpoints-and-exact-remote-leases).

After [ORB-10363], `JobV2.failure_activity` is a terminal, best-effort hook distinct from retry recovery. It receives the merged job input, all completed pipeline checkpoints, the failing step/action, and the structured error; it runs once and never replaces the original failure. `task_pr_pipeline` binds this hook to `pr_failure_handoff` ([Terminal PR shipment uses a job-level failure handoff](./4_decisions.md#terminal-pr-shipment-uses-a-job-level-failure-handoff)). When [ORB-11281]'s bounded conflict repair fails or its deterministic retry still conflicts, this action aborts the stopped rebase back to the prepared branch, commits any remaining candidate, performs non-overwriting push classification, and opens or reuses the same blocked PR. Its body retains original/target SHAs and conflicting paths, and the task stays `blocked` with `pr_conflict_blocked`; it never restarts implementation or loops recovery. A successful recovery continues the original run through push, PR open/reuse, and review promotion. If that run carried `completion: done`, the unchanged verified-merge checkpoint retains the authorization and moves the task to `done` only after GitHub reports the same PR merged.

The preserved live ORB-11472 / PR1478 incident is an installed-runtime follow-up, not validation performed by [ORB-11488]. After this change is merged to `agent-main` and that build is installed on the owning host, first verify `orbit run show jrun-20260907-0339-14 --json` still names ORB-11472 and its preserved worktree/branch, and verify `gh pr view 1478 --json number,state,mergeStateStatus,headRefName,baseRefName` still reports the same open candidate. Then run `orbit job resume jrun-20260907-0339-14 --json` and retain the returned descendant run ID. Resume reuses successful publication and promotion checkpoints but resolves the current `complete_pr` definition, so the installed typed recovery path—not a blind retry of the old failed process—must perform any repair. Finally, verify the descendant run succeeded, PR1478 is `MERGED`, ORB-11472 is `done`, and its task branch was updated once rather than replaced by a new branch or PR. Until those host-side checks are recorded, source tests establish the recovery mechanism only; they do not establish that PR1478 recovered live.

The ORB-11479 / PR1481 and ORB-11477 / PR1482 preserved conflicts remain
installed-runtime verification targets. Source tests exercise the host
continuation boundary, but do not prove the installed Linux sandbox/process
composition or a same-PR GitHub completion. After deploying this change, the
orchestrator must select an eligible, unconsumed recovery checkpoint, verify
its run/task/worktree/branch/base and open PR identity, resume it without
restarting implementation, and record that the real sandboxed leaf edits files
while the host completes Git metadata mutation. The descendant must reuse and
complete the same PR under retained `completion: done` authority. A consumed
checkpoint must not be blindly replayed or have its history reset.

After [ORB-10385] / [The runtime reports its deterministic-action registry, and job validation gates on it](./4_decisions.md#the-runtime-reports-its-deterministic-action-registry-and-job-validation-gates-on-it), a job's reachable deterministic actions are checked against the executing runtime before its first step runs. `RuntimeHost::has_deterministic_action` reports the shared typed registry; `validate_job_deterministic_actions` walks the job's `recovery_activity`, `failure_activity`, every step's `recovery_activity`, and every resolved deterministic target (recursing through `parallel:`, `fan_out:`, and `loop:`) and fails the run with `DeterministicActionUnavailable` naming both the activity and the action. Because the check runs inside `execute_job_with_resume` ahead of step one, the run never reaches `worktree_setup`, so no task is admitted and no worktree is created. Unknown actions are never skipped, and the default trait implementation reports `true`, so a host that cannot enumerate its registry keeps surfacing the miss at dispatch. The gate does not weaken the failure hook: an action that becomes unavailable after admission still leaves the original failed-step error authoritative.

[ORB-10630] centralizes action names and core-versus-engine ownership in `orbit-types` ([Typed deterministic action declaration spans core and engine](./4_decisions.md#typed-deterministic-action-declaration-spans-core-and-engine), Proposed). The generated typed enums now derive core's advertised capability check and its engine forwarding path, while the engine matches its ownership-specific enum exhaustively. A declared action without its matching core or engine implementation therefore fails compilation rather than becoming runtime registry skew; an implementation cannot name an undeclared typed action. The duplicated dispatch-table assertion and standalone core asset scan were removed because they only compared duplicate lists. Catalog coverage remains: shipped YAML action strings are external inputs and must still be checked against the generated registry.

The linked-worktree boundary guard now treats a clean same-branch primary fast-forward as concurrent base movement, not provider escape. The assigned checkout retains its own HEAD, so the later `pr_prepare`/`git_rebase` checkpoints reconcile that movement. Primary resets, force-moves, and branch switches surface as `primary_checkout_drift`; inadmissible history or branch movement inside the assigned worktree surfaces as `worktree_content_conflict`. Conflict diagnostics report `run_changed_paths`, `primary_changed_paths`, and their `conflicting_paths` separately instead of presenting the primary's entire moved set as the task's conflict.

Before any dirty integrity failure leaves the boundary, Orbit writes content-bearing recovery evidence under the repository Git common directory at `orbit/worktree-recovery/<run-id>/`: `tracked.patch` is a binary/full-index diff against the recorded HEAD, `untracked/` mirrors every untracked file payload, and `manifest.json` records the task, run, HEAD, branch, and payload inventory. The typed diagnostic names those paths. Because the Git common directory survives forced removal of a linked worktree, an operator can recreate a checkout at `recordedHead`, apply `tracked.patch` with `git apply --binary`, and copy the untracked payload back even after pipeline cleanup.

After [ORB-10471] / [Primary fast-forward acceptance is decided by interference with the run, not primary dirty-state byte-identity](./4_decisions.md#primary-fast-forward-acceptance-is-decided-by-interference-with-the-run-not-primary-dirty-state-byte-identity), whether primary working-content counts against that fast-forward is decided by interference with the run, not by byte-identity of the whole primary dirty state. The guard derives `primary_dirt_paths` — the paths whose index entry, index-to-worktree patch, worktree presence, or untracked blob identity actually moved — deliberately excluding the HEAD-relative `staged_patch_sha256`, which a fast-forward alone rewrites for every already-dirty path. `conflicting_paths` is that dirt intersected with `run_changed_paths`, so a merged sibling PR touching a file the run also touched stays base movement rather than a conflict. A fast-forward is accepted only when it is a proven same-branch ancestor advance *and* that intersection is empty; the ignored dirt is named in the acceptance log as `ignored_primary_paths` and in every drift diagnostic as `primary_dirt_paths`. This closes friction F2026-07-139, where one unrelated untracked primary file turned a benign base advance into `primary_checkout_drift` after valid implementation.

PR creation is restartable within its own checkpoint. `pr_open` first looks up the open PR by head branch; only the explicit no-PR result permits the engine-private PR-create operation. If creation succeeds but PR metadata refresh or local step-output persistence fails, the retry finds that same external PR and returns it as reused. Push, PR lookup/create/view, and merge execute through this private VCS boundary rather than the public tool registry, so ordinary agents cannot discover, authorize, or dispatch shipment operations. `pr_promote` then idempotently applies the GitHub PR external ref and the per-task implementation attribution before moving tasks to `review`. This boundary was made private in [ORB-10738].

The seeded `list_backlog_tasks` deterministic activity starts `task_auto_pipeline`. Automatic mode admits tasks by `status: backlog`. It emits `task_count`, `task_ids`, `tasks`, singleton `bundles`, and an `excluded` array for admitted backlog tasks filtered because their context files overlap `in-progress` or `review` locks. `excluded` covers only lock overlap; status-based admission and `max_tasks` truncation stay silent, and explicit `task_ids` mode omits it. This attribution contract was added in [T20260421-0542-2].

`task_gate_pipeline` reserves a bundle's context files before it dispatches `task_pr_pipeline` or `task_local_pipeline` through `invoke_and_wait`. The reservation owner is the gate run that executed `reserve_locks`, not the child shipment run. Its 3600-second admission-starvation limit remains independent of delivery. After admission, the seeded 14400-second child wait covers the supported 10800-second implementation activity plus a bounded 3600-second delivery/review tail, and `ttl_seconds` matches that full dispatch budget. The outer `task_auto_pipeline` wait is explicit at 21600 seconds: gate admission plus child delivery plus a bounded 3600-second parent tail. These finite nested budgets fit under the validated 21600-second `invoke_and_wait` ceiling; the generic omitted wait remains 3600 seconds. Workspace overrides must preserve `ttl_seconds >= dispatch_timeout_seconds` and an outer wait greater than or equal to gate admission plus dispatch [T20260427-36]. Owned reservations are engine-cleaned when that owner run reaches a terminal state (`success`, `failed`, `cancelled`, or `timeout`), so correctness does not depend on every workspace override preserving a YAML release step. The seeded deterministic `release_locks` activity still calls `orbit.task.locks.release` after a terminal child wait as an early-release optimization; idempotent terminal cleanup then finds nothing left to release. After [T20260427-34], `invoke_and_wait` remains a raw child-status join primitive, and seeded shipment parents use `pipeline_success_guard` to fail after required cleanup whenever a child run reports anything other than `success`. `task_gate_pipeline` guards the direct child after release; `task_auto_pipeline` guards collected gate results after fan-in and skips that guard for an empty backlog. A wait timeout reports the linked child run as `timeout` but does not terminalize that child, so operators can inspect and continue supervising it by run ID. Unowned/manual reservations remain explicit-release-or-TTL only. TTL is the fallback for abandoned/manual reservations or cases where no terminal cleanup or reserve-pressure reconciliation trigger runs. This lifecycle was tightened in [T20260430-26] and made engine-owned in [T20260505-10].

`reserve_locks` checks dependency edges before locks, and after [ORB-10593] / [Dispatch admission separates unmet dependencies from unsatisfiable ones](./4_decisions.md#dispatch-admission-separates-unmet-dependencies-from-unsatisfiable-ones) it splits them by whether waiting can ever help. A dependency that has not reached `done` but still could — `proposed`, `backlog`, `in-progress`, `review`, `blocked`, `someday` — yields `reserved: false` with `waiting_on_deps` populated, and the gate's `wait_for_window` loop polls as before. A dependency that can never reach `done` — `archived`, `rejected`, or an ID that resolves to no task at all — fails the activity on the first call with a `task.dependencies.unsatisfiable:` error naming each blocked task, its offending dependency, that dependency's status, and the remedy. Both branches publish `waiting_on_deps` (empty on the lock path) so the pipeline can reference `steps.reserve.output.waiting_on_deps` unconditionally, and `gate_starvation_fail` now reports it alongside `conflicting_files` — a dependency-starved bundle previously timed out naming no blocker on either axis. What counts as *satisfied* is unchanged and stays `done`-only: this is a failure-reporting change, not a widening of admission. Note that the separate epic-rollup predicate `is_feature_child_terminal_status` still folds `archived` and `review` into `"done"`; it answers a different question (should this child be dispatched again?) and was deliberately left unconverged.

The HTTP epic pipeline layer — the `task_epic_pipeline` job, its `epic_orchestrator` activity, and the deterministic `pipeline_wait` join — was removed as unused in [ORB-10332]. The live gate/auto/PR/workspace-ship pipelines retain `invoke_and_wait` and the `orbit.pipeline.invoke` / `orbit.pipeline.wait` tools for child dispatch and joining.

Reserve conflict checking also performs bounded, opportunistic stale-owned-reservation reconciliation before reporting reservation conflicts. It inspects only overlapping owned reservations under current reserve pressure; it is not a background sweeper. Existing job-run list/show reconciliation remains in place, and both paths release run-owned reservations with `release_reason: stale_run_reconciled` when they prove the owner is already terminal or stale. Release audit rows use the task-lock audit surface and include `reservation_id`, `owner_run_id` when present, and `release_reason` (`explicit`, `run_terminal`, `stale_run_reconciled`, or TTL expiration).

---

## 5. Retired Agent Backend Selection

Orbit dispatches every `agent_loop` through the CLI agent path. The
`backend: http | cli | auto` selector, its precedence chain
(`--backend` → `ORBIT_BACKEND` → `[runtime] backend` → `cli`), the per-crew
`backend` field, and the engine-driven HTTP agent loop were removed in
[ORB-10801].

Retired declarations are recognized rather than ignored, so nothing is silently
reinterpreted: `backend: cli` parses and is inert, `backend: http` / `auto` are
refused, and any `session:` binding is refused because cross-iteration
conversation history was an HTTP-loop capability with no CLI equivalent.
`AgentLoopSpec::max_iterations` is likewise inert — it bounded only that loop.

`openai_compat` has no CLI runtime, so selecting it fails structurally rather
than falling back to another provider.

The prescriptive contract for this area lives in [specs/backend-resolution.md](./specs/backend-resolution.md).

---

## 6. Engine-Core Boundary

Activity / Job is where orbit-core hands work to orbit-engine without depending on `orbit-agent` types.

After [ORB-10633] / [Job execution crosses one RuntimeHost boundary](./4_decisions.md#job-execution-crosses-one-runtimehost-boundary), `RuntimeHost` is the single capability boundary and `OrbitRuntime` has one implementation. It presents run/task persistence, environment resolution, provider dispatch, audit/checkpoint hooks, and core-owned deterministic actions in one declaration; no parallel host-trait family remains.

- run a core-owned deterministic action by name
- resolve a provider's CLI executor command plus static args
- build `ToolContext` for an activity, including policy, filesystem audit hooks, and trusted reservation-owner context from the active run id
- persist invocation traces for completed agent-loop work

The boundary remains primitive — strings, `Value`, and `ToolContext`, not `orbit-agent` transport objects.

`dispatch_v2_activity(...)` is the central per-activity entry. It emits `ActivityStarted` / `ActivityFinished` envelope events, then delegates by spec kind:

- `agent_loop` → the CLI agent path
- `deterministic` → parse the shared typed action once; core-owned actions cross `RuntimeHost`, while engine-owned actions enter their engine implementation directly

For example, `git_commit` now follows dispatcher → `execute_engine_action` → the engine's commit implementation, whose runtime needs cross `RuntimeHost` once. It no longer follows dispatcher → core registry → engine forwarding → the former second host family. Task workflow-admission policy likewise has one owner in orbit-core: the read-only gate and mutating admission share the same status predicate, and worktree setup makes one mutating admission call per task.

---

## 7. Agent Loop Dispatch

`agent_loop` has one execution path [ORB-10801]. Orbit does not enforce tool
allowlists on it: the effective tool set is recorded as an advisory
(`tool_allowlist.harness_delegated`) and enforcement is delegated to the
provider harness.

[ORB-11069] makes the activity `tools` list a baseline for task-backed
dispatch. Before provider construction or launch, orbit-core loads every task
selected by the agent activity and computes `effective_tools =
deduplicate(activity.tools union selected_tasks.required_tools)`. Required
tools are immutable creation-time task authority; task update paths cannot
alter the stored list. Required
names are exact canonical registered tools;
wildcards, malformed or unknown names, disabled tools, and registered tools not
on the agent-facing surface fail with `RequiredToolAdmission` naming the task
and tool. An empty requirement list returns the baseline byte-for-byte. Task
requirements are normalized in the task store and freeze once the task enters
`in-progress`, including later blocked retries.

### 7.1 CLI agent path

The path is driven by `cli_runner.rs`, added in [T20260419-0104]. The flow is:

1. Ask the host for the concrete CLI executor and resolve every selected task's requested tools and the effective list.
2. Emit the advisory `ToolAllowlistHarnessDelegated` event with the selected task ids, requested list, and effective list.
3. Resolve the subprocess cwd from runtime-owned workspace context.
4. Build an `Agent` and its provider-specific `AgentInvocationSpec`.
5. Emit `CliInvocationStarted` with redacted argv, stdin blob ref, and resolved cwd.
6. Spawn the subprocess in that cwd with a wall-clock timeout.
7. Emit `CliInvocationFinished` with stdout/stderr blob refs and timeout state.
8. Parse the captured provider output with the existing Orbit response parser and persist its `InvocationTrace` through the host. After [ORB-10231] / [CLI response envelopes are optional for artifact-backed activities](./4_decisions.md#cli-response-envelopes-are-optional-for-artifact-backed-activities), envelope parsing is best-effort by default: provider exit status and timeout determine transport success while durable task/review/git artifacts remain authoritative. A valid envelope still projects its result fields, and an invalid or absent envelope is retained as bounded/redacted diagnostic metadata. Activities whose downstream templates require response fields set `require_response_envelope: true`, preserving fail-closed validation for that explicit contract.
9. Apply the step-completion protocol check ([ORB-10449]) — see §7.6a.

### 7.6a Step-completion protocol vs. response content

[ORB-10231] / [CLI response envelopes are optional for artifact-backed activities](./4_decisions.md#cli-response-envelopes-are-optional-for-artifact-backed-activities) left one flag carrying two unrelated questions, and the
second one went unasked. `require_response_envelope: false` was read as "this
activity's response does not matter", when what it actually means is "nothing
downstream *consumes* this activity's response". Whether the invocation ran its
contract to the end is a different question, and nothing was asking it.

[ORB-10449] splits them:

| Flag | Question | Default | Reads |
|---|---|---|---|
| `require_completion_envelope` | Did the invocation finish with an accepted outcome? | `true` | envelope frame, then status token |
| `require_response_envelope` | Can downstream templates trust the fields? | `false` | full envelope incl. `result` |

The frame check (`response_envelope_protocol_check` in `orbit-agent`) asks only
whether stdout carried a well-formed Orbit response envelope: present,
`schemaVersion: 1`, and one of the three protocol status tokens. It never reads
`result` or `error`, so all three tokens prove protocol termination. After
[ORB-10733], the CLI runner then gives the token its narrow control-plane meaning:
under a required completion contract, `success` may checkpoint the step while
explicit `failed` and `timeout` fail it at that step. The `result` and `error`
payloads remain advisory and durable task, Git, review, and deterministic
validation state remain authoritative. **This preserves the doctrine's actual
boundary**: protocol termination and status outcome are control flow; agent prose
and payload content are not evidence that can satisfy durable workflow guards.

Every agent invocation is prompted with the response-envelope contract
(`render_prompt_with_embedded_envelope`), so exiting 0 without one is a protocol
violation, not a stylistic choice.

Deliberate asymmetry: the completion check is *more* permissive than the content
parser about stream shape. When the document stream will not parse — a wrapped
tool writing to the same stdout, a stray warning line — it falls back to scanning
the raw text for an embedded envelope. Failing a step that genuinely completed,
over stdout tidiness, would be a worse defect than the one this check exists to
catch.

**Failure semantics.** A missing or malformed frame, or a completed frame that
declares `failed` or `timeout`, fails the step exactly as an opted-in envelope
failure does: `DispatchOutcome { success: false }` with a message naming the step
and the protocol violation or declared status. Concretely, for `implement_one`
in `task_pr_pipeline`:

- **Not retried.** The step has no `retry:` block, and a repeat invocation of a
  stalled agent has no new information to work with.
- **No recovery agent.** `recovery_activity` fires on `Err`, not on a failed
  outcome — which is the behaviour we want here. `step_failure_recovery` exists
  to repair the *delivery path for completed work*; a stalled implementer is
  incomplete work, and having it publish the candidate is the opposite of the fix.
- **Run terminalizes at that step.** No later step runs, the step is audited as
  `failed`, and the job-level `failure_activity` (`pr_failure_handoff`,
  [Terminal PR shipment uses a job-level failure handoff](./4_decisions.md#terminal-pr-shipment-uses-a-job-level-failure-handoff)) still fires to preserve recoverable work.
- **Task and worktree.** The worktree is retained with whatever the agent wrote
  before it stopped; tasks coupled to the run move to `blocked` under the normal
  terminal-run rules. Re-dispatch is an orchestrator decision, not an automatic
  one.

The durable `execution_summary` delivery gate ([ORB-10313] / [Fail delivery before Git mutation when execution outcome is not success](./4_decisions.md#fail-delivery-before-git-mutation-when-execution-outcome-is-not-success)) is
unchanged and remains the last line. Nothing here weakens it or bypasses it —
this check simply means a stalled implementer no longer reaches it.

After [ORB-10603] / [Derive the delivery execution summary from the change, not from the agent](../worktree-artifacts/4_decisions.md#derive-the-delivery-execution-summary-from-the-change-not-from-the-agent), the gate is also no longer reachable in the
ordinary case, because the commit step fills the field it reads. When — and only
when — durable state carries no meaningful summary, `commit_batch_changes`
derives one from `git status` in the delivery worktree (the same file set
`git_commit` will validate), persists it to the task record with an
`execution_summary_derived` event, and then meets the unchanged gate. The
derivation reads durable, re-checkable state and never the agent's advisory
response envelope ([L-0115]); an agent-authored summary is always preserved; and
a worktree with nothing to describe still yields no summary and is still
refused.

After [ORB-12050], task delivery has a concrete path-intent contract at that
same deterministic boundary. Every tracked modification, deletion, or rename
is a candidate because Git already identifies it as repository content. A new
path is a candidate only when the implementing worker appends its exact
`file:` selector to durable task state after creating it. This ordering
respects the authoring rule that selectors name existing paths and also works
in managed workers whose Git metadata is read-only. An explicitly staged path
remains accepted for callers with a writable index, but staging is not required
of managed workers. Directory selectors are ownership boundaries, not
new-file intent. Before staging anything, `git_commit` compares all untracked
paths with those explicit signals and refuses every unknown path by exact name.
It therefore cannot be bypassed by a persuasive summary, empty selectors, or
one legitimate file already in the index, and refusal preserves the prior
index and every worktree byte. Accepted candidates are staged by explicit path;
per-task publication commits only the paths assigned to that task so an
already-populated index cannot leak a sibling candidate.

Failure-candidate preservation uses the same task-candidate boundary. The
before-PR reviewer is deliberately different: it starts from a committed,
pinned candidate, is forbidden to run staging commands, and its activity
contract makes every path it leaves changed an intended reviewer repair.
Reviewer scratch, logs, and attachment staging files must live outside the
worktree; the host may therefore stage that reviewer's entire repair delta
without heuristically forcing implementer task selectors onto it.

**Declared exceptions.** No shipped `agent_loop` activity opts out of
`require_completion_envelope`. Every seeded agent step performs work whose
absence must stop the pipeline, so the empty opt-out list is pinned by a test
and any future exception requires a deliberate edit with a stated reason.

### 7.6b Structured output is the Claude prevention layer

§7.6a is a *detector*, and by [ORB-10746] it was the only thing standing behind
a machine protocol that was otherwise enforced by asking a model nicely.
`jrun-20260812-0312-9` is the bill for that: a QA sweep on [ORB-10734] ran 89
turns over ~11.5 minutes for $3.17, finished its work, persisted a meaningful
`execution_summary` — and then answered in prose. `stop_reason: end_turn`,
`terminal_reason: completed`, `subtype: success`, empty stderr, exit 0, and no
envelope. The guard correctly refused to checkpoint it, and the task blocked.
Detection was working; prevention did not exist.

The installed Claude CLI exposes `--json-schema`, so the frame can be a
constraint rather than a request. `crates/orbit-agent/src/types/response/protocol_schema.rs`
is the single protocol definition: the Rust parser and the schema handed to the
provider are generated from the same constants, so they cannot disagree about
what `schemaVersion: 1` or the status token set means.
`ClaudeCliTransport::args` emits the flag per request rather than adding it to
the two `claude.yaml` copies — the packaged asset and the installed workspace
resource are edited independently, and a per-request flag has no second copy to
drift from. The prompt contract
(`render_prompt_with_embedded_envelope`) stays as human-readable guidance; it is
no longer the enforcement mechanism.

**What the schema cannot say.** The status/error correlation — `failed`
requiring a non-empty `error.code` — is absent from the schema and stays in
`parse_json_envelope`'s Rust checks. This is a constraint, not a preference:
expressing it needs a conditional subschema, and Anthropic's structured-output
subset rejects those outright with `input_schema does not support oneOf, allOf,
or anyOf at the top level`. The rejection surfaces mid-run rather than at argv
parse, so getting it wrong is expensive. After [ORB-10770], `error` is typed
as an object (`code` + `message`) and omitted on success — Serde's
`#[serde(default)]` already treats a missing field as `None`. The schema
still cannot express "require the object on `failed`, omit it on
`success`", so the Rust correlation checks remain load-bearing rather than
redundant.

The earlier untyped property plus a description that said "null when status
is success" made Claude's constrained decoder emit the JSON *string*
`"null"` (`jrun-20260813-0451-3`). The parser accepts that token — and the
empty string — as absent `error`, the same as JSON `null` or a missing
field. A `status=failed` envelope with missing, JSON-null, or
string-`"null"` `error` is still a protocol violation (`failed status
requires error object`), not a synthesized success.

**Two capability failures, two places.** A CLI that *lacks* `--json-schema`
fails at argument parsing, before any agent work and any cost; the evidence is
`error: unknown option '--json-schema'` on stderr. A CLI that *rejects* the
schema fails mid-run, after the request reaches the API; the evidence is in the
response wrapper (exit 1, `is_error: true`, `structured_output: null`, and a
prose `result` of `API Error: 400 tools.0.custom.input_schema: ...`). Note that
`subtype` still reads `"success"` in that second shape, so no classification
logic may key on it. `provider_invocation_diagnostic` reports each specifically;
neither falls back to an unconstrained invocation.

**What structured output does not fix.** It removes the prose cause of an
exit-0 ending without an envelope, not the category. A turn-limit ending still
exits 0 with `is_error: true`, `subtype: "error_max_turns"`,
`terminal_reason: "max_turns"`, and both `result` and `structured_output` null.
Those endings now map to a synthesized `failed` envelope naming the provider's
own reason, instead of collapsing into the generic [ORB-10449] missing-envelope
text after a full-cost run.

**The backstops stay provider-neutral and fail-closed.** Every addition here can
only improve a failure *message*; none can change a failure *decision*. Success
is never synthesized from exit code, `is_error`, `subtype`, `terminal_reason`,
persisted `execution_summary`, no-diff tags, task state, worktree state, or
provider prose — a pinned test enumerates the wrapper-signal matrix and asserts
no combination yields anything but `failed`. §7.6a's frame check and the
[ORB-10733] status handling remain authoritative for every provider, including
those with no structured-output mechanism at all; Codex, Gemini, Grok, and mock
fixtures parse unchanged. Extraction now reads the wrapper's top-level
`structured_output` object ahead of the `result` string, because Claude emits
the validated envelope in both places and `result` is null on several terminal
paths where `structured_output` is not.

After [T20260426-2313], stdout/stderr readers emit line-level `tracing::info!` events while the child runs, carrying `provider`, `stream`, `job_run_id`, `task_id`, and `line`. After [T20260508-8], those events also carry `cwd` when the CLI subprocess has a resolved cwd. After [T20260426-2349], the default tracing subscriber redacts formatted output. The readers still retain original bytes for the existing audit/blob path, so run logs follow blob refs rather than the live feed.

Executor args are prepended before provider runtime args. For seeded Codex, the subprocess starts as `codex exec --json ...`, not the interactive TUI. [T20260423-0114] exposed the earlier command-only boundary.

After [T20260427-48], provider runtime args receive provider config through `RuntimeHost`. Static executor definitions keep command-shape flags (`exec --json`); dynamic Codex settings such as sandbox mode, side-write roots, and approval policy stay in the retained provider runtime. Codex approval policy is an exec-compatible config override, not the interactive-only `--ask-for-approval` flag.

After [T20260427-51], macOS CLI invocations declaring `sandbox: macos-sandbox-exec` run under `/usr/bin/sandbox-exec -f <profile.sb> <provider> ...`; [T20260509-30] made that wrapper resolution trusted and absolute instead of `PATH`-based. Orbit treats that SBPL profile as filesystem authority and neutralizes provider-native sandbox flags. After [T20260428-10], the profile grants Codex state (`$CODEX_HOME` or `$HOME/.codex`) plus side-write roots from provider config so inherited Orbit subprocesses can persist workflow state while project writes remain governed by `fsProfile`. After [T20260505-22], dispatch also runs `apply_provider_static_arg_fixups` before spawn, separately from sandbox neutralization. Today this only rewrites Claude's `--debug-file` value to `<claude_state_dir>/<basename>` so the log lands inside the already-writable state dir instead of `.orbit/**`, which the default policy denies.

After [T20260509-40], bare Unix CLI subprocesses enter their own process group, matching the macOS sandbox wrapper's kill boundary. The supervisor kills that process group on timeout, also kills any remaining group members after the main child exits before joining output readers, and bounds timeout-path reader joins so a leaked pipe writer cannot hang the activity supervisor. Non-Unix platforms keep the immediate-child kill fallback until Orbit has an equivalent process-tree primitive there.

After [ORB-10456], every bare provider launcher is resolved before audit and
spawn at the shared `orbit-engine` CLI boundary ([Provider launchers resolve at the shared CLI spawn boundary](./4_decisions.md#provider-launchers-resolve-at-the-shared-cli-spawn-boundary)). Lookup preserves
configured paths verbatim; bare names search the process `PATH` first, then
portable user-local directories derived from `HOME` (`.local/bin`,
`.orbit/bin`, `.cargo/bin`, and `bin`), then portable supported system
prefixes (`/opt/homebrew/bin`, `/usr/local/bin`) so a launchd-minimal Mac
`PATH` still finds Homebrew-installed provider CLIs ([ORB-11808]). Searched
candidates must be regular files with execute permission on Unix; explicit
overrides skip that search. Dashboard Ship, routine sweep, `orbit run ship`,
and direct job execution all converge on this boundary, so none depends solely
on the environment inherited by its entry process. A missing launcher remains
a permanent failure, but the diagnostic names the provider and every path
Orbit searched.

After [T20260430-15], the CLI stdin envelope carries rendered activity input and durable `run_id` beside instruction, prompt, tools, and model. [ORB-11069] defines `tools` there as the computed effective list and adds the task's requested `required_tools`; the child also receives the effective list in `ORBIT_ACTIVITY_TOOLS`. When input identifies one task, orbit-core embeds a canonical task snapshot with `input.workspace_path` / `input.repo_root` taking precedence over stored paths. After [T20260508-8], agent dispatch also uses a shared workspace resolver for subprocess cwd: `input.workspace_path`, then `task.workspace_path`, then best-effort `ToolContext.workspace_root`. Declared input/task paths must already be directories; stale worktrees fail as `CliInvocationFailed` before `CliInvocationStarted` is emitted. After [T20260505-10], Orbit-managed CLI subprocesses receive `ORBIT_RUN_ID` plus an Orbit-managed run-context marker; `orbit tool run` requires both before it populates `ToolContext` reservation ownership. Direct manual CLI tool calls, including calls with only `ORBIT_RUN_ID`, remain unowned.

The older `AgentRuntime` trait and `providers/*_cli.rs` files are not deprecated leftovers; they are the shipped CLI agent implementation.

---

## 8. Job Execution Semantics

The executor implementation lives under `crates/orbit-engine/src/activity_job/job_executor/` after [T20260509-2]. `mod.rs` owns the public exports and run entrypoint, while responsibility-focused child modules own audit projection, execution context, templating, step retry/recovery, target dispatch, parallel/fan-out/loop constructs, validation, and the small fan-out semaphore. The outward `activity_job::job_executor::{JobOutcome, execute_job_with_resume, resolve_job_catalog_refs_for_execution, validate_job}` surface is the supported execution API; the former non-resume convenience wrapper was removed as unreachable cleanup in [ORB-10629].

### 8.1 Template rendering and pipeline context

The executor exposes outputs as `{{ steps.<id>.output.* }}`. Initial context follows the §3 merge contract: object caller input overlays object `job.default_input`, while `null` and non-object inputs keep their special cases. Step `default_input` is rendered recursively; strings that parse as JSON convert back into `Value`.

`fan_out` workers see `{{ item }}` / `{{ input.item }}`. Loop bodies see `{{ input.iteration }}`.

#### Durable agent-step state

Direct agent responses are advisory audit/diagnostic data. Workflows gate on durable records: task fields and artifacts written through the task surface, deterministic action checkpoints persisted by the executor, or purpose-built durable operations. No generic agent-callable pipeline-state read/write tools are registered. `orbit.task.update` remains the task persistence boundary for `execution_summary`, comments, and lifecycle fields; deterministic step output is persisted by the engine rather than trusted from an agent response. The generic state-tool surface was removed in [ORB-10738].

### 8.2 `when` and `retry`

`when` is evaluated once, before retry. A skipped step is a successful no-op and does not retry.

After [T20260509-11], the shipped condition grammar remains equality-only (`==` / `!=`, combined with `&&` / `||`); skip-on-empty guards express emptiness as `!= 0` or `!= []` rather than numeric comparisons, preserving the `orbit run ship` auto-mode empty-backlog no-op behavior.

**A step output read by any `when:` / `break_when:` condition must belong to a step that always runs** (ORB-11325). Two facts combine to make this a hard requirement rather than a style preference: `condition::evaluate_bool_expr` (`crates/orbit-engine/src/condition.rs`) renders the *whole* expression through the template engine before it parses it, so there is no short-circuit — both sides of `&&` / `||` render even when the left side alone would decide the result — and a step skipped by `when:` records no output at all, so any reference to it resolves to `template.rs`'s `no data recorded for step '{step_id}'` error. Together, a condition that reads `steps.<id>.output` for a step that itself carries a `when:` fails at runtime on exactly the branch where the referenced step would have been skipped — the branch an author is least likely to exercise first, since the happy path usually runs it.

The working alternative is to end the run on the stranding branch instead of skipping past the reader: give the branch that would leave the reference unresolved its own unconditional failure (as `task_gate_pipeline`'s `starvation_check` does — its `when:` is the exact complement of the gated step's, and its activity always errors, so the run never reaches a downstream reader on that branch). Three plausible-looking workarounds do not help, and are not worth retrying:

- `A == x && B != true` — no short-circuit, so `B` still renders even when `A == x` is already false.
- Gating the reading step with the same `when:` as the referenced step — the reading step's own `when:` is still evaluated (and rendered) for every run, so the reference inside it still renders and still fails when the referenced step was skipped.
- A single-iteration `loop:` wrapping just the referenced step as a grouping construct — this only renames which id is skippable. If the loop step itself carries the `when:` that used to guard the body step, then when that condition is false the *loop* is skipped, the body step never runs, and a reader outside the loop still finds no data recorded for the body step's id. Wrapping the referenced step *and every reader of its output* together in the same guarded block does work (nothing outside the block needs the skipped id), but wrapping only the referenced step does not.

Catalog load enforces this: `validate_job` (`crates/orbit-engine/src/activity_job/job_executor/validate.rs`) walks every step's `when:` and every loop's `break_when:` for `steps.<id>.output` references and rejects the job, naming both the reading and the referenced step, when the referenced step may be skipped. Skippability is inherited (ORB-11346): `run_step` returns before running any body when a guard is false, so a step nested in a `parallel:`, `fan_out:`, or `loop:` block under a `when:`-carrying ancestor is skipped with that ancestor and records nothing, however unguarded the nested step looks on its own. The validator therefore compares guard *chains* rather than a per-step flag: a guard the reading step itself sits under skips both steps together and can never strand the reader, which is exactly what keeps the admissible shapes above valid — wrapping the referenced step and every reader of its output in the same guarded block, and a guarded loop's `break_when:` reading its own body step.

That shared-guard exemption is relative to execution order, not mere set membership (ORB-11361). `when:` is evaluated before the step body, so a step does not sit under its own guard when its condition is rendered: a container `when:` that reads a nested body or worker output, and a step `when:` that reads that same step's output, are rejected even though the referenced step's recorded chain includes the reader id. `break_when` is the opposite — it is evaluated after the loop body — so a guarded loop may still read an earlier body step. A later sibling `when:` that shares an enclosing guard with the referenced step remains valid.

The retry wrapper re-runs the whole step body up to `max_attempts`, with exponential or linear backoff. Some errors bypass retry:

- tool denial
- unknown deterministic action
- host-required structural errors
- job validation errors

That rule comes straight from `DispatchError::is_non_retryable()`.

### 8.3 `parallel`

Parallel branches run under `std::thread::scope`. Join policy is:

- `all`
- `any`
- `quorum { n }`

The executor emits `StepJoin` with per-branch outcomes. If the join policy fails and any branch produced a structural error, the first error is surfaced instead of only `success: false`.

### 8.4 `fan_out` / `fan_in`

`fan_out.items` is template-rendered into an array. Workers run concurrently behind a counting semaphore, so `max_workers` is a true concurrency bound, not just metadata. `fan_in.collect` can persist the ordered worker outputs under a separate pipeline key in addition to the step id itself.

Collection does not itself interpret a successful activity call whose payload
reports a failed child run. A parent that waits on child workflows must pass
the collected wait entries to `pipeline_success_guard` after fan-in. The CI
failure sweep does this only when its filer reports a nonzero pilot-candidate
count: independent successful pilots finish and apply before the guard makes a
failed, stale, cancelled, interrupted, or timed-out pilot visible as a failed
sweep, while a genuinely empty candidate list remains a successful no-op.

Workers use isolated pipeline/session maps. The validator rejects any worker template with `session:` because concurrent workers would otherwise share one mutable `Session`.

### 8.5 `loop`

A loop runs either:

- once per rendered `items` entry
- or up to `max_iterations` when `items` is absent

The body runs before `break_when`, so steps can populate fields the break expression reads. If `items` exceeds `max_iterations`, execution fails structurally instead of truncating.

### 8.6 Persisted state for v2 job runs

Every job run is a persisted pipeline run. `orbit run ship`, `orbit.pipeline.invoke` + `orbit.pipeline.wait`, and — after [ORB-10801] — `orbit run job` / `orbit job run` all submit through the same path in `pipeline.rs`: the run is persisted, a detached worker is spawned, and the caller returns. `orbit run history -j <job_id>` and `orbit run show <run_id>` inspect the returned ID. `--wait` blocks on the submitted run and maps a non-success terminal state onto a nonzero exit.

Named `ci_failure_sweep_pipeline` submission resolves and persists one
`integration_branch` before inserting the run. Precedence is non-empty run
`integration_branch` (then its supported `base_branch` alias), non-empty trusted
job `default_input.integration_branch`, registered workspace `base_branch`, then
explicit `[workflow] base_branch` config for an unbound workspace. An empty or
null run field requests the default. Whitespace and an `origin/` prefix are
normalized once before persistence, matching pilot branch resolution. Missing
authority, a non-string override,
or an invalid Git branch name refuses admission. The built-in config default
and GitHub's default branch do not establish integration authority. Both
`collect.integration_branch` and every pilot child's `base_branch` use this
persisted input; GitHub's default remains the release branch, scanned separately
when it differs. Replay/resume with a resolved input retain that branch.

This preserves the shipped-job trust boundary: named execution searches explicit
job directories and the global catalog, excluding workspace copies of shipped
jobs even though catalog listing can display them. Editing the workspace CI job
therefore cannot configure a scheduled named sweep. Set the registered base,
explicit config for standalone use, or supported run/trusted-catalog input.
Direct YAML execution retains its explicit-definition/input contract.

Read-only verification after deployment: use `orbit run history -j
ci_failure_sweep_pipeline` and `orbit run show <run_id>` on an existing scheduled
run. Confirm persisted `input.integration_branch`, the collect evidence's
`heads` (`agent-main` integration, `main` release), and each pilot child's
`input.base_branch`. Compare the registered workspace base via
`orbit tool run orbit.workspace.list --input '{}'`; for a standalone checkout,
inspect `orbit config get workflow.base_branch` and its source. Catalog listing
alone does not prove which YAML named execution used. These reads require no new
sweep, task filing, release, or package change.

A direct YAML path names an unmanaged file, so its exact validated definition is snapshotted to `state/job-runs/<run_id>.job.yaml` before submission returns; the worker prefers that snapshot over catalog resolution, and an edit or deletion of the source afterwards cannot change the submitted run. Catalog ids keep name resolution as their contract ([Job catalog discovery honors layer precedence](./4_decisions.md)). Workflow-specific `orbit run <workflow> list/show` aliases were removed in [T20260425-2010], and duplicate job-level aliases in [T20260426-0742].

Before [T20260423-0445], early v2 failures could leave `steps: []` and no surfaced `error_message`. The current contract is:

- if a persisted v2 pipeline fails and no recorded step already carries error detail, the pipeline worker writes a synthetic failed `JobRunStep`
- if a foreground run (`orbit job replay` / `orbit job resume`) succeeds, its wrapper writes a synthetic successful `JobRunStep` containing the final pipeline snapshot
- that synthetic step uses `target_type: job` and `target_id: <job_id>`
- the step's `error_message` carries the concrete executor error (or a fallback `success=false` summary for message-carrying non-success results)

This operator-surface repair keeps `orbit run ship --json`, direct `orbit job run`, `orbit run history`, and `orbit run show` actionable without adding a second run-level error channel.

After [T20260430-27], the former `orbit run ship-auto` path interpreted the parent `task_auto_pipeline` snapshot for operator output. Text and JSON modes kept the persisted run state and exit-code semantics, but added `workflow_status` labels: `empty_backlog`, `gated_noop`, `gate_waiting`, `gate_failed`, and `completed`. `empty_backlog` means no candidates and no exclusions. `gated_noop` means zero dispatched bundles with one or more `list_backlog.excluded` entries. `gate_waiting` means a child `task_gate_pipeline` run is still pending/running or the parent wait timed out while the child remains active. `gate_failed` means a child gate run reached a failed or cancelled state. After [ORB-00075], `orbit run ship` is the single public shipment command: omitted task IDs run auto mode, provided task IDs seed explicit singleton bundles, and both forms submit `task_auto_pipeline` asynchronously. The dispatch output is now just workflow/job/run identity plus pointers to `orbit run history -j task_auto_pipeline` and `orbit run show <RUN_ID>`; waiting reasons and terminal details live on those durable inspection surfaces rather than in CLI dispatch output.

After [T20260505-8], active job runs can be cancelled through the same durable run surface. `pending` and `running` runs transition to `cancelled`; terminal runs remain immutable. Pending cancellation only rewrites the run bundle and pipeline snapshot, so a later pipeline worker observes `cancelled` and exits without claiming the run. Running cancellation first validates the stored owner PID start-time token, then signals the owner process group on Unix with a bounded graceful period and `SIGKILL` escalation. `JobRunCancelled` audit payloads include run id, previous/final state, actor/source, whether signaling was attempted, and the signal outcome.

After [T20260505-21], whole-run replay creates a fresh durable `JobRun` from an existing run's persisted input and the current catalog job definition. Replay never mutates the source run bundle or source audit envelope; lineage lives on the new run as `retry_source_run_id` and in the new v2 `run.started` audit envelope. This is intentionally whole-run only: every step executes from step 0, and changed or deleted job YAML is resolved at replay time rather than read from a source-run snapshot.

After [ORB-10002], the executor also persists per-step recovery checkpoints, and interrupted runs are resumable. After each completed *top-level* step, `execute_job_with_resume` calls `RuntimeHost::checkpoint_step`, which orbit-core records into the run's existing persisted `PipelineState` (`step_states`, `step_outputs`, `next_step_index`, plus the cumulative step-output pipeline snapshot) — no new table; the checkpoint store is the `pipeline_state_json` column that already backs run state. Checkpoint failures are non-fatal (a `tracing` warning; the run continues without durability). Orphan reconciliation — which already probed `pid` + `pid_start_time` owner identity on run list/show/exec and now also runs best-effort at workspace open (`OrbitRuntime::from_resolved_roots`) — finalizes conclusively-dead-owner `running` runs to a new terminal `interrupted` state (`RunEvent::Interrupt`) instead of `failed`, with an `interrupted` diagnostic step carrying the liveness reason; inconclusive probes still leave the run alone. `orbit job resume <run_id>` accepts `interrupted` / `failed` / `timeout` runs and creates a fresh linked run (`retry_source_run_id`, `attempt + 1`) whose seeded `PipelineState` comes from the source's checkpoints: top-level steps recorded `success` are skipped (audited as `step.skipped` with a resume reason) and their outputs are fed back into the pipeline for later steps' templates. Checkpoint granularity is intentionally the top-level step — `parallel:` / `fan_out:` / `loop:` blocks re-run as a whole if incomplete — and in-memory agent sessions are not restored across processes. A source run with no successful checkpoint degrades to whole-run replay semantics.

After [ORB-10470] / [Resume is a durable submission scoped by explicit retry lineage](./4_decisions.md#resume-is-a-durable-submission-scoped-by-explicit-retry-lineage), resume is a durable *submission* scoped by explicit retry lineage. `OrbitRuntime::submit_resume_run` persists the resumed run seeded with the source's checkpoints and hands it to the same detached pipeline worker `submit_ship_run` uses, so `POST /job-runs/:id/resume` returns `{run_id, retry_source_run_id, state: submitted|queued}` as soon as the run is durable and the resumed pipeline never occupies a request thread; `orbit job resume` keeps the in-process foreground behavior. The pipeline worker takes its resume cursor from the run's *own* persisted `PipelineState`, which makes checkpoint reuse a property of the run record rather than of the caller and makes a worker restart idempotent — steps already recorded `success` are skipped, never re-dispatched. Before the first step runs, resume reconciles the tasks its lineage owns (the source run, its `retry_source_run_id` ancestors, and their descendants, narrowed by the run input's `task_ids` when present): a task blocked by that lineage's own failure is re-admitted to `in_progress`, and its `job_run_id` is realigned to the batch id the reused checkpoints carry (`steps.<worktree>.output.job_run_id`). This closes both halves of the F2026-07-122 catch-22 — checkpoint resume skips `worktree_setup`, so nothing else would re-claim the task — while leaving `load_handoff_context`'s ownership equality check untouched: a task owned by a run outside the lineage is never re-admitted or re-stamped, and whatever reconciliation cannot repair still fails closed at the delivery seam (F2026-07-121).

After [ORB-10631], all four ship surfaces — interactive `orbit run ship`, the dashboard endpoint, the MCP `orbit.workflow.ship` tool, and the deterministic routine action — enter `OrbitRuntime::submit_ship_run` before run insertion. Surface adapters still resolve their own request syntax and attribution, but the runtime owns canonical input construction, the in-flight explicit-task guard, pipeline audit emission, and durable job-run insertion. This keeps the CLI's output projection behavior while making the guard and every other submission check impossible to bypass by choosing a different front door.

After [ORB-10070], orphan reconciliation also covers `pending` runs. Pipeline workers claim their queued run at startup (`claim_pending_job_run_owner` records `pid` + start-time token while the run is still `pending`), so a queued run polling for its admission slot carries a probeable owner exactly like a running run. Reconcile finalizes a pending run as `interrupted` (`Pending + Interrupt` is now a legal transition) in two conclusive cases: its claimed owner is `Mismatch`/`Missing`, or it was never claimed and is older than a 30-minute grace window (covering workers that died before claiming and queued runs written by pre-claim binaries, e.g. stranded by a host reboot after their parent run was interrupted). Inconclusive probes and fresh unclaimed runs stay `pending`. `orbit doctor`'s `job-runs` check reports both orphan classes read-only, and `orbit run cancel <run_id>` gives operators a direct terminalization path.

After [ORB-10461], every detached pipeline worker appends stdout and stderr to the private run-addressable path `.orbit/state/logs/<run_id>.worker.log`. The parent-side startup observer records that path in claimed/failure audit events. If the child exits before setting its pending-run owner, the observer terminalizes the same run as `interrupted` and copies a redacted, bounded tail of the worker output into the synthetic diagnostic step; startup and action-registration errors are therefore inspectable by run id without waiting for the stale-run grace window. Normal claimed-worker execution and admission polling are unchanged.

The loop shares one pipeline map and session map across iterations, which makes cross-iteration `session:` meaningful.

### 8.7 Invocation metrics

The dashboard metrics endpoints read knowledge usage from job-run state (`/api/metrics/knowledge`) and agent, tool, task, and invocation usage from the SQLite invocation store (`/api/metrics/activity`, `/api/metrics/tools`, `/api/metrics/task/:id`, `/api/metrics/invocations`). They do not scrape `.orbit/state/audit/v2_loop/` or diagnostics JSONL.

V2 jobs persist invocation traces explicitly after [T20260426-0526]. `DispatchOutcome` carries optional trace data; the executor attaches run and step IDs; orbit-core stores canonical agent/model names plus task IDs from rendered input and refreshes the token scoreboard.

The trace comes from the provider's structured stdout, using the same parser that validates Orbit response envelopes.

### 8.8 Run trace inspection

`orbit run show`, `logs`, `events`, and `trace` inspect already-scheduled runs and resolve an omitted run ID to the most recent run.

After [T20260426-0709], `orbit run show <run> -s <id>` treats the v2 envelope's activity DAG `step.id` as primary. This matters because durable v2 runs may store a synthetic job-level `JobRunStep`, while the envelope records actual YAML step IDs. `JobRunStep.target_id` and numeric `step_index` remain fallbacks.

After [T20260426-0705], `orbit run events <run>` reads the v2 envelope chronologically and filters by step ID or event type. `orbit run trace <run>` renders the parent/child tree from `event_id` and `parent_event_id`. JSON mode is deterministic.

The CLI does not own envelope storage. `orbit-core` exposes accessors for v2 audit events and CLI invocation records, including derived step IDs and blob-backed stdout/stderr, keeping storage knowledge with the runtime layer.

### 8.9 Workflow worktree base synchronization

Task-shipping workflows that create worktrees (`task_pr_pipeline`, `task_local_pipeline`, and callers such as `task_auto_pipeline`) default `base_sync` to `remote` after [T20260427-45]. Remote mode fetches `origin/<base_branch>` and creates or resets task worktrees at that remote-tracking ref, so every candidate starts from published history rather than a stale local base.

Direct callers can set `base_sync: local` for local-only repos or unpublished base branches. That mode resolves the local base ref and skips origin fetch.

The local pipeline deliberately separates those two moments after [ORB-10604]: worktree setup still honors the caller's sync mode, but its merge checkpoint always uses `base_sync: local`. A previous bundle can advance the shared local base and then fail during bookkeeping or publication; treating that in-session commit as the next merge's rebase target prevents one post-merge failure from turning every later bundle into the remote-mode "local base is ahead" refusal. Each retry re-resolves the current local base and rebases only its own worktree, so concurrent disjoint bundles converge through the existing fast-forward/rebase loop. Exhausted contention can still fail one bundle, but it does not poison the base for later local merges.

The remote-mode refusal remains part of `git_merge`: a caller that requests remote synchronization still fails when the local base carries commits absent from the fetched remote. Local mode is safe here by construction because the local pipeline owns the in-session base history. Publishing before bookkeeping was rejected as the general repair because local pipelines may intentionally run with `auto_push: false`; making publication unconditional would change the workflow's external-side-effect contract, while leaving it conditional would preserve the cascade for those sessions.

### 8.10 Workflow task admission

After [T20260428-8], task-starting workflows own explicit admission instead of relying on generic task updates. After [ORB-11305] the admissible set is exactly `backlog` and `in-progress`: `backlog` is fresh authorized work, and `in-progress` is either this run's own idempotent retry or work a human explicitly restarted through `orbit.task.update`. `worktree_setup` moves the former into `in-progress` and leaves the latter alone.

Everything else is refused, because every other status is somebody's decision that the task should not be running: `proposed` and `someday` are unapproved or withdrawn, `archived` and `rejected` are closed, `review` and `done` already landed, and `blocked` is a failed run nobody has looked at yet. The set previously included `proposed`, `rejected`, and `archived`, which let admission silently overturn a withdrawal — the production incident [ORB-11305] investigated, where a bundle admitted while its task was `backlog` waited an hour on locks, the owner withdrew and then archived the task during that wait, and the gate dispatched anyway on its stale snapshot: `worktree_setup` moved the archived task to `in-progress` and launched a provider against withdrawn work.

The repair is that eligibility is re-asked at the boundaries where it is consequential, not just where dispatch was first decided, and that the answer cannot go stale between the check and the write:

- `invoke_and_wait` re-checks live admission for `admission_task_ids` immediately before submitting the child run. Already-shipped tasks (`review`/`done`) return the pre-existing succeeded no-op; a withdrawn task returns a non-success synthetic child result carrying the reason, so the gate's `release_reservation` step still frees the reservation before `require_child_success` fails the run. Both are audited (`gate.stale_noop`, `gate.withdrawn`). A task id that resolves to no task at all remains a hard activity failure — that is a malformed bundle, not a lifecycle decision.
- `admit_task_for_workflow_as_system` writes its `in-progress` transition under a store-level compare-and-set (`TaskHistoryUpdateParams::expected_status`), evaluated inside the per-task exclusive file lock after the bundle is re-read. A withdrawal landing between the predicate and the write loses the write rather than being overwritten.
- Symmetrically, the failure/cancellation cleanup in `block_on_run_failure` no longer blocks a task that is `proposed` or `someday`, joining `done`/`review`/`blocked`/`rejected`/`archived`. A withdrawal is routinely what *causes* the cancellation, and cleanup that ran after it would replace the owner's newer decision with `blocked`.

Recovery paths are unchanged: `orbit.task.update` with `status: in_progress` accepts `blocked` and leaves the task `in-progress`, which admission accepts, and a resumed run's `reclaim_task_for_resumed_run` restoration is a separate lineage-proven path that does not go through this predicate.

Workflow admission stays separate from operator lifecycle updates and generic deterministic metadata stamping. An authorized `orbit.task.update` transition to `in-progress` records the guarded pickup semantics, while a transition to `backlog` approves proposed work; neither transition dispatches, cancels, merges, or completes a workflow. The actual workflow admission boundary still accepts only `backlog` and `in-progress`, still checks plan/dependency/delivery prerequisites, and records system-actor lifecycle history. Moving a task to `backlog` can make it eligible for a later configured sweep, but the edit itself does not dispatch anything.

Activity-authored status writes carry the status the worker observed and compare it again under the task lock. If an operator reclassifies, archives, restores, or reopens the task before the delayed worker writes, the activity loses without applying its status or accompanying execution evidence. Completion authorization and delivery checks remain at their own boundaries rather than being inferred from a task's status label.

After [ORB-10464] / [Workflow admission verifies dependency delivery into the pinned base, not just lifecycle completion](./4_decisions.md#workflow-admission-verifies-dependency-delivery-into-the-pinned-base-not-just-lifecycle-completion), status is only half of readiness. `worktree_setup` also verifies that every dependency the task declares `done` has actually been delivered into the base it pins — the check runs after `base_sha` is resolved and before the worktree, the branch, or the `in-progress` transition exist, so a refusal leaves nothing behind. Delivery is decided by the `[ORB-NNNNN]` marker every Orbit commit message carries, since squash and rebase merges rewrite the sha but preserve the message: a dependency is refused only when the repository holds marked commits for it (`git log --all`, remote-tracking refs included) and none is reachable from `base_sha`. A dependency with no marked commit anywhere is not refused. The refusal is the typed `OrbitError::DependencyNotDelivered`, naming the task, the dependency, the base ref and sha, and the commits found elsewhere; `input.dependency_delivery: 'ignore'` disables the gate for work delivered under a different commit message. Nothing in the check reads GitHub, so PR-backed and local-only dependencies verify identically.


### 8.11 Task PR handoff summaries

`task_pr_pipeline` sends the selected task IDs to `pr_open` as `completed_task_ids`. Before `pr_open` pushes or creates the pull request, the deterministic action reloads each task record, checks that the task still belongs to the batch, confirms it can enter review, and requires a meaningful persisted `execution_summary` for every completed task. Empty, whitespace-only, and explicit placeholder summaries fail the PR step with an error naming the task id; generated default PR bodies also omit placeholder summary details blocks. When callers pass a non-empty `body`, `pr_open` preserves that body verbatim after the same durable-summary guard passes. This handoff contract was tightened in [T20260430-31].

After [T20260508-3], generated one-task PR bodies render the task contract first: `## Task`, optional collapsed `## Execution Summary`, `## Validation`, then `## Branch Freshness`. The task section includes the task link, description, and plain-bullet acceptance criteria so reviewers can see the requested work beside the implementation summary. Multi-task callers keep the legacy `## Tasks` plus files-changed layout until those paths are retired.

After [ORB-10644] / [Delivery fails closed against a base branch that can no longer carry work to the landing branch](./4_decisions.md#delivery-fails-closed-against-a-base-branch-that-can-no-longer-carry-work-to-the-landing-branch), `pr_open` and `pr_promote` also ask whether the base itself can still carry the work. The base name flows from `input.base_branch` through `prepare_branch` and `sync_base` untouched, and `resolve_worktree_start_point` is satisfied by any `origin/<base>` that resolves, so a base that merged and was deleted — or was restored to its pre-merge tip — still passed every check while the merge never reached the branch work lands on. A base is refused when either (a) the repository has an `origin` remote and the base branch is gone from it, or (b) an `input.landing_branch` is declared, differs from the base, and the base carries nothing that branch does not already have: the pinned `base_sha` is an ancestor of the landing tip, or every commit unique to the base is already delivered there under its task marker. Test (b) reuses the same `[ORB-NNNNN]` marker rule as §8.10, lifted into `vcs::delivery_marker` so the dependency gate and the base gate share one definition; the pinned `base_sha` is the subject and only the landing branch is resolved live. The refusal is recorded as the `obsolete-base` handoff phase and names the stale base, the landing branch, the marker that already landed, and a recovery path. Ordinary non-stacked delivery is unaffected — with no `landing_branch`, or with it equal to the base, only the remote-existence probe runs — and `input.base_obsolescence: 'ignore'` disables the gate for a base deliberately kept off `origin` or one whose commits repeat an already-landed task id.

After [ORB-00016], `pr_open` treats a branch with zero commits ahead of the selected base as a successful no-repository-diff handoff after the same durable task guards pass. This path advances completed `in-progress` tasks to `review`, returns `pr_created: false` with base/head freshness fields, and does not call GitHub PR creation or stamp `github-pr` external refs. The normal branch-with-commits path still pushes, opens the PR, returns `pr_created: true`, and records the PR ref on participating tasks.

After [ORB-10313], the VCS handoff seam uses one shared durable predicate (`reject_failed_delivery`) to block a task whose first nonblank execution-summary line is exactly `Outcome: failed`; other meaningful summary shapes remain deliverable, while empty and placeholder summaries keep their existing rejection. The predicate is enforced at two points so both fresh and resumed delivery stop against an explicit durable failure. First, `commit_batch_changes` invokes it immediately after loading the single coupled task and before it resolves the delivery checkout, stages files, mutates the index, or creates a commit — covering both the PR and local task pipelines. Second, `load_handoff_context` invokes the same predicate, so every direct or resumed `pr_prepare`, rebase, push, `pr_open`, `pr_promote`, and no-diff promotion revalidates durable state and cannot deliver a task that now reports failure. This reads the durable task record only; it does not make the advisory agent response envelope authoritative and does not teach `pipeline_success_guard` to parse task prose. This gate closes friction F2026-07-091, where `task_pr_pipeline` published a PR whose durable summary began `Outcome: failed`. See [Fail delivery before Git mutation when execution outcome is not success](./4_decisions.md#fail-delivery-before-git-mutation-when-execution-outcome-is-not-success).

### 8.12 Test surfaces guarding executor invariants

Risk-weighted regression tests live next to the executor modules they guard
under `crates/orbit-engine/src/activity_job/job_executor/tests/`
([T20260509-7]). Each executor-block module has a matching test module under
`tests/`, and each test names the specific invariant it guards in the function
name. The
current surface:

- `step.rs` (`step.rs`) — linear step success and pipeline propagation,
  failure short-circuit (mod.rs:131-148), retry `max_attempts` exhaustion,
  non-retryable bypass, success on intermediate attempt, and
  `compute_backoff_ms` linear/exponential monotonicity and cap behavior.
- `parallel.rs` (`parallel.rs`) — `JoinMode::All`, `JoinMode::Any`,
  `JoinMode::Quorum`, `StepJoin` audit event ordering, and audit
  parent-stack inheritance into branch threads.
- `fanout.rs` (`fan_out.rs`) — empty items emit `FanoutDispatched{0}`
  and `FaninJoined{0,0}`, collected outputs are spawn-index ordered even
  when workers complete out of order, `max_workers` semaphore caps
  in-flight workers, structural error surfaces under unsatisfied join,
  `fan_in.collect` writes the collected value under the alias key, and
  per-worker `WorkerState` events appear in `dispatched`→`finished` order.
- `loop.rs` (`loop_block.rs`) — `items` length over `max_iterations`
  errors, `break_when` exits with `LoopIterationEnd{broke=true}`,
  exhausting iterations emits `LoopDidNotConverge`, and the loop exits on
  first body failure.
- `pipeline_durability.rs` (`exec_ctx.rs`, `fan_out.rs:53-56`) —
  a step's output remains visible to later steps via
  `{{ steps.<id>.output.* }}`, and the pipeline snapshot taken into
  fan-out workers preserves upstream values past the fan-out boundary.

Shared host scaffolding (`ScriptedHost`, `Action`, job/step builders) lives
in `tests/mod.rs` so each block module stays focused on its own invariants.
New executor blocks must land with a matching test module under `tests/`
covering the analogous invariants — see [Each new executor block ships with a sibling test module](./4_decisions.md#each-new-executor-block-ships-with-a-sibling-test-module).

### 8.13 Recoverable automatic workflows (planned)

The current executor can recover one configured step and retry it once. This includes the
default conflict-only PR base-sync repair above, and both direct ship and auto reach it through
the same `task_pr_pipeline`. Higher-level
shipment parents still fail when a required child run fails. The planned
[`orbit run auto --recover` contract](./specs/recoverable-auto-workflows.md) adds a separate
task-scoped orchestration layer: preserve per-task child outcomes, run bounded recovery for failed
tasks, persist an evidence-gated completion disposition, and continue draining unrelated work.

This does not make agent output authoritative and does not generalize `pipeline_success_guard` into
a prose interpreter. Semantic similarity only retrieves possible already-landed implementations;
a deterministic gate rechecks delivered commits, the pinned base, and the current task's acceptance
checks before `already_satisfied` can move a task to `done`. Without the explicit recover input,
existing fail-fast job behavior remains unchanged.

---

## 9. Filesystem Policy and `fsProfile`

Both `ActivityV2` and `TargetStep` can attach an `fsProfile`. orbit-core uses `tool_context_for_activity(...)` to build the policy-aware `ToolContext`, and `V2AuditWriter` can attach filesystem audit logging so read/write denials appear in the envelope.

Runtime/CLI enforcement landed in [T20260419-0503]. `fsProfile` is therefore part of the activity/job contract, not a CLI presentation detail.

One subtlety: profile attachment happens at two layers.

- An activity asset may declare its own `fsProfile`.
- A target step may override or supply one around an inlined activity spec.

Readers must distinguish "profile on the reusable activity" from "profile on this call site."

Host-owned state is denied after both layers resolve, so no profile or provider
convenience grant can reopen it: Git metadata (see [ORB-11506]) and the recovery
authority root described in §8.6 are appended as subtree denies at the end of
the resolved modify list.

---

## 10. Legacy Surfaces and Retention Boundaries

This feature spans a migration, so the retained surfaces are explicit.

### 10.1 Retention Table

| Surface | Current status | Rationale |
|---------|----------------|-----------|
| `schemaVersion: 1` activity/job assets | Retired | Load-time hard error after [T20260419-2156]. |
| v2 `agent_loop` HTTP path | Kept | Canonical typed runtime path from [T20260418-2010]. |
| v2 `agent_loop` CLI path | Kept | Implemented by the retained `AgentRuntime` trait and `providers/*_cli.rs` after [T20260419-0104]. |
| `TargetRef` authoring form | Kept at authoring/load time only | Human-friendly YAML surface; resolved away before execution since [T20260418-2019]. |
| v1 `crate::job_runner` | Kept, condition grammar only | The older sequential/DAG runtime was removed in [ORB-10390]; the module now holds only `condition::evaluate_bool_expr`, consumed by the v2 executor's `when` and `break_when` evaluation (`job_executor/step.rs`, `job_executor/loop_block.rs`). |
| v1 executor stack (`ActivityExecutor`, `ActivityExecutorRegistry`, `direct_agent` / `external` / `cli_command` executors, v1 `ExecutionContext`, v1 `Activity`) | Removed | Deleted in [ORB-10395]. v2 dispatch consults no executor registry; executor defs are read only for provider CLI/sandbox resolution and for the `local_shell` deterministic action ([ORB-11294], see [executors/specs/local-shell.md](../executors/specs/local-shell.md)). |
| External Executor Protocol v1 (`executor_type: external`) | Removed | Never a supported surface; retired with the v1 stack in [ORB-10395]. `ExecutorType::External` still parses so pre-existing defs load, but nothing spawns them — see `docs/design/executors/4_decisions.md` §[External Executor Protocol for dynamic out-of-process executor registration (retired)](../executors/4_decisions.md#external-executor-protocol-for-dynamic-out-of-process-executor-registration-retired). |
| Legacy `run_parallel_task_pipeline` | Removed | The legacy parallel-batch executor was removed as unused in [ORB-10332]; the live pipelines still dispatch and join children through `orbit.pipeline.invoke` / `orbit.pipeline.wait`. |
| Seeded reference activities and jobs | Kept | They act as runnable contracts and examples, and were moved into init seeding in [T20260419-2347]. |

### 10.2 Seeded Assets in Practice

Seeded assets are part of the design. Today they include:

- small reference activities such as `agent_loop_reference` and `agent_loop_cli_reference`
- control-plane jobs such as `task_gate_pipeline`
- higher-level dispatch workflows such as `task_auto_pipeline`

The gate/auto assets from [T20260419-0622-3] and [T20260419-0623] exercise real v2 constructs:

- `loop + break_when`
- `fan_out + fan_in`
- cross-iteration `session:` binding
- deterministic child-job dispatch

That seeded corpus is Activity / Job's executable reference documentation.

---

## 11. Concerns & Honest Limitations

### 11.1 Provider typing is broader than provider wiring

The `Provider` enum names `claude`, `codex`, `gemini`, `grok`, `ollama`, and `openai_compat`, but `openai_compat` has no CLI runtime and therefore cannot be executed. The schema is broader than the runtime.

### 11.2 Tool allowlists are advisory at dispatch

Agent loops emit an advisory event and rely on the provider harness to enforce
the effective baseline-plus-task list. Composition is not an authorization
bypass: nested tool execution still applies caller role, host capability,
tool-specific policy, filesystem profile, subprocess allowlist, and external
authentication checks independently.

### 11.3 Some structural controls are still literals

`LoopBlock.max_iterations` and `FanOutBlock.max_workers` are structural `u32`s, not templated expressions, so workflows must fork YAML to change them dynamically.

### 11.4 Validation is split across phases

Some bad shapes fail at load time, some at job preflight, and some during dispatch. The "where will this fail?" answer is not yet uniform.

### 11.5 The audit story is powerful but split

The v2 envelope tree lives in `.orbit/state/audit/v2_loop/`, HTTP loop details materialize lazily in `.orbit/state/audit/loop/`, and payload blobs live in `.orbit/state/audit/blobs/`. Reviewers still need to know the split layout. [T20260426-0519] moved these traces under `.orbit/state/` so top-level `.orbit/` stays for config, resources, tasks, graph artifacts, and the SQLite command-audit database; [T20260506-2] stopped creating empty loop JSONL files for runs with no loop-level events.

### 11.6 The substrate still leaks into the public product story

README frames tasks, jobs, and activities as substrate. The CLI and seeded assets still expose this layer because Orbit needs it to operate today.

### 11.7 Nearby comments still carry migration-era drift

Some module prose still reflects earlier phase names or pass ordering. orbit-core entrypoints and executor behavior are authoritative.

### 11.8 Historical run inspection belongs to the run surface

Read-only history does not need the same dependencies as live execution. [T20260423-0447] kept retired workflow runs observable without live assets, [T20260425-2010] removed workflow-specific history browsers, and [T20260426-0742] removed duplicate job-level inspection aliases. Current inspection belongs to `orbit run history -j <job_id>` and `orbit run show <run_id>`; `orbit job` is for catalog browsing and direct execution.

---

## Task References

- **[ORB-11606]** — Bound heavyweight Git operations and recover only owned interrupted mutations ([Timeout recovery is not conflict or failure-handoff recovery](./4_decisions.md#timeout-recovery-is-not-conflict-or-failure-handoff-recovery)).
- **[ORB-11281]** — Route only typed task-PR rebase conflicts to one system-crew leaf, retry the same checkpoint once, preserve stale-base races, and retain normal review or authorized verified-completion gates.
- **[ORB-11488]** — Reuse that bounded pinned-rebase recovery when an already-published PR becomes `DIRTY` during authorized completion, with exact branch/PR leases and no protection or auth bypass.
- **[ORB-11982]** — Require fresh, candidate-pinned pull-request delivery evidence before the automatic `review -> done` transition, and report whether a refused candidate is behind its current base.
- **[ORB-11493]** — Refresh one authenticated stale `prepare_branch` checkpoint on preserved-candidate resume, retain source/descendant provenance, and reuse the bounded conflict-recovery and same-PR delivery tail.
- **[ORB-10770]** — Type the protocol schema's `error` as an object omitted on success, and accept the JSON string `"null"` as absent `error` so a completed Claude structured-output wrapper still counts as an envelope (see [§7.6b](#76b-structured-output-is-the-claude-prevention-layer)).
- **[ORB-10746]** — Enforce the Orbit response envelope through Claude CLI structured output (`--json-schema`), read `structured_output` as the authoritative extraction source, and map abnormal exit-0 endings to a specific failed-envelope diagnostic (see [§7.6b](#76b-structured-output-is-the-claude-prevention-layer)).
- **[ORB-10606]** — Supply the complete reviewer worktree pair and distinguish review startup failure from a reviewer rejection at the parent and task-history boundaries ([Classify independent-review startup separately from reviewer rejection](./4_decisions.md#classify-independent-review-startup-separately-from-reviewer-rejection)).
- **[ORB-10519]** — Restore one workflow-owned shipment commit, reject every provider-side HEAD change, and preserve dirty-work recovery plus process-scoped attribution ([Workflow alone creates shipment commits while dirty failures remain recoverable](./4_decisions.md#workflow-alone-creates-shipment-commits-while-dirty-failures-remain-recoverable), superseding [Preserve failed worktree state before cleanup and admit only proven task commits](./4_decisions.md#preserve-failed-worktree-state-before-cleanup-and-admit-only-proven-task-commits) and [Workflow commit authors use the persisted crew model](../auditability/4_decisions.md#workflow-commit-authors-use-the-persisted-crew-model)).
- **[ORB-10468]** — Introduce run-keyed dirty integrity recovery plus the now-superseded provider-commit admission policy ([Preserve failed worktree state before cleanup and admit only proven task commits](./4_decisions.md#preserve-failed-worktree-state-before-cleanup-and-admit-only-proven-task-commits), superseded by [Workflow alone creates shipment commits while dirty failures remain recoverable](./4_decisions.md#workflow-alone-creates-shipment-commits-while-dirty-failures-remain-recoverable)).
- **[T20260413-0141]** — Support step default inputs in jobs.
- **[T20260418-2010]** — Add the first v2 activity runtime scaffolding.
- **[T20260418-2018]** — Add `JobV2` DAG constructs (`parallel`, `fan_out`, `loop`, `retry`, `when`).
- **[T20260418-2019]** — Add v2 activity name resolution and pipeline skeleton assets.
- **[T20260418-2143]** — Wire `V2RuntimeHost` in orbit-core and add `orbit activity run-v2`.
- **[T20260418-2210]** — Reshape `V2RuntimeHost` to keep `orbit-agent` types out of orbit-core.
- **[T20260419-0002]** — Add `workspace_path` provenance to the v2 audit envelope.
- **[T20260419-0104]** — Add `backend: cli` dispatch for v2 `agent_loop`.
- **[T20260419-0339]** — Add v2 job kinds to the job catalog.
- **[T20260419-0503]** — Enforce `fsProfile` rules across runtime and CLI surfaces.
- **[T20260419-0622-3]** — Add `task_gate_pipeline`.
- **[T20260419-0623]** — Add `task_auto_pipeline`.
- **[T20260419-2156]** — Retire v1 assets and drop the transitional v2 naming.
- **[T20260419-2347]** — Seed activities and workflows on `orbit init`.
- **[T20260421-0542-2]** — Add pre-gate lock-overlap exclusion attribution to `list_backlog_tasks`.
- **[T20260423-0114]** — Expose the `backend: cli` executor-args gap during a local task ship run.
- **[T20260423-0445]** — Merge object-valued job defaults over explicit run input and persist synthetic failed job steps for early v2 pipeline failures.
- **[T20260423-2004-4]** — Persist direct v2 `orbit job run` executions into durable job-run records and state.
- **[T20260425-0204]** — Make v2 job catalog discovery honor workspace-over-global `MergeByKey` precedence.
- **[T20260425-2010]** — Refactor `orbit run` task workflow commands and remove workflow-specific history browsers.
- **[T20260426-0047]** — Make v2 activity catalog discovery honor workspace-over-global `MergeByKey` precedence and remove the public `orbit activity run` command.
- **[T20260426-0526]** — Restore v2 job invocation trace persistence so dashboard metrics surfaces can report agent and tool usage.
- **[T20260426-0519]** — Move file-backed activity/job audit traces under `.orbit/state/audit`.
- **[T20260426-0705]** — Expose v2 run audit events through `orbit run events` and `orbit run trace`.
- **[T20260426-0709]** — Align run step selectors on activity `step.id` and move CLI invocation log reading behind orbit-core runtime accessors.
- **[T20260426-0742]** — Remove duplicate job-level run inspection aliases and keep run inspection under `orbit run`.
- **[T20260426-2313]** — Stream CLI subprocess stdout/stderr through structured tracing events while retaining the existing audit/blob path.
- **[T20260426-2349]** — Move CLI tracing output redaction from `cli_runner` call sites into the default tracing formatter layer.
- **[T20260427-34]** — Add seeded pipeline success guards so non-succeeded child runs fail parent shipment workflows.
- **[T20260427-36]** — Align task-gate reservation TTL with the child dispatch wait budget.
- **[T20260427-45]** — Use freshly fetched remote base refs for default task-shipping worktrees.
- **[T20260427-48]** — Thread provider config into the v2 CLI backend and keep Codex dynamic flags exec-compatible.
- **[T20260427-51]** — Wrap cli-backend agent invocations in `sandbox-exec` on macOS.
- **[T20260428-8]** — Add explicit workflow admission for task-starting workflows and remove the plan prerequisite from those workflow starts.
- **[T20260428-10]** — Allow Codex CLI state writes under the macOS sandbox.
- **[T20260430-15]** — Embed task-aware input and run context in backend: cli agent envelopes.
- **[T20260430-19]** — Shorten the Activity / Job design docs while preserving required structure.
- **[T20260430-26]** — Release task-gate reservations after terminal child shipment runs and expose active reservations through the lock view.
- **[T20260430-27]** — Make the auto shipment output distinguish empty backlog, gated no-op, and waiting gate children.
- **[T20260430-30]** — Make auto shipment default text output human-readable while preserving JSON fields.
- **[T20260430-31]** — Require populated execution summaries before opening task PRs.
- **[T20260505-2]** — Admit accepted backlog friction reports in automatic backlog listing.
- **[T20260505-8]** — Add dashboard/runtime controls to cancel active job runs.
- **[T20260505-10]** — Release run-owned task lock reservations through engine-owned terminal cleanup and reserve-pressure reconciliation.
- **[T20260505-21]** — Add whole-run replay with `retry_source_run_id` lineage and current-definition semantics.
- **[T20260506-2]** — Lazily materialize loop audit JSONL files only when loop-level events are emitted.
- **[T20260508-8]** — Resolve backend: cli subprocess cwd from workspace context and record it in audit/tracing.
- **[T20260509-2]** — Split the v2 job executor into responsibility-focused modules without changing runtime behavior.
- **[T20260509-7]** — Establish focused test coverage for the activity/job DAG executor (linear, retry, parallel, fan-out, loop, pipeline durability) and the macOS sandbox / policy boundary.
- **[T20260509-11]** — Keep condition guards on equality-only grammar and repair the `task_auto_pipeline` empty-backlog guard.
- **[ORB-00075]** — Unify ship aliases into async `orbit run ship`.
- **[ORB-10002]** — Job-run checkpoint/resume: per-step `PipelineState` checkpoints, the terminal `interrupted` state for orphaned runs, workspace-open orphan scan, and `orbit job resume`.
- **[ORB-10470]** — Make resume a detached submission whose worker resumes from the run's own checkpoints, and reconcile blocked/re-stamped tasks against the run's explicit retry lineage ([Resume is a durable submission scoped by explicit retry lineage](./4_decisions.md#resume-is-a-durable-submission-scoped-by-explicit-retry-lineage)).
- **[ORB-10471]** — Judge a primary fast-forward against the dirt that interferes with the run instead of the primary's whole dirty state ([Primary fast-forward acceptance is decided by interference with the run, not primary dirty-state byte-identity](./4_decisions.md#primary-fast-forward-acceptance-is-decided-by-interference-with-the-run-not-primary-dirty-state-byte-identity)).
- **[T20260509-30]** — Resolve the macOS `sandbox-exec` wrapper from a trusted absolute path before CLI spawn.
- **[T20260509-38]** — Run legacy parallel-batch workers through cancellable pipeline runs so timeout failure paths return promptly.
- **[T20260509-40]** — Run CLI subprocesses in killable process groups and bound timeout-path output reader joins.
- **[ORB-00016]** — Treat no-repository-diff `task_pr_pipeline` handoffs as successful no-PR completions.
- **[ORB-00374]** — Remove the `shell` activity variant and `run_shell` dispatch (fail-closed resolution of security bug [ORB-00363]).
- **[ORB-10232]** — Model recoverable PR handoff as checkpointed job activities with exact-SHA force-push provenance.
- **[ORB-10332]** — Remove the unused Groundhog activity kind and the epic/parallel pipeline layer (`task_epic_pipeline`, `epic_orchestrator`, `pipeline_wait`, legacy parallel-batch executor).
- **[ORB-10414]** — Make HTTP replay an explicit default-off cargo feature and keep replay environment variables inert in default builds.
- **[ORB-10434]** — Extend the replay opt-in to orbit-core (`orbit-core/replay`) so its fixture-backed v2_host test keeps running hermetically instead of demanding a live credential.
- **[ORB-10456]** — Resolve provider launchers at the shared CLI spawn boundary and report provider-aware searched-location diagnostics.
- **[ORB-11808]** — Search portable Homebrew `/opt/homebrew/bin` and `/usr/local/bin` prefixes after `PATH` and `$HOME` bins so scheduled Mac drains resolve the same launcher as interactive launches.
- **[ORB-10461]** — Persist detached pipeline-worker output by run id and terminalize pre-claim exits with the captured startup diagnostic.
- **[ORB-10464]** — Verify that done dependencies are delivered into the pinned base before a worktree is created.
- **[ORB-10499]** — Identify the bounded post-recovery attempt as the duplicate implement invocation, and let a re-dispatched attempt exit on a write-gated task.
- **[ORB-10593]** — Fail dispatch at admission when a `blocked_by` target is archived, rejected, or dangling, naming the blocker.
- **[ORB-10603]** — Derive the durable `execution_summary` from the delivered change when the implementing agent persisted none, leaving the delivery gate itself unchanged ([Derive the delivery execution summary from the change, not from the agent](../worktree-artifacts/4_decisions.md#derive-the-delivery-execution-summary-from-the-change-not-from-the-agent)).
- **[ORB-10644]** — Refuse to open or promote a PR against a base branch that is gone from `origin` or has already landed on the declared landing branch ([Delivery fails closed against a base branch that can no longer carry work to the landing branch](./4_decisions.md#delivery-fails-closed-against-a-base-branch-that-can-no-longer-carry-work-to-the-landing-branch)).
- **[ORB-10604]** — Reconcile local-pipeline merges against the current in-session base while retaining the remote-mode divergence refusal.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
