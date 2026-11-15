---
type: design
summary: "Spec: orbit.notice.post, close, list and show; the CLI; and the opt-in notice MCP toolset with machine.mcp.toolsets"
last_validated: 2026-11-05
title: Spec — Notice tools and the notice toolset
owner: claude
status: Draft
feature: fleet-notices
tags: [fleet-notices, spec, mcp]
related_features: [fleet-notices, operations-as-data, federated-mcp]
related_artifacts: [ORB-21207]
last_updated: 2026-11-05
---

# Spec: Notice tools and the notice toolset

Four tools make up the notice surface: `orbit.notice.post`, `orbit.notice.close`,
`orbit.notice.list` and `orbit.notice.show`. They are one operations-as-data noun, `NOTICE`
(`cli_name: "notice"`, `tool_prefix: "orbit.notice"`, `audit_target_type: "notice"`), declared once
in `orbit-common`'s governance registry, so the tool schemas, the `orbit notice …` CLI and the MCP
descriptions come from one spec. On MCP the noun registers in the opt-in `notice` toolset.
`orbit tool run`, the CLI and activities reach the tools whatever the toolset selection.

## Why This Exists

The operator asked for an optional MCP feature (`--toolset notice`): most MCP clients should not
see four more tools in their default list, and a long-lived MCP client's `tools/list` must not
change under it on upgrade. Activities and the CLI must not depend on a host's MCP configuration,
because brief delivery and orchestrator posting have to work wherever the work runs.

## The `notice` toolset

- `McpToolset::Notice`, name `notice`, joins `McpToolset::ALL`
  (`crates/orbit-types/src/tool/mcp_toolset.rs`). `all` includes it.
- `orbit mcp serve --toolset notice` and `orbit mcp listen` with the same selection advertise and
  dispatch the four tools. `machine.mcp.toolsets = ["notice"]` selects it by host configuration;
  `--toolset` replaces that list for one server, as for every toolset.
- A server without the toolset neither lists nor dispatches the tools; a call is refused with the
  existing out-of-toolset refusal, which names the toolset to enable.
- The default toolset, and therefore every existing client's `tools/list`, is unchanged. The
  goldens for `orbit mcp serve --help`, the `machine.mcp.toolsets` description and the toolset
  error text are regenerated in slice 1.
- Managed runs: the dispatcher stamps the `notice` toolset onto a managed run's own MCP server when
  the activity grants a notice tool
  ([2_design.md §4](../2_design.md#4-managed-agents-posting)). The stamp adds to the configured
  toolsets.

## `orbit.notice.post`

Effect: write. MCP scope: workspace required (the home workspace). Idempotent for a
[repeat](./notice-record.md#retries) of the open notice's latest write, which covers a lost-reply retry:
a repeat changes no stored field, including `expires_at` and `updated_at`, for an editor or a
corroborator.

| Input | Type | Notes |
|---|---|---|
| `workspace` | selector | Home workspace. Falls back to the session binding; for an `agent` caller it must equal the bound task's workspace. |
| `scope` | `workspace` \| `global` | Default `workspace`. |
| `kind` | `incident` \| `maintenance` \| `info` | Required. |
| `title` | string | Required. |
| `body` | string | Optional. |
| `dedupe_key` | string | Optional; derived when omitted ([notice-record](./notice-record.md#dedupe)). |
| `linked_task` | string | Task id; resolved on the home owner. |
| `close_on_linked_done` | boolean | Default `true` with `linked_task`. |
| `evidence` | array of `{type, value}` | Optional. |
| `hosts` | array of machine ids or names | `maintenance` only; names resolve against the host registry and are stored as ids. |
| `expires_in_hours` | number | Mutually exclusive with `expires_at`. |
| `expires_at` | RFC 3339 | Mutually exclusive with `expires_in_hours`. |
| `model` | string | Writer provenance, as on other governed writes. |

There is no field for the author class, the author's task or run, `status`, `revision` or
`reports`; unknown fields are refused, not warned, because a stray `by` or `task_id` must never be
mistaken for an accepted assertion.

Output: `{notice, outcome}` with `outcome` one of `created`, `updated`, `corroborated`
([notice-record](./notice-record.md#dedupe)). A lost reply may be retried with the same input. The
retry matches the open key; when it is a [repeat](./notice-record.md#retries) (same poster, same
request, and no later write by anyone else) it returns `updated` (the poster may edit the notice) or
`corroborated` (it may not), changes no content, `expires_at`, `updated_at`, `revision` or `reports`,
and is never refused by a limit. Because the repeat check runs before expiry validation, a repeat
is never refused with `notice_expiry_invalid`, even when the original absolute `expires_at` is now
less than 5 minutes ahead: it still returns `updated` or `corroborated` with the stored expiry
untouched. A post that changes no content field does not bump `revision`.

## `orbit.notice.close`

Effect: write. MCP scope: workspace required.

| Input | Type | Notes |
|---|---|---|
| `id` | string | Either `id` or `dedupe_key`, not both. |
| `dedupe_key` | string | Closes the open notice holding the key in the home workspace. |
| `reason` | string | Required, 1–240 characters. |
| `workspace` | selector | Home workspace. |

Output: the notice with `closed` set, including `reports` committed before the close write.
Close does not bump `revision`. Closing a closed or expired notice returns it unchanged with
`already_inactive: true`; it is not an error and writes no second close. Rights:
[notice-authority](./notice-authority.md#rights).

## `orbit.notice.list`

Effect: read. MCP annotation: read-only. MCP scope: workspace optional (absent = every workspace
the answering server reaches, [notice-federation](./notice-federation.md#reads)).

| Filter | Type | Default | Meaning |
|---|---|---|---|
| `workspace` | selector or list | session binding, else all | Home workspaces to read. Global notices homed elsewhere are included when `include_global` is true. |
| `all` | boolean | `false` | Owner-local aggregate: every workspace homed on the answering owner, not only the selected one. Without a `limit`, every match is returned; a federated caller issues one such read per owner and limits once after merging. |
| `include_global` | boolean | `true` | Also return effective `global` notices from every reachable home, deduplicated by `id`. |
| `scope` | `workspace` \| `global` | both | |
| `kind` | enum, repeatable | all | |
| `status` | `open` \| `closed` \| `expired` \| `all` | `open` | Effective state. |
| `linked_task` | task id | — | |
| `author_class` | enum | all | |
| `host` | machine id or name | — | `maintenance` notices whose `hosts` include it. |
| `limit` | 1–200 | 50 | |

Order: `incident`, `maintenance`, `info`; then `updated_at` descending. Output:
`{notices: [...], degraded: [...]}`; `degraded` names any home that could not be read
(unreachable owner, a replica with no route), so an empty list never claims "no notices" when a
home was not read.

## `orbit.notice.show`

Effect: read. Input `id` (and optional `workspace`). Output one notice in any effective state,
including its untruncated body and, for the operator capability, its audit history (posts,
updates, corroborations, close).

## CLI

`orbit notice post|close|list|show`, generated from the noun spec, in the *Operate* help group
beside `lease`. Flags mirror the inputs (`--kind`, `--scope`, `--title`, `--body` or
`--body-file`, `--dedupe-key`, `--link <task>`, `--keep-after-done`, `--evidence type=value`,
`--host`, `--expires-in <hours>`, `--expires-at`). `orbit notice list` renders a table with kind,
scope, title, author class, age and time to expiry; `--json` gives the tool output. Help text
stands alone: no source paths and no internal task ids.

## Errors and audit

Refusal codes are listed in [notice-authority](./notice-authority.md#refusals). A post or close
write or core-use-case refusal records a system audit row with target type `notice`; `target_id` is
populated when the operation resolves or creates a notice. Operation parsing and argument conversion
can refuse a request before that audit runs. An unchanged post retry and a close of an already-inactive
notice write no row. A posted write's audit row carries the resolved poster class and request fingerprint
for inspection; the retry check reads the fingerprint from the notice's latest post event, not the
system audit row ([notice-record](./notice-record.md#retries)). Reads write nothing.

## Agent Signature

claude (claude-opus-5-5), 2026-11-02, [ORB-21207].
