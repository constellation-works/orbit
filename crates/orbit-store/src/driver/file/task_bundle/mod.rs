//! Pure task-bundle file persistence: codecs, atomic publication, and bundle
//! stable lock identity. Registry coordination lives in the task repository.

pub(crate) mod bundle_io;
mod lock;
mod migrations;
mod types;

pub(crate) use bundle_io::{
    PENDING_WRITE_FILE_NAME, PendingWriteGuard, append_jsonl_row,
    cleanup_partial_bundle_best_effort, is_unpublished_stub, publish_envelope, read_bundle_at,
    read_bundle_lightweight_at, read_envelope_at, read_search_docs_at, reap_unpublished_stub,
    recover_pending_bundle_at, recover_pending_write, replace_bundle_at, truncate_jsonl_file,
    write_bundle_at, write_bundle_with_artifacts_at,
};

pub(crate) use lock::bundle_lock_target;
pub(crate) use types::{TaskBundleV2, TaskDocumentV2, TaskSearchDocs};
