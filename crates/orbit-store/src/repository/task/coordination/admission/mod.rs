//! Owner admission foundation. Reached only through `orbit-core`'s trusted
//! owner seams — the `orbit.task.pull` tool and the owner-local drain adapter —
//! which supply the caller identity from the session, never from the request.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::selector::canonical_selector_in_workspace;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{Task, TaskComment, TaskHistoryEntry};
use serde::{Deserialize, Serialize};

use super::TaskCommitBoundary;
use crate::contracts::*;

mod admit;
mod census;
mod footprint;
mod lookup;
mod refusal;
mod retention;

pub use refusal::admission_refusal;

/// The in-section re-check of assessment-scoped holds a caller gives
/// [`TaskCommitBoundary::admit_task`]: the candidate with the comments and
/// history read with it, to the hold's diagnostic, or `None` when it is free.
pub type ValidationHold<'a> =
    dyn Fn(&Task, &[TaskComment], &[TaskHistoryEntry]) -> Result<Option<String>, OrbitError> + 'a;

/// Owner callback invoked only for the selected task, before claim persistence.
pub type AdmissionGuidanceCapture<'a> =
    dyn Fn(&Task) -> Result<AdmissionTaskGuidance, OrbitError> + 'a;

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
