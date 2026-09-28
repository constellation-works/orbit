# orbit-agent

Agent provider abstraction for Orbit. Two transport families coexist:

- **CLI transports** drive `claude`, `codex`, `gemini`, `grok`, `ollama`, and `mock`
  as subprocesses via the existing `AgentRuntime` trait. An invocation
  builds an `AgentInvocationSpec` (program, args, stdin envelope) that the
  engine runs through `orbit-exec`. **This is the path Orbit executes
  activities on.**
- **HTTP transports** drive providers directly through the sibling
  `LoopTransport` trait. The provider-agnostic `AgentLoop` runs the
  send/parse/dispatch cycle, enforcing guardrails and tool-allowlist rules
  and emitting a complete structured audit trail.

> **Not an Orbit execution backend.** ORB-10801 retired the
> `backend: http | cli | auto` selector and the engine's HTTP agent-loop
> driver, so no Orbit activity or job dispatches through `loop_engine`. It
> remains a standalone SDK surface for this crate's own consumers and
> examples (`cargo run -p orbit-agent --example ...`). Nothing in
> `config.toml`, an activity asset, or a job asset can select it.

The two trait shapes diverge enough — one-shot command descriptor vs.
iterative conversation driver — that they are kept as siblings.

## HTTP loop primitives

```
crates/orbit-agent/src/loop_engine/
  agent_loop.rs     AgentLoop::run, guardrails, iteration bookkeeping
  session.rs        Session (id, provider, model, system_prompt, history)
  transport.rs      LoopTransport trait + Message/ContentBlock/TurnRequest/TurnResponse
  tool_dispatch.rs  Thin adapter around orbit_tools::ToolRegistry::execute
  audit/            LoopAuditEvent, AuditSink, InMemorySink, BlobStore, redaction
```

- `AgentLoop::run(session, cfg, transport, registry, ctx, sink, prompt)`
  runs a conversation turn: builds a `TurnRequest` from replayed history,
  hands it to the transport, parses `tool_use` blocks out of the response,
  dispatches each through the shared `ToolRegistry`, and appends tool
  results as the next user turn until the provider returns `end_turn` or a
  guardrail fires.
- `Session::new(provider, model, system_prompt, audit_tag)` creates an
  in-process conversation handle with a stable opaque identifier.
  `Session::send(cfg, transport, registry, ctx, sink, prompt)` is a thin
  wrapper that delegates to `AgentLoop::run`. Sessions are not persisted
  to disk; `close(run_id, sink)` records a `SessionClose` event.
- `LoopTransport` has one hot method — `send_turn(&TurnRequest) ->
  Result<TurnResponse, TransportError>` — plus `provider()` and `model()`
  identifiers. The trait is shaped around the most expressive wire format
  (Anthropic content blocks and `cache_control` markers) so collapsing
  other providers into it does not lose fidelity.

## HTTP transports

- `providers::anthropic::AnthropicMessagesTransport` — `POST
  https://api.anthropic.com/v1/messages` via blocking `reqwest`. Applies
  `cache_control: ephemeral` to the last system block and, per the loop's
  cache hint, to the last message in the replayed history.
- `providers::openai_compat::OpenAiCompatTransport` — `POST
  {base_url}/v1/chat/completions` via blocking `reqwest`, with
  configurable `base_url`, optional custom headers, optional bearer auth,
  and an override for the endpoint path when a compatible deployment uses
  a different route. Tool calls use the OpenAI `tools` / `tool_calls`
  schema and cached prompt tokens are surfaced from
  `usage.prompt_tokens_details.cached_tokens` when present.
- `providers::gemini_http::GeminiHttpTransport` — `POST
  https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent`
  via blocking `reqwest`. Supports Gemini's distinct `functionCall`/`functionResponse`
  tooling. Includes native `cachedContents` support: when history length exceeds
  `cache_content_threshold_turns`, automatically issues a `POST .../cachedContents`
  before generation to cache the multi-turn session.

All three transports read response bodies through a shared byte ceiling
that applies while reading, so chunked bodies without `Content-Length` are
bounded as well. A success body above 16 MiB (including a Gemini
`cachedContents` response) fails with a `TransportError::Decode` naming the
limit, and a declared `Content-Length` above it is refused before reading.
For a non-2xx status only the first 64 KiB of the body are read into the
`BadStatus` / `Auth` diagnostic, with a truncation marker when more
followed. `max_response_tokens` is only a request hint and does not bound
the bytes a server returns.

### OpenAI-compatible config surface

- `OpenAiCompatTransport::hosted(api_key, model)` targets the default
  hosted OpenAI base URL: `https://api.openai.com`.
- `OpenAiCompatTransport::new(base_url, api_key, model, custom_headers)`
  lets callers point the same transport at hosted OpenAI, Codex, or local
  servers such as Ollama, LM Studio, llama.cpp server, or vLLM.
- `with_bearer_auth(false)` disables the default `Authorization: Bearer
  ...` header for local deployments that reject it.
- `with_endpoint_path(...)` overrides `/v1/chat/completions` for
  compatible gateways that expose the same wire contract on a different
  route.

## Tool-allowlist contract

Two independent knobs on `AgentLoopConfig`:

- `tool_allowlist: Vec<String>` — the **dispatch** allowlist. The only
  tools the loop will actually execute. Empty = **no tools**, not "all
  tools".
- `advertised_tools: Option<Vec<String>>` — the set advertised to the
  model in the request payload. When `None`, this equals
  `tool_allowlist` (the common case: the model only knows about tools
  it's allowed to call). When `Some`, the advertised set can be a
  superset of the allowlist — useful when exercising the enforcement
  path end-to-end, since a model won't emit a `tool_use` block for a
  tool it was never told exists.

When the model returns a `tool_use` block whose `name` is not in
`tool_allowlist`, the loop emits a `PolicyDenial` audit event naming the
tool and returns `AgentLoopError::PolicyDenied`. The tool is never
dispatched through `ToolRegistry::execute`.

All tool dispatch runs through `orbit_tools::ToolRegistry::execute` —
there is no parallel tool path. Tool attribution, workspace boundaries,
process allowlists, and `OrbitToolHost` routing flow from the same
`ToolContext` used elsewhere in Orbit.

## Guardrails

Three distinct structured errors, each configurable on
`AgentLoopConfig`:

| Guardrail | Field | Error |
|---|---|---|
| Iteration cap | `max_iterations: u32` | `AgentLoopError::MaxIterations { limit, observed }` |
| Token budget | `max_total_tokens: u64` | `AgentLoopError::TokenBudget { limit, observed }` |
| Wall-clock deadline | `wall_clock_timeout: Duration` | `AgentLoopError::Timeout { limit_ms, observed_ms }` |

Each check runs at iteration start and after every HTTP response. The
first to trip wins.

The wall-clock deadline is also checked before each tool dispatch and
once more after a response's tools finish, before the loop returns
success or starts the next turn. A tool already running is not
interrupted, but once the budget expires no further tool in that
response executes and the loop returns `Timeout`. The session keeps a
consistent history: completed calls keep their real `tool_result`, and
every call skipped after expiry gets an `is_error` `tool_result` with
code `wall_clock_timeout`, so each `tool_use` stays paired on replay.

## Audit model

Every operation emits a structured event. Events carry sha256 pointers
to redacted payloads; full bodies live in a separate content-addressed
store keyed by the post-redaction bytes. Event kinds:

- `session_spawn`, `session_close`
- `http_request`, `http_response`
- `tool_call_requested`, `tool_call_result`
- `iteration_boundary`
- `policy_denial`

Every event carries `run_id`, `session_id`, optional `task_id`, and
`iteration` (when applicable) so downstream querying can scope results.

### Default sink layout

Orbit runtime callers construct the v2 audit writer in `orbit-engine` with a
SQLite-backed sink. Loop and envelope events are inserted into the v2 audit
tables under the runtime audit database; redacted payload bodies still use the
content-addressed blob store:

```
{audit_root}/
  blobs/{hash[..2]}/{hash}         content-addressed redacted payloads
```

The SQLite rows and blob store are designed to be read by Orbit's v2 audit
query surfaces. Humans can inspect blob payloads by hash; event metadata and
payload JSON are queried from SQLite rather than per-run JSONL files.

Orbit runtime callers pass `.orbit/state/audit` as `audit_root`; standalone
examples use temporary roots under the system temp directory.

## Redaction

Blob writes run through `redact_all()` before the bytes are hashed or reach
disk. Callers can layer a stronger `PatternRedactor` on top, but cannot weaken
the default env-value and pattern redaction. The default ruleset scrubs:

- live sensitive environment variable values such as `*_TOKEN`, `*_SECRET`,
  `*_PASSWORD`, `*_API_KEY`, auth/session/cookie names, and private keys
- `"authorization": "..."`, `"x-api-key": "..."`, `"api_key": "..."`
  (JSON-shaped)
- `Authorization: ...`, `x-api-key: ...`, `api_key: ...` (raw header
  lines)
- `Bearer <token>` anywhere in the payload
- high-confidence provider token shapes such as `sk-...`, `ghp_...`, and
  `xox...` values
- structural OpenSSH public-key fingerprints and comments, plus host/address
  identifiers in canonical connection diagnostics

The author-facing field policy, complete pattern-family inventory, and response
detail contract live in
[`docs/design/auditability/specs/artifact-redaction.md`](../../docs/design/auditability/specs/artifact-redaction.md).

Redaction runs at **write time**, not read time — the stored bytes are
already safe, and blob references point to the redacted content hash. A future
`orbit.audit.loop.blob.get` tool does not need to re-apply redaction.

## Running the examples

Six runnable examples under `crates/orbit-agent/examples/`:

| Example | Needs credentials | Demonstrates |
|---|---|---|
| `anthropic_messages` | yes (skips cleanly if unset) | Single-turn prompt, usage + terminate reason printed |
| `openai_compat` | hosted: yes; local localhost path: no | Hosted OpenAI 1-turn prompt, or clean skip when `OPENAI_BASE_URL` points at an unreachable localhost-compatible endpoint |
| `google_gemini` | yes (skips cleanly) | Single-turn prompt, usage + terminate reason printed using Gemini `generateContent` API |
| `session_continuation` | yes (skips cleanly) | 3 consecutive `send()` calls; asserts history replayed + `cache_read_input_tokens > 0` on turn 2+ |
| `tool_allowlist` | yes (skips cleanly) | Allowlist `["orbit.task.show"]` + prompt pressuring `orbit.task.delete`; asserts `PolicyDenied` error |
| `guardrails_smoke` | no | All three guardrails trip via an in-process scripted transport; verifies distinct error variants |
| `redaction_smoke` | no | Writes a payload containing `Bearer secret-xyz` and asserts the stored blob does not contain `secret-xyz` |

Run any example with `cargo run -p orbit-agent --example <name>`. The
API-key-backed examples skip with exit 0 and a printed notice when the
key is unset, and `openai_compat` also skips cleanly when
`OPENAI_BASE_URL` points at localhost with no server listening. This
keeps `cargo build --examples -p orbit-agent` safe in CI and on laptops
without provider credentials or a local model server.

## What didn't land in this task

These are split to follow-up tasks that build on the primitives here:

- **`orbit.audit.loop.*` query tools** (`list`, `show`, `blob.get`) so
  agents can programmatically inspect the JSONL + blob layout.

## Dependency direction

`orbit-types`, `orbit-tools` → `orbit-agent` → `orbit-engine`. The HTTP
loop introduces no edge on `orbit-engine`, `orbit-core`, or `orbit-cli`:
since ORB-10801 the engine consumes only this crate's CLI runtimes and
response types.
