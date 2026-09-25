//! On-disk task bundle I/O.
//!
//! `write` stages and publishes whole bundles, `read` loads and validates
//! them, `jsonl` owns the append-only sidecars, `artifacts` the manifest and
//! blobs, `stub` unpublished-stub cleanup, and `commit` the multi-file update
//! protocol.

mod artifacts;
mod commit;
mod jsonl;
mod read;
mod stub;
mod write;

// `commit` reaches these through `super::`.
use super::types::TaskBundleV2;
use jsonl::scan_jsonl_records;
use read::read_required_text;

pub(crate) use artifacts::copy_artifact_blobs;
#[cfg(test)]
pub(crate) use artifacts::take_artifact_payload_reads;
#[cfg(test)]
pub(crate) use commit::inject_bundle_write_faults;
pub(crate) use commit::{
    BundleWriteFault, PENDING_WRITE_FILE_NAME, PendingWriteGuard, fail_if_injected,
    publish_envelope, recover_pending_bundle_at, truncate_jsonl_file,
};
pub(crate) use jsonl::append_jsonl_row;
pub(crate) use read::{read_bundle_at, read_bundle_lightweight_at, read_envelope_at};
pub use stub::is_unpublished_stub;
pub(crate) use stub::reap_unpublished_stub;
pub(crate) use write::{
    cleanup_partial_bundle_best_effort, replace_bundle_at, write_bundle_at,
    write_bundle_with_artifacts_at,
};

#[cfg(test)]
mod tests;
