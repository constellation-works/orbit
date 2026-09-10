---
type: design
summary: "Spec: Canonical Audit Actor Identity"
tags: ["auditability"]
last_validated: 2026-08-16
---

# Spec: Canonical Audit Actor Identity

`audit_events.role` is a single free-text label that conflates five unrelated kinds of
value. Measured over a 30d window on the production audit database:

| kind | observed values |
|---|---|
| agent family | `codex`, `claude`, `grok` |
| model string | `claude-opus-5`, `gpt-5.6-luna`, `opus`, `sonnet`, `claude-sonnet-5` |
| system / synthetic | `admin`, `hook` |
| unattributed | `unknown`, `unverified`, `agent` |
| human | `human` |

Two defects follow. **Granularity split**: `claude`, `opus`, and `claude-opus-5` are one
actor recorded at three grains, so every per-agent denominator is split across rows that
should aggregate. **Kind conflation**: `admin` is hardcoded on ID-allocation and direct-CLI
rows, `hook` is machinery, and `unknown` is overwhelmingly human-run read-only inspection —
none are agents, yet all outrank real agents in a role-ordered view.

[ORB-10888] adds a canonical actor projection beside `role` rather than replacing it.

## What This Spec Does Not Change

Trust classification. This normalizes identity *shape* only:

- `role` is never rewritten, re-derived into, or reinterpreted. It stores exactly the bytes
  the attribution path produced.
- `unverified` — the MCP trust boundary's marker, set by
  `audit_role_label_for_entry_point` when a standalone MCP call has no authenticated
  managed envelope — resolves to its own unattributed actor. It is never resolved into
  whatever agent its surrounding context hints at.

Which callers count as authenticated is unchanged for every existing and future row.

## The Canonical Actor

`crates/orbit-types/src/telemetry/audit_actor.rs` owns the one mapping from a recorded
label to a `CanonicalActor`:

| field | meaning |
|---|---|
| `kind` | `human` \| `agent` \| `system` \| `hook` \| `unattributed` |
| `id` | grouping key within `kind`: the agent family for agents, the canonical label otherwise |
| `vendor` | `anthropic`, `openai`, `google`, `xai`, `ollama` — agents only |
| `family` | `claude`, `codex`, `gemini`, `grok`, `ollama` — agents only |
| `model` | the model string as recorded, when the label named one rather than a bare family |
| `alias_version` | the alias-map version that produced this record |

`kind` is what makes "real agents only" expressible without string-matching a label.
`id` is what collapses the granularity split: `claude`, `opus`, and `claude-opus-5` all
carry `id = "claude"` while `model` keeps the finer grain retrievable.

## The Alias Map

`canonical_actor_for_role_label` resolves in this order:

1. Blank label → unattributed `unknown`.
2. A known non-agent alias — `admin` and `system` → system, `hook` → hook, `human` →
   human, `unknown` / `unverified` / `agent` → unattributed. Each keeps its own `id`, so
   distinct diagnostics stay distinct.
3. A bare agent family name (`claude`) → agent with no model recorded.
4. A model string whose family is inferable via `identity::agent_from_model`
   (`claude-opus-5`, `opus`, `fable-5.1`, `gpt-5.6-luna`), or a shorthand in the
   explicit table (`haiku`) → agent with both family and model.
5. Anything else → an agent with unknown family, keeping the label as both `id` and
   `model`. Every label reaching this point came from an attribution path that held a
   model or family, so an unrecognized *agent* loses less than a discarded row would.

Matching folds case and trims; `model` preserves the label's original casing.

**Adding a model.** A new `claude-*` or `gpt-*` build is resolved by rule 4 with no code
change at all, and never requires touching an aggregate query. Only a genuinely new
shorthand or a new family needs an edit here.

## Versioning

`ACTOR_ALIAS_MAP_VERSION` stamps every derived record, and every persisted row carries the
version that produced it. Re-running an aggregate over old rows is therefore stable and
reproducible: a map change is an explicit version bump plus a re-derivation step, not a
silent reinterpretation of history.

Bump the version when a label's canonical resolution changes — a new alias, a re-kinded
label, a corrected family. Do **not** bump it for a model that an existing rule already
resolves. A bump requires a new append-only ledger migration that calls
`backfill_audit_actor_identity`, which re-derives exactly the rows whose stamped version
is not current. Map history: v2 (migration v18 `audit_actor_alias_v2`) promoted `fable`
from a shorthand alias to a family rule so versioned Fable labels resolve to `claude`.

## Persistence

Migration v16 `audit_actor_identity` adds `actor_kind`, `actor_id`, `actor_vendor`,
`actor_family`, `actor_model`, and `actor_alias_version` to `audit_events`, indexes
`(actor_kind, actor_id)`, and backfills every existing row.

The backfill keys on `SELECT DISTINCT role` — a handful of labels, not a row-by-row pass —
and runs each through the same Rust function the insert path uses. New rows and migrated
rows therefore land in identical buckets, so a 30d or 90d window that spans the change
stays comparable.

Columns exist for SQL `GROUP BY`; they are not the read path for a single event.
`AuditEvent::actor()` derives the actor from `role` on hydration, so a row always reads
back under the *current* map while aggregates stay pinned to the stamped version.

## Aggregates

`get_audit_event_aggregates_by_actor` groups on `(actor_kind, actor_id, actor_vendor,
actor_family)` and carries the same MCP-vs-CLI surface split as the older
`get_audit_event_aggregates_by_role`. `model` is deliberately absent from the group key —
that is the grain the raw role aggregate already splits on.

The role-grouped aggregate is retained: it is the raw, un-normalized view, and keeping
both makes the normalization auditable rather than implicit.

The dashboard summary emits `actor_split` beside `role_split`, and
`audit_event_to_json` exposes `actor`, `actor_kind`, `actor_vendor`, `actor_family`, and
`actor_model` next to the untouched `role`.
