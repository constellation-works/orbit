---
type: context
summary: Backup and recovery are not optional for long-lived artifacts.
created_date: 2026-05-10
last_validated: 2026-10-29
---

# L02. The May 2026 Artifact Loss Incident

On 2026-05-11, hundreds of task artifacts were wiped out due to our reckless workspace cleanup. The lost copies could not be recovered. Orbit now supports portable `orbit task export` / `orbit task import` archives and an explicit per-workspace task-publication workflow that can restore published snapshots, but those workflows only recover copies that were made and retained before the loss.

The original numbered decision cited for that proposal was among the bodies lost to worktree reaping. This lesson preserves only the proposal already recorded here; it does not reconstruct the missing rationale. The same incident temporarily orphaned the bodies now preserved as MCP ambient workspace session context (since folded into [mcp-session-context decisions](../design/mcp-session-context/4_decisions.md)), [The v2 shell activity surface is removed, not sandboxed](../design/activity-job/4_decisions.md#the-v2-shell-activity-surface-is-removed-not-sandboxed), [Default Claude to opus/sonnet CLI aliases; centralize model defaults in orbit-common::model_defaults](../design/agent-families/4_decisions.md#default-claude-to-opussonnet-cli-aliases-centralize-model-defaults-in-orbit-commonmodel_defaults), and [PR handoff recovery follows job checkpoints and exact remote leases](../design/activity-job/4_decisions.md#pr-handoff-recovery-follows-job-checkpoints-and-exact-remote-leases).

This was catastrophic, but also gave us a chance to amend for the sins of our bad design decisions that have been plaguing us for a while now. [docs/design/task-artifacts/4_decisions](../design/task-artifacts/4_decisions.md)

**Lesson**: Backup and recovery are not optional for long-lived artifacts.

