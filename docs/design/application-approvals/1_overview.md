---
title: Application Approvals — Overview
owner: claude
created_date: 2026-11-01
updated_date: 2026-11-28
last_validated: 2026-11-04
status: Draft
stage: building
next: "Orbit slices 1-5 shipped; office profile remains ORB-20363, compatibility retirement ORB-21043"
feature: application-approvals
doc_role: overview
type: design
summary: "Parameterised application consent scopes (typed per-field constraints bound to live task state, bound read callbacks, named transitions) so an app can carry a user's approvals without operator authority"
tags: [application-approvals, application-authority, plugins, security]
---

# Application Approvals — Overview

This folder is the design deliverable of [ORB-20852]. Orbit implementation slices 1-5
have shipped as [ORB-21038], [ORB-21039], [ORB-21040], [ORB-21041] and [ORB-21042]: schema v2, constrained entries, bound
and read callbacks, audit fields, and payload-bound auto-task callbacks. The office
profile remains [ORB-20363]; compatibility retirement is [ORB-21043]. The folder
pairs this overview with [2_design.md](./2_design.md), which holds the mechanisms, and with
[4_decisions.md](./4_decisions.md).

Bearing: [ORB-20362] shipped scoped application authority in orbit 0.41.0. An operator
consents, once, to an application id, an expiry and a list of exact operations, bound to
one workspace id and checkout root. Orbit mints a credential that the service passes to
CLI children through an inherited descriptor. Authority lives in a SQLite row plus an
owner-only witness file, so a modified row alone grants nothing. Every call is audited
with an `application` object. Exact scopes keep their original matching; constrained
scopes admit entries and callbacks against typed fields and current task state. The
contract is documented for users in the installed `orbit-setup` skill reference
`applications.md`. The admission code is `crates/orbit-runtime/src/host/application_consent/invocation.rs`.

## Problem

The original problem was that the orbit-office app (the user's Today and Needs-you
screens) could not carry the user's approval clicks through application authority.
Friction F2026-11-006 records that problem. The first two restrictions below describe
the exact-only baseline at `9a03c7cc5`, before slices 1-5 shipped; they are not current
limits on constrained scopes. Routine control remains refused. The office integration
is still tracked by [ORB-20363].

1. **Admission matches the whole input exactly.** A direct call is admitted only when a
   consented scope has the same tool and a structurally equal input
   (`admit_application_call`, `crates/orbit-runtime/src/host/application_consent/invocation.rs`).
   Every app approval embeds a value that does not exist at consent time: a proposal id
   and the payload digest the user reviewed (`office.proposal-accept {id, sha256}`), a
   draft id and digest (`gateway.http-send`), or a comment the user typed (send-back,
   dismiss). A standing consent cannot list them. A per-click consent would need the
   operator, and that consent would itself be the approval.
2. **Callbacks are limited to two exact writes.** Only `orbit.task.update` and
   `orbit.decision.answer` may be consented
   (`APPLICATION_OPERATION_CEILINGS` in `crates/orbit-common/src/governance/authorization/catalog/application.rs`, formerly `APPLICATION_BUILTIN_OPERATIONS`).
   `orbit.task.update` admits only `id`, `status`, `comment` and `workspace`, with
   `status` limited to `backlog`, `done`, `rejected` or `archived`
   (`application_authority.rs:270-296`). `office.proposal-accept` reads the task and its
   `proposal.json` artifact, moves the task to `in-progress` with a `plan`, then to
   `review` and `done` with an `execution_summary`. Auto-task proposals also call
   `orbit.auto_task` action `add` and `orbit.auto_task` action `update`. Every one of those callbacks is
   refused before the plugin applies anything.
3. **Routine control is refused at consent time.** `orbit.routine.control` and
   `orbit.auto_task` action `mint` fall to the "does not support application consent" arm
   (`application_authority.rs:298-302`).

The original app integration used `ORBIT_OPERATOR` to accept proposals. That standing
operator grant to a login service is what ORB-20363 rules out; the scoped office profile
is the remaining integration work.

## Goal

An operator can consent once to a bounded **approval profile** for one app. Under it the
app can accept, dismiss, approve, send back and send exactly the kinds of work the profile
names, and only at the state the user saw. Every property application authority has today
stays: the one-workspace and checkout bound, expiry and revocation, the protected witness,
digest binding of plugin manifests, per-call audit, and refusal of everything not listed.
No capability is granted. Unattended-execution controls (`orbit.routine.control`,
`orbit.auto_task` action `mint`) stay outside application authority.

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
  rows in `catalog/entry.rs`, listed in `catalog/operation.rs`) and the CLI.
- **No application MCP sessions or Windows support.** Both still fail closed.
- **No Office-specific code in Orbit core.** Constraints name tags, statuses, artifact
  paths and JSON pointers. The office profile is data the operator consents to.

## Acceptance (feature level)

- Given an operator consented to the office profile, the app accepts a pending proposal
  at the digest the user saw. The task ends `done`, and no `ORBIT_OPERATOR` is in the
  app's environment.
- Given the payload changed after review, the same accept is refused before any callback
  writes, and the audit row names the failing constraint without the payload.
- Given the same credential, a call on a task outside the predicate, an unlisted
  callback, `proposed` to `backlog`, `orbit.routine.control` or `orbit.auto_task` action `mint` is
  refused. So is a call after expiry or revocation, or from another workspace.
- Given an existing v1 exact consent, it keeps working unchanged across the upgrade.

## Task References

- [ORB-20362] — shipped scoped, audited application authority (orbit 0.41.0).
- [ORB-20363] — wires the office app's approval actions to application authority;
  office profile integration remains outstanding.
- [ORB-20852] — this design.
- [ORB-21038], [ORB-21039], [ORB-21040], [ORB-21041], [ORB-21042] — shipped Orbit slices 1-5.
- [ORB-21043] — retire v1-only consent writing after the compatibility window.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
