---
name: orbit-setup
description: Sets up and maintains Orbit on a user's machine and repositories according to their needs. Use for installation, `orbit workspace init`, onboarding, provider/crew and MCP configuration, the Linux sandbox, the scheduler (sweep clock, routines, auto-tasks), worktree GC, `orbit doctor`, upgrades, task publication and backup, remote access, the dashboard, or host and service log troubleshooting. Supports existing installations as well as first-time setup; task execution belongs to orbit and backlog dispatch to orbit-orchestrate.
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
| First installation or another repository | [First run](references/first-run.md) |
| Providers, crews, policies and workspace settings | [Configuration](references/configuration.md) |
| Linux execution prerequisites or sandbox failure | [Linux sandbox](references/linux-sandbox.md) |
| Scheduled execution | [Automation](references/automation.md) |
| Recurring QA and other task templates | [Auto-tasks](references/auto-tasks.md) |
| MCP clients, SSH federation or remote dashboard | [Remote access](references/remote-access.md) |
| Installing plugins, their `orbit <ns>` commands and dashboard panels, and the `.orbit/plugins.yaml` pin file | [Plugins](references/plugins.md) |
| Owner/replica roles and multiple machines | [Multi-host](references/multi-host.md) |
| Single-owner distributed drain setup or claimed-attempt recovery | [Distributed drain](../orbit/references/setup/distributed-drain.md) |
| Task snapshots, backup and restore | [Publication](references/publication.md) |
| Upgrade, resource sync, doctor or worktree GC | [Maintenance](references/maintenance.md) |
| Scheduler/service logs and host incidents | [Operational logs](references/operational-logs.md) |

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

Seeded schedules are disabled until deliberately enabled; preserve existing
schedule and enablement choices on an established host. Shared repositories
need an explicit owner; copying a checkout does not migrate its control plane.
