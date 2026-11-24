---
name: orbit-setup
description: Sets up and maintains Orbit on a user's machine and repositories according to their needs. Use for installation, `orbit workspace init`, onboarding, provider/crew and MCP configuration, the Linux sandbox, the headless browser kit for UI checks, the scheduler (sweep clock, routines, auto-tasks), worktree GC, `orbit doctor`, upgrades, task publication and backup, remote access, the dashboard, or host and service log troubleshooting. Supports existing installations as well as first-time setup; task execution belongs to orbit and backlog dispatch to orbit-orchestrate.
---

# Orbit Setup

Help the user reach the workflow they want with the smallest necessary setup.
Inspect what is already working before proposing changes. Choose a path below;
loading this skill is not a request to install every optional component.

## Establish the target

Use the request and local evidence to establish the machine/OS, repository,
existing Orbit executable/version, and desired use: task tracking, agent-assisted
work, unattended delivery, or remote access. Ask only for choices that cannot
be inferred, such as the integration branch, provider, or which host owns a
shared workspace. Preserve the user's installation method and existing config.

## Reference files

Open a file only when its condition holds; each stands alone.

| Open when | Read |
|---|---|
| First installation or another repository: detect state, install, initialize the machine (steps 1–3) | [first-run-machine](references/first-run-machine.md) |
| Initialize a workspace (step 4): `workspace init`, MCP registration, presets; Grok's MCP server and which source Grok loaded | [first-run-workspace](references/first-run-workspace.md) |
| Verify and hand off a first run, or choose what to set up next | [first-run-verify](references/first-run-verify.md) |
| Add a Copilot, Antigravity or Muse crew; fix provider authentication; pass Claude or Codex credentials, per-crew API key, stale service environment; crew selection, provider aliases, a dispatch refusal | [providers](references/providers.md) |
| Linux execution prerequisites or sandbox failure | [linux-sandbox](references/linux-sandbox.md) |
| Windows host (Orbit runs inside WSL2) | [windows-wsl2](references/windows-wsl2.md) |
| macOS: Claude worker token and the clock env file | [macos](references/macos.md) |
| Headless browser kit (`orbit config browser-kit`) so workers can render and screenshot UI changes | [browser-checks](references/browser-checks.md) |
| Where config lives and how layers merge; `seeding.*`: which shipped routines and auto-tasks a workspace carries, reconciling a policy change, excluded defaults | [configuration-layers](references/configuration-layers.md) |
| Choose an MCP role, configure global MCP roles and toolsets, understand server flag precedence, or inspect the advertised surface | [configuration-mcp-roles](references/configuration-mcp-roles.md) |
| Settings table for workflow policy: base branch, default crew, crew pools, generated paths, lock modes, CI infrastructure retries, provider limits, task types | [configuration-settings-workflow](references/configuration-settings-workflow.md) |
| Settings table for host pressure, forecast admission, GitHub quota, `execution.*`, `tasks.id_start`, fixed limits | [configuration-settings-host](references/configuration-settings-host.md) |
| `review.*` settings, review baseline commands and owed host evidence | [configuration-review](references/configuration-review.md) |
| PR, commit and branch templates, PR labels and reviewers, the PR screenshot section | [configuration-pull-requests](references/configuration-pull-requests.md) |
| Per-tag instruction files and review-skip tag mappings | [configuration-tags](references/configuration-tags.md) |
| Completion holds; owner completion policy for distributed-drain handoffs (`review` versus automatic owner landing) | [configuration-completion](references/configuration-completion.md) |
| Standing-drain keeper (`workflow.standing_drain.*`): what it submits, keys and ranges, enabling or turning it off, verifying with `orbit doctor --check standing-drain` | [configuration-standing-drain](references/configuration-standing-drain.md) |
| Configure or verify mid-run steering, including rollout timing and limits | [configuration-mid-run-steering](references/configuration-mid-run-steering.md) |
| The fixed bounds of fleet notices, the attention feed, the resident evolution ledger and the failing-job warning; host capability labels (`machine.labels`, `requires_labels`, placement waits) | [configuration-fleet](references/configuration-fleet.md) |
| Experiments: `[experiments]` settings, config overlays, lifecycle, shadow/live modes, readouts; crew comparison | [configuration-experiments](references/configuration-experiments.md) |
| Executor image input: `spec.image_input`, `vision = false`, the canary and visual hold, `orbit agents vision-probe` | [configuration-image-input](references/configuration-image-input.md) |
| Recovery settings: generated-file conflict recovery, prose-only validation, CI infrastructure reruns | [configuration-recovery](references/configuration-recovery.md) |
| Crews, human crews (`kind = "human"`), crew selection; every `orbit` command fails to load because a crew name contains `:` (repair the file by hand) | [configuration-crews](references/configuration-crews.md) |
| Automatic per-draw effort (`<crew>@auto` bands, `workflow.auto_effort.*`): which pools accept it, verifying a resolved effort without dispatching work | [configuration-auto-effort](references/configuration-auto-effort.md) |
| Activity wall-clock timeouts, environment passthrough, filesystem profiles, executor definitions (host OS support, auth probes, inspection tools, shared Cargo-cache grants) | [configuration-execution](references/configuration-execution.md) |
| Activity-scoped workspace knowledge: `[workflow.agent_corpus]`, `INDEX.md`, verifying `activity.agent_corpus_applied` | [configuration-agent-corpus](references/configuration-agent-corpus.md) |
| Landed-delivery line classes (`workflow.test_paths`, `workflow.non_code_paths`); backfilling with `migrate --backfill-diff-stats` | [configuration-diff-classification](references/configuration-diff-classification.md) |
| Tune a repository's workspace policy from evidence, steps 1–5: base branch, required validation, footprint locks, generated files, light delivery | [workspace-tuning-policy](references/workspace-tuning-policy.md) |
| Tune crew routing (task types, complexity and purpose pools, draw policy), review layers, analytics and regression fix layers | [workspace-tuning-crews](references/workspace-tuning-crews.md) |
| Keep one task out of automatic drains while an interactive session owns it: the reserved `no-drain` tag, its admission and approval effects, verification | [workspace-tuning-no-drain](references/workspace-tuning-no-drain.md) |
| Roll out a tuned policy, optional features, a worked example | [workspace-tuning-rollout](references/workspace-tuning-rollout.md) |
| Turn the scheduler on, enable or disable a routine as an operator, verify it, tell an enabled routine from a host-local pause or the host clock, "why didn't it fire?" | [automation-scheduler](references/automation-scheduler.md) |
| The seeded routines, authoring a routine, changing a trigger, condition triggers, routines a plugin seeds | [automation-routines](references/automation-routines.md) |
| Final recovery of blocked tasks, red-base re-checks, Linux CodeQL evidence for held reviews, GitHub evidence sweeps | [automation-built-in](references/automation-built-in.md) |
| Workspace scripts (`target: script:<name>` job steps): grant or revoke one, grant staleness, network and root grants, verify a grant | [automation-scripts](references/automation-scripts.md) |
| Configure task-pilot eligibility or freshness, inspect readiness, request a later assessment; see also [task-pilot admission](../orbit-orchestrate/references/orchestration-pilot-admission.md) | [automation-task-pilot](references/automation-task-pilot.md) |
| Create a recurring QA or other task template: fields, dedupe, skipping a tick, completion contracts | [auto-tasks-authoring](references/auto-tasks-authoring.md) |
| Manage definitions; grant tools to every minted task, including the upgrade rule before setting one | [auto-tasks-managing](references/auto-tasks-managing.md) |
| Adopt a current bundled body from a stale fork | [auto-tasks-adopting](references/auto-tasks-adopting.md) |
| Delete a definition, definitions a plugin seeds | [auto-tasks-deleting](references/auto-tasks-deleting.md) |
| The seeded definitions from `qa-sweep` to `host-health` | [auto-tasks-catalog-reviews](references/auto-tasks-catalog-reviews.md) |
| The seeded definitions from `pilot-block-patterns` to `agent-guidance-audit`; shipped defaults promoted on proven yield | [auto-tasks-catalog-audits](references/auto-tasks-catalog-audits.md) |
| Delivery consumers: stalls, automatic replay, `reset` | [auto-tasks-delivery](references/auto-tasks-delivery.md) |
| Scheduler/service logs and host incidents; inspect one CLI audit event by ID | [operational-logs-triage](references/operational-logs-triage.md) |
| Prompt-size cost estimates, token usage and unknown cost; delivery profiles and admission diagnostics | [operational-logs-cost](references/operational-logs-cost.md) |
| A task held after a provider failure: which crews, accounts and hosts it excludes and when it clears | [operational-logs-provider-holds](references/operational-logs-provider-holds.md) |
| Worker resource containment: a run killed or throttled by a worker limit | [operational-logs-containment](references/operational-logs-containment.md) |
| Install plugins: `orbit <ns>` commands, dashboard panels, the `.orbit/plugins.yaml` pin file, switching one off in a workspace | [plugins-install](references/plugins-install.md) |
| Certify a plugin for this Orbit; a plugin that is not serving its tools | [plugins-certify](references/plugins-certify.md) |
| Plugin permissions, grants and secrets | [plugins-permissions](references/plugins-permissions.md) |
| Decision channels for plugin answer submission | [plugins-decision-channels](references/plugins-decision-channels.md) |
| Plugin backends: Python with dependencies, built from source | [plugins-backends](references/plugins-backends.md) |
| Manage a legacy external tool (`orbit tool add`, `list`, `show`, `run`, `enable`, `disable`, `remove`, `doctor`); migrate one to a plugin | [plugins-legacy-tools](references/plugins-legacy-tools.md) |
| Host-local application acting on an operator's consent for exact tool calls: credentials, callbacks, rotation, revocation | [applications](references/applications.md) |
| MCP clients, SSH federation, role and toolset selection, authority on the destination, TCP MCP | [remote-access-mcp](references/remote-access-mcp.md) |
| Remote dashboard and switching hosts in the dashboard | [remote-access-dashboard](references/remote-access-dashboard.md) |
| Owner/replica roles, multiple machines, a breaking workspace-layout upgrade across hosts | [multi-host](references/multi-host.md) |
| Task snapshots: bind a workspace, transfer the source repository, HTTPS credentials, the weekly routine | [publication-bind](references/publication-bind.md) |
| Publish a task snapshot, inspect one, restore deliberately | [publication-snapshots](references/publication-snapshots.md) |
| `orbit doctor` repairs and task locks | [maintenance-diagnose](references/maintenance-diagnose.md) |
| Worktree GC and workspace scratch | [maintenance-worktree-gc](references/maintenance-worktree-gc.md) |
| Upgrade, resource sync, persistent MCP clients during upgrades, database and layout migrations, offline SQLite permission repair | [maintenance-upgrades](references/maintenance-upgrades.md) |
| Deploy a local build pinned to a source commit; reclaim what deploys leave behind | [maintenance-local-deploys](references/maintenance-local-deploys.md) |
| Workspace removal or teardown, source-remote rebinding, re-pointing this host at a moved checkout (`workspace rebind --path`) | [maintenance-removal-and-rebind](references/maintenance-removal-and-rebind.md) |
| Installed skill catalog or user-level skill links (`orbit skill list`, `show`, `doctor`, `link`); a client cannot find a skill | [maintenance-skills](references/maintenance-skills.md) |
| Retired features and their leftover keys, commands or toolset names, or a removed command, MCP tool name or task input: what to delete, what replaces it, the deadline | [maintenance-retired-features](references/maintenance-retired-features.md) |
| Deprecated keys that are translated, ignored or fixed; verify the cleanup with `orbit doctor` | [maintenance-deprecated-keys](references/maintenance-deprecated-keys.md) |
| Store retention, admission receipt storage, audit events, logs, search indexes, what is evidence and must not be edited | [maintenance-storage](references/maintenance-storage.md) |
| Operator procedure for mid-run steering | [mid-run steering](../orbit-orchestrate/references/mid-run-steering.md) |
| Single-owner distributed drain setup or claimed-attempt recovery | [distributed drain setup](../orbit/references/setup/distributed-drain-setup.md) |
| Compare implementer models or crews: `orbit agents rank` | [model ranking](../orbit-orchestrate/references/model-ranking.md) |

Task tracking alone does not require a provider, PR credentials, a scheduler or
remote hosts. Add those only for the requested workflow. Agent execution needs
an authenticated supported provider and working sandbox; PR delivery also needs
its Git/PR credentials. Task search uses local SQLite FTS5 and requires no download.

## Apply and verify

1. Inspect installed command help, effective config and relevant prerequisites.
   Use the owning host's registered tools for durable state; see the shared
   [tool surface](../orbit/references/tool-surface-routing.md) for authority and routing.
2. Make the authorized configuration changes. Existing approval carries forward;
   do not ask again for routine steps already covered. New scheduling, downloads,
   remote roles or credentials are separate choices when the request omits them.
   Keep secrets out of task records and examples.
3. Verify the requested workflow through its actual client and destination.
   A version string is not proof of a working MCP connection; discover the
   intended workspace and exercise an audited operation. Use disposable fixtures
   for execution checks unless running real work is authorized.
4. Report what changed, what works, and any specific remaining operator action.
   Distinguish installed, configured and enabled. Return to
   [orbit](../orbit/SKILL.md) for task work or
   [orbit-orchestrate](../orbit-orchestrate/SKILL.md) for authorized dispatch.

## Preserve existing work

An upgrade must preserve running-client compatibility or refuse before changing
live state. Check long-lived MCP and service processes, not only active jobs.
Never delete admission/identity files, bypass schema guards, or start a shadow
store to recover access. Follow [maintenance-upgrades](references/maintenance-upgrades.md).

Seeded schedules are disabled until an operator deliberately enables them, except
the `worktree-gc` and `task-pilot` routines, which seed enabled when the
seeding policy selects them. The `host-health`
auto-task seeds disabled. Preserve
existing schedule and enablement choices on an established host. Shared repositories
need an explicit owner; copying a checkout does not migrate its control plane.
