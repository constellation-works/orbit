---
title: "Human decision channel — Design"
owner: claude
last_updated: 2026-10-25
last_validated: 2026-10-25
status: Draft
feature: human-decision-channel
doc_role: design
type: design
summary: "Design: decision-request record, delegated-human authority, email reply authentication, stale guard, triggers and digest for deciding held Orbit work by email"
tags: [human-decision-channel, plugins, routines, blocked-recovery, task-pilot]
paths: ["crates/orbit-core/**", "crates/orbit-types/**", "crates/orbit-common/**", "crates/orbit-tools/**"]
related_features: [plugins, automation-triggers, routines, state-compatibility]
related_artifacts: [ORB-17979]
---

# Human decision channel — Design

This doc specifies the mechanisms behind [1_scope.md](./1_scope.md): the
decision-request record, the delegated-human channel that lets a transport submit a
human's answer without becoming the human, reply authentication, the stale guard,
the reply grammar, triggers, the digest, noise control, and compatibility. It ends
with the ordered implementation slices. Code anchors were verified at `1985068c2`
(2026-10-25); line numbers are orientation, symbols are the anchor. §9 (Concerns &
Honest Limitations) precedes §10 only because the slice list must close the doc;
CONVENTIONS.md §3 otherwise puts Concerns last.

## 1. Settled decisions, kept

The operator settled four points before this design was written; each one holds, and
the mechanisms below are built inside them.

1. **Core/plugin split.** Core gets a generic decision-request record and a
   delegated-human channel; email is a plugin transport. No SMTP, IMAP, MIME, or
   mailbox vocabulary enters core crates — Orbit stays generic, and a second
   transport later is a second plugin against the same core contract.
2. **Dedicated mailbox, SMTP out, IMAP polled in.** No inbound listener, no public
   webhook; the Orbit server stays loopback-only. Inbound work happens in a cron
   routine the plugin seeds (`job:` → deterministic `plugin.tool_call`), the same
   execution shape every plugin routine already uses
   ([plugins](../plugins/1_scope.md) §1).
3. **Closed decision scope.** Blocked `propose`/`report` choices, pilot-hold
   verdicts, proposed approve/reject (digest), review approve. A proposed approval
   from a decision request is screened by the task-pilot preparation and
   admission checks before promotion
   (`crates/orbit-core/src/application/task/decision.rs::ensure_proposal_admissible`):
   while a pilot admission hold or a `verified_no_diff` finding stands, or the
   task has no prepared footprint (empty `context_files` without the
   `no-diff-expected` disposition, or an unassessed complexity), the answer is
   refused, nothing is written and the request stays open — it never lands in
   `backlog` with an empty footprint. The direct operator approval beside its
   warnings stays advisory. No `--force`, no free-form task edits, no requeue
   by email.
4. **Host: dk-server-2**, owner of the workspaces the channel serves. The channel
   registration, plugin install, and secrets all live on that host; a claimed or
   remote run never submits a decision.

No settled decision had to be revisited: the authority gap (§3) is closable inside
the split, because the apply step can live in core behind one governed tool while
the plugin stays a pure transport.

## 2. The decision-request record

### 2.1 Fields

`DecisionRequest` (new, `crates/orbit-types/src/task/decision.rs`, beside
`PilotAdmissionVerdict` at line 191):

| Field | Meaning |
|---|---|
| `id` | Opaque request id (`dr-<random>`); the token binds to it, mail subjects carry it |
| `task_id` | The held task |
| `gate` | `block_triage` \| `pilot_admission` \| `proposed_approval` \| `review_approval` |
| `answers[]` | Allowed answers: stable `id`, short `label`, one-line `consequence` (rendered in mail and dashboard) |
| `binding` | Stale-guard binding (§5): per gate, the episode/operation/revision the request was raised against |
| `raised_by`, `raised_at` | The mechanism that raised it (`block_triage`, `task_pilot`, `digest`) and when |
| `expires_at` | `raised_at + request_ttl_hours` (default 72h); the token expires with it |
| `state` | `open` → `answered` \| `superseded` \| `expired` (§2.2) |
| `resolution` | On `answered`: chosen answer id, answering human, channel provenance, note text, applied-at |

Answer ids come from the source that already owns the vocabulary, never invented
per request:

- **Block triage `propose`:** the option ids `record_block_triage` records
  (`BlockTriageDecision::Propose { options, recommended }`,
  `crates/orbit-core/src/application/task/blocked_recovery/triage.rs`), plus
  `dismiss` (leave blocked, stop asking for this episode). Answering an option
  applies that option's ready-to-apply replacement text and returns the task to
  `backlog` — exactly the manual steps the triage comment instructs today — as one
  human-attributed update. Each option carries a `target` (`acceptance_criteria`
  or `description`) recorded in the proposal comment beside its replacement; the
  replacement is that field's complete new value. An option with no resolvable
  target, an empty or redacted replacement, or a replacement equal to the current
  field is refused (`answer_not_applicable`) before anything is written, so the
  task stays blocked and the request open.
- **Block triage `report` / final-recovery escalation:** answers are `done` (the
  `human_action:` the readout carries,
  `crates/orbit-core/src/application/task/blocked_recovery/readout.rs`, has been
  performed; requeue through the normal final-recovery path) and `dismiss`.
- **Pilot admission:** the `PilotAdmissionVerdict` spellings
  (`approve_anyway`, `clear`, `evaluated`; `rescope` is excluded — it edits
  criteria, which is a free-form edit). A duplicate or already-landed hold
  offers and accepts only `approve_anyway` and `clear`: `evaluated` does not
  override those findings, and an answer that cannot resolve the hold is
  refused before the request is consumed. The answer records the same
  `TaskDecision::PilotAdmission` history entry a verdict comment records, against
  the hold's `operation_id`, so `operator_resolution`
  (`crates/orbit-core/src/application/task/pilot/admission.rs:377`) accepts it
  unchanged.
- **Proposed / review approval:** `approve` / `reject`, applied through the
  screened approval / the existing reject path. A proposed `approve` first
  passes the pilot preparation and admission screening above; a review
  `approve` keeps its current guards.

### 2.2 States

- `open` — raised, unanswered, binding still current.
- `answered` — an allowed answer was applied; terminal. `resolution` records what
  was applied, by whom, through which channel.
- `superseded` — the binding stopped matching before an answer arrived (new block
  episode, newer pilot assessment, task moved, a human answered on another
  surface). Terminal; a late reply is refused under §5.
- `expired` — `expires_at` passed while open. Terminal; the digest may re-raise a
  fresh request for a still-held task (§7), which is a new request with a new
  token, never a revival.

Transitions are computed, not just stored: any read of an `open` request re-checks
its binding against the task, so a request whose hold evaporated reads as
`superseded` even if no sweeper ran. This mirrors how `PilotAdmissionHold`
(`admission.rs:30`) is derived from the latest assessment rather than stored as a
flag.

### 2.3 Storage

Requests are task-history entries, not a new store. Raising appends a
`decision_requested` history entry carrying the full record as its structured note
(the `operation_id` field holds the request id); answering appends
`decision_answered` referencing it, beside the history entry the answer itself
produces (the pilot verdict, the triage application, the approval). Supersession
and expiry need no write — they are derived on read (§2.2). This gives three
properties for free: the request travels with the task record everywhere the task
goes, publication and sync need no new table, and the audit trail a reviewer reads
is the same history they read today. The cost is a scan of task history to list
open requests across a workspace; that is the same shape as the block readout and
is bounded by the listing tools' existing task scans. If volume ever makes that
scan hot, a derived index can be added without changing the record
([state-compatibility](../state-compatibility/): additive history events are the
compatible direction; old binaries ignore unknown events).

The token's single-use marker is the `decision_answered` entry itself plus a
`decision_token_consumed` entry written on any *refused-after-authentication*
submission (§4), so replay detection is a history lookup keyed by request id and
token fingerprint.

### 2.4 Listing and surfaces

- `orbit.decision.list` (CLI `orbit decision list`): open requests, filterable by
  gate and task; `orbit.decision.show` renders one request with its answers and
  binding. Both read-only, agent-callable — an agent may *see* that a human is
  being asked; it may not answer.
- `orbit.task.show` gains `open_decisions[]` (id, gate, answer ids, expiry) so any
  surface already showing the task shows the hold.
- Dashboard: the task view renders the request with one button per answer; a
  workspace-level open-requests list aggregates them. Answering there is the
  existing human path and supersedes the email token.

## 3. Delegated-human authority

### 3.1 The gap, precisely

A decision counts as human today through one funnel: `update_task_as_human`
(`crates/orbit-core/src/application/task/update/entry.rs:165`) sets
`operator_decision_authority: true` and an explicit human `actor_label`; the bare
CLI and the dashboard are its only callers. Everything downstream keys off the
persisted writer label: `operator_resolution` drops a resolution whose writer
`is_automation_actor` (`crates/orbit-types/src/identity/actor.rs:308` — `system`,
`task-pilot`, machine ids, agent families). A plugin backend's callbacks are
confined to `permissions.orbit_tools` ∩ grant
([plugins](../plugins/1_scope.md) §4) and, at the surveyed revision, the callback
session (`crates/orbit-tools/src/plugin/callback/session.rs`) asserts no human
writer label — a plugin calling `orbit.task.update` writes as automation and its
"decision" is correctly ignored. The design therefore does **not** teach the plugin
to write as the human. It adds one core tool whose whole job is to receive an
authenticated submission and apply it *in core*, through the human funnel, when —
and only when — every check in §3.3 passes.

### 3.2 Channel registration

The operator registers a channel once, on the host, from the interactive CLI:

```
orbit decision channel register email --human daniel --plugin decision-mail
orbit decision channel revoke email
```

The registration is host config (not workspace config a task could carry): channel
name, the human identity it speaks for, and the plugin namespace allowed to submit
through it. Registration is the consent moment, like `orbit plugin enable --grant`;
revocation is immediate and also invalidates outstanding tokens (§4 keys tokens to
a registration epoch). `orbit decision channel list` shows registrations.
Registering requires the operator surface — it is refused from an agent envelope
and from any plugin callback, so no running code can mint itself a human.

### 3.3 The submission tool and the apply step

One new governed operation, `orbit.decision.submit`, joins `GOVERNED_OPERATIONS`
(`crates/orbit-common/src/governance/authorization/catalog.rs:324`) with a narrow
row: callable by the registered plugin's callback session and by the operator CLI
(for testing a channel), never by an agent session. Authorization alone grants
nothing decision-shaped; the tool then checks, in order:

1. The named channel is registered, unrevoked, and bound to the calling plugin.
2. The request exists, is `open`, and its binding still matches (§5).
3. The submitted answer id is in the request's `answers[]`.
4. The transport evidence the plugin attaches (sender, `Authentication-Results`
   verdicts, token) verifies under §4. Core verifies the token HMAC itself —
   the plugin presents evidence, core judges it.

Only then does core apply the answer through the same code the human surface uses
(`update_task_as_human` for triage/pilot answers; `approve_task` for approvals),
with the registered human's label as writer and a provenance suffix the history
entry and audit row both carry: `via decision-channel email (request dr-…, token
sha256:…)`. The writer label stays a human label — `is_automation_actor` is false
for it, which is the point — while provenance keeps the channel distinguishable
from a dashboard click forever. `CallerProvenance`
(`crates/orbit-common/src/governance/authorization/caller.rs:16`) gains no variant:
the *caller* is still a plugin callback session (`Session` provenance, plugin
capabilities); what is new is that this one operation, after its own checks,
performs a human-attributed write. Keeping provenance untouched avoids re-opening
every `authorize` call site, and the audit record states both facts (plugin caller,
human writer, channel) instead of collapsing them into one misleading label.

What this deliberately does not do: the channel cannot edit fields, move statuses
beyond the answer's own meaning, answer a request that was never raised, or widen
an answer set. `orbit.task.update` and `force_update_task_with_identity` stay
exactly as closed to plugins and agents as they are today. The guard test the
implementation must carry: an agent-envelope caller and an unregistered plugin
caller submitting a well-formed answer to an open request are both refused, and a
pilot hold so "answered" remains held.

## 4. Reply authentication

An approval admits agent code execution, so the reply is authenticated as a
credential, in layers; every layer must pass.

1. **Per-request token.** When a request is raised, core mints
   `token = base32(HMAC-SHA256(channel_key, request_id ‖ task_id ‖ epoch ‖ expires_at))`
   truncated to 26 chars, rendered into the mail (§6). The key is generated at
   channel registration and held as a plugin secret
   (`orbit plugin secret set decision-mail channel_key`, delivered per call in
   `context.secrets`, never in argv, env, logs, or the manifest —
   [plugins](../plugins/1_scope.md) §3) *and* mirrored to core's channel store at
   registration, because core verifies (§3.3 step 4) and must not take the
   plugin's word for it. Rotation: `orbit decision channel rotate-key email`
   bumps `epoch`, invalidating every outstanding token; open requests are
   re-mailed by the next digest under fresh tokens. Routine rotation is an
   operator habit the runbook sets (suggested: on any suspicion, and at least
   yearly); revocation (§3.2) is the stronger form.
2. **Single use.** A token that has produced an apply — or a post-authentication
   refusal — is consumed (§2.3). Reuse is refused and alerted (§5), never applied
   twice; this also makes a forwarded mail inert after the real reply lands.
3. **Expiry.** The token dies at the request's `expires_at` (default 72h). No
   grace period: an expired reply gets a refusal mail naming the dashboard path.
4. **Sender allowlist.** The envelope-and-`From` sender must be on the channel's
   allowlist (plugin config, e.g. the operator's addresses). Anything else is
   dropped by the plugin before it ever calls core — cheap, and keeps core's
   refusal log signal-only.
5. **DKIM/SPF.** The plugin reads `Authentication-Results` as stamped by the
   mailbox provider and requires `dkim=pass` *and* `spf=pass` with alignment to
   the allowlisted domain; it forwards the raw header in the submission and core
   re-checks the verdict strings. A provider that does not stamp
   `Authentication-Results` cannot serve as the channel mailbox — stated plainly
   in the plugin's install docs, because silently skipping this layer would
   reduce authentication to "knows a token from a possibly-forwarded mail".
6. **Binding.** The token names one request; the reply's answer is interpreted
   against that request only. A token pasted into a reply about another task
   answers the token's request or nothing — there is no free-floating "approve".

Header hygiene: the plugin uses the topmost `Authentication-Results` whose
authserv-id matches the configured provider, ignores any below it (attacker-added
headers sit below the provider's stamp), and rejects on `dmarc=fail` even if DKIM
or SPF alone passed.

## 5. Stale guard and refusals

The binding (§2.1) is what makes a slow reply safe. Per gate, the request binds to:

| Gate | Binding | Superseded when |
|---|---|---|
| `block_triage` | Block episode key (the `episode=` the triage marker records, `blocked_task_triaged`) | Task left `blocked`, a new episode began, or a human touched the task after the block (the same exclusion triage candidacy uses) |
| `pilot_admission` | Pilot `operation_id` of the holding assessment | A newer assessment landed, or an operator verdict already resolved that `operation_id` |
| `proposed_approval` | Task revision + `proposed` status | Status moved, or description/criteria changed since the digest rendered |
| `review_approval` | Task revision + `review` status + candidate identity | Status moved, or a new candidate replaced the reviewed one |

Every refusal writes nothing to the task beyond, where noted, a history marker, and
produces a refusal reply mail (rate-limited per §8) so the human learns the answer
did not land — silence after a decision mail is the failure mode this design most
avoids. The named refusal cases:

| Case | Expected behaviour |
|---|---|
| **Forged or unknown sender** (not allowlisted, or DKIM/SPF/DMARC not `pass`) | Plugin drops the message, logs sender + verdicts, no core call, no reply mail (never confirm to a forged sender that the mailbox is live). A digest of drops surfaces in the plugin's status panel. |
| **Reused token** | Core refuses `token_consumed`; refusal mail to the *allowlisted* address (not the sender of the replay) warning that a consumed token was presented again; the original answer stands. |
| **Expired token** | Core refuses `request_expired`; refusal mail explains the request lapsed and links the dashboard list; if the task is still held, the next digest re-raises with a fresh token. |
| **Task moved off the episode/revision** (superseded) | Core refuses `request_superseded`, naming what changed in the refusal mail ("task was re-triaged", "newer pilot assessment", "already answered on the dashboard by …"). Nothing applied; the human decides on the *current* hold via its own request. |
| **Answer not in the allowed set** (bad number, unknown word, `rescope` by email) | Core refuses `answer_not_allowed`; the token is *not* consumed (a typo must not burn the credential); refusal mail re-lists the allowed answers and invites a corrected reply until expiry. |
| **Unparseable reply** (no answer token found) | Treated as `answer_not_allowed` with the grammar shown; token not consumed. After three unparseable replies to one request, stop replying about it (loop guard) and note it in the next digest. |
| **Channel revoked / key rotated since mailing** | Core refuses `channel_revoked` / `token_consumed`-class refusal; no apply. Open requests wait for re-mailing under the new epoch. |
| **Unknown request id / malformed token** | Dropped like a forged sender: no reply mail. A structurally valid token for a *deleted* task gets `request_superseded`. |

Two refusals consume the token (successful apply, and any authenticated submission
whose binding or channel check fails), because in those cases the legitimate reply
has demonstrably arrived and been judged; parse-level refusals do not, because no
decision was expressed yet.

## 6. Reply grammar and message shape

Outbound mail (plugin-rendered from the request record):

- Subject: `[orbit] decision needed: <task title> (<task id>) [dr-…]`.
- `Reply-To: orbit+<token>@<mailbox domain>` where plus-addressing is available;
  the token is repeated as a first body line (`orbit-token: <token>`) as the
  fallback for clients or providers that strip it. The plugin accepts either,
  requiring exactly one token total.
- Body: what is held and why (the triage blocker quote, the holding pilot
  findings, or the approval summary), then the numbered answers with
  consequences, then "reply with just the number (or word); anything after the
  first line is kept as your note." A dashboard link closes the mail.

Inbound grammar, applied to the first non-quoted line of the reply body (quoted
history and signatures below `-- ` are ignored):

```
reply     := answer ("," answer)*        # digest mail only may carry several
answer    := <n> | <word>                # the printed number, or the answer word
word      := approve | reject | approve-anyway | clear | evaluated | done | dismiss
```

- `1`, `approve`, `reject 2,5` are all valid. Numbers index the answers as
  printed *in that mail*; words map to answer ids case-insensitively,
  `-`/`_`/space-insensitively (`approve anyway` ≡ `approve-anyway`).
- Everything after the first line is the **note**: stored in `resolution` and the
  history entry as the human's evidence text. For the pilot `evaluated` verdict,
  which today needs evidence after the header line
  (`stated_verdict`, `admission.rs`), the note *is* that evidence and an empty
  note is refused as `answer_not_allowed` with a re-listing. An `evaluated`
  answer whose evidence must be an *artifact reference* is out of scope for
  email v1: the mail for such a hold offers `approve-anyway`/`clear` and directs
  `evaluated` to the dashboard, rather than inventing an attachment contract.
- In a digest mail (§7), answers are per-item (`approve 1,3`, `reject 2`); an
  unqualified `approve` in a digest reply is refused as ambiguous — "all listed"
  is never inferred. In a single-request mail, bare `approve` is unambiguous and
  accepted.
- Attachments are ignored entirely. HTML-only replies are read through their
  `text/plain` alternative; a reply with no readable text part is unparseable.

## 7. Triggers and the digest routine

**Decision: add a new state-trigger kind.** `StateTriggerKind`
(`crates/orbit-types/src/workflow/automation/members.rs:16`) gains
`DecisionRequested`: a task with an `open` decision request that has not yet been
notified. A plugin-seeded state routine on this kind runs the mail job; the member
settles when the request leaves `open` or the notification marker is written.

Why not poll the existing markers (`blocked_task_triaged`,
`pilot_warning_triaged`)?

- The markers record that *triage* happened, not that a *decision is owed*:
  `propose` and `report` owe one, a `mechanical` requeue does not, and the marker
  alone forces every consumer to re-derive that from comment text.
- Polling has no exactly-once story. The state-trigger machinery already owns
  debounce, per-member settlement, retries, and the batching the noise rules need
  (§8); a plugin poll would rebuild them worse, per transport.
- The trigger keys off the request record (§2), so pilot holds, approvals, and
  future gates ride the same kind instead of one marker per source.

Costs, accepted: `StateTriggerKind` is a closed, persisted enum whose existing
comment shows the discipline — variants are retained for persisted-state
compatibility even after retirement (`ExecutionFailed`). Adding `DecisionRequested`
is the compatible direction (new definitions name it; old member state never
contains it), but it is forever: the variant, its observation query (§2.4's open
list), and its settlement rule become core maintenance. That is the right trade —
the alternative smears the same logic across every transport plugin.

**Raising.** Requests are raised by the mechanisms that create the hold, in the
same write: `record_block_triage` on `Propose`/`Report` dispositions; pilot
admission when a hold is first derived for an `operation_id`; the digest routine
for `proposed` tasks (raising is idempotent per binding — one open request per
(task, gate, binding), enforced at raise time). Raising happens only while
`[decision_channel] enabled`; with the channel off, no records, no trigger
members, no behaviour change.

**Digest.** A cron routine (plugin-seeded, default daily 08:00 host-local,
`enabled: false` until the operator turns it on) calls a plugin tool that lists
`proposed` tasks without an open request plus every still-open request nearing
expiry, raises what's missing, and sends one digest mail: proposed tasks first
(title, one-line summary, pilot state), then open-request reminders. Digest
answers follow §6. The digest is also the re-mail path after key rotation or
expiry — there is no separate reminder machinery.

Core's half of that call is one governed operation, `orbit.decision.digest`
(CLI `orbit decision digest --channel <channel>`), implemented in slice 6. One
call raises what's missing and returns the whole digest: for every `proposed`
task without an open `proposed_approval` request — and every `review` task
without an open `review_approval` request — it raises one open request,
idempotently per (task, gate, binding), with answers `approve`/`reject` and
`raised_by: digest`; then it returns those requests beside every still-open
request expiring within **24 hours** (the digest window), split into
`requests` (the digest), `raised` (what this call raised) and `expiring`. A
second call raises nothing new and returns the same request ids. The approval
binding's revision is the task's spec digest — its description and acceptance
criteria hashed — not `updated_at`: appending the raise itself moves
`updated_at`, which would supersede every approval request as it was raised,
while the spec digest changes exactly when what the digest rendered changed.
`review_approval` additionally binds the candidate identity the task history
currently names (the latest held candidate or review-gate note candidate), so
a replacement candidate supersedes it. The operation is governed like
`orbit.decision.submit`: callable only by the named channel's bound plugin
through its callback session or by the operator, never by an agent session
(`caller_not_allowed`); a plugin the channel is not bound to, and an
unregistered or revoked channel, are refused (`channel_not_registered`,
`channel_revoked`) and nothing is raised. With `[decision_channel]` disabled
the digest raises nothing, writes nothing and returns no requests. Rendering,
batching, quiet hours and multi-answer parsing stay the plugin's (§6, §8).

## 8. Noise control and transport failure

- **Once per episode.** One notification per (task, gate, binding); the trigger
  member settles on notify. A task that blocks three times gets at most one mail
  per episode, and only the digest ever repeats a still-open request.
- **Batching mode.** `[decision_channel] digest = immediate | daily | off`:
  `immediate` mails each request as raised; `daily` (default) holds non-blocked
  gates for the digest and sends blocked-task requests immediately (a blocked
  task is stopped work; a proposed task is unstarted work). `off` means requests
  exist on the dashboard only.
- **Quiet hours.** `[decision_channel] quiet_hours = "22:00-07:00"` defers sends
  to the window's end; expiry is computed from send time so a deferred mail never
  arrives pre-expired.
- **Confirmation.** Every apply sends a short confirmation (task, answer, new
  state) — batched into the next digest when several digest answers land at
  once. Refusals reply per §5, rate-limited to one mail per request per hour.
- **SMTP down.** Send failures retry with the routine's normal backoff; the
  request stays `open` and un-notified, visible on the dashboard, and the plugin
  status panel shows the transport error. No request is ever marked answered or
  dropped because mail failed.
- **IMAP down / auth failure.** The poll tool fails its run visibly (routine
  history + status panel) and retries next tick; messages accumulate in the
  mailbox and are processed in arrival order on recovery, stale-guarded as
  usual. A prolonged outage therefore degrades to "decisions apply late or
  expire", never to "decisions apply wrongly".
- **Mailbox hygiene.** The plugin processes only messages to its own address(es),
  marks handled messages seen and files them to a processed folder, and never
  deletes mail it did not understand — an unparseable human mail stays for
  eyeballs.

## 9. Concerns & Honest Limitations

- **The mailbox is part of the trusted computing base.** DKIM/SPF verdicts are
  the provider's assertion; a provider that stamps them dishonestly or a
  compromised operator mailbox defeats layers 4–5 of §4, leaving the token and
  the closed answer set as the remaining bounds. That is still a materially
  smaller surface than "any mail can move a task", but it is not equivalent to
  sitting at the dashboard, and the design does not pretend otherwise.
- **Core judges evidence the plugin gathered.** Core re-checks token and verdict
  strings, but without the raw message it cannot independently verify DKIM — it
  trusts the plugin to forward the provider's header faithfully. The containment
  is structural: the plugin is operator-installed and grant-bound, the answers
  are closed, and a lying plugin could already do damage within its other
  grants. Slice 7 narrows this: a submission may carry `raw_message` (at most
  262144 bytes, 256 KiB), and when it does core verifies the DKIM signature
  itself — selector key from DNS, body hash, signed-header signature, and
  relaxed alignment of the signing domain with the sender's domain — refusing
  `dkim_verification_failed` when its own result contradicts the forwarded
  verdicts. A channel registered with `--require-dkim-verification`
  (`require_dkim_verification`, off by default) refuses a submission without a
  raw message as `dkim_verification_required`; with the switch off and no raw
  message, submission is exactly as before. A DNS lookup failure refuses as
  retryable `dkim_verification_unavailable` and consumes nothing; an oversized
  raw message refuses as `raw_message_too_large`. The verification runs before
  the channel store lock is taken, with a 5-second per-nameserver timeout, so
  it never blocks revoke or rotate. Dependency choice: RSA-SHA256 verification
  reuses the `rsa` crate already in the workspace for release-signature
  verification (public-key verification only — see the RUSTSEC-2023-0071 note in
  `deny.toml`), and TXT lookups use a minimal UDP client over the standard
  library against the host's `/etc/resolv.conf` nameservers, behind an
  injectable resolver trait for tests. No new DNS crate was taken: a single TXT
  query shape needs no general resolver, and keeping the supply chain unchanged
  (licenses and advisories already reviewed in `deny.toml`) outweighs a
  dependency's broader resolver machinery. Only `rsa-sha256` signatures are
  verified; `ed25519-sha256` and ARC remain out of scope.
- **History-scanned listing is a deliberate simplicity tax.** Listing open
  requests scans task histories until volume proves otherwise; on a workspace
  with very long histories the trigger observation inherits that cost.
  Batch/debounce defaults must be set with the scan in mind.
- **Email is a lossy, quotable medium.** Tokens sit in inboxes and quoted
  threads. Single use, expiry, binding, and epoch rotation bound a leaked
  token's value to one answer on one already-held task, but the operator's mail
  retention is outside Orbit's control.
- **Two humans, one channel.** The registration maps a channel to one human.
  Shared-mailbox teams would need per-sender identity mapping, which v1 refuses
  rather than half-supports: an allowlisted sender who is not the registered
  human is refused `answer_not_allowed`-class with an explanation.
- **Compatibility.** Additive surfaces only: new history events, new tools, a new
  enum variant, new optional fields on `orbit.task.show`. The 30-day compat
  window for internal/CLI/MCP surfaces is honoured by keeping every existing
  shape byte-stable and gating all new behaviour behind
  `[decision_channel] enabled = false`. Plugin protocol impact: none in slices
  1–3; the email plugin (slice 5) uses existing secrets, routines, and callback
  machinery — if it turns out to need a host-API addition, that is called out as
  its own slice with its own compat note, not folded in silently. The persisted
  `DecisionRequested` variant follows the retain-don't-remove rule from its
  first release.

## 10. Implementation slices

Ordered; each slice is independently deliverable and leaves the tree shippable.
"Owner" names the crate(s) or plugin that change; filing these as tasks is the
operator's step after review.

1. **Decision-request record and raising.** Owner: `orbit-types`
   (`DecisionRequest`), `orbit-core` (raise from `record_block_triage` and pilot
   admission, derived state, `[decision_channel] enabled`). Depends on: nothing.
   Acceptance sketch: with the channel enabled, a `propose` triage and a pilot
   hold each raise exactly one open request with the right answers and binding;
   disabled, nothing is written.
2. **Listing and local answering surfaces.** Owner: `orbit-tools`
   (`orbit.decision.list/show`), `orbit-core` (`orbit.task.show` open decisions,
   the core apply step reused by §3), `orbit-web` (dashboard request list and
   answer buttons), CLI (`orbit decision …`). Depends on: 1. Acceptance sketch:
   an operator answers a raised request from CLI and dashboard; the answer lands
   through `update_task_as_human`/`approve_task` and the request reads
   `answered`; goldens updated via `make goldens`.
3. **Delegated-human channel and governed submission.** Owner: `orbit-common`
   (`GOVERNED_OPERATIONS` row), `orbit-core` (registration store,
   `orbit.decision.submit`, token mint/verify, §3.3 checks, §5 refusals).
   Depends on: 1, 2. Acceptance sketch: a registered test channel applies an
   allowed answer as the human with provenance; agent-envelope and unregistered
   callers are refused and the pilot hold stays held (the §3.3 guard test).
4. **`DecisionRequested` state trigger.** Owner: `orbit-types`
   (`StateTriggerKind`), `orbit-core` (observation over slice 2's listing,
   settlement on notify/answer). Depends on: 1. Acceptance sketch: a state
   routine fires once per open request per binding, settles on answer or
   supersession, and persisted member state from before the variant still loads.
5. **Email transport plugin (`decision-mail`).** Owner: new plugin (SMTP send,
   IMAP poll job + cron routine, §6 rendering/parsing, §4 layers 4–5, secrets).
   Depends on: 3, 4. Acceptance sketch: end-to-end on dk-server-2 against the
   real mailbox — raise → mail → reply `2` → applied + confirmed; each §5
   refusal case demonstrated once; plugin conformance tests for the grammar.
6. **Digest and approval gates by email.** Owner: `orbit-core` (proposed/review
   request raising, digest query), `decision-mail` plugin (digest routine,
   batched answers, quiet hours, batching mode). Depends on: 5. Acceptance
   sketch: daily digest lists proposed tasks and expiring requests; `approve
   1,3` promotes through task-pilot only; unqualified digest `approve` refused.
7. **Hardening and operations (optional, schedulable any time after 5).**
   Owner: `orbit-core`/`decision-mail` (core-side DKIM verification of the raw
   message, drop/refusal metrics on the status panel, runbook for registration,
   rotation, and revocation). Depends on: 5. Acceptance sketch: a forged-header
   fixture that fools forwarded verdicts is refused by core-side verification;
   the runbook walks register → rotate → revoke against the live mailbox.

## Task References

- [ORB-17979] — designed the email channel for human decisions on held Orbit work (this folder)

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
