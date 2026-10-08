# orbit-agent

Provider CLI runtimes and audit contracts for Orbit. Provider adapters drive
`claude`, `codex`, `copilot`, `cursor-agent`, `gemini`, `agy`, `grok`, `ollama`,
`opencode`, `pi`, and `mock-agent` through subprocess command descriptors.
The engine runs those descriptors through `orbit-exec`; stdout helpers project
provider answers into Orbit response envelopes and diagnostics.

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
