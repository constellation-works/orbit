# Machine checks — STD-05 Security boundaries

The gate that enforces each rule in [STD-05](STD-05.md), or `review-only`. Ship a change
with the gate its rule names, or say it is review-only.

Each gate below is real and can be adopted. "Orbit:" says what Orbit runs today.

| Rule | Gate |
|---|---|
| R1 | review-only. Add a test in which the child unsets or forges every environment variable and label, and assert it is refused or treated as an ordinary caller (Orbit: `crates/orbit-core/src/adapter/command/dispatch/tests/callback.rs`, `crates/orbit-cli/src/tests/plugin_callback_surface.rs`). |
| R2 | Test: a caller claims an identity, and the authenticated field is unchanged (Orbit: `audit_event_store/tests/self_reported_actor.rs`). |
| R3 | Test: call a governed operation from every surface, including an unlisted one, and assert the same decision (Orbit: `crates/orbit-common/src/governance/tests/authorization.rs`, `crates/orbit-core/src/runtime/tests/authorization.rs`). |
| R4 | Test: using the override writes an audit row with distinct provenance. Otherwise review-only. |
| R5 | review-only |
| R6 | Test: symlink fixtures, including a symlinked ancestor with a missing tail and a dangling link, must be refused (Orbit: `crates/orbit-exec/src/tests/path_identity.rs`). |
| R7 | Same symlink fixtures, asserting nothing is written outside the root. Otherwise review-only. |
| R8 | Test: create state under a permissive umask (`0002`) and assert `0600`/`0700` (Orbit: `crates/orbit-common/src/storage/tests/sqlite.rs`). |
| R9 | Test: a symlinked or group-writable state file is refused or repaired (Orbit: `private_open_rejects_symlink_database`, `private_open_repairs_existing_database_and_sidecars`). Runtime: a `doctor` check for group- or world-writable state (Orbit: `orbit doctor`). Checking the owning UID is review-only. |
| R10 | Test: build the child environment from a stated parent snapshot and assert benignly named credentials are absent (Orbit: `crates/orbit-common/src/security/tests/child_env.rs`). |
| R11 | Same test, with the privilege-bearing names listed in the pass list. |
| R12 | review-only. A secret scanner does not see runtime argv. |
| R13 | Rust: an exhaustive `match` over every persisting action with no `_` arm, so a new writer fails to compile until it is classified (Orbit: `policy_for_action`). Test: each covered family redacts (Orbit: `crates/orbit-core/src/adapter/tool_host/tests/artifact_redaction.rs`). Other languages: an inventory test that enumerates writers. |
| R14 | Test: ordinary words and hyphenated identifiers survive redaction (Orbit: `ordinary_words_are_not_redactable_env_values` in `crates/orbit-common/src/security/tests/redaction.rs`). |
| R15 | review-only. Optional: a secret scanner such as `gitleaks` over the tool's state directory in a test run. It also backs CONVENTIONS "Never commit secrets" in CI or pre-commit. Orbit: none configured. |
| R16 | Test: a forged `Host` on every route, health included, is refused (Orbit: `crates/orbit-web/src/api/tests/origin.rs`, `healthz_require_localhost_origin_rejects_forged_host_detailed`). |
| R17 | Test: cross-origin `POST` and prefix-matching origins are refused (Orbit: `require_localhost_origin_rejects_prefix_match` and siblings). |
| R18 | Test: send a browser-style HTTP `POST` whose body holds valid protocol lines, and assert nothing is dispatched (Orbit: `loopback_listener_closes_http_before_dispatching_post_body` in `crates/orbit-mcp/tests/mcp_wire_roundtrip.rs`). |
| R19 | Test: a non-loopback bind is refused without an explicit opt-in (Orbit: `crates/orbit-mcp/src/tests/listener.rs`). |
| R20 | review-only. Optional: run the CLI test suite in a network namespace with no route (`unshare -rn`), so an unexpected request fails. Orbit: not configured. |
| R21 | Test: rewrite an approved manifest and assert the grant no longer applies (Orbit: `a_rewritten_manifest_is_registered_inactive_naming_both_digests` in `crates/orbit-core/src/runtime/plugin/tests/host.rs`). |
| R22 | Test: register a colliding name and assert refusal, not replacement. Otherwise review-only. |
| R23 | `cargo build --locked` (or `npm ci`, `pip install --require-hashes`); `cargo deny check sources` with `unknown-registry = "deny"` and `unknown-git = "deny"`; `sha256sum -c` on every downloaded tool. Pinning CI actions by SHA is review-only unless a linter enforces it (optional: `zizmor`). Orbit: all of these except the linter. |
| R24 | `cargo deny check advisories` in CI (Orbit: `make audit`, `scripts/ci-guardrails.sh`); Dependabot or an equivalent for each ecosystem; a digest check for vendored files (Orbit: `scripts/check-dashboard-vendor.py` in `make ci-fast`). |
| R25 | review-only. Rust: `clippy::disallowed_methods` in `clippy.toml` can ban non-cryptographic generators in token code. Orbit: not configured. |
