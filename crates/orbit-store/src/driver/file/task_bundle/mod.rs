//! Pure task-bundle file persistence: codecs, atomic publication, and bundle
//! stable lock identity. Registry coordination lives in the task repository.

pub(crate) mod bundle_io;
mod lock;
mod migrations;
mod types;

pub(crate) use bundle_io::{
    BundleWriteFault, PENDING_WRITE_FILE_NAME, PendingWriteGuard, append_jsonl_row,
    cleanup_partial_bundle_best_effort, fail_if_injected, publish_envelope, read_bundle_at,
    read_bundle_lightweight_at, read_envelope_at, recover_pending_bundle_at, replace_bundle_at,
    write_bundle_at, write_bundle_with_artifacts_at,
};

#[cfg(test)]
pub(crate) use bundle_io::{inject_bundle_write_faults, take_artifact_payload_reads};
pub(crate) use lock::bundle_lock_target;
pub(crate) use types::{TaskBundleV2, TaskDocumentV2};
