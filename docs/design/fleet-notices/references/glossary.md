---
type: design
summary: "Glossary: fleet notices"
last_validated: 2026-11-14
---

# Glossary: Fleet Notices

Vocabulary specific to fleet notices. General Orbit terms (owner, follower, replica, toolset,
safe point) keep their meanings from their own features and are excluded, except where notices give
them a narrower sense.

| Term | Meaning |
|------|---------|
| **Author class** | `operator`, `orchestrator` or `agent`, derived by the owner from the caller's session; never an input. [notice-authority](../specs/notice-authority.md#poster-classes). |
| **Brief block** | The `fleet_notices` JSON value in a managed agent's task context, marked `trust: untrusted_data`. [notice-delivery](../specs/notice-delivery.md#shape). |
| **Corroboration** | A non-repeat post matching an open notice by someone not allowed to edit it; bumps `reports` only. [notice-record](../specs/notice-record.md#dedupe). |
| **Dedupe key** | The subject identity of a notice; at most one open notice per key per home workspace. [notice-record](../specs/notice-record.md#dedupe). |
| **Dispatch hold** | The later extension in which an operator- or orchestrator-authored `maintenance` notice withholds unattended admission on named hosts. Not in v1. [2_design.md §7](../2_design.md#7-extension-point-maintenance-dispatch-hold). |
| **Effective state** | `open`, `closed` or `expired`, computed on read from the row, the clock and the linked task. [notice-record](../specs/notice-record.md#effective-state). |
| **Fleet notice** | The record this feature adds, also just "notice". Not a lease notice. [1_overview.md](../1_overview.md). |
| **Home workspace** | The workspace whose owner store holds a notice, global or not. [notice-federation](../specs/notice-federation.md#scope). |
| **Lease notice** | An event in one path lease's notice stream (`orbit.lease.notices`); unrelated to fleet notices. [path-leases 2_design.md §5](../../path-leases/2_design.md#5-notices). |
| **Materializer** | The writers that persist a linked-task closure already visible on read. [notice-record](../specs/notice-record.md#materializing-closure). |
| **`notice` toolset** | The opt-in MCP toolset that advertises the notice tools. [notice-tools](../specs/notice-tools.md#the-notice-toolset). |
| **`notice_open`** | The `orbit attention` kind listing effective-open notices. [notice-delivery](../specs/notice-delivery.md#attention-kind). |
| **Reports** | `{count, last_at}` of posts that matched a notice, including the first, except repeats. [notice-record](../specs/notice-record.md#record). |
| **Repeat** | A post whose fingerprint equals the latest write on its open notice: a lost-reply retry, or the same poster re-sending the same request. Bumps no `reports` or `revision` and counts against no limit. [notice-record](../specs/notice-record.md#retries). |
| **Scope** | `workspace` (the home workspace) or `global` (every workspace whose readers reach the home owner). [notice-federation](../specs/notice-federation.md#scope). |
