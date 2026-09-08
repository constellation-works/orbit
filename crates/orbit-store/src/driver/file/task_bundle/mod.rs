//! Pure task-bundle file persistence: codecs, atomic publication, and bundle
//! lock-sentinel mechanics. Registry and checkout projection coordination live
//! in the task repository.

pub(crate) mod bundle_io;
mod lock;
mod migrations;
mod types;

pub(crate) use bundle_io::{
    BundleWriteFault, PendingWriteGuard, append_jsonl_row, cleanup_partial_bundle_best_effort,
    fail_if_injected, publish_envelope, read_bundle_at, read_envelope_at,
    recover_pending_bundle_at, write_bundle_at, write_bundle_with_artifacts_at,
};

#[cfg(test)]
pub(crate) use bundle_io::{PENDING_WRITE_FILE_NAME, inject_bundle_write_faults};
pub(crate) use lock::{remove_task_bundle_lock_sentinel, task_bundle_lock_sentinel_path};
pub(crate) use types::{TaskBundleV2, TaskDocumentV2};
