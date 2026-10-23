//! Atomic filesystem primitives.
//!
//! Consolidates the variants that historically existed across the workspace:
//! - `orbit-core::fs_utils::atomic_write_text` (volatile)
//! - `orbit-store::file::fs_utils::write_atomic` (volatile, with separate flock helper)
//! - the former `orbit-knowledge` durable write (parent-dir fsync), since removed
//!
//! The durable variant is the canonical one: rename-into-place plus
//! parent-directory fsync so the rename itself is flushed. Volatile is
//! offered for hot paths where the caller accepts post-crash inconsistency.
//!
//! All functions return `io::Result`; callers map to their domain error type
//! (`OrbitError`, `KnowledgeError`, etc.) at the boundary. Keeping this
//! module domain-free preserves the `types::` / `utility::` split inside
//! `orbit-common`.

mod atomic;
mod lock;
mod private;

pub use super::file_lock::{
    DEFAULT_FILE_LOCK_TIMEOUT, FileLockGuard, FileLockHolderInfo, FileLockOptions, FileLockProbe,
    FileLockTimeout, LeaseFile, acquire_exclusive_file_lock, hold_step, probe_file_lock,
    read_file_lock_holder, try_acquire_exclusive_file_lock,
};
pub(crate) use atomic::{DURABLE_SYNC_HOLDS, atomic_write_private_bytes};
pub use atomic::{
    RELAXED_DURABILITY_ENV, StagedTextFile, atomic_write_bytes, atomic_write_text,
    atomic_write_text_volatile, durability_syncs_relaxed, durability_syncs_requested,
    relaxed_durability_requested, remove_path_if_exists, sqlite_synchronous, sync_file,
    sync_file_data, sync_parent_dir, write_new_private_text, write_text_with_parent,
};
pub use lock::{
    HeldFileLock, hold_exclusive_file_lock_options, with_exclusive_file_lock,
    with_exclusive_file_lock_named_options, with_exclusive_file_lock_options,
    with_shared_file_lock, with_shared_file_lock_options,
};
pub(crate) use lock::{classify_lock_io, classify_or_wrap_lock_io};
#[cfg(any(unix, windows))]
pub use private::create_dir_symlink;
pub(crate) use private::{
    append_private_file, create_new_private_file, open_private_file,
    set_private_file_permissions_for_open_file, write_access_error_message,
};
pub use private::{
    create_private_dir, create_private_dir_all, is_readonly_or_access_error,
    open_read_only_no_follow,
};
#[cfg(feature = "sqlite")]
pub(crate) use private::{open_regular_file_no_follow, set_private_file_permissions};

#[cfg(test)]
mod tests;
