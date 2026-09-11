---
type: runbook
summary: Post-v0.18.0 release survey and breaking-change handoff.
tags: [operations, release, survey]
last_validated: 2026-09-06
---

# Post-v0.18.0 release survey

This is a release-drafter handoff, not release metadata. It records the
user-visible work between the last release and the pinned integration head,
and identifies candidates for the human confirmation step in
[`RELEASING.md`](../../RELEASING.md).

## Survey boundary

- Baseline tag: `v0.18.0` at `6bbc693fa3ab9ea87f4184088f2fb930258bb542`.
- Head: `agent-main` at `d06324053ad858ec9200933834532eff7c138e75`.
- Range: `v0.18.0..d06324053ad858ec9200933834532eff7c138e75` — 160 commits.
- Evidence: the `agent-main` checkout, the range diff/log, and authoritative
  `ws_orbit` task outcomes. The pinned head was not refreshed.

## User-visible themes

### Execution lanes and provider contracts

ORB-11279 and ORB-11286 add validated per-crew effort forwarding. ORB-11294
restores deterministic `local_shell`; ORB-11296 adds Pi; ORB-11295 adds
OpenCode; and ORB-11299 makes Antigravity (`agy`) the current Google CLI lane
while retaining the legacy Gemini lane. ORB-11303 documents the onboarding
contract. The shipped evidence is the executor catalog and provider registry:

- `crates/orbit-core/assets/executors/{antigravity,gemini,local-shell,pi,opencode}.yaml`
- `crates/orbit-core/src/application/executor.rs:6-13`
- `crates/orbit-agent/src/runtime/backend.rs:47-58`
- `crates/orbit-cli/src/command/init/agent_detect.rs:104-185`
- `docs/CONFIG.md:124-126,172-194,583-638`

The provider work is fixture-tested, with live-provider limits called out in
the task outcomes: ORB-11299 verified `agy` 1.1.27 but did not claim an
authenticated run; ORB-11295 and ORB-11296 likewise used fixture/fake-CLI
coverage where the provider was not installed or authenticated.

### Automation, orchestration, and delivery controls

ORB-11330 establishes the `orbit-automation` crate and shared delivery/coverage
consumers. ORB-11331 adds state-driven preparation and failure triage with
fingerprints, bounded retries, incident coupling, and durable receipts. The
new behavior is opt-in/disabled by default where the task outcomes say so;
existing Core/Store ownership and no-live-automation policy remain intact.
ORB-11280 adds `orbit update`; ORB-11283 adds `orbit run auto --stop`; ORB-11313
adds session-scoped task orchestrator attribution; ORB-11372 adds the optional
`--allow-crew` / MCP `allowed_crews` restriction. These are additive controls
with unrestricted compatibility preserved.

Evidence:

- `crates/orbit-automation/src/members/preparation.rs:7-49`
- `crates/orbit-automation/src/members/incidents.rs:18-46`
- `crates/orbit-automation/src/delivery/`
- `crates/orbit-core/assets/activities/{prepare_task_pilot,task_pilot,apply_task_pilot_results}.yaml`
- `crates/orbit-cli/src/command/run/auto.rs`
- `crates/orbit-cli/src/command/update.rs` and `crates/orbit-cmd/src/update/`

### Task artifacts and visual consumption

ORB-11413 adds `orbit task artifact get`, the read-only MCP
`orbit.task.artifact.get` tool, dashboard artifact retrieval, bounded base64
for binary data, and safe raster-image content blocks. PNG/JPEG/GIF/WebP are
byte-checked; SVG and HTML remain opaque. Existing artifact storage remains
byte-oriented and `schema_version: 1` is unchanged.

Evidence:

- `crates/orbit-types/src/task/model.rs:504-660`
- `crates/orbit-tools/src/builtin/orbit/task/artifact_get.rs:8-57`
- `crates/orbit-core/src/adapter/tool_host/json.rs:213-279`
- `crates/orbit-mcp/src/adapter/structured.rs:10-71`
- `docs/design/task-artifacts/2_design.md:149-205`

This is optional additive behavior: callers that do not use the new retrieval
tool or image presentation see the existing attach/list path, and no stored
artifact migration is required.

### Distribution, workspace operations, and reliability

ORB-11426 adds explicit `workspace source-remote show|rebind` while preserving
workspace/task/checkout identity and refusing publication-bound changes until
the operator resolves them. ORB-11427 updates active constellation-works
distribution references and adds a pre-cutover checklist; it did not transfer
credentials, publish a release, mutate a registry, or tag a commit. ORB-11379
added a release-gated website publication path during this survey window; that
automation is retired, and Daniel now deploys the website manually. ORB-11354
adds an operator-only agent-invocation path, and ORB-11418 closes recursive
test-worker spawning.

Evidence:

- `crates/orbit-cli/src/command/workspace/source_remote.rs`
- `crates/orbit-registry/src/workspace_registry/`
- `RELEASING.md:72-117`
- `docs/runbooks/state-and-backup.md:163-218`
- `website/README.md` and `docs/runbooks/website-validation.md`

## Breaking-change candidates for human confirmation

Per `RELEASING.md`, these are candidates only. The release drafter must accept,
downgrade, or reject each one before choosing the version.

### Candidate 1 — Antigravity becomes the detected Google default (ORB-11299)

- Contract: fresh detection/configuration and the managed default executor.
- Before: a host with the Gemini CLI seeded `gemini`, invoking `gemini` with
  `--approval-mode yolo`, `--allowed-mcp-server-names orbit`, and `-o json`.
- After: `agy` is detected as `antigravity` ahead of `gemini`, invoking the
  stream-json stdin/stdout contract with `--dangerously-skip-permissions`; the
  shipped model is `gemini-3.8-flash-high`.
- Compatibility: explicit `gemini` definitions, the `gemini` executor, legacy
  crews, and customized assets remain readable; untouched managed defaults
  migrate through provenance. A fresh host with `agy` but no authenticated
  Antigravity session can therefore have a different prerequisite/failure
  mode. This is a real default-behavior change, but current policy does not
  explicitly classify provider preference changes, so human classification is
  required.
- Code evidence: `crates/orbit-cli/src/command/init/agent_detect.rs:168-188`,
  `crates/orbit-core/assets/executors/{antigravity,gemini}.yaml:1-27`, and the
  ORB-11299 outcome.

### Candidate 2 — Built-in automation activity contracts changed shape
  (ORB-11330, ORB-11331)

- Contract: YAML `input_schema_json`/`output_schema_json` for shipped
  task-pilot and CI-failure activities.
- Before: `apply_task_pilot_results` required `crew` and emitted
  `status: success`; `prepare_task_pilot` did not require `source`; `task_pilot`
  did not require `inspection_revision`; `file_ci_failure_tasks` did not
  require `pilot_candidates`/`deferred` or nested `match_kind`/
  `match_evidence`.
- After: `apply_task_pilot_results` accepts the new authorization shape and
  requires `status` in `succeeded|failed` plus `error`, `source`, partition
  counts, stale-partition data, and CI-sweep admission; `prepare_task_pilot`
  requires `source`; `task_pilot` requires `inspection_revision`; and the CI
  filing output requires the new evidence fields.
- Compatibility: the bundled jobs and consumers were updated together, and
  the task outcomes report validated current pipelines. No separate migration
  or compatibility version for user-authored copies of these schema contracts
  was found in the range. A custom producer/consumer pinned to the old output
  can fail validation or reject the new status, which meets the runbook's
  activity/job schema candidate definition. Human confirmation should decide
  whether these are internal implementation assets or a released contract.
- Code evidence: compare the baseline files with current files:
  `.orbit/resources/activities/apply_task_pilot_results.yaml:18-52`,
  `.orbit/resources/activities/prepare_task_pilot.yaml:19-56`,
  `.orbit/resources/activities/task_pilot.yaml:19-50`, and
  `.orbit/resources/activities/file_ci_failure_tasks.yaml:65-160`.

### Candidate 3 — `cli_command` is renamed to `local_shell` (ORB-11294)

- Contract: serialized `ExecutorType` / executor resource value.
- Before: `executor_type: cli_command` and `ExecutorType::CliCommand`.
- After: canonical `executor_type: local_shell` and `ExecutorType::LocalShell`,
  with `#[serde(alias = "cli_command")]`.
- Compatibility: old bundled and user-authored definitions still load, while
  reserialization emits `local_shell`; the action is intentionally a new
  deterministic shell boundary, not the old agent-shaped v1 path. This is a
  rename with an explicit compatibility alias, so it is probably non-breaking
  under the current policy, but consumers comparing serialized canonical names
  are the uncertainty to confirm.
- Code evidence: `crates/orbit-types/src/workflow/executor_def.rs:12-40`,
  `crates/orbit-core/assets/executors/local-shell.yaml:5-20`, and the ORB-11294
  outcome.

## Investigated changes not currently classified as breaking

- ORB-11290 removes `model_pair_override` from shipped defaults, but the field
  remains readable, persistent, and runtime-compatible for older/customized
  definitions. Current code documents it as legacy at
  `crates/orbit-types/src/workflow/executor_def.rs:134-181`; the executor tests
  cover both omission in fresh defaults and preservation in customized files.
- ORB-11413 is additive retrieval/presentation. Task artifact storage remains
  byte-oriented and schema version 1; safe image blocks are an additional MCP
  content representation, not a replacement for `content_base64`.
- ORB-11330/ORB-11331 introduce automation consumers, receipts, and state
  diagnostics without changing the task bundle schema or enabling live
  automation by default.
- ORB-11280, ORB-11283, ORB-11313, ORB-11372, ORB-11426, and ORB-11427 add
  optional commands, controls, attribution, or cutover configuration while
  preserving existing inputs and identity. ORB-11427's active owner URLs are
  user-visible metadata, but no remote publication or registry mutation was
  performed.

## Version recommendation and release gate

Provisional recommendation: prepare `0.19.0` if the human confirms Candidate 1
or Candidate 2 (or treats Candidate 3's canonical serialization as a released
contract). `RELEASING.md` defines a pre-1.0 breaking release as a minor bump.

If the human confirms that Antigravity preference is a fresh-install default
only and that the automation YAML is an internal, co-versioned asset contract,
then the remaining surveyed work is additive or compatibility-preserving and
`0.18.1` is the conservative patch recommendation. No version bump should
start until this decision is recorded.

No release metadata, tag, publication, registry mutation, or release artifact
was created by this survey.

## Validation and handoff

- Passed: authoritative `ws_orbit` task reads for ORB-11299, ORB-11290,
  ORB-11330, ORB-11331, and the related user-visible tasks cited above.
- Passed: `git rev-parse --verify v0.18.0` and `git rev-parse --verify HEAD`;
  head is the pinned `d06324053ad858ec9200933834532eff7c138e75`.
- Passed: `git rev-list --count v0.18.0..HEAD` (`160`) and range diff/log review.
- Not run: Rust tests and full CI; this is a docs-only survey and no source
  behavior was changed.
- Pending pipeline step: commit and push this tracked report. The managed
  execution envelope owns commit/push, so this worktree intentionally leaves
  the report uncommitted and does not claim publication.
