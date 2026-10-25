---
title: "Scope: human decision channel"
owner: claude
last_updated: 2026-10-25
last_validated: 2026-10-25
status: Draft
feature: human-decision-channel
doc_role: scope
type: design
summary: "Scope: a core decision-request record and delegated-human channel, with email as a plugin transport, so held Orbit work can be decided by replying to an email"
tags: [human-decision-channel, plugins, routines, blocked-recovery, task-pilot]
---

# Scope: human decision channel

Status: proposed. This folder is the design deliverable of [ORB-17979]; no code in this
change. The folder pairs this scope doc with [2_design.md](./2_design.md), which carries
the mechanisms. That pairing deviates from the two tracks in
[CONVENTIONS.md](../CONVENTIONS.md) §1 (scope track is `1_scope.md` alone; full track
starts at `1_overview.md`); the deviation is the task's own acceptance shape and is
recorded here per CONVENTIONS.md §11. If the folder grows specs or a glossary it
graduates to the full track in that change (Problem and Goal become `1_overview.md`).
Bearing: block triage
(`crates/orbit-core/src/application/task/blocked_recovery/triage.rs`), task-pilot
admission holds (`crates/orbit-core/src/application/task/pilot/admission.rs`), the
human update path (`update_task_as_human`,
`crates/orbit-core/src/application/task/update/entry.rs`), the authorization catalog
(`crates/orbit-common/src/governance/authorization/`), state and cron triggers
(`crates/orbit-types/src/workflow/automation/members.rs`,
[automation-triggers](../automation-triggers/3_vision.md)), and the plugin standard
([plugins](../plugins/1_scope.md)). Anchors verified at `1985068c2`
(2026-10-25); [2_design.md](./2_design.md) cites them per mechanism.

## Problem

Orbit already knows when work is waiting on a human, and records that fact in four
different shapes: a block triage that ends in `propose` (2–3 options with a
recommendation) or `report` (`human_action:`); a final-recovery escalation carrying a
`human_action:` line; a task-pilot admission hold waiting for an operator verdict
comment; and the ordinary approval gates (`proposed → backlog`, `review → done`).
None of these reaches the human. There is no outbound notification for blocked tasks
or pilot holds — the desktop watcher (`desktop/src/watcher.rs`) does not cover them —
so a held task sits until someone happens to open the dashboard or a task list. The
hold is durable and well-described; the last mile to the person who can clear it does
not exist.

The return path is the harder half. A human decision counts today only when it is
written by a non-automation writer label through the human surfaces: the dashboard
comment endpoint and the bare CLI, both funnelling into `update_task_as_human`
(`operator_decision_authority: true`). Pilot admission's `operator_resolution`
explicitly discards any resolution whose writer `is_automation_actor`
(`crates/orbit-types/src/identity/actor.rs:308`). A plugin backend calling back into
Orbit tools is an automation actor with a plugin provenance; at the surveyed revision
nothing in the plugin callback path
(`crates/orbit-tools/src/plugin/callback/session.rs`) can stamp a human writer label.
So even a perfect email transport built purely as a plugin cannot record the decision
it carried: the authority gap is in core, and closing it without weakening the
agent/automation exclusion is the central design problem. An approval also has real
consequences — it admits agent code execution — so the reply path is a security
boundary, not a convenience shortcut.

## Goal

When work parks on a human decision and the channel is enabled, Orbit raises a
*decision request* naming the task, the gate, and the closed set of allowed answers;
a transport plugin emails it to the operator; the operator replies with one allowed
answer; Orbit authenticates the reply, checks it is still about the same hold, and
records the decision as the human, with the channel's provenance attached. Core owns
the request record, the delegated-human authority, and the apply step; the plugin
owns SMTP, IMAP, and message formatting. Four decisions are settled by the operator
and this design keeps them (re-stated in [2_design.md](./2_design.md) §1): the
core/plugin split with no email specifics in core; a dedicated mailbox with SMTP out
and IMAP polled in, no inbound listener; a closed decision scope (blocked
propose/report choices, pilot-hold verdicts, proposed approve/reject by digest,
review approve) with proposed approvals always pilot-gated and no `--force` or
free-form edits; and dk-server-2 as the host. Everything else about how Orbit runs —
the loopback-only server, the automation exclusion, the no-event-bus rule — stays
unchanged.

## Shape

### Decision requests (core)

Core gains one record, `DecisionRequest`: the task, the gate kind
(`block_triage`, `pilot_admission`, `proposed_approval`, `review_approval`), the
allowed answers as stable ids taken from the source of truth that raised the hold
(triage option ids, `PilotAdmissionVerdict` spellings, `approve`/`reject`), a
stale-guard binding (block episode key, pilot `operation_id`, or task revision), an
expiry, and a state (`open`, `answered`, `superseded`, `expired`). Requests are
stored beside the task's own history rather than in a new store, listed through new
read tools, and rendered by the dashboard so the email is never the only place a
hold is visible. [2_design.md](./2_design.md) §2 specifies fields, states, storage,
and listing.

### Delegated-human channel (core)

The operator registers a named channel once (`email → daniel`). A channel may answer
an *open* request with one of its *allowed* answers and nothing else: no arbitrary
task edits, no status changes outside what the answer itself means, no `--force`.
The apply step runs in core and writes through the existing human path, so the
recorded writer is the human (`is_automation_actor` stays false) while the history
entry and audit row carry the channel provenance (`via email`, request id, token
fingerprint). The plugin never writes the task; it submits a *decision submission*
to one governed core tool, and core either applies it as the human or refuses it.
[2_design.md](./2_design.md) §3 shows the fit with `CallerProvenance`
(`crates/orbit-common/src/governance/authorization/caller.rs:16`) and
`GOVERNED_OPERATIONS` (`crates/orbit-common/src/governance/authorization/catalog.rs:324`).

### Email transport (plugin)

A plugin (working name `decision-mail`) sends over SMTP and polls a dedicated
mailbox over IMAP from a cron routine; credentials and the token-signing key are
plugin secrets. Each outbound mail carries a per-request, single-use, expiring HMAC
token bound to the request id; a reply counts only from an allowlisted sender with
DKIM/SPF `pass` in `Authentication-Results` and a live token. The reply grammar is
deliberately tiny (`1`, `approve`, `reject 2,5`, plus free text kept as the note).
[2_design.md](./2_design.md) §4–§6 specify authentication, the stale guard with its
refusal table, and the grammar.

### Triggers and digest

A new state-trigger kind fires when a triage records a human decision, so the mail
goes out once per block episode instead of whenever a poll next notices a marker;
pilot holds ride the same kind. A cron routine sends the proposed-task digest and
reminds about still-open requests. [2_design.md](./2_design.md) §7 gives the choice
and its reasons, and §8 the noise controls (once per episode, batching, quiet hours,
confirmation mail, transport-down behaviour).

### Configuration

| Key | Default | Meaning |
|---|---|---|
| `[decision_channel] enabled` | `false` | Master opt-in, per workspace; nothing is raised or sent while off |
| `[decision_channel] channel` | unset | Registered channel name → human identity mapping (operator-set, host config) |
| `[decision_channel] request_ttl_hours` | `72` | Request (and token) expiry |
| `[decision_channel] digest` | `daily` | Digest cadence for proposed tasks and open-request reminders |
| `[decision_channel] quiet_hours` | unset | Local-time window in which non-urgent mail waits |
| `[plugins.decision-mail] …` | — | SMTP/IMAP endpoints, mailbox, sender allowlist (plugin config schema; secrets via `orbit plugin secret set`) |

### Surfaces

- **CLI/MCP (core):** `orbit decision list|show`, `orbit decision answer` (the local
  equivalent of replying), `orbit decision channel register|revoke`; tools
  `orbit.decision.list`, `orbit.decision.show`, and the governed
  `orbit.decision.submit` the plugin calls. `orbit.task.show` gains the task's open
  requests.
- **Dashboard:** an open-requests list with the same answer buttons the email offers,
  so email is an accelerator, never the only path.
- **Email:** one message per decision request (or one digest), a confirmation after
  apply, and a refusal reply naming why a decision was not applied.

## Non-goals

- **No event bus, no resident listener.** Ruled out already
  ([automation-triggers](../automation-triggers/3_vision.md)); inbound mail is polled
  by a cron routine and the server stays loopback-only.
- **No free-form task editing by email.** No field edits, no `--force`, no requeue,
  no criteria changes. The channel picks from a closed answer set on an open request.
- **No other transports in v1.** The core record is transport-neutral, but only the
  email plugin is designed here; a second transport is a later plugin against the
  same core contract.
- **No change to who counts as automation.** `is_automation_actor` and the pilot and
  recovery exclusions keep their meaning; the channel enters through the human path
  with provenance, not by reclassifying a plugin as human.
- **No implementation in this task.** [2_design.md](./2_design.md) ends in ordered
  slices; filing them is the operator's job after review.

## Concerns and honest limitations

- **Email authentication is probabilistic at the edges.** DKIM/SPF plus a token is a
  strong combination, but a compromised operator mailbox is a compromised channel;
  the design bounds the blast radius (closed answers, single-use tokens, expiry,
  stale guard) rather than eliminating it. Revoking the channel registration is the
  kill switch and must be one command.
- **Latency is poll-bound.** IMAP polling on a cron routine means decisions apply on
  the poll interval, not instantly. That is acceptable for holds measured in hours
  and is the price of no listener.
- **The `evaluated` pilot verdict does not fit a bare reply.** It needs attached
  evidence; v1 either excludes it from email or treats the reply body as the
  evidence note ([2_design.md](./2_design.md) §6 decides: body-as-note, artifact
  reference excluded).
- **A new persisted record is a compatibility surface.** The request store rides the
  task history to avoid a new database schema, but the new `StateTriggerKind`
  variant is persisted member state and must follow the enum's
  retain-don't-remove rule ([state-compatibility](../state-compatibility/)).
- **Plugin callback provenance is only partly traced.** At the surveyed revision the
  callback session carries no human writer label, which is what forces the
  core-side apply step; if implementation finds an existing labelled path, slice 2
  shrinks but the governed submit tool stays.

## Phasing

Phases mirror the slice list in [2_design.md](./2_design.md) §10; each phase is
independently deliverable and useful before email exists.

1. **Decision requests in core.** Record, raising from block triage and pilot holds,
   CLI/MCP/dashboard listing and local answering. No channel yet.
2. **Delegated-human channel.** Registration, the governed submission tool, and the
   core apply step with provenance — exercisable from the CLI with a test channel.
3. **Trigger and digest plumbing.** The new state-trigger kind and the digest
   routine definitions (no transport wired).
4. **Email plugin.** SMTP out, IMAP poll in, token authentication, reply grammar,
   confirmation and refusal mail.
5. **Approvals by email.** Proposed-task digest answers (pilot-gated) and review
   approve join the allowed scope.

## Open questions

1. **Token placement: subject, body, or reply-to address?** A `Reply-To` of
   `orbit+<token>@` survives most clients untouched but needs plus-addressing on the
   mailbox; a first-line body token is visible and quotable. [2_design.md](./2_design.md)
   §4 recommends reply-to with a body fallback; the plugin slice confirms against the
   real mailbox provider.
2. **One mailbox for every workspace on dk-server-2, or one per workspace?** One
   mailbox with the workspace named in the token payload is simpler to operate; the
   request id already disambiguates. Per-workspace mailboxes isolate a leak.
3. **Should a digest `approve` batch dozens of proposed tasks at once?** The grammar
   allows `approve 1,3,7`; whether an unqualified `approve` means "all listed" is a
   foot-gun the digest slice must decide explicitly (design says: never).

## Acceptance (feature level)

- Given a blocked task triaged `propose` with the channel enabled, exactly one email
  per block episode offers the triage's options; replying `2` records option 2 as the
  human, with channel provenance, and a confirmation mail follows.
- Given a pilot admission hold, replying `approve-anyway` (or its digest number)
  records the same history entry a dashboard verdict comment records, and
  `operator_resolution` accepts it; an agent or automation writer attempting the
  same submission is still refused.
- A reply from a non-allowlisted sender, with failing DKIM/SPF, with a reused or
  expired token, for a superseded episode, or naming an answer outside the allowed
  set changes nothing and produces the refusal behaviour in
  [2_design.md](./2_design.md) §5.
- A proposed task approved from the digest enters task-pilot like any other
  approval; it never lands in `backlog` with an empty footprint.
- With the channel disabled (the default), no requests are raised, no mail is sent,
  and every existing surface behaves as today.

## Task References

- [ORB-17979] — designed the email channel for human decisions on held Orbit work (this folder)

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
