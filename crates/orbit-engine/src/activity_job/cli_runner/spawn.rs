//! Compatibility re-exports of the crate-root `sandbox_spawn` leaf for
//! `cli_runner` internals. Migrating callers and retiring this shim is a
//! follow-up.

#[cfg(feature = "test-support")]
pub use crate::sandbox_spawn::with_bwrap_probe_unavailable;
#[cfg(all(test, target_os = "linux"))]
pub(super) use crate::sandbox_spawn::with_post_run_guard_without_user_namespace;
pub(super) use crate::sandbox_spawn::{
    CODEX_CA_CERTIFICATE_ENV, PreparedSandbox, SSL_CERT_FILE_ENV, SpawnError, SpawnedChild,
    linux_bwrap_mask, prepare_sandbox_for_dispatch, spawn_child_with_optional_sandbox,
};
