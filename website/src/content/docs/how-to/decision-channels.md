---
title: Decision channels
description: "Register a human decision channel, request approval digests, and connect a plugin that returns authenticated answers."
---

Use a decision channel when you want a transport plugin to present held work to
a human and return that human's answer to Orbit. Orbit checks the caller,
sender, authentication evidence, request binding and token before applying the
answer as the channel's registered human.

You need a plugin that implements this exchange. Registering a channel does
not install a mail plugin, send a digest or start polling for replies.
Agents can inspect requests; they cannot answer for a human.

## Enable requests in a workspace

Run from the registered workspace:

```bash
orbit config set decision_channel.enabled true
```

The setting defaults to `false`. Enabling it allows blocked-task final recovery and
task-pilot admission holds to raise decision requests. The digest below also
raises approval requests for proposed and review tasks. Each request names its
allowed answers, consequences, binding and expiry. `orbit task show` exposes a
task's open requests in `open_decisions`.

This setting is workspace configuration; channel registrations belong to the
host and can be used by its workspaces. With the setting disabled, an authorized
digest call returns no requests and writes nothing; channel submissions are
refused with `decision_channel_disabled`.

## Prepare the transport plugin

[Install and grant](../plugins/) your transport plugin on the same host. It must
implement delivery, reply parsing and these Orbit callback operations. The
following is an **illustrative manifest excerpt**, not a complete plugin or a
bundled integration; `approvals` is an example namespace:

```yaml
metadata:
  name: approvals
spec:
  permissions:
    orbit_tools:
      - orbit.decision.digest
      - orbit.decision.submit
  secrets:
    - name: channel_key
      description: Decision-channel token key managed by Orbit.
      rotatable: true
```

Review the complete manifest and grant its `orbit_tools` permission along with
any network or other permissions its actual transport needs. Declare
`channel_key` so the backend receives the mirrored key in its secret context;
registration supplies the value. Keep transport credentials in the plugin's
other declared secrets. Use a dedicated namespace for this channel: registering
or rotating another channel for that namespace replaces its `channel_key`.

Orbit keeps every channel's key in its own channel store as well, and no plugin
backend can read that store, even with `orbit_tools`. The backend does not need
to: when it runs `orbit tool run orbit.decision.digest` or
`orbit.decision.submit`, the Orbit process that started the backend answers the
call. This works under the default plugin sandbox; the backend needs no
`backend.sandbox: none`. Agent sandboxes hide the store too.

Callbacks need the host-issued plugin callback session, an enabled plugin, the
manifest's callback list, the recorded host grant and the spawning caller's
allowance. A name in tool input or an environment variable does not establish
plugin identity. An agent or managed worker cannot gain human authority by
calling the transport. See the [callback and secret contract](../../reference/plugin-manifest/#permission-and-callback-boundaries).

## Register as the operator

Run these commands yourself on the host, outside an agent run or plugin callback.
Replace the example human, plugin namespace and sender with yours:

```bash
orbit decision channel register human-approvals \
  --human 'Alex Rivera' --plugin approvals \
  --allow-sender alex@example.com \
  --require-dkim-verification
orbit decision channel list
```

Registration creates a key, mirrors it to the plugin's `channel_key` secret and
starts key epoch 1. The human label must identify a human, not an automation
actor. Channel names start with a lowercase letter and use lowercase letters,
digits, `-` or `_`. Listing shows registration information, including the epoch
and revocation state; key material is never shown.

Repeat `--allow-sender` for multiple senders. Omitting it sets no sender
restriction in core, but plugin submissions still need a sender and
authentication evidence. Sender matching trims whitespace and ignores case.

`--require-dkim-verification` is off by default. This example turns it on so
every submission must forward the raw reply and pass core-side DKIM verification.
Without the flag, core checks forwarded authentication verdicts and still
verifies DKIM itself whenever a raw message is supplied.

Registration, revocation and key rotation are operator CLI operations. They
have no active MCP tools and are refused from agent envelopes and plugin
callbacks. A plugin cannot register itself as a human.

## Inspect requests or answer directly

```bash
orbit decision list
orbit decision show '<request-id>'
orbit decision list --all
```

`list` and `show` are read-only and available as `orbit.decision.list` and
`orbit.decision.show` over MCP. `--all` includes answered, superseded and
expired requests. Copy the actual request ID and one of its allowed answer IDs.
For example, an operator can answer an open approval request directly:

```bash
orbit decision answer '<request-id>' approve --note 'Reviewed the proposed scope.'
```

The dashboard offers the same human answers. Direct answering needs no channel
token and has no active answering tool for agents. A stale or expired request
is refused without applying the answer.

## Decisions an agent raises on a human task

An agent that needs a person to choose can raise a multiple-choice decision on
a task assigned to a [human crew](../../reference/config/#human-crews), instead
of filing a free-text task. Raising needs no transport plugin and no channel.
The request shows in `orbit decision list` and on the task's dashboard detail,
and the operator answers it there.

```bash
orbit decision raise --task '<task-id>' \
  --question 'Cut over billing before or after the schema migration?' \
  --options '[
    {"label": "Before", "consequence": "Billing reads the new column from day one.", "recommended": true},
    {"label": "After", "consequence": "Billing stays on the old column until the migration lands.", "recommended": false}
  ]'
```

The raise checks these rules before it writes anything:

- `--options` is a JSON array of two to four objects. Each has a one-line
  `label`, a one-line `consequence` and a `recommended` boolean. Exactly one
  option is recommended.
- `--question` is one or two sentences, at most 600 characters.
- `--topic` is optional. It names a structured topic or a corpus rule id. When
  it names a current rule the operator has set to auto, the request is answered
  from that rule at once. That answer is recorded as a precedent, not as a
  human answer.
- The task must be assigned to a human crew and still open. A task that is
  done, rejected or archived refuses the raise, and so does a task that already
  holds a different open request. Raising an identical open question on the
  same task returns the request that already holds it.
- A workspace holds at most 20 open agent-raised requests, and one filer at most
  5. Past either cap the raise is refused and nothing is written.

The same call is `orbit.decision.raise` over MCP. An agent can raise a decision,
but it cannot answer one: there is no answering tool for agents.

### Answer a raised decision

`orbit decision show '<request-id>'` lists each option with its answer id. The
recommended option is always `1`:

```bash
orbit decision answer '<request-id>' 1 --note 'Confirmed with billing.'
```

You can also press the matching button on the task's dashboard detail. Answering
completes the task, which unblocks any task waiting on it. If the task has left
its human crew since the raise, the answer is refused and the request stays open.

### Precedent for repeated decisions

A corpus rule lets the operator answer a kind of decision once and have later
raises with that topic answered from the rule.

- `orbit decision rule-auto <rule> <on|off>` turns auto on or off for one
  current rule in the workspace's decision corpus. `<rule>` is `topic/rule` or a
  unique rule id. The command rewrites that rule's file; commit the one-line
  change like any other edit.
- `orbit decision reverse <id>` re-opens a request that a precedent answered and
  restores the task status that the precedent recorded. Only the operator can
  run it.
- `orbit decision consolidate [--since YYYY-MM-DD]` reads the human answers on or
  after the date and proposes corpus rule edits. It writes nothing. Put the
  proposals in a pull request rather than editing rules directly.

These three commands are CLI only. They have no MCP tool.

## Request an approval digest

As the operator, inspect the digest with:

```bash
orbit decision digest --channel human-approvals --json
```

The bound plugin calls `orbit.decision.digest` through its callback session:

```bash
orbit tool run orbit.decision.digest --input '{"channel":"human-approvals"}'
```

This is a write: it raises missing `proposed_approval` and `review_approval`
requests with `approve` and `reject` answers. Proposed approvals bind to the
description, acceptance criteria and proposed status; review approvals also
bind to the candidate under review. Raising is idempotent for the same task,
gate and binding while its request is open.

The JSON contains `requests`, `raised` and `expiring`: the digest, requests
created by this call, and still-open requests expiring within 24 hours.
It carries approval requests plus those nearing expiry; use `decision list`
for all open requests. Calling the digest neither sends a message nor approves
work. The plugin renders and delivers it to the human. Agents are refused
`caller_not_allowed`; a plugin bound to another channel is refused
`channel_not_registered`.

## Return an authenticated reply

The transport mints a token for each request using its `channel_key` secret and
the current channel epoch. The digest and read-only request tools do not return
tokens, and there is no token-minting tool. For plugin authors, the token contract
is the first 26 characters of unpadded RFC 4648 Base32 of:

```text
HMAC-SHA256(hex_decode(channel_key), UTF8(request_id|task_id|epoch|expires_at_unix_seconds))
```

The separators are literal `|` characters; the epoch and expiry are decimal
integers. Present the request, its permitted answers and the token to the human,
then parse the reply. From the bound plugin's callback session, call
`orbit.decision.submit` with an input shaped like this **illustrative example**:

```json
{
  "channel": "human-approvals",
  "id": "<request-id>",
  "answer": "approve",
  "token": "<per-request-token>",
  "note": "Reviewed the proposed scope.",
  "sender": "alex@example.com",
  "auth_results": "<actual Authentication-Results header from the transport>",
  "raw_message": "<complete original raw reply, preserving headers and body>"
}
```

Forward actual evidence from the reply. Plugin submissions require `sender`
and `auth_results`; core requires `dkim=pass` and `spf=pass`, and refuses
`dmarc=fail`. A sender outside the configured allowlist is refused. With a raw
message, core verifies the DKIM body hash, header signature and signing-domain
alignment with the sender, using the selector key from DNS. Raw messages are
limited to 262144 bytes (256 KiB) and are required by the registration above.

An operator testing a channel can use the same governed submission path:

```bash
orbit decision submit '<request-id>' approve \
  --channel human-approvals --token '<per-request-token>' \
  --sender alex@example.com --auth-results "$AUTH_RESULTS" \
  --raw-message "$RAW_MESSAGE" --note 'Reviewed the proposed scope.'
```

Here `AUTH_RESULTS` and `RAW_MESSAGE` hold the actual reply evidence, and the
token comes from the transport. Operator tests may omit sender and forwarded
verdicts, but a channel requiring raw-message verification still requires the
message. Successful submissions write as the registered human and record the
channel and token fingerprint as provenance. Core still enforces the gate's
rules: for example, a pilot-admission `evaluated` answer needs an evidence note,
and proposed approval cannot bypass an admission hold.

## Handle expiry, replay and key changes

Requests have a default lifetime of 72 hours; use each request's `expires_at`.
A changed task binding or review candidate supersedes the old request. Tokens
expire with their request and are single-use. Every refusal leaves the requested
answer unapplied, though an authenticated stale-token attempt can consume the
token and record that refusal.

| Refusal | What to do |
|---|---|
| `caller_not_allowed`, `channel_not_registered`, `channel_revoked` | Check callback identity and the host registration. Agents cannot submit. |
| `sender_not_allowed`, `authentication_failed` | Fix the sender or forwarded evidence; these refusals consume no token. |
| `dkim_verification_required`, `raw_message_too_large`, `dkim_verification_failed` | Supply an intact, within-limit raw reply that passes verification; no token is consumed. |
| `dkim_verification_unavailable` | Retry when the DNS lookup is available; nothing was consumed or applied. |
| `token_invalid` | Check request, channel, key and epoch; an invalid token consumes nothing. |
| `answer_not_allowed` | Use a listed answer and any required note. An empty or disallowed answer consumes no token. |
| `request_expired`, `request_superseded` | Fetch current requests and present the current decision again; authenticated stale attempts consume the token. |
| `token_consumed` | Do not replay the reply. The token was already judged, or key rotation invalidated it; inspect the current request. |

To replace a key as the operator:

```bash
orbit decision channel rotate-key human-approvals
```

Rotation increments the epoch and mirrors a fresh key to the bound plugin.
Every outstanding token becomes invalid, even for unexpired requests. Open
requests remain waiting; have the plugin re-present them with fresh tokens.

To stop a channel immediately:

```bash
orbit decision channel revoke human-approvals
```

Revocation refuses further submissions with `channel_revoked`. Rotating a
revoked channel is refused. To restore it, register it again as the operator;
its epoch advances so tokens from the revoked registration cannot verify.
