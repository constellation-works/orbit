//! Owner admission foundation. Reached only through `orbit-core`'s trusted
//! owner seams — the `orbit.task.pull` tool and the owner-local drain adapter —
//! which supply the caller identity from the session, never from the request.

mod admit;
mod census;
mod footprint;
mod lookup;
mod refusal;
mod retention;

use super::TaskCommitBoundary;
use crate::contract::*;
pub(super) use lookup::decode_claim;
#[cfg(test)]
pub(super) use lookup::{claim_decodes, reset_claim_decodes};
use orbit_common::OrbitError;
use orbit_common::fs::selector::canonical_selector_in_workspace;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{Task, TaskComment, TaskHistoryEntry};
pub use refusal::admission_refusal;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The in-section re-check of assessment-scoped holds a caller gives
/// [`TaskCommitBoundary::admit_task`]: the candidate with the comments and
/// history read with it, to the hold's diagnostic, or `None` when it is free.
pub type ValidationHold<'a> =
    dyn Fn(&Task, &[TaskComment], &[TaskHistoryEntry]) -> Result<Option<String>, OrbitError> + 'a;

/// Owner callback invoked only for the selected task, before claim persistence.
pub type AdmissionGuidanceCapture<'a> =
    dyn Fn(&Task) -> Result<AdmissionTaskGuidance, OrbitError> + 'a;

/// What an owner supplies to [`TaskCommitBoundary::admit_task`] besides the
/// request and its ordering: the owner's version and workspace, and the holds
/// that defer a task.
///
/// `admission_holds` maps each task held by a live owner-local delivery, a
/// successful pilot preparation or a current pilot finding to its trusted
/// diagnostic. It is deferred even while it is still `backlog`: a local
/// drain's gate waiting for context locks has neither moved the task nor
/// reserved its footprint yet, so status and reservations alone would hand it
/// out a second time [ORB-13918].
///
/// `held` maps each `backlog` task the owner is withholding for a red base to
/// why [ORB-14258]. Its last delivery failed a required command the base
/// fails the same way; it is deferred until the held command passes on a new
/// base tip, as it would be on the owner's own drain.
///
/// `validation_hold` re-checks assessment-scoped holds inside the section for
/// each candidate about to be committed, given the comments and history of
/// the bundle read there; it must not read the task store itself. A pilot
/// apply that lands after the caller computed `admission_holds` therefore
/// cannot leave a newly held task claimed from an older snapshot.
#[derive(Clone, Copy)]
pub struct AdmissionContext<'a> {
    /// The only caller version the owner admits.
    pub owner_version: &'a str,
    /// The owner's repository root.
    pub repo_root: &'a Path,
    /// The owner's `.orbit` directory.
    pub orbit_dir: &'a Path,
    /// Tasks deferred by a live owner-local delivery or a pilot finding.
    pub admission_holds: &'a BTreeMap<String, String>,
    /// `backlog` tasks withheld for a red base.
    pub held: &'a BTreeMap<String, String>,
    /// The in-section re-check of assessment-scoped holds.
    pub validation_hold: &'a ValidationHold<'a>,
}

pub(super) const RECEIPT_KIND: &str = "distributed-admission-receipt-v1";
const CLAIM_KIND: &str = "distributed-execution-claim-v1";
pub const ADMISSION_RESERVATION_TTL_SECONDS: u32 = 14_400;

#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum StoredReceipt {
    Full {
        receipt: Box<AdmissionReceipt>,
    },
    Tombstone {
        machine_id: String,
        request_id: String,
        input_digest: String,
    },
}

fn encode<T: Serialize>(value: &T) -> Result<String, OrbitError> {
    serde_json::to_string(value).map_err(|e| OrbitError::Store(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, OrbitError> {
    serde_json::from_str(value).map_err(|e| OrbitError::Store(e.to_string()))
}
pub(super) fn digest<T: Serialize>(value: &T) -> Result<String, OrbitError> {
    Ok(sha256_hex(encode(value)?.as_bytes()))
}
fn receipt_key(machine: &str, request: &str) -> Result<String, OrbitError> {
    digest(&(machine, request))
}
fn row<T: Serialize>(kind: &str, id: &str, value: &T) -> Result<TaskCoordinationRow, OrbitError> {
    Ok(TaskCoordinationRow {
        kind: kind.into(),
        row_id: id.into(),
        payload_json: encode(value)?,
    })
}
pub(super) fn canonical_footprint(
    files: &[String],
    root: &Path,
) -> Result<Vec<String>, OrbitError> {
    files
        .iter()
        .map(|f| {
            canonical_selector_in_workspace(f, root)
                .map_err(|e| OrbitError::InvalidInput(format!("invalid context selector {f}: {e}")))
        })
        .collect::<Result<BTreeSet<_>, _>>()
        .map(|files| files.into_iter().collect())
}
