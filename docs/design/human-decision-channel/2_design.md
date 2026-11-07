---
title: "Human decision channel — Design"
owner: claude
last_updated: 2026-11-07
last_validated: 2026-11-07
status: Draft
feature: human-decision-channel
doc_role: design
type: design
summary: "Design: decision-request record, delegated-human authority, email reply authentication, stale guard, triggers and digest for deciding held Orbit work by email; plugin-raised requests with an apply callback"
tags: [human-decision-channel, plugins, routines, blocked-recovery, task-pilot]
paths: ["crates/orbit-core/**", "crates/orbit-types/**", "crates/orbit-common/**", "crates/orbit-tools/**"]
related_features: [plugins, automation-triggers, routines, state-compatibility, decision-assistance]
related_artifacts: [ORB-17979, ORB-21089, ORB-22824]
---

# Human decision channel — Design

[ORB-17979] authored this design. This doc specifies the mechanisms behind [1_scope.md](./1_scope.md): the
decision-request record, the delegated-human channel that lets a transport submit a
human's answer without becoming the human, reply authentication, the stale guard,
the reply grammar, triggers, the digest, noise control, and compatibility. §10
lists the ordered implementation slices, all done. §11 records the request
fields, model-answer provenance and veto path for precedent-answered decisions,
with their own slices (the first two implemented). §12 ([ORB-22824], designed, not
implemented) adds plugin-raised requests whose effect is the plugin's own apply tool,
with its own concerns, slices and compatibility notes. Code anchors were re-checked at `b32997c50`
(2026-11-01); symbols are the anchor, and line numbers that no longer sit on the
symbol were dropped. §9 (Concerns &
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
   (`crates/orbit-core/src/application/task/decision/answering.rs::ensure_proposal_admissible`):
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

`DecisionRequest` (new, `crates/orbit-types/src/task/decision/request/model.rs`):

| Field | Meaning |
|---|---|
| `id` | Opaque request id (`dr-<random>`); the token binds to it, mail subjects carry it |
| `task_id` | The held task |
| `gate` | `block_triage` \| `pilot_admission` \| `proposed_approval` \| `review_approval` \| `agent_request`; proposed: `model_answer_review` (§11.3) |
| `answers[]` | Allowed answers (the request's options): stable `id`, short `label`, one-line `consequence` (rendered in mail and dashboard); per-answer `risk_tier` (§11.1) |
| `risk_tier`, `category` | The most severe answer tier (§11.1), `unknown` when undeclared; the stored category the request is measured under |
| `binding` | Stale-guard binding (§5): per gate, the episode/operation/revision the request was raised against |
| `raised_by`, `raised_at` | The mechanism that raised it (`block_triage`, `task_pilot`, `digest`, `agent_request`) and when |
| `topic`, `filer` | Agent-raised requests only: an exact-match structured topic (or corpus rule id) and the raising agent's writer label (the per-filer cap counts by it) |
| `expires_at` | `raised_at + request_ttl_hours` (default 72h); the token expires with it |
| `state` | `open` → `answered` \| `superseded` \| `expired` (§2.2) |
| `resolution` | On `answered`: chosen answer id, `answered_by` (a human label, the `precedent` actor, or the `model` actor, §11.2), channel provenance, note text, applied-at, and the precedent or model citation when not human |

Answer ids come from the source that already owns the vocabulary, never invented
per request:

- **Final-recovery `decide`** ([ORB-20629]): the request is self-contained. It carries the
  option ids final recovery's `decide` decision proposes (each with a summary, a
  consequence, an optional field rewrite and an optional action from `requeue`, `re_pilot`,
  `change_crew`, `split`, `reject`, `archive`), the recommended one marked, and `dismiss`
  (leave blocked, stop asking for this episode). Answering an option is one human-attributed
  operation: a comment naming the option and note, the rewrite applied as the field's complete
  new value, the action run, the task moved out of `blocked`, and the request closed. An
  option with neither a rewrite nor an action, an unknown action, or a rewrite equal to the
  current field is refused (`answer_not_applicable`) before anything is written, so the task
  stays blocked and the request open. The same card shape serves `proposed_approval` for
  pilot-warning triage, where an option approves or re-pilots the held proposed task.
  `re_pilot` explicitly submits one pilot even without a field rewrite; while a pilot is
  pending or running, the answer is refused (`answer_not_applicable`, nothing applied, the
  request stays open) until that pilot settles. The first routine assessment is automatic. A certified
  description or acceptance-criteria edit later queues a fresh routine pilot unless a live execution claim or an unresolved failed pilot holds the task; edits to other material or policy never queue one through the routine, but an `--approve-proposed` drain repilots a stale assessment automatically when its certified description/criteria or other material drifted and no live execution claim holds it; delivery waits for a new
  assessment of edited description/criteria (drain repilot, `orbit task repilot <id>`,
  `orbit.task.repilot`, or task update `repilot: true`). A held warning retains
  one decision across routine windows until answered or explicitly reassessed.
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
  (`crates/orbit-core/src/application/task/pilot/admission/backlog.rs`) accepts it
  unchanged.
- **Agent request** ([ORB-20736], `orbit.decision.raise`): an agent puts a multiple-choice question
  on a task assigned to a human crew. It carries two to four plain answers `1`..`4` (one-line
  label and consequence), the one the agent recommends first as `1`, and no rewrite or action.
  It is task-scoped: no store, table or precedent lookup; the record is the task's history.
  Raising is bounded: the same open question (case- and spacing-insensitive, same topic)
  returns the existing request, a task holds one open request, and open agent-raised requests
  are capped at 20 per workspace and 5 per filer, refused past either with nothing written.
  Only the operator answers (an agent label is refused `caller_not_allowed`): the answer
  records who, when and which option, comments the option's consequence, moves the human-crew
  task to `done` as the operator (the human-crew bypass) and so satisfies its `blocked_by`
  dependents. An answer that changes lasting behaviour is carried by the task it unblocks into
  the owning doc, config or code. The corpus append of an answered request carries its `topic`.
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

Workspace decision listings and digests omit corrupt task bundles and return
per-task `warnings` when the results are partial. The CLI displays those warnings;
`GET /api/decisions` includes them beside `items` and `total`. A task listing that
projects `open_decisions` retains a damaged task's row with that field set to
`null` (unknown), plus a warning; `[]` still means no open decisions. Task-list
CLI warnings go to stderr, preserving its JSON record array. Explicit single-task
reads, request lookup and answering stay strict. I/O and authorization failures
still fail the operation.

A workspace scan reads a task's whole bundle only when the task may hold a
request. Requests come only from `decision_requested` history events, so once a
clean read finds none, the process remembers the task with a stamp of its
bundle: the stamps of the bundle and artifact directories plus its event and
comment logs, taken without reading them. Later scans skip the task while that
stamp holds. Any store write to the bundle (a raised, answered or reopened
request, a status move, a comment, an artifact) moves the stamp, so the next
scan reads it again. A bundle the store cannot stamp is read every time.
Corruption the store writes, such as a removed artifact blob, also moves the
stamp and still warns. An out-of-band in-place edit to a file published by
rename stays unseen until the bundle's next write. The first scan after a
process starts reads every bundle. [ORB-21835] bounds this decision-list scanning
for the Tasks waiting count.

## 3. Delegated-human authority

### 3.1 The gap, precisely

A decision counts as human today through one funnel: `update_task_as_human`
(`crates/orbit-core/src/application/task/update/entry.rs`) sets
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

The store is one file, `<global_root>/state/decision-channels/channels.json`,
written through a staged file in the same directory, and every registration in it
carries its channel's token key (§4). The directory is therefore a host secret
like the plugin secret store: every plugin child is denied reading it, whatever it
was granted (`orbit_tools` grants read access to the global root), and every
sandboxed agent launch masks it for reads and writes, created before launch so a
later registration stays hidden ([plugins](../plugins/1_scope.md) §4.3,
[policy sandbox](../policy-sandbox/2_design.md)). Core refuses a hidden store as
not visible rather than reading it as "no channel registered". Every channel
read and write, the digest and submission included, runs where the store is
visible: the operator CLI, or the host process that spawned the calling backend.
A backend reaches its `orbit.decision.digest` and `orbit.decision.submit`
callbacks through a host route, sandboxed or not: when the call's ceiling
admits either tool, the host passes the child one end of a socket pair beside its
callback credential, and the child's nested `orbit` hands the call back over it
instead of opening the store. The host answers only those two tools, plus an
application-bound session's consented callbacks
([application-approvals](../application-approvals/2_design.md) §5), for the
session it minted, through the same dispatch, ceiling, governed row and audit as
any identified callback, so the §3.3 caller and channel binding checks run
unchanged; a confined child itself still cannot read the store.

### 3.3 The submission tool and the apply step

One new governed operation, `orbit.decision.submit`, joins `GOVERNED_OPERATIONS`
(`crates/orbit-common/src/governance/authorization/catalog/operations.rs`) with a narrow
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
(`crates/orbit-common/src/governance/authorization/caller.rs`) gains no variant:
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
   plugin's word for it. The bound plugin's only copy is `context.secrets`; the
   store is denied to every plugin child and masked from agents (§3.2), so the
   mirror never reaches another plugin. Rotation: `orbit decision channel rotate-key email`
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
| `block_triage` | Block episode key (the latest transition into `blocked`; older requests also carry the `episode=` a retired `blocked_task_triaged` marker recorded) | Task left `blocked`, a new episode began, or a human touched the task after the block (the same exclusion triage candidacy uses) |
| `pilot_admission` | Pilot `operation_id` of the holding assessment | A newer assessment landed, or an operator verdict already resolved that `operation_id` |
| `proposed_approval` | Task revision + `proposed` status | Status moved, or description/criteria changed since the digest rendered |
| `review_approval` | Task revision + `review` status + candidate identity | Status moved, or a new candidate replaced the reviewed one |
| `agent_request` | Task revision + the task's status at raise | Status moved, or description/criteria changed since the raise |

Core judges the binding, state, expiry and allowed answer on a fresh read of
the task under its write lock, and applies the gate's effect and records
`decision_answered` under that same lock. An edit that lands after an answer
looked its request up, but before the answer applied, therefore refuses the
answer as `request_superseded` with nothing applied; the token it carried is
not consumed by that refusal, and presenting it again is refused the same way.
A review approval's effects on other records (resolved frictions and tasks)
run only after the lock is released.

Every refusal writes nothing to the task beyond, where noted, a history marker, and
produces a refusal reply mail (rate-limited per §8) so the human learns the answer
did not land — silence after a decision mail is the failure mode this design most
avoids. The named refusal cases:

Decision refusals expose their named kind in CLI `--json`'s `code` and MCP
`structuredContent.code`, including `request_not_found` for a missing request in
show, answer and submit. Clients match that code rather than parsing the message.
`dkim_verification_unavailable` means the same submission can be retried after the
verifier recovers; it consumes no token. Other codes require a corrected answer,
channel, credential or fresh request as described below.

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

Why not poll the existing markers (`pilot_warning_triaged`, and the retired
`blocked_task_triaged`)?

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
same write: final recovery's `decide`/`escalate` decisions (formerly `record_block_triage`); pilot
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
  any answer is applied or its token consumed. Both tool ingress and CLI
  `--raw-message` forwarding preserve the message exactly, including trailing
  spaces and CRLF significant to DKIM simple body canonicalization. DNS runs before
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
"Owner" names the crate(s) or plugin that change. Every slice below is done.

1. **Decision-request record and raising — done** ([ORB-18175]). Owner: `orbit-types`
   (`DecisionRequest`), `orbit-core` (raise from final recovery and pilot
   admission, derived state, `[decision_channel] enabled`). Depends on: nothing.
   Acceptance sketch: with the channel enabled, a final-recovery `decide` and a pilot
   hold each raise exactly one open request with the right answers and binding;
   disabled, nothing is written.
2. **Listing and local answering surfaces — done** ([ORB-18176]). Owner: `orbit-tools`
   (`orbit.decision.list/show`), `orbit-core` (`orbit.task.show` open decisions,
   the core apply step reused by §3), `orbit-web` (dashboard request list and
   answer buttons), CLI (`orbit decision …`). Depends on: 1. Acceptance sketch:
   an operator answers a raised request from CLI and dashboard; the answer lands
   through `update_task_as_human`/`approve_task` and the request reads
   `answered`; goldens updated via `make goldens`.
3. **Delegated-human channel and governed submission — done** ([ORB-18177]). Owner: `orbit-common`
   (`GOVERNED_OPERATIONS` row), `orbit-core` (registration store,
   `orbit.decision.submit`, token mint/verify, §3.3 checks, §5 refusals).
   Depends on: 1, 2. Acceptance sketch: a registered test channel applies an
   allowed answer as the human with provenance; agent-envelope and unregistered
   callers are refused and the pilot hold stays held (the §3.3 guard test).
4. **`DecisionRequested` state trigger — done** ([ORB-18178]). Owner: `orbit-types`
   (`StateTriggerKind`), `orbit-core` (observation over slice 2's listing,
   settlement on notify/answer). Depends on: 1. Acceptance sketch: a state
   routine fires once per open request per binding, settles on answer or
   supersession, and persisted member state from before the variant still loads.
5. **Email transport plugin (`decision-mail`) — done** ([ORB-18304]). Owner: new plugin (SMTP send,
   IMAP poll job + cron routine, §6 rendering/parsing, §4 layers 4–5, secrets).
   Depends on: 3, 4. Acceptance sketch: end-to-end on dk-server-2 against the
   real mailbox — raise → mail → reply `2` → applied + confirmed; each §5
   refusal case demonstrated once; plugin conformance tests for the grammar.
6. **Digest and approval gates by email — done** ([ORB-18305] core, [ORB-18307] plugin). Owner: `orbit-core` (proposed/review
   request raising, digest query), `decision-mail` plugin (digest routine,
   batched answers, quiet hours, batching mode). Depends on: 5. Acceptance
   sketch: daily digest lists proposed tasks and expiring requests; `approve
   1,3` promotes through task-pilot only; unqualified digest `approve` refused.
7. **Hardening and operations — done** ([ORB-18308] core, [ORB-18309] plugin).
   Owner: `orbit-core`/`decision-mail` (core-side DKIM verification of the raw
   message, drop/refusal metrics on the status panel, runbook for registration,
   rotation, and revocation). Depends on: 5. Acceptance sketch: a forged-header
   fixture that fools forwarded verdicts is refused by core-side verification;
   the runbook walks register → rotate → revoke against the live mailbox.

## 11. Precedent-answered decisions

[ORB-21089] designs a decision-assistance consumer that predicts a request's
answer from recorded human precedent and, only in enabled low-risk categories,
answers on its own ([precedent-decisions](../decision-assistance/specs/precedent-decisions.md)).
This section records what that consumer needs from the channel. §11.1 and
§11.2–§11.4 are implemented. The model apply step's only caller is the
self-answer step, which runs only in a category the operator enabled. With the
consumer off, or no category enabled, every surface above behaves as it does
today.

### 11.1 Request options, risk tier and category

- **Options.** A request's `answers[]` are already its closed options. The
  consumer's choice question is built from exactly those ids plus an
  `insufficient_precedent` abstention, and core re-checks a model's chosen id
  against the live request before applying it.
- **Risk tier.** Each answer gains `risk_tier`: `routine`, `reversible`,
  `irreversible`, `authority` or `spend`. The request's tier is the most severe
  answer tier. A missing or unrecognised value, and every request recorded before
  the field, reads as `unknown`. Core gates fix their tiers at raise:
  `review_approval` is `irreversible`; `proposed_approval` and `pilot_admission`
  are `authority`; final-recovery options take their tier from their action
  (`reject` is `irreversible`, `dismiss` is `routine`, the others `reversible`).
  `orbit.decision.raise` accepts an optional per-answer `risk_tier`; when it is
  omitted the answer is `unknown`. Only `routine` and `reversible` requests can
  ever be answered by the model. `irreversible`, `authority`, `spend` and
  `unknown` always go to the human.
- **Category.** Derived and stored at raise: `<workspace_id>/<gate>`, or
  `<workspace_id>/agent_request/<topic file>` (`untopiced` without a topic).
  Measurement, enablement and demotion are per category.
- `orbit.decision.list`/`show` and `open_decisions` show tier and category.

### 11.2 `answered_by: model` provenance

The consumer answers through the same apply step as curated-rule auto-answer
(binding, state, expiry and answer re-checked under the task write lock), as the
`model` actor. Like `precedent`, `model` is an automation actor: no guard that
requires a human resolution accepts it. The resolution carries a `model`
citation: assessment ids, category, policy version and digest, model identity and
revision, probability and threshold, cited precedents with their sources, the
operator enablement it ran under, and the task status to restore on veto. The
writer label is never a human label, and the channel's delegated-human authority
(§3) is not used. Decision listings show a model answer as distinct from a human
or precedent answer.

As implemented ([ORB-21092]), the apply step is an internal entry point
(`apply_model_decision_answer` in `decision/model.rs`) whose only caller is the
self-answer step of an enabled category (§11.4). It
refuses, writing nothing, a request that is not open, an answer it does not
allow, a tier other than `routine` or `reversible`, a `model_answer_review`
request, a citation whose category differs from the request's, and a probability
outside [0, 1]. It applies `agent_request` requests only: their effect (the
human-crew task completes) is exactly what a veto restores. Other gates are
refused `answer_not_applicable` until a later slice applies their effects as the
`model` actor. The answer's status move carries the precedent bookkeeping note,
so restoring the status re-opens the request instead of superseding it.

### 11.3 Veto digest and path

A model answer raises, in the same write, a `model_answer_review` request on the
same task, with answers `confirm` (`routine`) and `veto` (`reversible`), bound to
the original request id and its resolution time. The model never answers this
gate. It rides the existing machinery: the `DecisionRequested` trigger, tokens,
the stale guard, and the digest. `orbit.decision.digest` returns model answers
awaiting review in a `model_answered` section, and the daily mail lists them
for `veto <n>` / `confirm <n>` replies under the §6 grammar. The dashboard and
`orbit decision answer` answer it like any request.

`veto` reuses the reversal step precedent answers already have
(`orbit decision reverse`), generalised to model answers: it re-opens the original
request, restores the recorded task status, comments the veto, and notifies the
human. It does not undo downstream work that already started, which is why only
`routine` and `reversible` requests qualify. `confirm` records the human's
confirmation; only a confirmed model answer becomes a precedent or a corpus
entry. An expired review leaves the answer standing and unconfirmed.

As implemented: the review binds as `{kind: model_answer, request_id,
answered_at}` and stays current while that model answer stands, so a veto, an
`orbit decision reverse` or a later answer supersedes it. It neither supersedes
nor is superseded by other requests on the task, so it can be open beside them.
`model_answered` lists every open review with the request it reviews (each pair
until a human answers or the review expires), and the review is left out of
`expiring`. `confirm` writes a `decision_model_confirmed` event (the human, the
request and the model's `answered_at`), which the built-in precedent provider
reads as `human_confirmed`, and reads back as the resolution's derived
`confirmation`; it then appends the corpus entry with the confirming human as the
answerer. A model answer and the review answer append none.

### 11.4 Consulting the consumer before notifying

Agents raise requests as today. After raise (and after curated-rule auto-answer
leaves the request open), the channel enqueues the consumer's assessment. In
shadow mode it notifies the human immediately and never shows the prediction. In
an enabled category it waits up to the consumer's `assist_wait_seconds` before
notifying, and notifies as usual unless the model answered. No agent-side call,
answer authority or channel registration changes. Shadow enqueue is implemented
and default-off: the raise path records the request before it builds a packet,
does not wait for a model, and does not show the prediction.

In an enabled category ([ORB-21097]), the `DecisionRequested` trigger withholds
the notification (`precedent_decision_wait`) while a `routine` or `reversible`,
non-holdback request is inside `assist_wait_seconds` and not yet routed. The
owner tick then applies a qualifying answer through the §11.2 apply step or
routes the request to the human. A demoted category is listed in the digest's
`assist_demoted`. The consumer spec's "As implemented" section has the rules.

### 11.5 Slices

Filed as `ws_orbit` tasks; the consumer spec's Phasing lists all seven.
The channel-side ones are:

1. **Risk tier and category on requests** ([ORB-21091]) — §11.1.
2. **Model-answer provenance, review gate and veto** ([ORB-21092]) — §11.2–§11.3.
   Implemented; the self-answer step is the apply step's caller.
3. **Enablement and self-answer, including the notification wait**
   ([ORB-21097]) — §11.4. Implemented, default-off.

## 12. Plugin-raised decision requests with an apply callback

[ORB-22824] designs this section; nothing in it is implemented. It lets a plugin put a
closed question on a task through this channel and own what happens when a human
answers. The first consumer is orbit-office: a Library proposal (`knowledge_add`,
`knowledge_update`, `runbook_from_friction`, `retire`, `auto_task`) that a human accepts
from the dashboard, the CLI or an email reply. The daily "dreaming" auto-task that
proposes consolidated learnings is the first producer. Its prompt, schedule and inputs
are designed elsewhere. Code anchors were read at `a16a8e0d9` (2026-11-07).

### 12.1 What is missing today

Every gate's effect is a hard-coded arm of the one apply step
(`OrbitRuntime::apply_decision_answer`, `application/task/decision/answering.rs`): an
option rewrite, a pilot verdict, `approve_task_guarded_deferring_effects`, a resident
grant, a script grant. `DecisionGate` (`orbit-types/src/task/decision/request/gates.rs`)
is a closed, persisted enum, so each new effect has so far meant a new core gate
(`resident_authority`, `script_grant`). That does not scale to effects core must not
understand. An office acceptance writes a Library page or creates an auto-task from a
payload the human reviewed (`office.proposal-accept {id, sha256}`, in orbit-office's
proposals spec). Core cannot apply it, and should not learn Library semantics.

There is a second, sharper gap. An office proposal is an Orbit task in `proposed`. With
the channel enabled, `orbit.decision.digest` raises a `proposed_approval` request for
every `proposed` task that has no open one
(`raise_approval_decision_request`, `application/task/decision_channel/digest.rs`).
Answering `approve` runs the generic approval toward `backlog`, which is the wrong
effect for a proposal: office acceptance goes `proposed → in-progress` through its own
tool and never through dispatch. So today the channel can offer a proposal for approval
and apply something other than what the human meant.

### 12.2 Shape

One new generic gate, `plugin_request`, and one new binding kind. The plugin raises; core
records, routes and guards; a human answers; core runs the plugin's own apply tool with
the answer; the plugin applies or refuses. Core never reads the payload and knows
nothing about what the answer does.

| Step | Owner | What happens |
|---|---|---|
| Raise | Plugin, through a callback | `orbit.decision.plugin_raise` puts a request on one task with plugin-declared answers, an apply tool and an opaque revision (§12.3) |
| Route | Core | Listing, `open_decisions`, dashboard, digest, `DecisionRequested` trigger and email, unchanged (§2.4, §7) |
| Answer | Human | Dashboard, `orbit decision answer`, or a channel submission (§3.3). Never precedent or model (§12.6) |
| Apply | Core calls the plugin | The registered apply tool runs once with the pinned input and the answer; the plugin re-checks its revision (§12.4) |
| Record | Core | The human's answer and the apply outcome, both on the task history (§12.4) |

### 12.3 The raise contract

`orbit.decision.plugin_raise` is a new governed write with the same row shape as
`orbit.decision.submit` and `orbit.decision.digest` (`catalog/operations.rs`): operator
only for every capability-resolved caller, plus an *identified* plugin callback session
admitted past the row and bound by the handler. An agent session is refused
`caller_not_allowed`. An agent never raises one directly. It calls the plugin's own
agent-callable tool (office: `office.proposal-create`), whose backend calls back. The
plugin is the raiser of record; the agent is only a requester.

Input:

```json
{
  "task_id": "OFF-123",
  "summary": "Accept the proposed update to runbook rb-release-checklist?",
  "answers": [
    {"id": "accept",  "label": "Accept",  "consequence": "Writes the reviewed diff to the Library."},
    {"id": "dismiss", "label": "Dismiss", "consequence": "Closes the proposal; it is suppressed for 30 days."}
  ],
  "recommended": "accept",
  "apply": {
    "tool": "office.proposal-accept",
    "input": {"id": "OFF-123", "sha256": "9f2c…"},
    "revision": "sha256:9f2c…"
  }
}
```

The handler checks, in order, and writes nothing on any refusal:

1. **Caller.** The callback session's plugin (`PluginCallbackIdentity::provenance.name`,
   `crates/orbit-tools/src/plugin/callback/session.rs`) owns the namespace of `apply.tool`. A plugin can name
   only its own tools.
2. **Apply tool.** `apply.tool` is in the plugin's installed, enabled manifest. It is
   `execution_kind: mutating` with `mcp_scope: none`, so no agent can call the effect
   directly and bypass the decision. It is also on the host's apply-tool allowlist
   (§12.5). The manifest digest is recorded on the request.
3. **Answers.** Two to four, ids matching `[a-z][a-z0-9_]{0,31}`, one-line label and
   consequence, the same bounds as `orbit.decision.raise`. `recommended` names one of
   them or is absent. A `risk_tier` the plugin supplies is ignored (§12.6).
4. **Input and revision.** `apply.input` is a JSON object of at most 8 KiB with no
   top-level `decision` key, which core reserves (§12.4). `apply.revision` is 1-128
   printable characters, opaque to core.
5. **Task.** The task is in the session's workspace, open (not `done`, `rejected` or
   `archived`), and owned by this host: the raise is a coordination write and
   `ensure_coordination_task_write_permitted` refuses a replica checkout or a claimed
   worker. The plugin must also be enabled for that workspace on this host (§12.7).
6. **Bounds.** A task holds one open `plugin_request`. A re-raise with the same
   `(plugin, tool, revision)` returns the open request (`deduplicated: true`). A re-raise
   with a new revision supersedes the old request, as a new script-grant request
   supersedes the old one on its holder task. Open plugin requests are capped per
   workspace (50) and per plugin (20). Past either cap the raise is refused and nothing
   is written.

The record is a `DecisionRequest` with `gate: plugin_request`, `raised_by: plugin:<ns>`,
the `summary`, `recommended` and `answers`, and one new optional field:

```rust
pub struct PluginApply {
    pub plugin: String,            // namespace
    pub tool: String,              // <ns>.<verb>
    pub manifest_digest: String,   // at raise
    pub input: serde_json::Value,  // pinned, ≤ 8 KiB
    pub revision: String,          // opaque; the plugin's stale guard
    pub requested_by: Option<String>, // writer label of the caller that spawned the backend
}
```

The binding is a new `DecisionBinding::PluginObject { plugin, revision, manifest_digest,
status }`. The status is the task's status at raise. The request reads `superseded`
when the task's status moves, when a newer `plugin_request` on the task carries another
revision, or when the installed manifest digest differs from the recorded one, because an
upgraded plugin may give the same tool a different meaning. `requested_by` needs the
host to stamp the spawning caller's writer label into the callback session record when
it mints the session (slice 1). It is provenance only and grants nothing.

While a task holds an open `plugin_request`, `orbit.decision.digest` does not raise a
`proposed_approval` or `review_approval` request on it, and an `--approve-proposed`
drain holds it as `decision_pending` instead of approving it. That closes the §12.1
mis-apply: one task, one question, and the plugin's answers are the only ones offered.

### 12.4 The apply step

Answering a `plugin_request` uses the existing entry points and their checks unchanged.
On the dashboard and `orbit decision answer`, `answer_decision_request_as_human` refuses
automation and `precedent` writers. On a channel submission, the §3.3 checks and §4
token run first. The gate's own step then runs in two phases. An out-of-process plugin
call whose callbacks write the same task cannot run under that task's write lock.

**Phase 1, under the task write lock.** Core takes a fresh read and checks state,
binding, expiry and the allowed answer (§5). It checks that the plugin is installed and
enabled on this host and in the workspace, and that the apply tool is still on the
allowlist with the digest the request recorded (§12.5). Then it appends
`decision_answered` with the human as writer and the channel provenance, as every gate
does, plus a new `apply` block `{tool, input_sha256, revision, state: "pending"}`. The
human's decision is now final and the token is consumed. A second answer is refused
`request_superseded` ("already answered by …"), which is the existing answered-state
refusal (`ensure_request_answerable`).

**Phase 2, lock released.** Core calls `apply.tool` once through the ordinary audited
tool dispatch, the same path a deterministic `plugin.tool_call` step takes
(`plugin_tool_call`, `adapter/engine_host/actions/dispatch.rs`). The input is the pinned
`apply.input` with one reserved key added:

```json
{"id": "OFF-123", "sha256": "9f2c…",
 "decision": {"request_id": "dr-…", "answer": "accept", "revision": "sha256:9f2c…",
              "answered_by": "daniel", "via": "email", "note": "…"}}
```

The plugin re-checks `decision.revision` against its live object and refuses with its
own code when they differ (office already refuses a changed payload digest, and
`base_sha256` for a changed page). Core then appends `decision_applied {request_id,
outcome, code, summary, finished_at}`, where `outcome` is `applied` when the tool
returned success and `failed` otherwise, with the plugin's error code kept verbatim.
Core does not interpret plugin codes. It stays generic, and the plugin's code says
whether anything changed (the office contract: a refusal before its reservation writes
nothing).

**Authority.** The human's authority is recorded once, on `decision_answered`, by core.
The apply call runs with the authority the operator has when running the same tool from
the CLI. The callback session the host mints for it carries the operator's ceiling
∩ `permissions.orbit_tools` ∩ the plugin's grant ([plugins](../plugins/1_scope.md)
§4), exactly as for `ORBIT_OPERATOR=1 orbit tool run
office.proposal-accept`, which is office's documented operator path today. The plugin's
callback writes keep their plugin provenance and are never written under the human's
label ([4_decisions.md](./4_decisions.md#a-plugins-effect-is-never-written-under-the-humans-label)).
The session record and every audit row of the call and its callbacks carry a new
`decision` provenance field `{request_id, answered_by, via}`, so the audit trail reads:
human answered through the channel, then plugin applied under that answer.

**Derived apply state** (on read, like §2.2): `pending` (no `decision_applied` yet),
`applied`, `failed`, and `unknown` when the `pending` entry is older than the backend's
`timeout_ms` plus one minute, or the process that wrote it is gone. Nothing retries a
`failed` or `unknown` apply automatically, matching office's "no callback is retried on
an unknown outcome". The operator inspects it, and the plugin may raise a fresh request.
`orbit.decision.show`, `open_decisions` and the dashboard show the apply state beside
the answer. The submit response returns it, so the transport's confirmation or refusal
mail states what actually happened, not only that the answer was recorded.

### 12.5 Operator consent: the apply-tool allowlist

A human answer is consent to one effect. It is not consent for a plugin to run tools at
operator ceiling whenever its own request is answered. That consent is the operator's,
given once per tool from the interactive CLI and refused from agent envelopes and plugin
callbacks, like channel registration and `assist enable`:

```
orbit decision apply-tool allow office.proposal-accept
orbit decision apply-tool revoke office.proposal-accept
orbit decision apply-tool list
```

Each entry records the tool, the manifest digest at allow time and the operator, in
`<global_root>/state/decision-channels/apply-tools.json`. The file sits beside the
channel store and is masked the same way. A plugin upgrade that changes the digest
suspends the entry until it is allowed again, mirroring how a grant change suspends a
precedent enablement. Revoking an entry is immediate: open requests naming that tool
stay open, and answering them is refused `plugin_unavailable` until it is re-allowed or
the requests expire. This is a host-local store, not a manifest field, so the plugin
protocol (`host_api`) does not change.

### 12.6 Auto-answer policy: never by precedent or model

A `plugin_request` is answered only by a human. This is structural, not a default the
operator can switch on in v1:

- `DecisionRequest::new` fixes every answer of a `plugin_request` at `authority` tier,
  whatever the plugin declared, as it does for `resident_authority` and `script_grant`.
  The model applies only `routine` and `reversible` requests
  (`apply_model_decision_answer`, `decision/model.rs`), and its self-answer step routes
  any other tier `excluded_tier`.
- Curated-rule auto-answer runs only for `agent_request` (`decision/precedent.rs`).
  `plugin_request` joins the gates the corpus never appends
  (`decision_corpus_append`, `application/agent_corpus.rs`), so no answered plugin
  request becomes precedent that automation could answer from. The plugin keeps its own
  memory (office's 30-day dismissal suppression).
- `answer_decision_request_as_human` already refuses `model`, `precedent` and every
  automation label. A channel submission writes as the registered human only after
  §3.3 and §4.

**Why.** For the first consumer the request *is* agent memory. The dreaming run, the
office orchestrator or a knowledge-maintenance auto-task files a proposal, and the
answer decides what every later agent reads as knowledge or runs as automation. An
agent that approved such a request would curate its own memory, which [ORB-10736]
retired native learnings to prevent. It would also break office's invariant that "the
orchestrator cannot approve its own proposals". That invariant rests on
`office.proposal-accept` being `mcp_scope: none` and not agent-callable
(orbit-office's office design, §3 Authority boundaries). This design keeps it: the apply
tool must be `mcp_scope: none` (§12.3 step 2), only core calls it, and core calls it
only after a human answer. Measured precedent accuracy does not help here, because
the model predicting what Daniel would accept is the same self-approval loop with one
more step. A future plugin whose requests are not memory and want a lower tier needs
its own design and its own operator enablement. A plugin's declared tier is a claim
by the raiser, the weakness the precedent spec already names for agent-declared tiers.

### 12.7 Cross-host apply: the request lives where the effect lives

Decision requests are task history, so they live on the host that owns the task's
workspace. Answering is a coordination write. In a replica checkout,
`coordination_task_write_refusal` (`orbit-runtime/src/orbit_runtime/identity.rs`)
refuses it, and a claimed worker is refused the same way. The apply tool must run where
the plugin and its data are. The office Library is Mac-only (`office-constellation`).
dk-server-2, the channel host and owner of `ws_orbit`, does not install `office`: its
`ws_orbit` config names an `[plugin_enablement] office` toggle and dk-server-2 warns that
the plugin "is not installed on this host" (observed 2026-11-07).

**Rule: a plugin request is raised only on a task its plugin can apply on the same
host, and core never forwards an apply to another host**
([4_decisions.md](./4_decisions.md#a-decision-effect-runs-on-the-host-that-owns-its-request)).
Enforcement:

- *Raise* (§12.3 step 5): the task is owned by this host, and the plugin is installed
  and enabled here for that workspace. Otherwise the raise is refused `host_not_owner`
  or `plugin_unavailable` and nothing is written.
- *Answer* (§12.4 phase 1): an answer arriving through a runtime that does not own the
  task is refused `host_not_owner`. That is today's `CapabilityRefused`, given the
  named decision code so clients can match it. A plugin that has since become
  unavailable on the owner host refuses `plugin_unavailable`. Neither writes anything,
  and the request stays open.

For office this is not a restriction in practice. A proposal is a task in the office
workspace, owned by the Mac, the same host as the Library. So the request, the answer
and `office.proposal-accept` are all local to the Mac. Two consequences are bounded
rather than solved:

- **Email for Mac-owned requests.** The registered email channel and `decision-mail`
  live on dk-server-2 (§1 decision 4), and a channel registration is host config. A
  request on the Mac is answerable from the Mac's dashboard and `orbit decision answer`.
  It reaches email only if the Mac registers its own channel with its own
  `decision-mail` install. Relaying submissions between hosts is out of scope, because
  it would move the delegated-human authority across a host boundary that §3.2 keeps
  local.
- **The dreaming run on another host.** A dream run in `ws_orbit` on dk-server-2 cannot
  file a Library proposal there, because `office` is absent. The dreaming design must
  run its Library-facing half in the office workspace on the Mac, where
  `office.proposal-create` raises the request locally. Its skill and runbook half stays
  in `ws_orbit` (§12.8). A cross-host hand-off of dream candidates, such as a Mac job
  that imports a dk-server-2 artifact, is possible later. It changes nothing here,
  because the request would still be raised on the Mac.

### 12.8 Dream PRs need only `review_approval`

Dream changes to skills (`crates/orbit-core/assets/skills/`) and runbooks
(`docs/runbooks/`) are code-reviewed repository changes. A dream-minted task opens a
PR and reaches `review`. It then needs a human approval through the existing
`review_approval` gate (digest raise, dashboard or CLI approve). No new kind is needed,
and a `plugin_request` would be wrong, because no plugin owns a repository change:

- `review_approval` is fixed `irreversible` tier (`DecisionRequest::new`), so the model
  never answers it. Curated rules answer only `agent_request`. The human is therefore
  the approver of agent-proposed skill text by construction, which is the same
  self-approval guarantee §12.6 gives the Library.
- Landing is not authorized by a task approval or a PR request (the Orbit agent guide's
  rule). Completion authority is granted per invocation by an operator, never by
  workspace configuration (`COMPLETION_IS_NEVER_WORKSPACE_RESOLVED`,
  `adapter/engine_host/actions/dispatch.rs`).

The one gap belongs to the dreaming design, not to the channel. A drain started with
`--approve-proposed` approves qualifying proposed tasks into `backlog`, which admits
execution, not landing. If Daniel wants to see a dream's proposed skill edits before
any run, the dream auto-task's template must tag minted tasks `no-auto-approve`, which
drain promotion already honours (`task/pilot/promotion.rs`).

### 12.9 Refusals

All are `DecisionRefusalCode`s (`orbit-common/src/error/kinds.rs`) exposed as `code` on
the CLI and MCP (§5). The new codes are `plugin_unavailable` and `host_not_owner`;
`apply_failed` is an outcome, not a refusal.

| Case | When | Code / outcome | Written | Request after |
|---|---|---|---|---|
| Stale revision | The plugin's object changed after raise | Plugin's own code, outcome `failed` | `decision_answered` + `decision_applied` | Answered, apply `failed`; the plugin re-raises on the new revision |
| Stale task | Task status moved, a newer revision was raised, or the manifest digest changed | `request_superseded` | Nothing (token rules as §5) | Superseded |
| Plugin disabled | Plugin removed, disabled on the host or in the workspace, or apply tool revoked or suspended (§12.5) | `plugin_unavailable` | Nothing; token not consumed | Open until expiry |
| Apply failure | The tool errors, times out, or its outcome is unknown | outcome `failed` / `unknown` | `decision_answered` + `decision_applied` (or none for `unknown`) | Answered; never retried; operator inspects |
| Host mismatch | Raise or answer on a runtime that does not own the task, or a claimed worker | `host_not_owner` | Nothing | Unchanged |
| Not a plugin's tool | `apply.tool` outside the caller's namespace, agent-callable, read-only, or not allowlisted | `caller_not_allowed` / `answer_not_applicable` at raise | Nothing | Not raised |
| Agent or automation answer | Any non-human answerer, including `model` and `precedent` | `caller_not_allowed` | Nothing | Open |

### 12.10 Contract office would adopt

This section names the contract only. orbit-office designs its own changes.

- Mark `office.proposal-accept` (and a dismiss tool, if office wants dismissal by
  decision) as the apply tool. It already is `mutating` and `mcp_scope: none`.
- Accept the reserved `decision` input key. Treat `decision.answer` as the choice
  (`accept`, `dismiss`; `theirs`/`mine` for `shared_conflict` map onto answers), and
  check `decision.revision`, which office sets to its `payload_sha256`, against the
  stored `proposal.json` before reservation.
- From `office.proposal-create`, after creating the task and artifact, call
  `orbit.decision.plugin_raise` with the proposal's answers, `apply.input = {id,
  sha256}` and `revision = sha256:<payload_sha256>`. Add `orbit.decision.plugin_raise`
  to the manifest's `permissions.orbit_tools`.
- Keep the app's Today accept on its own application-consent path. Both paths call the
  same tool, and whichever lands first wins, because the other's revision or status check
  refuses. The application-approvals rule "a relayed click is no stronger than its
  relayer" ([application-approvals 4_decisions.md](../application-approvals/4_decisions.md#a-relayed-click-is-no-stronger-than-its-relayer))
  stays intact. A decision request adds a surface the app cannot reach (email,
  the operator dashboard). It adds no ceremony to the app's own click.
- Proposal kinds, diffs, evidence pages, dismissal memory and reconcile stay office's.
  Core sees an opaque input, a revision and two answer ids.

### 12.11 Implementation slices

Ordered. Each is shippable alone and inert until a plugin raises a request.

1. **Record, binding and raise.** `orbit-types`: `DecisionGate::PluginRequest`,
   `DecisionBinding::PluginObject`, `PluginApply`, the `authority` tier arm.
   `orbit-common`: the governed row and the `plugin_request` spec. `orbit-tools`: the
   spawning caller's label in the callback session record. `orbit-core`: the
   `orbit.decision.plugin_raise` handler (§12.3 checks, bounds, supersession), the
   digest and drain exclusion, and listing and show of the new fields. Acceptance: a
   fixture plugin raises one request; an agent session, another plugin's namespace and
   an agent-callable tool are refused; a re-raise dedupes or supersedes by revision.
2. **Apply-tool allowlist.** `orbit-core` and the CLI: the host-only store and
   `orbit decision apply-tool allow|revoke|list`, with the sandbox mask beside the
   channel store and digest suspension. Depends on: 1.
3. **Two-phase apply.** `orbit-core`: the `plugin_request` arm of
   `apply_decision_answer`, `decision_applied`, the derived apply state, the operator
   ceiling session with `decision` provenance, the corpus exclusion, and the new
   refusal codes. Depends on: 1, 2. Acceptance: dashboard and CLI answers run the tool
   once with the reserved key; a plugin refusal and a timeout record `failed` and
   `unknown`; a revoked tool refuses `plugin_unavailable` and writes nothing; the model
   apply step refuses the gate.
4. **Channel and mail.** `orbit.decision.submit` returns the apply outcome, and
   `decision-mail` renders plugin requests (summary, answers, no payload) and reports
   the outcome in confirmation and refusal mail. Depends on: 3.
5. **Office adoption** (orbit-office, §12.10), then the dreaming auto-task's Library half
   in the office workspace. These are filed in their own repositories and designs.

### 12.12 Compatibility

- **Persisted enums.** `DecisionGate::PluginRequest` and
  `DecisionBinding::PluginObject` are new variants of closed, persisted serde enums, so
  they follow retain-don't-remove from their first release, like `ExecutionFailed` in
  `StateTriggerKind`. A binary older than slice 1 that reads a `plugin_request` entry
  fails to parse it, and `DecisionRequest::from_entry` returns `None`. The request is
  then invisible to that binary and cannot be answered there. That fails closed, but
  it is silent, so the release notes for slice 1 must say that mixed-version hosts do
  not list plugin requests.
- **History events.** `decision_applied` and the `apply` block on `decision_answered`
  are additive. Old readers ignore an unknown event, and `DecisionResolution` gains an
  optional field (`#[serde(default)]`).
- **`DecisionRequestState`** gains no variant. The apply state is a separate derived
  field, so existing consumers of `state` keep their four values.
- **Refusal codes.** `plugin_unavailable` and `host_not_owner` are new codes on an
  existing contract. A client that matches codes treats an unknown one as a refusal.
- **Surfaces.** New tool, new CLI verbs and new optional output fields only; existing
  shapes stay byte-stable, within the 30-day window
  ([state-compatibility](../state-compatibility/4_decisions.md#compatibility-paths-expire-on-a-30-day-release-gated-window)).
  Plugin protocol (`host_api`): unchanged. The apply input's reserved key is a
  per-tool contract the plugin opts into by raising.
- **Stores.** `apply-tools.json` is new, host-local and created on first allow. Its
  absence means no tool is allowed, so every plugin request refuses
  `plugin_unavailable` until an operator allows one.

### 12.13 Concerns & honest limitations

- **Core trusts the plugin's report of what it did.** `applied` means the tool returned
  success. A buggy or lying plugin could claim success without effect, or act beyond
  its answer within its grant. The containment is the same as §9's: an
  operator-installed, grant-bound plugin, an operator-allowed tool, pinned input, and
  a full audit trail.
- **Operator ceiling is broad.** The apply runs at the authority an operator CLI run
  has, bounded only by the plugin's grant. A narrower per-tool constraint profile,
  reusing application-approvals' constrained scopes, would bound callbacks to the
  request's own task. It is deferred because office's accept also creates auto-tasks
  and moves its task through `review`, which needs most of that profile. Revisit if a
  second plugin adopts this gate.
- **The answer is final before the effect.** Recording the human's answer first is what
  makes a crashed apply visible (`unknown`) and not silently re-runnable. It also means
  an answered request can carry a failed effect. Surfaces must show the apply state,
  and an email confirmation must not say "done" for `failed`.
- **Email reach is per host.** Plugin requests on the Mac are not emailed until the Mac
  has its own channel (§12.7). That is a real gap for Library proposals, accepted to
  keep delegated-human authority host-local.
- **The plugin writes the question.** The summary and consequences the human reads are
  the plugin's text. A plugin can mislead. The operator's allowlist is the consent that
  bounds which plugins may ask at all.

## Task References

- [ORB-22824] — designs plugin-raised decision requests with an apply callback (§12).
- [ORB-10736] — retired native learnings, so agents do not curate their own memory.
- [ORB-21835] — bounds decision-list scanning for the Tasks waiting count.
- [ORB-21097] — implements category enablement, self-answer and the notification wait for precedent decisions.
- [ORB-21092] — proposes model-answer provenance, the `model_answer_review` gate and veto.
- [ORB-21091] — proposes per-answer risk tier and a stored category on decision requests.
- [ORB-21089] — designs precedent-answered decisions and the channel changes in §11.
- [ORB-20736] — adds agent-raised multiple-choice questions on human-crew tasks.
- [ORB-20629] — makes final recovery the one decider for a blocked task, with a self-contained `decide` request.
- [ORB-18309] — shows decision-mail drops and refusals and writes the channel runbook.
- [ORB-18308] — verifies DKIM in core for email submissions.
- [ORB-18307] — sends the daily digest and accepts batched email answers.
- [ORB-18305] — raises proposed and review approval requests for the digest.
- [ORB-18304] — mails decision requests and applies authenticated email replies.
- [ORB-18178] — adds the `DecisionRequested` state trigger.
- [ORB-18177] — registers delegated-human channels and governs `orbit.decision.submit`.
- [ORB-18176] — lists and answers decision requests from CLI, MCP and the dashboard.
- [ORB-18175] — records decision requests and raises them from holds.
- [ORB-17979] — designed the email channel for human decisions on held Orbit work (this folder).

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
