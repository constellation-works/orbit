# Exemplars — STD-05 Security boundaries

Worked examples of the rules in [STD-05](STD-05.md), as pointers into the Orbit
codebase. Orbit is the example, not the audience: this file is optional reading, and
useful only with an Orbit checkout.

Paths are relative to `codebases/orbit/`.

- R1 — `crates/orbit-tools/src/plugin/callback/mod.rs` (module doc: identity rides on a
  host-written descriptor); `crates/orbit-tools/src/plugin/callback/resolution.rs#credential_from_descriptor`
  (the descriptor must resolve to the same inode as the host's named record);
  `docs/design/federated-mcp/4_decisions.md`.
- R2 — `docs/design/auditability/specs/actor-identity.md`;
  `crates/orbit-store/src/driver/sqlite/audit_event_store/tests/self_reported_actor.rs`.
- R3 — `crates/orbit-common/src/governance/authorization.rs` (module doc, "Placement is not
  permission"; `GOVERNED_OPERATIONS` and `authorize`).
- R4 — `crates/orbit-common/src/governance/authorization.rs#OPERATOR_OVERRIDE_ENV`
  (audited as `CallerProvenance::OperatorOverride`).
- R5 — `crates/orbit-common/src/governance/authorization.rs` (module doc, "What this is, and
  what it is not"); `docs/design/operations-as-data/4_decisions.md`.
- R6 — `crates/orbit-exec/src/path_identity.rs#physical_with_missing_tail`;
  `crates/orbit-policy/src/engine.rs` (nearest-existing-ancestor canonicalization);
  `SECURITY.md#Sandbox model and known limits` ("Symlinks");
  `docs/design/policy-sandbox/specs/fs-profile-resolution.md`.
- R7 — `crates/orbit-exec/src/path_identity.rs#create_write_root`;
  `crates/orbit-common/src/fs/io.rs#open_read_only_no_follow`;
  `crates/orbit-exec/src/linux_sandbox/write_grants.rs#inspect_host_owned_anchor` (refuses a
  symlinked anchor).
- R8 — `crates/orbit-common/src/fs/io.rs#create_private_dir_all` and `#create_new_private_file`
  (`PRIVATE_FILE_MODE`, `PRIVATE_DIR_MODE`); `crates/orbit-common/src/storage/sqlite.rs#open_private`.
- R9 — `crates/orbit-common/src/storage/sqlite.rs#open_private` (repairs existing files to
  owner-only, `SQLITE_OPEN_NOFOLLOW`) and `#validated_sqlite_path`;
  `crates/orbit-cli/src/command/doctor.rs` (group- or world-writable state directory check).
- R10 — `crates/orbit-common/src/security/child_env.rs#allowlisted_child_env_from`
  (`AGENT_SUBPROCESS_BASELINE_VARS`); `docs/DATA_HANDLING.md#Credentials and secrets`.
- R11 — `crates/orbit-common/src/security/child_env.rs#is_privilege_bearing_orbit_name`.
- R12 — `crates/orbit-tools/src/plugin/callback/session.rs#PLUGIN_CALLBACK_FD` (the credential
  travels as an inherited descriptor, not argv or env).
- R13 — `crates/orbit-core/src/adapter/tool_host/artifact_redaction.rs#policy_for_action` and
  `#is_covered_mutating_action`; `docs/design/auditability/specs/artifact-redaction.md`.
- R14 — `crates/orbit-common/src/security/redaction.rs#redact_sensitive_env_text` and
  `#is_redactable_value`.
- R15 — `docs/POSITIONING.md#Non-negotiables`; `docs/DATA_HANDLING.md#Credentials and secrets`.
- R16 — `crates/orbit-web/src/api/origin.rs#require_localhost_origin` (gate 1, `Host`).
- R17 — `crates/orbit-web/src/api/origin.rs#require_localhost_origin` (gate 2, `Origin`) and
  `#localhost_origin_matches_authority`.
- R18 — `crates/orbit-mcp/src/listener.rs#serve_connection` (first-byte peek) and the module doc.
- R19 — `crates/orbit-mcp/src/listener.rs#ensure_bind_allowed` (`ListenerExposure`);
  `docs/design/remote-access/4_decisions.md`.
- R20 — `docs/DATA_HANDLING.md#What leaves the machine`; `docs/POSITIONING.md#What Orbit is NOT for`.
- R21 — `crates/orbit-core/src/runtime/plugin/host.rs#digest_mismatch_diagnostic`;
  `crates/orbit-tools/src/plugin/loader/load.rs` (`manifest_digest` computed from the bytes
  read); `docs/design/plugins/1_scope.md`.
- R22 — `crates/orbit-core/src/runtime/plugin/host.rs` (module doc: a colliding namespace is
  registered inactive); `crates/orbit-core/src/runtime/plugin/discovery.rs#host_plugin_registry`.
- R23 — `.github/workflows/ci.yml` (actions pinned by SHA; `CARGO_DENY_LINUX_SHA256` checked
  with `sha256sum -c`); `deny.toml#[sources]`; `crates/orbit-common/src/security/release.rs#verify_checksum_signature`.
- R24 — `.github/dependabot.yml`; `crates/orbit-web/assets/dashboard/vendor/VENDOR.md`,
  `crates/orbit-web/assets/dashboard/vendor/vendor-manifest.json` and
  `scripts/check-dashboard-vendor.py`; `deny.toml#[advisories]`.
- R25 — `crates/orbit-tools/src/plugin/callback/storage.rs#random_token` (`getrandom::fill`).
