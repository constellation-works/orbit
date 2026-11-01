---
title: Bounded application approvals — Decisions
owner: claude
last_updated: 2026-11-01
last_validated: 2026-11-01
status: Draft
feature: application-approvals
doc_role: decisions
type: design
summary: "Decisions: a relayed click is no stronger than its relayer, identity fields bound to state, Operator-floored operations kept off application authority, no consent rewrite on migration"
tags: [application-approvals, application-authority, security]
paths: ["crates/orbit-runtime/src/application_authority.rs", "crates/orbit-common/src/governance/authorization/catalog.rs"]
related_features: [plugins, human-decision-channel, state-compatibility]
related_artifacts: [ORB-20852]
---

# Bounded application approvals — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

All four entries are proposed with the design in [ORB-20852]. They become binding when
Daniel accepts the design. The comparison of options (a), (b) and (c) itself is design
prose, in [2_design.md §2](./2_design.md#2-options-compared). The entries below are the
standing rules that comparison produced.

## A relayed click is no stronger than its relayer

**Recorded:** 2026-11-01 · [ORB-20852]

### Context

An app that carries a user's approval can be given authority in two ways. Orbit can
admit the app's call when it fits a consented, state-bound shape. Or Orbit can add
ceremony per click, such as a decision request the app answers. Per-click ceremony looks
stronger. But when the same app server relays the click and holds the credential, a thief
of that credential can perform the ceremony too. The real alternative was option (b): a
per-click decision request. It would have added a record, a round trip and an apply step
for every approval.

### Decision

Add per-click ceremony to an application path only when the human authenticates on a
channel the relaying credential cannot reach, for example a signed mail reply or an
operator surface. When the relayer is the only witness to the click, bind the call to
current state and audit it, and add no extra round trip. This applies to every future
application or channel that relays human input.

### Consequences

- Office approvals use parameterised consent scopes, not decision requests.
- Strengthening click authentication later means a new channel design, not more ceremony
  on the existing app path.
- Cost: no Orbit record proves that a human clicked. Audit shows that an authorised app
  made a state-bound call, and attribution rests on the app server's integrity.

## Identity fields bind to state, not to free text

**Recorded:** 2026-11-01 · [ORB-20852]

### Context

A parameterised scope has to accept values unknown at consent time. The tempting
generalisation is "any string" for an id. That turns a scope for one kind of task into a
scope for every task. The alternative was a pattern constraint, a regular expression over
ids. Ids carry no meaning, so a pattern is either too broad or a disguised constant.

### Decision

A field that selects what a call acts on (task id, artifact path, auto-task name, draft
id) is admitted only through a constraint that ties it to something the operator
reviewed. That is a constant, an enumerated set, a predicate on the target's current
state (tags, status, artifact digest), a binding to the entry call, or a pointer into a
digest-verified artifact. Free-text constraints are for annotation fields only. An opaque
`token` is a last resort for plugin-internal ids that Orbit cannot resolve, and the
plugin must then enforce its own binding. Apply this to every new constraint kind and
every new consentable tool.

### Consequences

- `crates/orbit-runtime/src/application_authority.rs::validate_application_operation`
  (as extended) refuses `text` on identity fields at consent time.
- Gateway draft ids are `token`-constrained, so Orbit relies on the gateway plugin for
  the draft digest (resolved question 3 in [1_scope.md](./1_scope.md#open-questions):
  "No, plugin checks it").
- Cost: every consumer's ids must be resolvable by Orbit or be a plugin-internal token.
  An app that addresses Orbit work by a value Orbit cannot check needs a new predicate
  kind and another design review before it can be consented.

## Operator-floored operations stay off application authority

**Recorded:** 2026-11-01 · [ORB-20852]
**Code anchors:** `crates/orbit-common/src/governance/authorization/catalog.rs::APPLICATION_BUILTIN_OPERATIONS` (to become the ceiling table), `crates/orbit-runtime/src/tool_exec.rs::authorize_registered_tool`

### Context

Office's routine screen toggles routines (`orbit.routine.control`), and its Review screen
mints auto-tasks on demand (`orbit.auto_task.mint` with `acknowledge_unconditional`).
Both change what runs with no human present. The catalogue holds them to Operator: one
by its own row, the other by the input-dependent floor in `authorize_registered_tool`.
The dashboard exposes them as operator-governed rows. The alternative was a constrained
scope for them too, for example "toggle routine X between enabled and disabled". That
would have let a stolen app credential start unattended work that no reviewed payload
named.

### Decision

Application authority never admits an operation, or an input, that carries an Operator
floor in the governed-operation catalogue. That includes the input-dependent floors
`authorize_registered_tool` adds. It also never admits the transition that approves work
for dispatch: `proposed`, `someday` or `blocked` to `backlog`. Consent-time validation
checks each scope against the catalogue, not against a hand-kept list. An operation that
is not Operator-floored may be consented only within the ceiling table. If it changes
unattended execution, every identity and content field must be bound to a
digest-verified artifact a human reviewed. Apply this to every future consentable tool,
and treat a change to a catalogue floor as a change to what applications may do.

### Consequences

- `orbit.routine.control`, `orbit.auto_task.mint` with `acknowledge_unconditional`,
  `orbit.auto_task.update` with `expected_enabled`, workflow and pipeline dispatch, and
  consent administration are refused at consent time. The app shows its not-permitted
  message for routine control and mint.
- `orbit.auto_task.add` and `orbit.auto_task.update {name, enabled}` carry no Operator
  floor. They are admitted only as callbacks bound to the reviewed proposal payload
  (2_design.md §9 slice 5).
- Approve and gateway-send close walks must avoid `proposed` to `backlog`, so they start
  at `in-progress` instead (2_design.md §9 slice 6).
- Cost: the office app cannot be the single front door for routine management, and the
  user must open the dashboard or CLI to toggle routines or mint. Lowering any
  operation's floor also widens what a consent can name, so floor changes need the same
  review as consent changes.

## Consent migration never rewrites authority

**Recorded:** 2026-11-01 · [ORB-20852]
**Code anchors:** `crates/orbit-runtime/src/application_authority.rs::verify_application_witness`

### Context

Consent schema v2 adds fields, and the 30-day compatibility window
([ORB-15732]) normally lets a migration rewrite old-shape records on open. Authority
here is the agreement of a SQLite row and an owner-only witness, so that a writable row
alone cannot create authority. A migration that rewrites both from the row would be that
forbidden path. The alternative was an on-open rewrite of v1 records to v2 `const`
constraints, which would have made the v1 reader retirable on a schedule.

### Decision

No migration, upgrade or repair derives or rewrites a consent's witness from its store
row. Old-shape consents are read as they are until they expire, are revoked, or the
operator rotates them, which is a fresh review. A reader of an old consent shape is
retired only after a read-only scan of every host finds none live. Apply this to any
future change to consent, channel registration or other dual-record authority.

### Consequences

- v1 exact consents keep working indefinitely, until expiry or revocation.
- The v1 record reader's removal depends on the host scan, not only on the calendar.
- Cost: a long-expiry v1 consent can keep the old reader alive past the 30-day window,
  until someone rotates or revokes it.

## Task References

- [ORB-15732] — recorded the 30-day, release-gated compatibility window.
- [ORB-20852] — designed bounded application approvals and proposed these decisions.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
