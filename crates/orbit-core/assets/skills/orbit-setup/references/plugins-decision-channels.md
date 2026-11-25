# Plugins: decision channels

## Contents

- [Decision channels](#decision-channels)

## Decision channels

A decision channel lets one plugin submit a human's answer to an open
decision request on held work. The channel is registered per host and binds
three things together: a channel name, the human identity the channel speaks
for, and the plugin namespace allowed to submit through it.

When the plugin
presents the per-request single-use token minted for that request under the
channel's key, core applies the answer as the registered human and records
the channel and the token fingerprint as provenance.

What a decision request
is, and how held work raises one, is described in
[run-debugging-decisions.md](../../orbit-orchestrate/references/run-debugging-decisions.md#decision-requests-on-held-work);
this section covers only the channel side: registering, listing, revoking
and rotating a channel, and the plugin-only submit path.

The channel is off until the workspace enables it. `decision_channel.enabled`
is a boolean, default `false`, that turns decision requests on for held work in
the workspace. With it `false`, the digest raises nothing and writes nothing,
and a submit from an allowed caller is refused `decision_channel_disabled`
before the channel is looked up, so nothing is applied. Enable it, then
register a channel:

```bash
orbit config set decision_channel.enabled true
```

`orbit config set` writes the workspace config file unless `--global` is given,
and ordinary settings inherit per key, so a workspace value overrides a global
one. Setting the key back to `false` turns the channel off again. Enabling it
registers nothing: `register` below is still required, and until a channel is
registered a submit is refused `channel_not_registered`.

Channel management is an operator job on this host. `orbit decision channel
register`, `list`, `revoke` and `rotate-key` are operator CLI commands; the
matching tools (`orbit.decision.channel_register`, `orbit.decision.channel_list`,
`orbit.decision.channel_revoke`, `orbit.decision.channel_rotate_key`) are
inactive with no MCP scope, so they are never reachable from a tool call.

Agents cannot register channels or submit answers: registering is refused
from an agent envelope and from any plugin callback, and submitting is never
callable from an agent session.

```bash
orbit decision channel register <channel> --human <human> --plugin <plugin>
orbit decision channel register <channel> --human <human> --plugin <plugin> \
  --allow-sender <sender> --allow-sender <other-sender>
orbit decision channel register <channel> --human <human> --plugin <plugin> \
  --require-dkim-verification
orbit decision channel list
orbit decision channel revoke <channel>
orbit decision channel rotate-key <channel>
```

- `register` needs the channel name plus `--human` (the human identity
  answers are written under) and `--plugin` (the plugin namespace allowed to
  submit through this channel). `--allow-sender` is optional and repeatable:
  when any senders are registered, a submission must present one of them.

  Registration generates the channel key and mirrors it to the bound plugin's
  `channel_key` secret; a new channel name starts at key epoch 1. An active name
  cannot be registered again; revoke it first (or use `rotate-key` to replace its
  key). Re-registering a revoked name uses the previous epoch + 1.
  `--require-dkim-verification`
  is the per-channel switch (off by default) that makes core-side DKIM
  verification required: every submission on that channel must forward the
  raw message and pass core's own DKIM check, or it is refused.
- `list` shows each channel's name, human, bound plugin, key epoch and
  fingerprint, allowed senders, whether core-side DKIM verification is
  required, and whether it is revoked. Key material is never shown.
- `revoke` takes effect immediately: outstanding tokens stop verifying and
  every submission through the channel is refused `channel_revoked`.
- `rotate-key` generates a fresh key, mirrors it to the bound plugin's
  `channel_key` secret, and bumps the key epoch. Every outstanding token is
  invalidated — a token minted under the previous epoch is refused even while
  its request is still open — and open requests wait to be re-presented under
  fresh tokens.

The plugin submits through the governed `orbit.decision.submit` path: only
the plugin the channel is bound to, through its callback session, or the
operator testing a channel. Core keeps every channel's key in
`~/.orbit/state/decision-channels/`, which no plugin backend can read, even
with `orbit_tools`, and which agent sandboxes hide.

The backend does not
need to read it: the Orbit process that started the backend answers its
digest and submit callbacks, so they work under the default plugin sandbox
and need no `backend.sandbox: none`. The CLI form is:

```bash
orbit decision submit <request-id> <answer-id> --channel <channel> --token <token>
orbit decision submit <request-id> <answer-id> --channel <channel> --token <token> \
  --note "<evidence>" --sender <sender> --auth-results "<header-value>" \
  --raw-message "<raw reply message>"
```

`<request-id>` is the id `orbit decision list` prints and `<answer-id>` is
one of the request's allowed answers. `--note` is evidence kept with the
answer (a pilot-admission `evaluated` answer is refused without one).
`--sender` is the transport sender the submission presents as evidence: a
plugin submission must present one, and it must be on the channel's
allowlist when the channel has one; the operator's own test submission may
omit it.

`--auth-results` is the forwarded Authentication-Results header: a
plugin submission must forward one showing `dkim=pass` and `spf=pass`, and is
refused on `dmarc=fail`; the operator's own test submission may omit it.
`--raw-message` is the raw reply message the transport forwards for
core-side DKIM verification, at most 262144 bytes (256 KiB).

When a raw
message is forwarded, core verifies its DKIM signature itself: it fetches
the selector's public key from DNS (a 5-second bounded lookup), checks the
body hash and the signed-header signature, and requires the signing domain
to align with the presented sender's domain. Core refuses when its own
result contradicts the forwarded verdicts.

On a channel registered with
`--require-dkim-verification` the raw message is required; otherwise it is
optional, and a submission without one behaves exactly as before. The
tool form takes the same fields as `id`, `channel`, `answer`, `token`,
`note`, `sender`, `auth_results` and `raw_message`. Never paste a real token or key into an
example, a task record or a shell history file; the examples above use
placeholders only.

Every refusal applies nothing. A parse-level refusal does not consume the
token. The refusal codes are:

| Code | What it means |
|---|---|
| `decision_channel_disabled` | The decision channel is not enabled in this workspace; nothing was applied. |
| `channel_not_registered` | No channel by that name is registered on this host. |
| `channel_revoked` | The channel is revoked; its tokens no longer verify. |
| `caller_not_allowed` | The caller is neither the bound plugin through its callback session nor the operator testing the channel. |
| `sender_not_allowed` | The presented sender is not on the channel's sender allowlist. |
| `authentication_failed` | Transport authentication evidence is missing or failing. |
| `dkim_verification_failed` | Core's own DKIM verification of the forwarded raw message failed (unsigned, body hash or header signature mismatch, no key record, or the signing domain does not align with the sender's domain); the forwarded verdicts do not override it. The token is not consumed. |
| `dkim_verification_unavailable` | The DNS lookup for DKIM verification failed or timed out. Retryable: the token is not consumed and nothing was applied; present the submission again. |
| `dkim_verification_required` | The channel was registered with `--require-dkim-verification` but the submission forwarded no raw message. |
| `raw_message_too_large` | The forwarded raw message exceeds the 262144-byte (256 KiB) bound for core-side DKIM verification. |
| `token_invalid` | The token does not verify for this request, channel and key epoch. |
| `token_consumed` | The token was already used (tokens are single-use), or was minted under the previous key epoch after rotate-key, which rotation invalidated; a previous-epoch token is consumed when refused. |
| `request_expired` | The request expired while open. |
| `request_superseded` | The hold changed before the answer arrived. |
| `answer_not_allowed` | The answer id is not one of the request's allowed answers. |
| `request_not_found` | No decision request has that id. |

The plugin's digest routine raises what it presents through the governed
`orbit.decision.digest` path, on the same terms as submit: only the plugin
the channel is bound to, through its callback session, or the operator —
never an agent session (`caller_not_allowed`), and a plugin the named
channel is not bound to is refused `channel_not_registered`, as is an
unregistered channel; a revoked channel is refused `channel_revoked`. The
CLI form is:

```bash
orbit decision digest --channel <channel>
```

One call returns the whole digest. For every proposed task without an open
`proposed_approval` request, and every review task without an open
`review_approval` request, it first raises one open request — idempotently
per task, gate and binding, with answers `approve`/`reject` — then returns
`requests` (the digest), `raised` (what this call raised) and `expiring`
(every still-open request expiring within 24 hours, the digest window).

A
second call raises nothing new and returns the same request ids. A
`proposed_approval` request binds to the task's description and acceptance
criteria plus its proposed status, and a `review_approval` request also to
the candidate under review, so either reads superseded once what it was
raised against changes and the next digest raises a fresh one. With the
decision channel disabled in the workspace, the digest raises nothing,
writes nothing and returns no requests.

The tool form takes the same
`channel` field. Mail rendering, batching, quiet hours and parsing a reply
that answers several digest items stay the plugin's.

Verify a channel without spending a real answer:

1. `orbit config get decision_channel.enabled` prints `true`. Confirm this
   first: while the key is `false`, the refusal in step 3 is
   `decision_channel_disabled` for any channel name, not the channel codes
   below. `orbit config show --all` lists the key with the file or default it
   came from.
2. `orbit decision channel list` shows the channel with the expected human,
   bound plugin, key epoch and fingerprint, and (when set) the allowed
   senders. A revoked channel reads revoked here.
3. Exercise the refusal path with a bad submit — for example a channel name
   that is not registered, or a revoked channel — and confirm the matching
   refusal code above. Because every refusal applies nothing, the open
   request is untouched; re-list the request to confirm it is still open
   before the real answer goes through the bound plugin.
