# orbit-common

Mechanisms every layer needs: `OrbitError` plus responsibility-named modules (`error`, `fs`, `governance`, `migration`, `model`, `observability`, `process`, `protocol`, `security`, `storage`). No feature, runtime, or storage backend. `orbit-types` says what a thing is; this crate says how to do something with it.

- Never add a module named after a caller or a vertical feature. If code fits no existing responsibility, it belongs in the crate that needs it. `mod.rs` = declarations + `#[cfg(test)] mod tests;` only.
- Single-owner invariants — never reimplement at a call site:
  - `security::child_env` — the only agent-subprocess env builder; allowlist over a cleared environment.
  - `security::release` — the only Rust copy of release keys and manifest verification (`scripts/check-installer-pubkey.sh` guards drift with the shell/npm installers).
  - `security::redaction` — the only redaction implementation (guardrail script forbids surface-local `fn redact_*`).
  - `fs::selector` — owns `SelectorParseError` and its `OrbitError` translator; no caller crate may translate it.
  - `migration` — forward-only, read-time YAML migration; no rollback, no write-back. One-shot importers belong in `orbit-store::workflow`.
- `governance::operation` is the operations-as-data kernel; specs live here, handlers live in `orbit-core` joined by the verb enum. Keep it transport/runtime-agnostic: no clap, axum, or `OrbitRuntime` types. Every registry string is shipped contract.
- Features: `sqlite` gates `storage::sqlite`; `test-util` exposes `test_env`/`test_fixtures` — dev-dependencies only; `clap` forwards to `orbit-types/clap`. Gate code behind the flag, don't `#[allow]` it.
