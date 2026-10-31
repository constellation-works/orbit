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

| User need | Reference |
|---|---|
| First installation or another repository | [First run](references/first-run.md); for an agent lane, continue with [Provider setup](references/providers.md#provider-setup) |
| Add a Copilot, Antigravity or Muse crew; fix provider authentication | [Configuration — Provider setup](references/providers.md#provider-setup) |
| Every `orbit` command fails to load configuration because a crew name contains `:`; repair the defining file by hand | [Configuration — Crew names containing a colon](references/configuration.md#crew-names-containing-a-colon) |
| Explain crew selection, provider aliases or a dispatch refusal | [Configuration — Provider identity and resolution](references/providers.md#provider-identity-and-resolution) |
| Configure task-pilot explore briefs, choose tasks by tag, or verify whether a brief was stored and delivered | [Configuration — Explore briefs](references/configuration.md#explore-briefs) |
| Experiments: `[experiments]` settings, approved config overlays, operator lifecycle, shadow/live modes and readouts | [Configuration — Experiments](references/configuration.md#experiments) |
| Decision assistance: the global-only `[assistance]` table, its off-by-default policy grants, modes, limits and budget, and how to inspect or verify it without activating a consumer | [Configuration — Decision assistance](references/configuration.md#decision-assistance) |
| Visual review: `visual_review.*` settings, `off`/`shadow`/`live` modes, verifying a capture, a disputed finding or a `not_established` record | [Configuration — Visual review](references/configuration.md#visual-review) |
| Executor image input: declaring `spec.image_input` on an executor, `vision = false` on a crew, the canary and visual hold, or reading `orbit agents vision-probe` results | [Configuration — Executor image input](references/configuration.md#executor-image-input-image_input) |
| Host capability labels: set `machine.labels`, understand the implicit OS label and `requires_labels` matching, or inspect placement waits | [Host capability labels](references/configuration.md#host-capability-labels) |
| Path leases: enabling or tuning `lease.*`, advisory versus hard coordination, or verifying a lease or a refused hard request | [Configuration — Path-lease policy](references/configuration.md#path-lease-policy) |
| Landed-delivery line classes (Analytics › Metrics › Lines changed): `workflow.test_paths`, `workflow.non_code_paths`, their defaults and verification; backfilling missing history with `migrate --backfill-diff-stats` | [Configuration — Landed diff classification](references/configuration.md#landed-diff-classification) |
| Tune a repository's workspace policy from evidence: required validation and baseline commands, footprint locks and generated files, task types, type and complexity crew pools, purpose pools, crew selection and performance draws, review layers | [Workspace tuning](references/workspace-tuning.md) |
| Crews, policies, workspace settings, executor definitions (host OS support, auth probes, inspection tools, shared Cargo-cache grants), generated-file conflict recovery, prose-only validation, executor admission order, host pressure, stalled providers, command timeouts, per-tag instruction files, review-skip tag mappings, review-finding verification and owed host evidence | [Configuration](references/configuration.md#review-finding-verification-and-owed-host-evidence) |
| Linux execution prerequisites or sandbox failure | [Linux sandbox](references/linux-sandbox.md) |
| Windows host (Orbit runs inside WSL2) | [Windows through WSL2](references/windows-wsl2.md) |
| macOS: Claude worker token and the clock env file | [macOS](references/macos.md) |
| Headless browser kit (`orbit browser-kit`) so workers can render and screenshot UI changes | [Browser kit](references/browser-checks.md) |
| Scheduled execution | [Automation](references/automation.md) |
| Workspace scripts (`target: script:<name>` job steps): what a script is, granting or revoking one, grant staleness, network and root grants, or verifying a grant | [Automation — Workspace scripts](references/automation.md#workspace-scripts) |
| Recurring QA and other task templates | [Auto-tasks](references/auto-tasks.md) |
| Prompt-size cost estimates, token usage and unknown cost | [Operational logs — Prompt-size cost accounting](references/operational-logs.md#prompt-size-cost-accounting) |
| Choose which shipped routines and auto-tasks an existing workspace carries, reconcile a policy change, or diagnose excluded defaults | [Configuration — Seeding policy](references/configuration.md#seeding-policy) |
| MCP clients, SSH federation, remote dashboard or the lease listener user and its tokens | [Remote access](references/remote-access.md) |
| Dashboard browser sessions: enabling `machine.web_terminal.enabled` or setting `machine.web_session.ttl`, one-time URLs from `orbit web open`, or checking whether the running dashboard serves `/api/session` (not the headless browser kit) | [Remote access — Browser sessions](references/remote-access.md#browser-sessions) |
| Installing plugins, their `orbit <ns>` commands and dashboard panels, the `.orbit/plugins.yaml` pin file, and decision channels for plugin answer submission | [Plugins](references/plugins.md#decision-channels) |
| Register Grok's MCP server, understand its shared Claude config and compatibility cases, or verify which source Grok loaded | [First run — Grok MCP registration](references/first-run.md#grok-mcp-registration) |
| Set up a host-local application that acts on an operator's consent for exact workspace-bound tool calls, including credentials, callbacks, rotation and revocation | [Scoped application authority](references/applications.md) |
| Manage a legacy external tool (`orbit tool add`, `list`, `show`, `run`, `enable`, `disable`, `remove`, `doctor`) | [Plugins — Managing legacy external tools](references/plugins.md#managing-legacy-external-tools) |
| Owner/replica roles and multiple machines | [Multi-host](references/multi-host.md) |
| Single-owner distributed drain setup or claimed-attempt recovery | [Distributed drain](../orbit/references/setup/distributed-drain.md) |
| Task snapshots, backup and restore | [Publication](references/publication.md) |
| Upgrade, resource sync, doctor, worktree GC, workspace removal or teardown, or source-remote rebinding | [Maintenance](references/maintenance.md#removing-or-re-targeting-a-workspace) |
| Inspect or compact admission receipt storage on an owner checkout | [Maintenance — Admission receipt storage](references/maintenance.md#admission-receipt-storage) |
| Scheduler/service logs and host incidents | [Operational logs](references/operational-logs.md) |
| Inspect one CLI audit event by ID | [Operational logs — Audit Event Log](references/operational-logs.md#audit-event-log) |
| A task held after a provider failure: which crews, accounts and hosts it excludes and when it clears | [Operational logs — Provider failure holds](references/operational-logs.md#provider-failure-holds) |

Task tracking alone does not require a provider, PR credentials, a scheduler or
remote hosts. Add those only for the requested workflow. Agent execution needs
an authenticated supported provider and working sandbox; PR delivery also needs
its Git/PR credentials. Task search uses local SQLite FTS5 and requires no download.

## Apply and verify

1. Inspect installed command help, effective config and relevant prerequisites.
   Use the owning host's registered tools for durable state; see the shared
   [tool surface](../orbit/references/tool-surface.md) for authority and routing.
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
store to recover access. Follow [maintenance](references/maintenance.md).

Seeded schedules are disabled until deliberately enabled (except
`crew-performance-refresh`, which is seeded enabled and no-ops while
`workflow.crew_performance` is `off`); preserve existing schedule and
enablement choices on an established host. Shared repositories
need an explicit owner; copying a checkout does not migrate its control plane.
