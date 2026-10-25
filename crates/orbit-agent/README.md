# orbit-agent

Provider CLI runtimes and audit contracts for Orbit. Provider adapters drive
`claude`, `codex`, `copilot`, `gemini`, `agy`, `grok`, `ollama`,
`muse`, and `mock-agent` through subprocess command
descriptors. A descriptor sends the prompt on stdin, or, for a CLI that cannot
read stdin (`muse`), names a prompt-file flag the engine fills with a private
file path.
The engine runs those descriptors through `orbit-exec`; stdout helpers project
provider answers into Orbit response envelopes and diagnostics.

## Response selection

`ParsedStdout` uses the same bounded discovery for status, declared failure,
completion-protocol checks and result parsing. Complete JSON documents and
arrays are searched newest first. JSON-encoded answer strings and answers with
surrounding prose select the last complete envelope too; warning lines cannot
make an earlier success override a terminal failure or timeout. Within provider
wrapper objects, preferred answer fields (starting with `structured_output`)
retain priority over other fields.

Selection recognizes a complete object with `schemaVersion` and `status` keys
before validating their types or any payload fields. A malformed terminal
candidate cannot be skipped in favor of an earlier or nested response. The
completion-protocol check validates only the schema version and status token;
result parsing also deserializes the selected payload and rejects invalid error
or duration fields. Status and failure peeks retain the terminal declaration
when readable, even if its other fields are invalid. An explicit candidate also
prevents an abnormal provider wrapper from synthesizing a replacement failure.

Discovery caps JSON parse attempts and visited nodes. Exhausting either budget
returns a protocol error, with no peeked status or failure, rather than returning
an earlier envelope or synthesizing an outcome. Surrounding prose never supplies
response fields.

Invocation usage reads counter blocks and known provider metadata containers,
including Codex terminal usage, Copilot usage events and Gemini model stats.
Tool inputs and outputs, reasoning, conversation histories and quoted provider
streams cannot supply counters. A complete Orbit answer envelope can declare
usage directly or on its immediate result object; deeper result data is ignored.
Claude's terminal usage rollup takes precedence over its duplicate model map and
retains the separate five-minute and one-hour cache creation counts. Raw Codex
tool events remain available to invocation tracing.

## Provider boundary tests

`./scripts/build-budget.py -- cargo test -p orbit-agent --test provider_invocation`
runs one integration binary on Unix. Recording shell executables exercise every
CLI adapter's public `Agent` invocation, including the internal mock provider.
`Provider::ALL` requires a CLI fixture or a structured unsupported-provider
error, including the persisted `openai_compat` provider value.

The CLI fixture launches descriptors with the shared
`orbit_common::security::child_env` allowlist, checking prompt delivery, model
and effort flags, admitted context, and exclusion of ambient credentials and
privilege variables. Each case runs in an isolated child with a disposable home
under `.orbit/tmp`, synthetic secrets, and bounded process waits. The fixtures
use `/bin/sh`, `/bin/cat` and `/usr/bin/env`; they need no installed provider,
external network or provider credentials. Engine-specific sandbox selection
and environment overrides remain engine responsibilities.

## Audit contracts

`loop_engine::audit` keeps the existing engine import path for `AuditSink`,
`LoopAuditEvent`, `InMemorySink`, `NullSink`, and the shared blob/redaction
re-exports. The event variants retain their schema-v1 `event_kind` tags and
payload fields for historical `v2_audit_events` rows, including session, HTTP,
tool, iteration and policy records. They support serialization and
deserialization; removing a runtime does not remove its persisted event shape.

The runtime's SQLite-backed sink lives in `orbit-engine`. Payload bodies are
stored separately under `.orbit/state/audit/blobs/`, keyed by their redacted
content hash. Blob writes use the common crate's redaction at write time,
before hashing and persistence. The boundary tests also verify sink redaction
and decode fixtures for every historical event variant.

The complete redaction contract lives in
[`artifact-redaction.md`](../../docs/design/auditability/specs/artifact-redaction.md).

## Dependency direction

`orbit-common`, `orbit-types` → `orbit-agent` → `orbit-engine`.
