---
type: runbook
summary: Post-v0.24.0 release survey and breaking-change handoff.
tags: [operations, release, survey]
last_validated: 2026-09-28
---

# Post-v0.24.0 release survey

This is a release-drafter handoff, not release metadata. It groups the
user-visible changes since v0.24.0 and leaves compatibility decisions for the
human confirmation step in [RELEASING.md](../../RELEASING.md). The survey was
requested by ORB-13670 and is also the evidence handoff for the canonical
`Prepare v0.25.0 release` task, ORB-13671 (proposed when checked).

## Survey boundary

- Baseline tag: `v0.24.0` at `abca0ee670d1a4e633990974356e889fded97878`; its
  workspace version is `0.24.0`.
- Survey head: `agent-main` at
  `f33e53b4aa5c172489619f56c4a01efc0371e4b7`.
- Range: `v0.24.0..f33e53b4aa5c172489619f56c4a01efc0371e4b7` — 458
  non-merge commits and 365 unique referenced task IDs.
- The executor checkout was at `de46ce86e10c62130e12ccf9dabdfb156a70c4bf`,
  later than the survey head. The range analysis and cited code evidence use
  the pinned `agent-main` head above; later commits were not included.
- Evidence: the pinned range log and diff, the CLI/MCP and seeded asset
  changes, current source at the pinned head, and Orbit task records for the
  release probe, canonical release task, and selected feature contracts. The
  365 task IDs were not each looked up individually; the range is summarized
  by user-facing themes as required by the survey rule in `RELEASING.md`.

## User-visible themes

### Distributed execution and delivery

ORB-13625 adds follower pull from an owner workspace, claim binding and
settlement, and `orbit run auto --pull`; ORB-13637 and ORB-13642 complete the
owner handoff path. ORB-13271 routes `orbit run ship` through a
plugin-contributed delivery job, and ORB-13272 reaps its worktree after
successful delivery. The follower runs one owner-admitted task at a time and
returns a typed handoff; the owner retains landing authority.

Evidence: `crates/orbit-cli/src/command/run/auto.rs:18-46,118`,
`crates/orbit-core/assets/jobs/workspace_pull_pipeline.yaml:1-14,27-43`,
`crates/orbit-core/assets/activities/resolve_delivery_job.yaml:1-28`, and
`crates/orbit-tools/src/builtin/orbit/drain/pull.rs:7-14`.

### Recurring work and task operations

ORB-12930, ORB-12931, and ORB-12932 add disabled-by-default documentation,
run-failure, and backlog-hygiene auto-tasks. ORB-13007 adds deletion and
restoration of shipped defaults with a durable opt-out. ORB-13633 hides
plugin-seeded definitions while their plugin is disabled, and ORB-13636 adds a
full-code-review coordinator. These extend scheduling and curation without
replacing existing task records.

Evidence: `crates/orbit-core/assets/auto_tasks/doc-duties.yaml:1-12`,
`crates/orbit-core/assets/auto_tasks/backlog-hygiene.yaml:1-12`,
`crates/orbit-core/assets/auto_tasks/run-failure-patterns.yaml:1-10`,
`crates/orbit-core/assets/auto_tasks/full-code-review.yaml:1-14`, and
`crates/orbit-cli/src/command/auto_task/delete.rs:6-11` plus
`crates/orbit-cli/src/command/auto_task/restore.rs:8-12`.

### Plugin authoring, secrets, and controls

ORB-13080 through ORB-13082 add declared plugin secrets, host-side secret
storage, delivery to backends, and rotation. ORB-13017 supports Python exec
backends. ORB-13236 through ORB-13360 add brokered plugin calls for sandboxed
agents. ORB-13249 and ORB-13274 add workspace-scoped plugin enablement and
dashboard controls. ORB-13626 changes the source tree layout; that compatibility
question is listed below.

Evidence: `crates/orbit-types/src/plugin/manifest.rs:19-26,233-258`,
`crates/orbit-cli/src/command/plugin/secret.rs:1-6,34-61`,
`crates/orbit-core/src/runtime/plugin/config.rs:64-83`, and
`crates/orbit-web/assets/dashboard/js/plugins.js:123-149`.

### Agent tool policy and worker containment

ORB-13315 through ORB-13317 add deny-list policy to shipped agent-loop
activities and move those defaults from tool/program allow-lists to
disallow-lists. The existing allow-list form remains supported for custom
activities, and the shipped list names sensitive operations to deny. ORB-13241
adds opt-in strict worker containment while retaining warn-and-launch as the
default. These are visible execution-policy changes; the reported task
compatibility contract preserves custom activity YAML and persisted records.

Evidence: `crates/orbit-core/assets/activities/agent_implement.yaml:221-235`,
`crates/orbit-core/assets/activities/agent_review_repair.yaml:168-182`,
`crates/orbit-core/assets/activities/task_pilot.yaml:331-367`, and
`docs/design/activity-job/2_design.md:513-550`.

### Upgrade safety, dashboard, and privacy

ORB-13631 makes upgrade admission use store/layout compatibility generations
and corrects older migrations that were marked write-safe. ORB-13634 makes an
active distributed drain visible in the dashboard. ORB-12924 publishes the
privacy policy, and ORB-13354 changes the website's default theme to light.
ORB-13638 makes task-pilot freshness inputs configurable; ORB-13658 improves
worktree cleanup on replica workspaces.

Evidence: `crates/orbit-store/src/driver/sqlite/migration/ledger.rs:98-341`,
`crates/orbit-store/src/workflow/layout/mod.rs:101-124`,
`crates/orbit-web/assets/dashboard/js/distributed.js:128-150`, and
`website/src/content/docs/privacy.md:1-14`. Further evidence:
`crates/orbit-automation/src/members/preparation.rs:40-58` and
`crates/orbit-cli/src/command/gc.rs:62-103`.

## Breaking-change candidates for human confirmation

The classifications below apply the rules in `RELEASING.md:25-41`. Candidates
are not release decisions; the human accepts, downgrades, or rejects each one
in step 3.

### Candidate 1 — Plugin source root moved to `.orbit-plugin/` (ORB-13626)

- Contract: the directory/archive source format accepted by `orbit plugin add`,
  `validate`, `test`, `sync`, and `upgrade`.
- Before: a plugin source could place `plugin.yaml` at its source root.
- After: the plugin must be inside `.orbit-plugin/`; a top-level-only
  `plugin.yaml` is refused, with no fallback. `orbit plugin scaffold` now
  creates that nested source shape.
- Evidence: `crates/orbit-tools/src/plugin/source.rs:8-11,714-748` and
  `docs/design/plugins/1_scope.md:204-224`.
- Why flagged: this changes an existing plugin-author input contract and
  requires source trees to move files. `RELEASING.md` does not name plugin
  source layout directly, so the human should decide whether this is a
  released compatibility contract or an authoring-format transition.

### Candidate 2 — Required `base_sync` added to a shipped activity output
  (ORB-13371)

- Contract: output from the `resolve_workspace_ship_input` activity.
- Before: the output schema required `mode` and `base_branch`.
- After: it also requires `base_sync` (`remote` or `local`), and bundled
  consumers pass it into workspace shipment.
- Evidence: `crates/orbit-core/assets/activities/resolve_workspace_ship_input.yaml:8-27`.
- The bundled workspace pipeline passes `base_sync` through to child shipment:
  `crates/orbit-core/assets/jobs/workspace_auto_pipeline.yaml:127`.
- Why flagged: an override or consumer using the previous activity contract
  may not produce or accept this required output field. The bundled workflow
  was updated in the same change. Human confirmation should decide whether
  this co-versioned activity contract is public; this is not an optional field
  with a safe default at the schema boundary.

### Candidate 3 — Upgrade compatibility classifications tightened (ORB-13631)

- Contract: whether an older binary or already-running process may continue
  writing a store after a newer binary has migrated it.
- Evidence: store migrations 8, 12, 16, 18, 21, 27, and 28, plus layout
  migration 2, change from `Additive` to `ReadCompatible`. The range also adds
  store migrations 29-33: 29-32 are additive; 33 adds job-run ID allocations
  and is read-compatible. See `crates/orbit-store/src/driver/sqlite/migration/ledger.rs:98-341`,
  `crates/orbit-store/src/workflow/layout/mod.rs:101-124`, and
  `docs/design/state-compatibility/2_design.md:24-60`.
- Compatibility effect: an older store reader may still read state but cannot
  safely write it; layout state cannot be write-gated and an older binary
  refuses a newer layout after a read-compatible migration. No old migration
  SQL or task record fields changed in this correction.
- Why flagged: this narrows downgrade/concurrent-writer behavior but prevents
  writes that can leave newer state inconsistent. The release rules do not
  classify this compatibility correction directly. Ask the human whether the
  previous ability to write across a misclassified migration was a supported
  contract or invalid behavior guarded by a documented safety rule.

### Candidate 4 — Optional fields added to existing MCP tool inputs
  (ORB-13633, ORB-13078)

- `orbit_auto_task_list` adds optional `include_inactive_plugins`.
- `orbit_friction_update` adds optional `rehome_to`.
- Evidence: `crates/orbit-cli/tests/snapshots/mcp_tools_list.json:143-158,633-693`.
  The pinned snapshot adds eight tool names and removes none; these are the only
  existing input schemas whose structural shape changed in the comparison.
- Why flagged: `RELEASING.md` explicitly lists MCP input-schema changes as
  breaking, while its general rule treats new optional fields with safe
  defaults as non-breaking. Both new fields are optional and preserve existing
  calls. Human confirmation should resolve how the MCP schema rule applies to
  additive optional parameters, especially for clients that pin exact schema
  fingerprints.

## Other breaking-rule checks

- CLI: comparison of changed CLI help snapshots found added flags and no
  removed flag; the top-level command enum has no removal. No CLI command or
  flag removal candidate was found.
- Seeded assets: no shipped activity, job, or skill was removed in the range.
  `agent_implement_contract_phrases.txt` was removed as an internal prompt
  helper, not a seeded activity/job/skill.
- Task data: `TaskStatus`, `TaskPriority`, `TaskComplexity`, and `TaskType`
  retain the same variants at both ends of the range; the serialized `Task`
  fields also remain the same. No task-field enum migration candidate was
  found.
- `.orbit/` layout: supported layout version 3 was already present at the
  baseline. No new layout migration was added in this range. Store migrations
  add audit/ID-allocation state and do not remove or reinterpret task records;
  their older-writer compatibility impact is covered by Candidate 3.

## Candidate version and human decisions

Provisional recommendation: **v0.25.0** (minor), because Candidates 1 and 2
change existing source/activity input-output contracts unless the human
classifies them as internal co-versioned formats. Candidate 3 needs a decision
about older-writer and rollback support. Candidate 4 is likely non-breaking
under the safe-optional-field rule, but the explicit MCP-schema rule needs the
human's interpretation.

If the human determines that none of Candidates 1-3 is a released breaking
contract, and treats Candidate 4 as an additive optional-field change, the
patch candidate is **v0.24.1**. Record the decisions before changing the
candidate in ORB-13671. Do not start a version bump until the in-flight queue is
settled or the human authorizes proceeding, as required by `RELEASING.md` step 1.

## Validation and handoff

- Passed: baseline tag resolves to `abca0ee670d1a4e633990974356e889fded97878`
  and its `Cargo.toml` version is `0.24.0`.
- Passed: pinned head resolves to
  `f33e53b4aa5c172489619f56c4a01efc0371e4b7`; the non-merge range contains 458
  commits and 365 unique referenced task IDs.
- Passed: diff review of existing CLI help snapshots, MCP tool-list snapshot,
  seeded activity/job/skill assets, task fields/enums, and migration registries.
- Passed: `scripts/generate-doc-indexes.sh`; `docs/INDEX.md` now lists this
  report.
- Passed: `make ci-lint` and `make goldens`.
- Failed: `make ci-fast` stops at `sync-plugin-skills`: the embedded
  `crates/orbit-core/assets/skills/orbit-orchestrate/references/workflows.md`
  omits the delivery-job sentence present in
  `plugin/skills/orbit-orchestrate/references/workflows.md`. The same failure
  reproduced after reverting this run's report and index changes to the
  starting checkout. It is pre-existing to this survey output; the affected
  skill mirror remains a release follow-up.
- No version files, `CHANGELOG.md`, tag, publication, or release state was
  changed by the survey.
