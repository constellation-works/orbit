---
title: Decision channels
description: "Answer the decision requests held work is waiting on, let agents raise them, and connect a plugin that delivers them to a human and returns authenticated answers."
---

A decision request is held work waiting on a human answer. You can answer one
from the terminal or the dashboard. An agent can raise one on a human task. A
transport plugin can deliver requests through a registered decision channel and
return the human's reply. Agents can read and raise requests, never answer them.

## Inspect requests or answer directly

```bash
orbit decision list
orbit decision show '<request-id>'
orbit decision answer '<request-id>' approve --note 'Reviewed the proposed scope.'
```

`list` shows open requests. Add `--all` for answered, superseded and expired
ones, or filter with `--task`, `--gate` or `--topic`. `show` prints the
request's allowed answer IDs. A stale or expired request refuses the answer.

The dashboard offers the same answers. Answering directly needs no channel and
no token. Over MCP, `orbit.decision` has the read-only actions `list` and
`show`, and no answering action.

## Decisions an agent raises on a human task

An agent that needs a person to choose can raise a multiple-choice decision on
a task assigned to a [human crew](/reference/config/#human-crews), instead of
filing a free-text task. Raising needs no transport plugin and no channel. The
request shows in `orbit decision list` and on the task's dashboard detail.

```bash
orbit decision raise --task '<task-id>' \
  --question 'Cut over billing before or after the schema migration?' \
  --options '[
    {"label": "Before", "consequence": "Billing reads the new column from day one.", "recommended": true},
    {"label": "After", "consequence": "Billing stays on the old column until the migration lands.", "recommended": false}
  ]'
```

Over MCP the same call is `orbit.decision` action `raise`.

| Input | Rules |
|---|---|
| `--task` | A task assigned to a human crew and still open. A done, rejected or archived task refuses the raise, and so does a task that already holds a different open request. Raising an identical open question returns the existing request. |
| `--question` | One or two sentences, at most 600 characters. |
| `--options` | A JSON array of two to four objects, each with a one-line `label`, a one-line `consequence` and a `recommended` boolean. Exactly one option is recommended. |
| `risk_tier` (per option, optional) | `routine`, `reversible`, `irreversible`, `authority` or `spend`. Omitted means `unknown`. Any other value refuses the raise. The request takes the highest tier among its options, and `unknown` ranks above `spend`. |
| `--topic` (optional) | A structured topic or a corpus rule ID. If it names a current rule the operator has set to auto, the request is answered from that rule at once and recorded as a precedent, not a human answer. |

A workspace holds at most 20 open agent-raised requests, and one filer at most
5. Past either cap the raise is refused.

### Answer a raised decision

`orbit decision show '<request-id>'` lists each option with its answer ID. The
recommended option is always `1`:

```bash
orbit decision answer '<request-id>' 1 --note 'Confirmed with billing.'
```

You can also press the matching button on the task's dashboard detail. Answering
completes the task, which unblocks any task waiting on it. If the task has left
its human crew since the raise, the answer is refused and the request stays open.

### Precedent for repeated decisions

A corpus rule lets the operator answer a kind of decision once. Later raises
with that topic are answered from the rule. These commands are CLI only:

| Command | Effect |
|---|---|
| `orbit decision rule-auto <rule> <on\|off>` | Turns auto on or off for one current rule in the workspace's decision corpus. `<rule>` is `topic/rule` or a unique rule ID. The command rewrites that rule's file. Commit the change. |
| `orbit decision reverse <id>` | Re-opens a request that a precedent or model answered, and restores the task status that answer recorded. Operator only. |
| `orbit decision consolidate [--since YYYY-MM-DD]` | Reads the human answers on or after the date and proposes corpus rule edits. It writes nothing. Put the proposals in a pull request. |

## Enable requests in a workspace

The rest of this page sets up a decision channel. You need a plugin that
implements the exchange: registering a channel does not install a mail plugin,
send a digest or poll for replies.

Run from the registered workspace:

```bash
orbit config set decision_channel.enabled true
```

The setting defaults to `false`. When it is on:

- blocked-task final recovery and task-pilot admission holds raise decision
  requests;
- the [digest](#request-an-approval-digest) raises approval requests for
  proposed and review tasks.

`orbit task show` lists a task's open requests in `open_decisions`.

The setting is per workspace. Channel registrations belong to the host and can
be used by its workspaces. While the setting is off, a digest call returns no
requests and writes nothing, and channel submissions are refused with
`decision_channel_disabled`.

## Prepare the transport plugin

[Install and grant](/how-to/plugins/) your transport plugin on the same host. It
must implement delivery and reply parsing, and call two Orbit tools. An
illustrative manifest excerpt, with `approvals` as an example namespace:

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

- Grant `orbit_tools`, plus whatever network or other permissions the transport
  needs.
- Declare `channel_key` so the backend receives the channel's key. Registration
  supplies the value. Keep transport credentials in other declared secrets.
- Use a dedicated namespace per channel. Registering or rotating another channel
  for the same namespace replaces its `channel_key`.
- The default plugin sandbox is enough. The backend needs no
  `backend.sandbox: none`.

Orbit identifies the plugin by the callback session it issued to the backend,
not by a name in tool input or an environment variable. See the
[callback and secret contract](/reference/plugin-manifest/#permission-and-callback-boundaries).

## Register as the operator

Run these yourself on the host, outside an agent run or plugin callback. Replace
the example human, plugin namespace and sender with yours:

```bash
orbit decision channel register human-approvals \
  --human 'Alex Rivera' --plugin approvals \
  --allow-sender alex@example.com \
  --require-dkim-verification
orbit decision channel list
```

Registration creates a key, mirrors it to the plugin's `channel_key` secret and
starts key epoch 1. `list` shows each registration with its epoch and revocation
state, never key material.

| Argument | Rules |
|---|---|
| `<CHANNEL>` | Starts with a lowercase letter. Lowercase letters, digits, `-` or `_`. |
| `--human` | Must identify a human, not an automation actor. |
| `--plugin` | The plugin namespace allowed to submit through this channel. |
| `--allow-sender` | Repeat for several senders. Matching trims whitespace and ignores case. Without it core sets no sender restriction, but plugin submissions still need a sender and authentication evidence. |
| `--require-dkim-verification` | Off by default. When on, every submission must forward the raw reply and pass core-side DKIM verification. When off, core checks the forwarded authentication verdicts, and still verifies DKIM whenever a raw message is supplied. |

Registering, revoking and rotating keys are operator CLI operations. They have
no MCP tools and are refused from agent runs and plugin callbacks.

## Request an approval digest

As the operator, inspect the digest with:

```bash
orbit decision digest --channel human-approvals --json
```

The bound plugin calls the same tool through its callback session:

```bash
orbit tool run orbit.decision.digest --input '{"channel":"human-approvals"}'
```

This is a write. It raises missing `proposed_approval` and `review_approval`
requests, each with `approve` and `reject` answers. Calling again does not
raise a duplicate while the request is open.

The JSON has three lists:

| Field | Contents |
|---|---|
| `requests` | The digest: approval requests, plus requests nearing expiry. |
| `raised` | Requests this call created. |
| `expiring` | Open requests that expire within 24 hours. |

The digest sends no message and approves nothing. The plugin renders and
delivers it. For every open request, use `orbit decision list`. Agents are
refused with `caller_not_allowed`, and a plugin bound to another channel with
`channel_not_registered`.

## Return an authenticated reply

The transport mints a token for each request from its `channel_key` secret and
the current channel epoch. No Orbit tool returns or mints tokens. The token is
the first 26 characters of unpadded RFC 4648 Base32 of:

```text
HMAC-SHA256(hex_decode(channel_key), UTF8(request_id|task_id|epoch|expires_at_unix_seconds))
```

The separators are literal `|` characters. The epoch and expiry are decimal
integers. Present the request, its allowed answers and the token to the human,
then parse the reply. From the bound plugin's callback session, call
`orbit.decision.submit` with input shaped like this:

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

Forward the actual evidence from the reply:

- Plugin submissions require `sender` and `auth_results`. Core requires
  `dkim=pass` and `spf=pass`, and refuses `dmarc=fail`.
- A sender outside the channel's allowlist is refused.
- With `raw_message`, core verifies the DKIM signature itself and checks that
  the signing domain aligns with the sender. The limit is 262144 bytes
  (256 KiB). The registration above requires a raw message.

A successful submission writes as the registered human and records the channel
and token fingerprint. The gate's own rules still apply: for example, a
proposed approval cannot bypass an admission hold.

To test a channel as the operator, use the same path:

```bash
orbit decision submit '<request-id>' approve \
  --channel human-approvals --token '<per-request-token>' \
  --sender alex@example.com --auth-results "$AUTH_RESULTS" \
  --raw-message "$RAW_MESSAGE" --note 'Reviewed the proposed scope.'
```

An operator test may omit the sender and forwarded verdicts. A channel that
requires raw-message verification still requires the message.

## Handle expiry, replay and key changes

Requests live 72 hours by default. Read each request's `expires_at`. An
approval request is bound to the task's description, acceptance criteria and
status, and a review approval also to the candidate under review. A change to
a request's binding supersedes it. Tokens are single-use and expire with their
request. A refusal never applies the answer.

| Refusal | What to do |
|---|---|
| `caller_not_allowed`, `channel_not_registered`, `channel_revoked` | Check the callback identity and the host registration. Agents cannot submit. |
| `sender_not_allowed`, `authentication_failed` | Fix the sender or the forwarded evidence. No token is consumed. |
| `dkim_verification_required`, `raw_message_too_large`, `dkim_verification_failed` | Supply an intact raw reply within the size limit that passes verification. No token is consumed. |
| `dkim_verification_unavailable` | Retry when the DNS lookup is available. Nothing was consumed or applied. |
| `token_invalid` | Check the request, channel, key and epoch. Nothing is consumed. |
| `answer_not_allowed` | Use a listed answer and any required note. No token is consumed. |
| `request_expired`, `request_superseded` | Fetch current requests and present the current decision again. An authenticated stale attempt consumes the token. |
| `token_consumed` | Do not replay the reply. The token was already judged, or a key rotation invalidated it. Inspect the current request. |

To replace a key as the operator:

```bash
orbit decision channel rotate-key human-approvals
```

Rotation increments the epoch and mirrors a fresh key to the bound plugin. Every
outstanding token becomes invalid. Open requests keep waiting: have the plugin
present them again with fresh tokens.

To stop a channel immediately:

```bash
orbit decision channel revoke human-approvals
```

Further submissions are refused with `channel_revoked`, and a revoked channel
cannot be rotated. To restore it, register it again. Its epoch advances, so
tokens from the revoked registration cannot verify.
