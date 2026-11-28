# Tools: decision requests

Contents:

- [Decision requests](#decision-requests)
- [Raising a decision](#raising-a-decision)
- [Plugin-raised requests](#plugin-raised-requests)

## Decision requests

`orbit_decision_submit` (`orbit.decision.submit`) and `orbit_decision_digest`
(`orbit.decision.digest`) are governed. Over MCP they are in the `decision-channel`
toolset. Both are callable only by the plugin the
channel is bound to, through its callback session, or by the operator; an agent
session is refused `caller_not_allowed`.

**Submit.** Core verifies that the channel is registered and unrevoked, that the
request is still open against its binding, that the answer is allowed, and the
per-request token: single-use, expiring with the request, and keyed to the
channel's current key epoch.

A submission with `raw_message` also gets core's own
DKIM check: core reads the selector key from DNS, verifies the body hash and the
header signature, checks signing-domain alignment with the sender, and refuses
when its result contradicts the forwarded verdicts.

A channel registered with
`require_dkim_verification` refuses a submission without a raw message
(`dkim_verification_required`). Only then is the answer applied as the registered
human, with the channel and the token fingerprint recorded as provenance.

A refusal applies nothing. The refusal codes are `channel_not_registered`,
`channel_revoked`, `caller_not_allowed`, `sender_not_allowed`,
`authentication_failed`, `dkim_verification_failed`,
`dkim_verification_unavailable`, `dkim_verification_required`,
`raw_message_too_large`, `token_invalid`, `token_consumed`, `request_expired`,
`request_superseded`, `answer_not_allowed`, `answer_not_applicable`,
`request_not_found` and `decision_channel_disabled`.

A parse-level
refusal does not consume the token. `dkim_verification_unavailable` is the only
retryable refusal: retry the same submission unchanged; it consumes nothing.

**Digest.** `orbit_decision_digest` takes one `channel`. For every proposed task
without an open `proposed_approval` request, and every review task without an
open `review_approval` request, it raises one open request per (task, gate,
binding), with answers `approve` and `reject` and `raised_by` `digest`. A plugin
the channel is not bound to is refused `channel_not_registered`; an unregistered
or revoked channel is refused with `channel_not_registered` or `channel_revoked`.

- `proposed_approval` binds to the task's description and acceptance criteria
  plus its proposed status.
- `review_approval` also binds to the candidate under review.
- Either request reads `superseded` once its binding stops matching.

The result carries `requests` (the digest), `raised` (what this call raised)
and `expiring` (every still-open request expiring within 24 hours). Repeated calls
reuse requests for unchanged task bindings, while newly eligible tasks can add
requests. A task with an open plugin request is skipped for approval requests.
With the decision channel disabled in this workspace, the call raises
nothing, writes nothing and returns no requests.

`orbit.decision` action `list` (`orbit.decision.list`) lists open human decision
requests on held work (or all with `include_closed`), filtered by `gate`,
`task_id` or exact `topic`; each request names its task, gate, allowed answers,
expiry and derived effective state (`open`, `answered`, `superseded` or
`expired`). Seeing a request never answers it.

Channel administration (`orbit decision channel register|list|revoke|rotate-key`)
is operator CLI only (refused from agent envelopes and plugin callbacks):
`register` generates the channel key, mirrors it to the bound plugin's
`channel_key` secret; a new channel name starts at key epoch 1. An active name
cannot be registered again; revoke it first (or use `rotate-key` to replace its
key). Re-registering a revoked name uses the previous epoch + 1. Registration
also accepts optional `require_dkim_verification`. `revoke` immediately stops
verifying outstanding tokens (`channel_revoked`); `rotate-key` generates and
mirrors a fresh key and bumps the key epoch, invalidating tokens minted under
prior epochs.

## Raising a decision

`orbit_decision` action `raise` (`orbit.decision` action `raise`) is the one decision tool an agent
session writes with. It raises a multiple-choice request on a task assigned to a
human crew that is not `done`, `archived` or `rejected`. Inputs:

- `task_id` and `question`: the decision needed, in one or two plain sentences.
- `options`: two to four `{label, consequence, recommended}` objects. `label` and
  `consequence` are one line each, and exactly one option has
  `recommended: true`; it is listed first as answer `1`, the rest follow as `2`
  to `4`.
  - Each option may declare `risk_tier` as `routine`, `reversible`,
    `irreversible`, `authority` or `spend`. An omitted tier is `unknown`;
    unrecognised values are refused. The request records its most severe tier
    and a category derived at raise. These fields do not answer the request.
- `topic` (optional): a structured token of 1-64 characters (letters, digits and
  `. _ : / -`), or a corpus rule id. `orbit_decision` action `list` filters by exact
  `topic`.

The filer the per-filer cap counts is the session, not an input `model`.

The request has gate `agent_request` and `raised_by` `agent_request`. It is
bounded: the same open question (same text ignoring case and spacing, same
topic) returns the existing request with `deduplicated: true` and writes nothing;
a task holds one open request; and open agent-raised requests are capped at 20
per workspace and 5 per filer. Past a cap, or on a task that is not a human
crew's, the call is refused with the reason and nothing is written; answering a
request frees its slot.

Answering stays the operator's (`orbit decision answer`, the dashboard or a
registered channel), except when the request's `topic` names a `file/rule`, or a
rule id that appears in exactly one decision file, and that rule is a current
corpus rule the operator marked `auto` with an option you offered. That match
answers immediately.

The history actor is `precedent`, the record names the rule id and the rule's citations, and it
states that the operator approved the rule. A comment tells the operator how
to reverse it with `orbit decision reverse`. Reversal re-opens the request and
restores the task status. An agent label cannot answer, cannot reverse, and
cannot set `auto` (`orbit decision rule-auto` is operator-only).

A non-auto,
stale, superseded, or ambiguous rule does not answer; neither does a topic
that is missing, only similar, or just a decision file's name. The answer is
recorded in the task's history (who, when, which option, with the option's consequence in a comment), the
task moves to `done`, and `blocked_by` dependents become eligible. Raw
decision history is not authority.

Only a merged `current` rule whose review
date has not passed and whose `depends_on` digests still match may be cited.
A weekly consolidation proposes rule edits as a pull request and never edits
a rule outside that pull request; a human merge is what makes a rule current.
An answer that changes lasting behaviour or policy is carried by the task it
unblocks into the doc, config or code that owns it, in that task's PR.

## Plugin-raised requests

An installed and enabled plugin can call `orbit.decision.plugin_raise` from an
authenticated callback session with `permissions.orbit_tools` consent for that
tool. Granting the callback does not expose it over MCP. The apply tool must
belong to the plugin and declare `execution_kind: mutating`,
`agent_callable: false` and `mcp_scope: none`.

Send `subject`, `summary`, `answers` and `apply`. Each answer has `id`, `label`
and `consequence`. Supply two to four unique ids matching
`[a-z][a-z0-9_]{0,31}`, with a one-line label and consequence of up to 240
characters. Optional `recommended` names an answer id. The gate and all
answers carry the `authority` tier.

`apply` carries `tool`, `revision` and `input`. The revision and subject are
opaque strings of 1–128 printable characters. The input is a JSON object of
at most 8 KiB with the top-level key `decision` reserved. Keep credentials
out of the input because the decision record includes it.

Supply `task_id` for an open task in the callback's workspace or omit it for
the plugin's `someday` holder task. Coordination writes require the owner host.
A repeat raise for the same task, plugin, subject and revision reuses the open
request. A new revision supersedes it. A changed installed manifest digest or
a closed task also supersedes the request. The workspace holds up to 50 open
plugin requests, with up to 20 per plugin. To withdraw, send `subject` and
`withdraw: true`, with the same optional `task_id`.

Read the request and its `plugin_apply` metadata with `orbit.decision` action
`list` or `show`. `orbit attention` folds a task-form request onto an
existing task item when one is present. A standalone item starts at low
severity. An approve-proposed drain holds the task as `decision_pending`
while its plugin request is open.

This release records and displays plugin decisions. Executing an answer
awaits support for apply-tool consent and execution. Older Orbit binaries
do not list plugin requests in mixed-version fleets.
