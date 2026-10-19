//! Bounded advisory file-lock acquisition and holder diagnostics.

mod acquisition;
mod hold;
mod holders;
mod lease;
mod options;

pub(crate) use acquisition::acquire_shared_file_lock;
pub use acquisition::{
    FileLockGuard, FileLockProbe, acquire_exclusive_file_lock, probe_file_lock,
    try_acquire_exclusive_file_lock,
};
pub use hold::hold_step;
#[cfg(test)]
pub(crate) use holders::read_file_lock_holder_after_resolve;
pub use holders::{FileLockHolderInfo, read_file_lock_holder};
pub use lease::LeaseFile;
pub use options::{DEFAULT_FILE_LOCK_TIMEOUT, FileLockOptions, FileLockTimeout};
