---
type: design
summary: "Spec: Self-Reported Actor Identity for Unauthenticated MCP Calls"
tags: ["auditability"]
last_validated: 2026-08-16
---

# Spec: Self-Reported Actor Identity for Unauthenticated MCP Calls

92.5% of MCP tool calls carry no actor identity. Over a 30d window on the production audit
database, `subcommand = 'run-mcp'` splits 1373 `unverified` against 112 attributed
(`agent` 64, `codex` 27, `claude-opus-5` 15, `sonnet` 4, `opus` 2).

The cause is structural, not a bug in the trust check. `audit_role_label_for_entry_point`
returns `unverified` for any MCP call outside a managed run context, and
`resolve_agent_identity_for_entry_point` additionally drops agent and model to `None` — so
the whole identity triple is lost. Both read `ORBIT_MANAGED_RUN_CONTEXT` + `ORBIT_RUN_ID`,
which are injected in exactly one place: `provenance_env` on the engine CLI-runner spawn
path. But `orbit mcp serve` is started by the MCP *client* from that client's own config,
so it never inherits them. The CLI runner configures no MCP for its children; managed runs
drive agents over the CLI. MCP is therefore effectively the interactive surface, and an
interactive session cannot satisfy the managed-run trust boundary by construction. The 112
attributed calls are the narrow case where a managed run's agent child spawned its own MCP
server and passed the environment down.

The consequence for metrics: any per-agent tool-call fail rate silently excludes ~92% of
that agent's MCP traffic while presenting itself as a complete denominator.

[ORB-10890] makes that traffic attributable-but-labelled instead of anonymous, by adding a
second field. It does not make it trusted.

## What This Spec Does Not Change

**The trust boundary does not move.** `unverified` remains the correct trusted label:
Orbit genuinely cannot authenticate these callers. `audit_role_label_for_entry_point` is
not widened to read caller input, and `role` is written from exactly the bytes it was
written from before.

**The canonical actor projection does not change.** `actor_kind` / `actor_id` and friends
[ORB-10888] stay a pure function of `role`, so `unverified` still projects to
`unattributed` / `unverified`. No self-reported value reaches any column a trust decision
reads.

**Existing rows are not reinterpreted.** Migration v17 is additive with no backfill. Every
pre-existing row has `self_reported_actor IS NULL` and reads as anonymous, because there
was no claim collected at the time to recover. Deriving one from `role` would retroactively
attribute traffic Orbit never authenticated.

## The Rule

A self-reported value is evidence, never a credential:

1. Stored in its own column; never merged into `role` or `actor_*`.
2. Never consulted by an authentication or authorization decision. It reaches
   `AuditEventInsertParams`' sibling `AuditInvocationFields` and stops there — it is not an
   input to `audit_role_label`, to `resolve_agent_identity`, or to any field of
   `ToolContext`.
3. Never rendered without being marked unverified.
4. Absent, empty, or malformed is *anonymous*, never defaulted and never inherited.

## Where The Claim Is Sourced

**Session initialization, not per call.**

`initialize` is the one point in the MCP protocol where a client describes itself, and it
is the natural scope for the claim: an identity that is announced once per session cannot
vary call to call. Sourcing it per call would mean reading model-authored tool JSON, which
is both a moving target and the input the trust boundary already refuses to read for
correlation.

Two sources, in precedence order, both equally unverified:

| source | meaning |
|---|---|
| `_meta.orbit.actor` (params meta, then transport meta) | the agent naming its own family or model, e.g. `claude-opus-5` |
| `clientInfo.name` | the MCP client product that opened the session, e.g. `claude-code` |

`_meta.orbit.actor` wins because the two answer different questions, not because one is
more trustworthy. `clientInfo` is what every MCP client already sends, so the common case
records something without the client knowing anything Orbit-specific;
`_meta.orbit.actor` lets a caller be more specific. The client's `version` is deliberately
excluded, so a per-agent denominator does not fragment on every client release.

A re-`initialize` replaces the claim outright rather than merging, so one session can never
accumulate two identities.

## Normalization

`normalize_self_reported_actor` is the single gate. It returns `None` — anonymous — for a
claim that is blank, longer than 128 characters, or contains any control character.
Control characters are rejected rather than stripped: a newline would let a claim forge
extra fields in any line-oriented rendering of the audit log, and sanitizing it would
record an actor the caller never named. Over-length claims are likewise rejected rather
than truncated.

Accepted claims are lowercased with internal whitespace runs collapsed, so `Claude  Code`
and `claude code` aggregate as one group. Case folding is safe precisely because the value
is never compared against a trusted label.

Reserved-looking claims (`admin`, `unverified`) are **not** rejected. Rejecting them would
imply the accepted ones carry trust; the separation is structural instead.

## Persistence

Migration v17 `audit_self_reported_actor` adds a nullable `self_reported_actor` column to
`audit_events` and indexes it. No backfill (see above).

The value travels on `AuditInvocationFields`, the focused seam already carrying `trace_id`
and `caller_ip`, rather than on `AuditEventInsertParams` — the two dozen non-tool audit
producers have no claim to supply and should not have to name the field.

## Aggregates

`get_audit_tool_call_counts_by_attribution` covers the same rows as
`get_audit_tool_call_counts_by_role` and classifies each into exactly one of three disjoint
buckets:

| attribution | condition | grouped on |
|---|---|---|
| `authenticated` | `actor_kind` present and not `unattributed` | `actor_id` |
| `self_reported` | otherwise, `self_reported_actor` present | `self_reported_actor` |
| `anonymous` | neither | the literal `anonymous` |

Because the buckets are disjoint and every row lands in one, the three denominators a
caller needs are all readable from one result set: filter on `attribution` for
authenticated-only or self-reported-only, sum for combined. Their total equals the
role-grouped denominator over the same rows, so the new view neither drops nor
double-counts anything.

The same actor may appear under two attributions — one agent, half of whose traffic Orbit
could authenticate. Those rows are not duplicates to merge. Merging them is exactly how an
unverifiable count gets published as a measured one.

A NULL `actor_kind` (a row the v16 backfill could not reach) reads as unattributed, matching
`get_audit_event_aggregates_by_actor`, so it falls through to self-reported or anonymous
rather than counting as authenticated.

## Surfaces

- `orbit audit show` prints a `Self-reported:` line carrying the `(unverified)` marker,
  beside the untouched `Role:`.
- `audit_event_to_json` (`orbit audit list --json`, `orbit audit show --json`) exposes
  `self_reported_actor`. The key names the trust level; it is deliberately not called
  `actor`.
- The dashboard audit summary emits `attribution_split` beside `role_split` and
  `actor_split`. Each row carries its own `attribution` plus a `verified` boolean, so a
  chart legend reading only `label` still says which half of the split it is in.
