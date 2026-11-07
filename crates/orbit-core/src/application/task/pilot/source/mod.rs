//! Landing-branch source snapshot for task-pilot prepare/apply.
//!
//! Inspection and deterministic selector validation share one pinned revision
//! so a newly merged target is not reported as missing when the primary
//! checkout lags origin. Fetch writes only objects, a short-lived private ref
//! and (under the shared fetch lock) the remote-tracking ref; the primary
//! HEAD, index and working files are never aligned or otherwise modified.

mod admission;
mod git;
mod snapshot;

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
pub(super) use git::fetch_origin_branch;
pub(crate) use git::{is_commit_id, tree_of};
pub(crate) use snapshot::{
    GitPathKind, INSPECTION_PROOF_KEY, SourceSnapshot, inspection_proof_error,
    inspection_proof_required, requested_base_branch, resolve_source_snapshot,
};
