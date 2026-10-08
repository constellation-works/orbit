//! Host-only authority for rebase-recovery evidence.
//!
//! A managed leaf is granted modify access to the shared run store
//! (`<global>/orbit.db` and its WAL/SHM sidecars) so that `orbit.task.*` and
//! audit tools keep working inside the sandbox. That grant also covers the run
//! row holding `PipelineState::rebase_recovery_checkpoints`, so those bytes are
//! progress data, not authority: a leaf can rewrite them.
//!
//! This module owns the separate durable record that *is* authority. It lives
//! in its own SQLite database under a root that appears in no leaf write grant,
//! and [`append_recovery_authority_denies`] appends an explicit deny for that
//! root after every convenience grant, the same shape
//! [`crate::runtime::git_sandbox`] uses for Git metadata.
//!
//! Confinement here is by *location*, not by a secret. Bubblewrap mounts the
//! host filesystem `--ro-bind / /` and enforces only write boundaries, so a key
//! file would be readable by every leaf and a keyed MAC would buy nothing.

mod certificate;
mod error;
mod root;
mod worker;

pub(crate) use certificate::RecoveryAuthority;
pub(crate) use root::append_recovery_authority_denies;
pub(crate) use worker::current_worker_binding;
#[cfg(target_os = "linux")]
pub(crate) use worker::{PROC_ROOT, namespace_key, worker_namespace_leader};

#[cfg(test)]
mod tests;
