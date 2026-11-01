---
title: "Scope: bounded application approvals"
owner: claude
last_updated: 2026-11-01
last_validated: 2026-11-01
status: Draft
stage: designing
next: "Daniel reviews the design; then file the slices in 2_design.md §9 (none filed yet)"
feature: application-approvals
doc_role: scope
type: design
summary: "Scope: parameterised application consent scopes (typed per-field constraints bound to live task state, bound read callbacks, named transitions) so an app can carry a user's approvals without operator authority"
tags: [application-approvals, application-authority, plugins, security]
---

# Scope: bounded application approvals

Status: proposed. This folder is the design deliverable of [ORB-20852]. It contains no
code. Implementation waits for Daniel's review, because the design widens what an
application credential may do. Like [human-decision-channel](../human-decision-channel/1_scope.md),
the folder pairs this scope doc with [2_design.md](./2_design.md), which holds the
mechanisms, and with [4_decisions.md](./4_decisions.md). That pairing is the task's own
acceptance shape. It deviates from the two tracks in [CONVENTIONS.md](../CONVENTIONS.md) §1
and is recorded here per CONVENTIONS.md §11.

Bearing: [ORB-20362] shipped scoped application authority in orbit 0.41.0. An operator
consents, once, to an application id, an expiry and a list of exact operations, bound to
one workspace id and checkout root. Orbit mints a credential that the service passes to
CLI children through an inherited descriptor. Authority lives in a SQLite row plus an
owner-only witness file, so a modified row alone grants nothing. Every call is audited
with an `application` object. Inside an app-bound plugin call, the plugin's Orbit
callbacks are admitted only when they exactly equal a listed completion callback. The
contract is documented for users in the installed `orbit-setup` skill reference
`applications.md`. The admission code is `crates/orbit-runtime/src/application_authority.rs`.

## Problem

The orbit-office app (the user's Today and Needs-you screens) cannot carry the user's
approval clicks through application authority. [ORB-20363] is blocked on this, and
friction F2026-11-006 holds the evidence. Three facts about the current code cause it:

1. **Admission matches the whole input exactly.** A direct call is admitted only when a
   consented scope has the same tool and a structurally equal input
   (`admit_application_call`, `crates/orbit-runtime/src/application_authority.rs:395-404`).
   Every app approval embeds a value that does not exist at consent time: a proposal id
   and the payload digest the user reviewed (`office.proposal-accept {id, sha256}`), a
   draft id and digest (`gateway.http-send`), or a comment the user typed (send-back,
   dismiss). A standing consent cannot list them. A per-click consent would need the
   operator, and that consent would itself be the approval.
2. **Callbacks are limited to two exact writes.** Only `orbit.task.update` and
   `orbit.decision.answer` may be consented
   (`APPLICATION_BUILTIN_OPERATIONS`, `crates/orbit-common/src/governance/authorization/catalog.rs:69`).
   `orbit.task.update` admits only `id`, `status`, `comment` and `workspace`, with
   `status` limited to `backlog`, `done`, `rejected` or `archived`
   (`application_authority.rs:270-296`). `office.proposal-accept` reads the task and its
   `proposal.json` artifact, moves the task to `in-progress` with a `plan`, then to
   `review` and `done` with an `execution_summary`. Auto-task proposals also call
   `orbit.auto_task.add` and `orbit.auto_task.update`. Every one of those callbacks is
   refused before the plugin applies anything.
3. **Routine control is refused at consent time.** `orbit.routine.control` and
   `orbit.auto_task.mint` fall to the "does not support application consent" arm
   (`application_authority.rs:298-302`).

The only way the app accepts proposals today is with `ORBIT_OPERATOR`. That is a standing
operator grant to a login service, which ORB-20363 rules out.

## Goal

An operator can consent once to a bounded **approval profile** for one app. Under it the
app can accept, dismiss, approve, send back and send exactly the kinds of work the profile
names, and only at the state the user saw. Every property application authority has today
stays: the one-workspace and checkout bound, expiry and revocation, the protected witness,
digest binding of plugin manifests, per-call audit, and refusal of everything not listed.
No capability is granted. Unattended-execution controls (`orbit.routine.control`,
`orbit.auto_task.mint`) stay outside application authority.

## Shape

The chosen flow is option (a) of the three the task asked to compare:
**parameterised consent scopes**. [2_design.md §2](./2_design.md#2-options-compared)
compares it with (b), a per-click decision request, and (c), an operator-held approval
surface in Orbit.

### Constrained scopes

A consent scope keeps today's exact form (`operation.input`). It gains a second form:
`operation.fields`, a closed map from input field to a typed constraint. Every field
the call sends must be in the map, and every non-optional field in the map must be sent.
The constraint kinds form a closed set:

| Constraint | Admits |
|---|---|
| `const`, `one_of` | an exact value, or one of at most 16 |
| `text` | a string up to `max_chars`, no control characters except newline and tab, unchanged by credential redaction |
| `token` | an ASCII identifier (`[A-Za-z0-9._:-]`) up to `max_len` |
| `sha256` | 64 lowercase hex characters |
| `workspace` | a selector that resolves to the consent's workspace |
| `task` | the id of a task in this workspace whose **current** state satisfies a predicate: required tags, allowed statuses, and optionally "artifact `<path>` has sha256 equal to field `<f>`" |
| `transition` | a target status allowed only from listed current statuses of the call's task |
| `bound` | (callbacks only) equal to the value the entry call was admitted with |
| `artifact_json` | (callbacks only) equal to the JSON value at a pointer inside the entry's digest-verified artifact |
| `subset_of` | an array whose elements all come from a listed set |
| `optional` | wraps another constraint; the field may be absent |

There are no regular expressions, wildcards or defaults.
[2_design.md §4](./2_design.md#4-consent-schema-v2) gives the exact schema.

### Bound callbacks

Inside an app-bound plugin call, a callback is admitted when it satisfies a listed
callback's constraints. Its `id` must either be `bound` to the entry's admitted task or
be pinned on first use. Two read callbacks become admissible: `orbit.task.show`, with
listed non-secret fields, and `orbit.task.artifact.get` on a constant path. When the
entry bound an artifact digest, every later read and write in that invocation rechecks
the digest against current state.

### Ceilings and fixed refusals

A closed per-tool ceiling table replaces `APPLICATION_BUILTIN_OPERATIONS`. A consent may
narrow a ceiling, never exceed it. Application authority never admits a transition that
first approves work for dispatch: `proposed`, `someday` or `blocked` to `backlog`. It never admits
`orbit.routine.control`, `orbit.auto_task.mint`, dispatch, force, tool grants or consent
administration.

### Configuration

Consent is host state, not workspace config. Nothing is added to `config.toml`.

| Limit | Value | Meaning |
|---|---|---|
| Consent request size | 8 KiB (was 4 KiB) | A constrained profile for one app is about 3-5 KiB |
| Plugin callback session record | 16 KiB (was 8 KiB) | Holds the invocation, now including its bindings |
| Constrained-scope consent expiry | at most 30 days | Renewed with `orbit application rotate` |
| `text` / `token` maxima | 4096 chars / 128 bytes | Per field |

### Surfaces

- `orbit application consent` and `rotate` accept constrained scopes and refuse an
  invalid constraint or one that exceeds a ceiling, naming the field.
- `orbit audit list` and `orbit audit show` carry the selected scope name, the bound
  values, the predicate evidence and text-field digests in the existing `application`
  object.
- The office app shows its existing not-permitted message for every action the profile
  refuses.

## Non-goals

- **No capability grant.** The app never holds Operator, Runner or Agent. Admission
  bypasses the capability floor only for the listed, constrained operations, exactly as
  it does today for exact ones.
- **No proof of a fresh human click.** The app server relays the user's click. Orbit can
  bind the click to current state but cannot authenticate the human behind it.
  [4_decisions.md](./4_decisions.md#a-relayed-click-is-no-stronger-than-its-relayer)
  records why.
- **No unattended-execution controls.** Routine toggles, trigger edits and unconditional
  mints stay on the operator surfaces: the dashboard (`routine.toggle`, `auto_task.mint`
  rows in `catalog.rs`) and the CLI.
- **No application MCP sessions or Windows support.** Both still fail closed.
- **No Office-specific code in Orbit core.** Constraints name tags, statuses, artifact
  paths and JSON pointers. The office profile is data the operator consents to.

## Concerns and honest limitations

- **A stolen credential approves what is pending.** Until expiry or revocation, a holder
  can accept every pending proposal at its current payload, dismiss approval tasks and
  send them back. It cannot change a payload, reach another workspace or task kind, or
  make new work dispatchable. [2_design.md §3](./2_design.md#3-threat-model) bounds it.
- **Tags classify; they do not authorise.** Agents can tag tasks, so a predicate on tags
  only selects which work the profile covers. The digest binding and the relayed click
  carry the approval.
- **Plugin side effects stay unverified.** Orbit checks what a plugin asks of Orbit, not
  what it writes elsewhere (Library pages, outbound HTTP). The plugin's pinned manifest
  digest is the only bound on that, as today.

## Phasing

1. **Schema and consent-time validation.** Constrained scopes parse and validate against
   the ceiling table. Admission still refuses them.
2. **Entry admission.** Direct constrained calls (dismiss, send-back) and plugin entries
   are admitted with bindings.
3. **Bound callbacks.** Read callbacks, bound and pinned ids, digest rechecks, callback
   session schema 5.
4. **Audit and documentation.** The new audit fields, their rendering, and the installed
   `applications.md` reference.
5. **Auto-task callbacks.** `artifact_json` and the `orbit.auto_task.add` and `update`
   ceilings.
6. **Office profile** (orbit-office). Consent profile, installer step, close walks that
   avoid `proposed` to `backlog`, and ORB-20363 re-scoped.
7. **Compatibility retirement.** After the 30-day window, the writer emits v2 records
   only.

[2_design.md §9](./2_design.md#9-implementation-slices) lists them as proposed tasks.

## Open questions

1. **Is a 30-day expiry cap right for constrained consents?** A shorter cap limits a
   stolen credential. A longer one avoids monthly operator renewal on a one-user host.
2. **Should a constrained scope carry a rate limit?** For example, at most N admissions
   per hour per scope. It would blunt bulk misuse of a stolen credential, but accept-all
   is legitimately bulk.
3. **Should Orbit bind the gateway draft digest?** Today only the gateway plugin checks
   it, because the draft lives in plugin state.

## Acceptance (feature level)

- Given an operator consented to the office profile, the app accepts a pending proposal
  at the digest the user saw. The task ends `done`, and no `ORBIT_OPERATOR` is in the
  app's environment.
- Given the payload changed after review, the same accept is refused before any callback
  writes, and the audit row names the failing constraint without the payload.
- Given the same credential, a call on a task outside the predicate, an unlisted
  callback, `proposed` to `backlog`, `orbit.routine.control` or `orbit.auto_task.mint` is
  refused. So is a call after expiry or revocation, or from another workspace.
- Given an existing v1 exact consent, it keeps working unchanged across the upgrade.

## Task References

- [ORB-20362] — shipped scoped, audited application authority (orbit 0.41.0).
- [ORB-20363] — wires the office app's approval actions to application authority;
  blocked on this design.
- [ORB-20852] — this design.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
